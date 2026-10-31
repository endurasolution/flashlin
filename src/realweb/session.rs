//! One RealWeb connection: the `hello` handshake, then a single task that
//! multiplexes inbound frames, queued channel messages, heartbeats and token
//! expiry. Replies to the client's own requests are written directly; channel
//! messages arrive through a bounded queue and are written in batches with
//! one flush per batch.

use std::borrow::Cow;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use serde_json::value::RawValue;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use super::token::{self, TokenError};
use super::{any_matches, hub, valid_channel, HubError, Retention, Subscriber};
use crate::config::parser::RealWebConfig;
use crate::config::{parse_duration_secs, parse_size};

/// Close codes (4000–4999 are reserved for applications by RFC 6455).
pub mod close {
    pub const PROTOCOL_ERROR: u16 = 4000;
    pub const TOKEN_EXPIRED: u16 = 4001;
    pub const SLOW_CONSUMER: u16 = 4002;
    pub const AUTH_FAILED: u16 = 4003;
    pub const HEARTBEAT_TIMEOUT: u16 = 4004;
    pub const RATE_LIMITED: u16 = 4008;
}

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Queued messages written before one flush.
const MAX_BATCH: usize = 256;

/// The `[realweb]` values a connection needs, resolved once at the handshake.
pub struct Settings {
    secret: Option<Arc<[u8]>>,
    public_channels: Vec<String>,
    max_message: usize,
    max_channels: usize,
    rate: f64,
    heartbeat: Duration,
    send_queue: usize,
    retention: Retention,
}

impl Settings {
    pub fn from_config(rw: &RealWebConfig) -> Self {
        Settings {
            secret: rw.secret.as_ref().map(|s| Arc::from(s.as_bytes())),
            public_channels: rw.public_channels.clone(),
            max_message: parse_size(&rw.max_message_size).unwrap_or(64 << 10) as usize,
            max_channels: rw.max_channels,
            rate: f64::from(rw.messages_per_second),
            heartbeat: Duration::from_secs(parse_duration_secs(&rw.heartbeat).unwrap_or(25)),
            send_queue: rw.send_queue,
            retention: Retention::from_config(rw),
        }
    }
}

#[derive(Deserialize)]
struct Frame<'a> {
    #[serde(borrow)]
    t: Cow<'a, str>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default, borrow)]
    ch: Option<Cow<'a, str>>,
    #[serde(default)]
    since: Option<u64>,
    #[serde(default, borrow)]
    epoch: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    token: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    data: Option<&'a RawValue>,
}

/// What a connection may do, from its token (or none, for anonymous clients).
#[derive(Default)]
struct Auth {
    uid: Option<Arc<str>>,
    subscribe: Vec<String>,
    publish: Vec<String>,
    expires: Option<u64>,
}

fn authenticate(token: Option<&str>, settings: &Settings) -> Result<Auth, &'static str> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(Auth::default());
    };
    let Some(secret) = settings.secret.as_deref() else {
        return Err("tokens_not_enabled");
    };
    let claims = token::verify(token, secret, token::unix_now()).map_err(TokenError::code)?;
    Ok(Auth {
        uid: claims.sub.map(Arc::from),
        subscribe: claims.subscribe,
        publish: claims.publish,
        expires: Some(claims.exp),
    })
}

/// When the connection must close because its token has expired.
fn expiry_deadline(expires: Option<u64>) -> Instant {
    let remaining = match expires {
        Some(exp) => (exp + token::LEEWAY_SECS).saturating_sub(token::unix_now()),
        None => 365 * 24 * 3600,
    };
    Instant::now() + Duration::from_secs(remaining)
}

