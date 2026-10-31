//! RealWeb — FlashLin's built-in realtime hub.
//!
//! Clients connect over a standard WebSocket (subprotocol `realweb.v1`), so it
//! works in every browser and through every proxy that carries WebSockets. On
//! top of that transport the hub adds what plain WebSockets leave to each
//! application:
//!
//! * authentication with signed, expiring tokens (HS256 JWTs) and per-channel
//!   subscribe/publish permissions, plus an Origin allowlist at the handshake;
//! * many named channels multiplexed over one connection;
//! * per-channel sequence numbers and retained history, so a client that
//!   reconnects resumes exactly where it stopped, or is told it missed data;
//! * acknowledged publishes, presence, heartbeats in both directions;
//! * bounded memory: message size, rate and per-connection send-queue limits,
//!   with stalled consumers disconnected instead of buffered without end;
//! * `POST /flashlin/realweb/publish` so any backend can push with one HTTP call.
//!
//! Each published message is serialized once and shared by reference with
//! every subscriber's queue. The wire protocol is in `docs/REALWEB.md`.

pub mod session;
pub mod token;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use hyper::{header, Method, Response};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Utf8Bytes;

use crate::config::parser::RealWebConfig;
use crate::config::{parse_duration_secs, parse_size, ServerConfig};
use crate::http::request::ParsedRequest;
use crate::http::response::{empty_response, error_403, error_500, ResponseBody};

pub const SUBPROTOCOL: &str = "realweb.v1";
const CLIENT_JS: &str = include_str!("client.js");
/// Channels that may exist at once across the whole process.
const MAX_CHANNELS: usize = 100_000;
/// Retained history per channel is also capped by bytes, whatever `history` says.
const HISTORY_BYTES_PER_CHANNEL: usize = 1 << 20;

// ─── Channel names and patterns ─────────────────────────────────────────────

/// Channel names are 1–128 characters of `A-Z a-z 0-9 - _ . : / @`.
pub fn valid_channel(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
}

/// A pattern is `*`, an exact channel name, or a prefix ending in `*`.
pub fn valid_pattern(pattern: &str) -> bool {
    pattern == "*" || valid_channel(pattern.strip_suffix('*').unwrap_or(pattern))
}

pub fn pattern_matches(pattern: &str, channel: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => channel.starts_with(prefix),
        None => pattern == channel,
    }
}

pub fn any_matches(patterns: &[String], channel: &str) -> bool {
    patterns.iter().any(|p| pattern_matches(p, channel))
}

// ─── Hub ────────────────────────────────────────────────────────────────────

/// How long and how much history a publish retains.
#[derive(Clone, Copy)]
pub struct Retention {
    pub max: usize,
    pub ttl: Duration,
}

impl Retention {
    pub fn from_config(rw: &RealWebConfig) -> Self {
        Retention {
            max: rw.history,
            ttl: Duration::from_secs(parse_duration_secs(&rw.history_ttl).unwrap_or(300)),
        }
    }
}

/// A connection's handle inside each channel it subscribes to.
#[derive(Clone)]
pub struct Subscriber {
    pub tx: mpsc::Sender<Utf8Bytes>,
    pub uid: Option<Arc<str>>,
    /// Signalled when `tx` is full: the connection is too slow and is closed.
    pub overflow: Arc<Notify>,
}

struct Retained {
    seq: u64,
    expires: Instant,
    frame: Utf8Bytes,
}

struct Channel {
    /// Identifies this incarnation of the channel. Sequence numbers restart
    /// when a channel is recreated or FlashLin restarts; a client resuming
    /// with another epoch's `since` is told it has a gap.
    epoch: Arc<str>,
    seq: u64,
    subscribers: HashMap<u64, Subscriber>,
    history: VecDeque<Retained>,
    history_bytes: usize,
    /// Set (under the lock) when the channel is removed from the map, so a
    /// caller that fetched it just before removal retries with a fresh one.
    dead: bool,
}