enum End {
    /// The peer went away or the socket failed; nothing more to send.
    Gone,
    Close(u16, &'static str),
}

struct Session<S> {
    id: u64,
    sink: SplitSink<WebSocketStream<S>, Message>,
    settings: Settings,
    auth: Auth,
    tx: mpsc::Sender<Utf8Bytes>,
    overflow: Arc<Notify>,
    subscriptions: HashSet<String>,
    tokens: f64,
    refilled: Instant,
}

pub async fn run<S>(io: S, settings: Settings, remote: SocketAddr)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let config = WebSocketConfig::default()
        .max_message_size(Some(settings.max_message))
        .max_frame_size(Some(settings.max_message));
    let ws = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
    let (sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::channel(settings.send_queue);
    let overflow = Arc::new(Notify::new());
    let mut session = Session {
        id: hub().next_connection_id(),
        sink,
        auth: Auth::default(),
        tx,
        overflow: overflow.clone(),
        subscriptions: HashSet::new(),
        tokens: settings.rate * 2.0,
        refilled: Instant::now(),
        settings,
    };

    let end = match session.hello(&mut stream).await {
        Ok(()) => session.serve(&mut stream, &mut rx, &overflow).await,
        Err(end) => end,
    };
    for channel in &session.subscriptions {
        hub().unsubscribe(channel, session.id);
    }
    tracing::debug!(ip = %remote, sid = session.id, "RealWeb connection closed");
    let Ok(mut ws) = session.sink.reunite(stream) else {
        return;
    };
    let farewell = async {
        // Sends our Close frame, or flushes the reply to the client's.
        let _ = ws
            .close(match end {
                End::Close(code, reason) => Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: Utf8Bytes::from_static(reason),
                }),
                End::Gone => None,
            })
            .await;
        // Lingering close: stop sending, then discard input until the client
        // hangs up. Closing a socket that still holds unread input (frames a
        // flooding client sent meanwhile, the rest of an oversized message)
        // makes TCP reset it, which can destroy the Close frame — and with it
        // the reason — before the client reads it.
        let io = ws.get_mut();
        let _ = io.shutdown().await;
        let mut discard = [0u8; 4096];
        while matches!(io.read(&mut discard).await, Ok(n) if n > 0) {}
    };
    let _ = tokio::time::timeout(Duration::from_secs(3), farewell).await;
}