impl Channel {
    fn new() -> Self {
        static EPOCHS: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Channel {
            epoch: format!("{:x}", nanos ^ EPOCHS.fetch_add(1, Ordering::Relaxed)).into(),
            seq: 0,
            subscribers: HashMap::new(),
            history: VecDeque::new(),
            history_bytes: 0,
            dead: false,
        }
    }

    fn prune(&mut self, now: Instant, max: usize) {
        while let Some(front) = self.history.front() {
            if front.expires > now
                && self.history.len() <= max
                && self.history_bytes <= HISTORY_BYTES_PER_CHANNEL
            {
                break;
            }
            self.history_bytes -= front.frame.len();
            self.history.pop_front();
        }
    }

    fn idle(&self) -> bool {
        self.subscribers.is_empty() && self.history.is_empty()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HubError {
    TooManyChannels,
}

pub struct Published {
    pub seq: u64,
    pub delivered: usize,
}

pub struct Subscribed {
    pub seq: u64,
    pub epoch: Arc<str>,
    /// True when the client asked to resume but messages it never received
    /// are no longer retained (or the channel was recreated): it should
    /// reload its state from the application.
    pub gap: bool,
    pub replay: Vec<Utf8Bytes>,
}

#[derive(Default)]
pub struct Hub {
    channels: DashMap<Arc<str>, Arc<Mutex<Channel>>>,
    next_connection: AtomicU64,
    connections: AtomicUsize,
    published: AtomicU64,
    delivered: AtomicU64,
    slow_disconnects: AtomicU64,
}

pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::default)
}