impl<S> Session<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    async fn hello(
        &mut self,
        stream: &mut futures_util::stream::SplitStream<WebSocketStream<S>>,
    ) -> Result<(), End> {
        let first = tokio::time::timeout(HELLO_TIMEOUT, async {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Text(text))) => return Some(text),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => return None,
                }
            }
        })
        .await;
        let Ok(Some(text)) = first else {
            return Err(End::Close(close::PROTOCOL_ERROR, "hello_required"));
        };
        let frame = match serde_json::from_str::<Frame>(&text) {
            Ok(frame) if frame.t == "hello" => frame,
            _ => return Err(End::Close(close::PROTOCOL_ERROR, "hello_required")),
        };
        self.auth = authenticate(frame.token.as_deref(), &self.settings)
            .map_err(|code| End::Close(close::AUTH_FAILED, code))?;
        self.reply(json!({
            "t": "welcome",
            "v": 1,
            "sid": self.id,
            "uid": self.auth.uid.as_deref(),
            "exp": self.auth.expires,
            "heartbeat": self.settings.heartbeat.as_secs(),
            "max_message": self.settings.max_message,
        }))
        .await
    }

    async fn serve(
        &mut self,
        stream: &mut futures_util::stream::SplitStream<WebSocketStream<S>>,
        rx: &mut mpsc::Receiver<Utf8Bytes>,
        overflow: &Notify,
    ) -> End {
        let heartbeat = self.settings.heartbeat;
        let mut ticker = tokio::time::interval_at(Instant::now() + heartbeat, heartbeat);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_seen = Instant::now();
        let mut expires = self.auth.expires;
        let expiry = tokio::time::sleep_until(expiry_deadline(expires));
        tokio::pin!(expiry);

        loop {
            tokio::select! {
                biased;
                _ = overflow.notified() => {
                    hub().record_slow_disconnect();
                    return End::Close(close::SLOW_CONSUMER, "slow_consumer");
                }
                Some(frame) = rx.recv() => {
                    if self.sink.feed(Message::Text(frame)).await.is_err() {
                        return End::Gone;
                    }
                    for _ in 1..MAX_BATCH {
                        let Ok(frame) = rx.try_recv() else { break };
                        if self.sink.feed(Message::Text(frame)).await.is_err() {
                            return End::Gone;
                        }
                    }
                    if self.sink.flush().await.is_err() {
                        return End::Gone;
                    }
                }
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        if let Err(end) = self.handle(&text).await {
                            return end;
                        }
                        if self.auth.expires != expires {
                            expires = self.auth.expires;
                            expiry.as_mut().reset(expiry_deadline(expires));
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
                        last_seen = Instant::now();
                    }
                    Some(Ok(Message::Binary(_))) => {
                        return End::Close(CloseCode::Unsupported.into(), "binary_not_supported");
                    }
                    Some(Err(WsError::Capacity(_))) => {
                        return End::Close(CloseCode::Size.into(), "message_too_big");
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return End::Gone,
                },
                _ = ticker.tick() => {
                    if last_seen.elapsed() > heartbeat * 2 + Duration::from_secs(5) {
                        return End::Close(close::HEARTBEAT_TIMEOUT, "heartbeat_timeout");
                    }
                    if self.sink.send(Message::Ping(Bytes::new())).await.is_err() {
                        return End::Gone;
                    }
                }
                _ = &mut expiry => return End::Close(close::TOKEN_EXPIRED, "token_expired"),
            }
        }
    }

    async fn reply(&mut self, value: serde_json::Value) -> Result<(), End> {
        self.sink
            .send(Message::Text(value.to_string().into()))
            .await
            .map_err(|_| End::Gone)
    }

    async fn error(&mut self, id: Option<u64>, code: &str, msg: &str) -> Result<(), End> {
        self.reply(json!({ "t": "err", "id": id, "code": code, "msg": msg }))
            .await
    }

    /// Token bucket of `messages_per_second`, burst twice that. Excess frames
    /// are refused; a client that keeps flooding far past the burst is closed.
    fn admit(&mut self) -> Result<bool, End> {
        let capacity = (self.settings.rate * 2.0).max(1.0);
        let now = Instant::now();
        let elapsed = now.duration_since(self.refilled).as_secs_f64();
        self.refilled = now;
        self.tokens = (self.tokens + elapsed * self.settings.rate).min(capacity) - 1.0;
        if self.tokens < -capacity {
            return Err(End::Close(close::RATE_LIMITED, "rate_limited"));
        }
        Ok(self.tokens >= 0.0)
    }

    fn can_subscribe(&self, channel: &str) -> bool {
        any_matches(&self.settings.public_channels, channel)
            || any_matches(&self.auth.subscribe, channel)
    }

    async fn handle(&mut self, text: &str) -> Result<(), End> {
        let Ok(frame) = serde_json::from_str::<Frame>(text) else {
            return self
                .error(None, "bad_request", "expected a JSON object with \"t\"")
                .await;
        };
        if !self.admit()? {
            return self
                .error(frame.id, "rate_limited", "too many messages; slow down")
                .await;
        }
        let id = frame.id;
        if frame.t == "ping" {
            return self.reply(json!({ "t": "pong", "id": id })).await;
        }
        if frame.t == "auth" {
            return self.reauthenticate(id, frame.token.as_deref()).await;
        }
        let channel = match frame.ch.as_deref() {
            Some(ch) if valid_channel(ch) => ch,
            _ if matches!(&*frame.t, "sub" | "unsub" | "pub" | "presence") => {
                return self
                    .error(
                        id,
                        "bad_channel",
                        "channel names are 1-128 of A-Z a-z 0-9 - _ . : / @",
                    )
                    .await;
            }
            _ => return self.error(id, "unknown_type", "unknown frame type").await,
        };
        match &*frame.t {
            "sub" => {
                if !self.can_subscribe(channel) {
                    return self
                        .error(id, "forbidden", "not allowed to subscribe")
                        .await;
                }
                if !self.subscriptions.contains(channel)
                    && self.subscriptions.len() >= self.settings.max_channels
                {
                    return self.error(id, "limit", "too many subscriptions").await;
                }
                let subscriber = Subscriber {
                    tx: self.tx.clone(),
                    uid: self.auth.uid.clone(),
                    overflow: self.overflow.clone(),
                };
                let resume = frame
                    .since
                    .map(|since| (since, frame.epoch.as_deref().unwrap_or("")));
                let subscribed = match hub().subscribe(channel, self.id, subscriber, resume) {
                    Ok(subscribed) => subscribed,
                    Err(HubError::TooManyChannels) => {
                        return self
                            .error(id, "limit", "server channel limit reached")
                            .await;
                    }
                };
                self.subscriptions.insert(channel.to_owned());
                self.reply(json!({
                    "t": "ok", "id": id, "ch": channel, "seq": subscribed.seq,
                    "epoch": &*subscribed.epoch, "gap": subscribed.gap,
                    "replayed": subscribed.replay.len(),
                }))
                .await?;
                for frame in subscribed.replay {
                    self.sink
                        .feed(Message::Text(frame))
                        .await
                        .map_err(|_| End::Gone)?;
                }
                self.sink.flush().await.map_err(|_| End::Gone)
            }
            "unsub" => {
                if self.subscriptions.remove(channel) {
                    hub().unsubscribe(channel, self.id);
                }
                self.reply(json!({ "t": "ok", "id": id, "ch": channel }))
                    .await
            }
            "pub" => {
                if !any_matches(&self.auth.publish, channel) {
                    return self.error(id, "forbidden", "not allowed to publish").await;
                }
                let Some(data) = frame.data else {
                    return self.error(id, "bad_request", "\"data\" is required").await;
                };
                let uid = self.auth.uid.clone();
                match hub().publish(channel, data, uid.as_deref(), self.settings.retention) {
                    Ok(published) => {
                        self.reply(
                            json!({ "t": "ok", "id": id, "ch": channel, "seq": published.seq }),
                        )
                        .await
                    }
                    Err(HubError::TooManyChannels) => {
                        self.error(id, "limit", "server channel limit reached")
                            .await
                    }
                }
            }
            "presence" => {
                if !self.can_subscribe(channel) {
                    return self
                        .error(id, "forbidden", "not allowed to see this channel")
                        .await;
                }
                let (count, users) = hub().presence(channel);
                let users: Vec<&str> = users.iter().map(|u| &**u).collect();
                self.reply(
                    json!({ "t": "ok", "id": id, "ch": channel, "count": count, "users": users }),
                )
                .await
            }
            _ => self.error(id, "unknown_type", "unknown frame type").await,
        }
    }

    /// `{"t":"auth","token":...}` replaces the connection's permissions before
    /// its token expires. The user id may not change; subscriptions the new
    /// token no longer allows are ended with a `kicked` frame.
    async fn reauthenticate(&mut self, id: Option<u64>, token: Option<&str>) -> Result<(), End> {
        let auth = match authenticate(token, &self.settings) {
            Ok(auth) if auth.expires.is_some() => auth,
            Ok(_) => return self.error(id, "bad_request", "\"token\" is required").await,
            Err(code) => return self.error(id, code, "token rejected").await,
        };
        if auth.uid != self.auth.uid {
            return self
                .error(
                    id,
                    "uid_mismatch",
                    "a refreshed token must be for the same user",
                )
                .await;
        }
        self.auth = auth;
        let revoked: Vec<String> = self
            .subscriptions
            .iter()
            .filter(|ch| !self.can_subscribe(ch))
            .cloned()
            .collect();
        for channel in revoked {
            self.subscriptions.remove(&channel);
            hub().unsubscribe(&channel, self.id);
            self.reply(json!({ "t": "kicked", "ch": channel, "reason": "forbidden" }))
                .await?;
        }
        self.reply(json!({ "t": "ok", "id": id, "exp": self.auth.expires }))
            .await
    }
}