fn lock(channel: &Mutex<Channel>) -> std::sync::MutexGuard<'_, Channel> {
    channel
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Hub {
    /// Runs `f` on the live channel `name`, creating it when `create` is set.
    fn with_channel<R>(
        &self,
        name: &str,
        create: bool,
        f: impl FnOnce(&mut Channel) -> R,
    ) -> Result<Option<R>, HubError> {
        let mut f = Some(f);
        loop {
            let channel = match self.channels.get(name) {
                Some(found) => found.clone(),
                None if !create => return Ok(None),
                None => {
                    if self.channels.len() >= MAX_CHANNELS {
                        return Err(HubError::TooManyChannels);
                    }
                    self.channels
                        .entry(name.into())
                        .or_insert_with(|| Arc::new(Mutex::new(Channel::new())))
                        .clone()
                }
            };
            let mut guard = lock(&channel);
            if guard.dead {
                continue;
            }
            return Ok(Some((f.take().expect("called once"))(&mut guard)));
        }
    }

    /// Assigns the next sequence number, retains the message and queues it to
    /// every subscriber. The frame is built once; queues share it.
    pub fn publish(
        &self,
        channel: &str,
        data: &RawValue,
        from: Option<&str>,
        retention: Retention,
    ) -> Result<Published, HubError> {
        let published = self.with_channel(channel, true, |ch| {
            ch.seq += 1;
            let frame = Utf8Bytes::from(msg_frame(channel, ch.seq, from, data));
            let mut delivered = 0;
            for subscriber in ch.subscribers.values() {
                match subscriber.tx.try_send(frame.clone()) {
                    Ok(()) => delivered += 1,
                    Err(mpsc::error::TrySendError::Full(_)) => subscriber.overflow.notify_one(),
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            if retention.max > 0 && !retention.ttl.is_zero() {
                let now = Instant::now();
                ch.history_bytes += frame.len();
                ch.history.push_back(Retained {
                    seq: ch.seq,
                    expires: now + retention.ttl,
                    frame,
                });
                ch.prune(now, retention.max);
            }
            Published {
                seq: ch.seq,
                delivered,
            }
        })?;
        let published = published.expect("created on demand");
        self.published.fetch_add(1, Ordering::Relaxed);
        self.delivered
            .fetch_add(published.delivered as u64, Ordering::Relaxed);
        Ok(published)
    }

    /// Registers `subscriber` and, when resuming, collects the retained
    /// messages after `since` — atomically, so nothing is missed or doubled
    /// between the replay and the live stream.
    pub fn subscribe(
        &self,
        channel: &str,
        connection: u64,
        subscriber: Subscriber,
        resume: Option<(u64, &str)>,
    ) -> Result<Subscribed, HubError> {
        let subscribed = self.with_channel(channel, true, |ch| {
            ch.prune(Instant::now(), usize::MAX);
            ch.subscribers.insert(connection, subscriber);
            let Some((since, epoch)) = resume else {
                return Subscribed {
                    seq: ch.seq,
                    epoch: ch.epoch.clone(),
                    gap: false,
                    replay: Vec::new(),
                };
            };
            let same_epoch = *ch.epoch == *epoch && since <= ch.seq;
            let since = if same_epoch { since } else { 0 };
            let oldest = ch.history.front().map_or(ch.seq + 1, |r| r.seq);
            Subscribed {
                seq: ch.seq,
                epoch: ch.epoch.clone(),
                gap: !same_epoch || (since < ch.seq && oldest > since + 1),
                replay: ch
                    .history
                    .iter()
                    .filter(|r| r.seq > since)
                    .map(|r| r.frame.clone())
                    .collect(),
            }
        })?;
        Ok(subscribed.expect("created on demand"))
    }

    pub fn unsubscribe(&self, channel: &str, connection: u64) {
        let _ = self.with_channel(channel, false, |ch| {
            ch.subscribers.remove(&connection);
        });
        self.channels.remove_if(channel, |_, ch| {
            let mut ch = lock(ch);
            ch.prune(Instant::now(), usize::MAX);
            ch.dead = ch.idle();
            ch.dead
        });
    }

    /// Subscriber count and distinct user ids (at most 1,000) of a channel.
    pub fn presence(&self, channel: &str) -> (usize, Vec<Arc<str>>) {
        self.with_channel(channel, false, |ch| {
            let users: BTreeSet<Arc<str>> = ch
                .subscribers
                .values()
                .filter_map(|s| s.uid.clone())
                .take(1000)
                .collect();
            (ch.subscribers.len(), users.into_iter().collect())
        })
        .ok()
        .flatten()
        .unwrap_or_default()
    }

    /// Drops expired history and channels nobody uses any more.
    fn sweep(&self) {
        let now = Instant::now();
        self.channels.retain(|_, ch| {
            let mut ch = lock(ch);
            ch.prune(now, usize::MAX);
            ch.dead = ch.idle();
            !ch.dead
        });
    }

    pub fn next_connection_id(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn record_slow_disconnect(&self) {
        self.slow_disconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({
            "connections": self.connections.load(Ordering::Relaxed),
            "channels": self.channels.len(),
            "published": self.published.load(Ordering::Relaxed),
            "delivered": self.delivered.load(Ordering::Relaxed),
            "slow_disconnects": self.slow_disconnects.load(Ordering::Relaxed),
        })
    }

    /// Prometheus lines appended to `/metrics`.
    pub fn render_metrics(&self) -> String {
        format!(
            "# HELP flashlin_realweb_connections Open RealWeb connections\n\
             # TYPE flashlin_realweb_connections gauge\n\
             flashlin_realweb_connections {}\n\
             # HELP flashlin_realweb_channels Live RealWeb channels\n\
             # TYPE flashlin_realweb_channels gauge\n\
             flashlin_realweb_channels {}\n\
             # HELP flashlin_realweb_published_total Messages published\n\
             # TYPE flashlin_realweb_published_total counter\n\
             flashlin_realweb_published_total {}\n\
             # HELP flashlin_realweb_delivered_total Messages queued to subscribers\n\
             # TYPE flashlin_realweb_delivered_total counter\n\
             flashlin_realweb_delivered_total {}\n\
             # HELP flashlin_realweb_slow_disconnects_total Connections closed for not keeping up\n\
             # TYPE flashlin_realweb_slow_disconnects_total counter\n\
             flashlin_realweb_slow_disconnects_total {}\n",
            self.connections.load(Ordering::Relaxed),
            self.channels.len(),
            self.published.load(Ordering::Relaxed),
            self.delivered.load(Ordering::Relaxed),
            self.slow_disconnects.load(Ordering::Relaxed),
        )
    }
}

fn ensure_sweeper() {
    static SWEEPER: Once = Once::new();
    SWEEPER.call_once(|| {
        tokio::spawn(async {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                hub().sweep();
            }
        });
    });
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `{"t":"msg","ch":..,"seq":..,"ts":..,"from":..,"data":..}`, with `data`
/// copied through verbatim (it was validated as JSON when received).
fn msg_frame(channel: &str, seq: u64, from: Option<&str>, data: &RawValue) -> String {
    let mut out = String::with_capacity(channel.len() + data.get().len() + 96);
    out.push_str(r#"{"t":"msg","ch":"#);
    out.push_str(&serde_json::to_string(channel).expect("string serializes"));
    out.push_str(&format!(r#","seq":{seq},"ts":{}"#, unix_millis()));
    if let Some(from) = from {
        out.push_str(r#","from":"#);
        out.push_str(&serde_json::to_string(from).expect("string serializes"));
    }
    out.push_str(r#","data":"#);
    out.push_str(data.get());
    out.push('}');
    out
}

// ─── HTTP entry points ──────────────────────────────────────────────────────

/// Holds one of `max_connections` slots for the lifetime of a connection.
pub(crate) struct ConnectionSlot;

impl ConnectionSlot {
    fn acquire(limit: usize) -> Option<Self> {
        hub()
            .connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .ok()
            .map(|_| ConnectionSlot)
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        hub().connections.fetch_sub(1, Ordering::AcqRel);
    }
}

fn header_has_token(headers: &hyper::HeaderMap, name: header::HeaderName, token: &str) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim().eq_ignore_ascii_case(token))
}

/// Browsers always send Origin on WebSocket handshakes; checking it stops
/// other sites from opening connections with a visitor's cookies
/// (cross-site WebSocket hijacking), which plain WebSockets do not prevent.
fn origin_allowed(headers: &hyper::HeaderMap, rw: &RealWebConfig) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if !rw.allowed_origins.is_empty() {
        return rw
            .allowed_origins
            .iter()
            .any(|allowed| allowed == "*" || allowed.eq_ignore_ascii_case(origin));
    }
    let origin_host = origin.split_once("://").map(|(_, rest)| rest);
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    matches!((origin_host, host), (Some(o), Some(h)) if o.eq_ignore_ascii_case(h))
}

/// Serves `<path>` (the WebSocket endpoint) and `<path>/client.js`; returns
/// `None` for every other path.
pub async fn handle(
    req: &ParsedRequest,
    method: &Method,
    rw: &RealWebConfig,
    remote_addr: SocketAddr,
    config: &ServerConfig,
) -> Option<Response<ResponseBody>> {
    let server_name = &config.server.server_name;
    if let Some(rest) = req.path.strip_prefix(rw.path.as_str()) {
        if rest == "/client.js" && (*method == Method::GET || *method == Method::HEAD) {
            return Some(
                Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
                    .header(header::CACHE_CONTROL, "public, max-age=3600")
                    .header(header::CONTENT_LENGTH, CLIENT_JS.len())
                    .header("x-content-type-options", "nosniff")
                    .body(ResponseBody::new(Bytes::from_static(CLIENT_JS.as_bytes())))
                    .unwrap_or_else(|_| error_500(server_name)),
            );
        }
        if !rest.is_empty() {
            return None;
        }
    } else {
        return None;
    }

    let plain = |status: u16, text: &'static str| {
        let mut response = empty_response(status);
        *response.body_mut() = ResponseBody::new(Bytes::from_static(text.as_bytes()));
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(header::CONTENT_LENGTH, text.len().into());
        response
    };
    if *method != Method::GET {
        let mut response = plain(405, "RealWeb endpoint: connect with a WebSocket GET\n");
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET"));
        return Some(response);
    }
    let headers = &req.headers;
    let key = headers.get(header::SEC_WEBSOCKET_KEY);
    let is_upgrade = header_has_token(headers, header::UPGRADE, "websocket")
        && header_has_token(headers, header::CONNECTION, "upgrade");
    if !is_upgrade || key.is_none() || req.upgrade.is_none() {
        let mut response = plain(
            426,
            "RealWeb endpoint: connect with the RealWeb client (<path>/client.js)\n",
        );
        response.headers_mut().insert(
            header::UPGRADE,
            header::HeaderValue::from_static("websocket"),
        );
        return Some(response);
    }
    if headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .is_none_or(|v| v.as_bytes() != b"13")
    {
        let mut response = plain(426, "Unsupported WebSocket version\n");
        response.headers_mut().insert(
            header::SEC_WEBSOCKET_VERSION,
            header::HeaderValue::from_static("13"),
        );
        return Some(response);
    }
    if !header_has_token(headers, header::SEC_WEBSOCKET_PROTOCOL, SUBPROTOCOL) {
        return Some(plain(400, "RealWeb requires the realweb.v1 subprotocol\n"));
    }
    if !origin_allowed(headers, rw) {
        tracing::warn!(ip = %remote_addr, origin = ?headers.get(header::ORIGIN), "RealWeb origin refused");
        return Some(error_403(server_name));
    }
    let Some(slot) = ConnectionSlot::acquire(rw.max_connections) else {
        return Some(plain(503, "RealWeb connection limit reached\n"));
    };
    let Some(upgrade) = req.upgrade.as_ref().and_then(|u| u.try_lock().ok()?.take()) else {
        return Some(error_500(server_name));
    };

    let settings = session::Settings::from_config(rw);
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
        key.expect("checked above").as_bytes(),
    );
    ensure_sweeper();
    tokio::spawn(async move {
        let _slot = slot;
        let Ok(Ok(upgraded)) = tokio::time::timeout(Duration::from_secs(10), upgrade).await else {
            return;
        };
        session::run(
            hyper_util::rt::TokioIo::new(upgraded),
            settings,
            remote_addr,
        )
        .await;
    });

    Some(
        Response::builder()
            .status(101)
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_ACCEPT, accept)
            .header(header::SEC_WEBSOCKET_PROTOCOL, SUBPROTOCOL)
            .body(ResponseBody::new(Bytes::new()))
            .unwrap_or_else(|_| error_500(server_name)),
    )
}

/// `POST /flashlin/realweb/publish` with `{"ch": "...", "data": ...}`: lets a
/// backend push to a channel. The caller has already passed the loopback and
/// control-token checks.
pub fn control_publish(body: &[u8], config: &ServerConfig) -> (u16, String) {
    let Some(rw) = config.realweb.as_ref().filter(|rw| rw.enabled) else {
        return (404, "realweb is not enabled on this server\n".into());
    };
    #[derive(serde::Deserialize)]
    struct Publish<'a> {
        #[serde(borrow)]
        ch: std::borrow::Cow<'a, str>,
        #[serde(borrow)]
        data: &'a RawValue,
        #[serde(default, borrow)]
        from: Option<std::borrow::Cow<'a, str>>,
    }
    let limit = parse_size(&rw.max_message_size).unwrap_or(64 << 10) as usize;
    if body.len() > limit {
        return (413, "message larger than realweb.max_message_size\n".into());
    }
    let publish: Publish = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            return (
                400,
                format!("expected {{\"ch\": \"...\", \"data\": ...}}: {e}\n"),
            )
        }
    };
    if !valid_channel(&publish.ch) {
        return (400, "invalid channel name\n".into());
    }
    match hub().publish(
        &publish.ch,
        publish.data,
        publish.from.as_deref(),
        Retention::from_config(rw),
    ) {
        Ok(p) => (
            200,
            serde_json::json!({ "seq": p.seq, "delivered": p.delivered }).to_string(),
        ),
        Err(HubError::TooManyChannels) => (503, "too many channels\n".into()),
    }
}

pub fn control_stats(config: &ServerConfig) -> (u16, String) {
    if !config.realweb.as_ref().is_some_and(|rw| rw.enabled) {
        return (404, "realweb is not enabled on this server\n".into());
    }
    (200, hub().stats().to_string())
}
