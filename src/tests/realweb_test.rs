//! RealWeb: token verification, channel patterns, and the hub end to end over
//! real TCP connections (handshake checks, permissions, fan-out, resume after
//! reconnect, backend publishing, limits and token expiry).
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::WebSocketStream;

use crate::realweb::token::{mint, unix_now, verify, Claims, TokenError};
use crate::realweb::{pattern_matches, valid_channel, valid_pattern};

const SECRET: &str = "test-secret-0123456789-abcdefghij-klmnop";

type Ws = WebSocketStream<TcpStream>;

fn token(user: &str, subscribe: &[&str], publish: &[&str], exp: u64) -> String {
    mint(
        &Claims {
            sub: Some(user.into()),
            exp,
            nbf: None,
            subscribe: subscribe.iter().map(|s| s.to_string()).collect(),
            publish: publish.iter().map(|s| s.to_string()).collect(),
        },
        SECRET.as_bytes(),
    )
}

#[test]
fn tokens_verify_signature_algorithm_and_expiry() {
    let now = unix_now();
    let good = token("alice", &["chat/*"], &["chat/lobby"], now + 60);
    let claims = verify(&good, SECRET.as_bytes(), now).unwrap();
    assert_eq!(claims.sub.as_deref(), Some("alice"));
    assert_eq!(claims.publish, ["chat/lobby"]);

    assert_eq!(
        verify(&good, b"another-secret-another-secret-xx", now),
        Err(TokenError::BadSignature)
    );
    // Swap in a payload granting more, keeping the old signature.
    let parts: Vec<&str> = good.split('.').collect();
    let forged = token("alice", &["*"], &["*"], now + 60);
    let forged_payload = forged.split('.').nth(1).unwrap();
    let tampered = format!("{}.{}.{}", parts[0], forged_payload, parts[2]);
    assert_eq!(
        verify(&tampered, SECRET.as_bytes(), now),
        Err(TokenError::BadSignature)
    );
    // `alg: none` with an empty signature must never be accepted.
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let unsigned = format!(
        "{}.{}.",
        b64.encode(br#"{"alg":"none","typ":"JWT"}"#),
        parts[1]
    );
    assert_eq!(
        verify(&unsigned, SECRET.as_bytes(), now),
        Err(TokenError::Malformed)
    );
    assert_eq!(
        verify(&token("a", &[], &[], now - 31), SECRET.as_bytes(), now),
        Err(TokenError::Expired)
    );
    // Within the clock-skew leeway.
    assert!(verify(&token("a", &[], &[], now - 5), SECRET.as_bytes(), now).is_ok());
    assert_eq!(
        verify(
            &token("a", &["bad channel!"], &[], now + 60),
            SECRET.as_bytes(),
            now
        ),
        Err(TokenError::Malformed)
    );
    assert_eq!(
        verify("a.b", SECRET.as_bytes(), now),
        Err(TokenError::Malformed)
    );
}

#[test]
fn channel_names_and_patterns() {
    assert!(valid_channel("chat/room-1"));
    assert!(valid_channel("user:42@team.example"));
    assert!(!valid_channel(""));
    assert!(!valid_channel("has space"));
    assert!(!valid_channel("star*"));
    assert!(!valid_channel(&"x".repeat(129)));
    assert!(valid_pattern("*") && valid_pattern("chat/*") && valid_pattern("chat/lobby"));
    assert!(!valid_pattern("*/chat") && !valid_pattern("chat/**"));
    assert!(pattern_matches("*", "anything"));
    assert!(pattern_matches("chat/*", "chat/lobby"));
    assert!(!pattern_matches("chat/*", "chatter"));
    assert!(pattern_matches("chat/lobby", "chat/lobby"));
    assert!(!pattern_matches("chat/lobby", "chat/lobby2"));
}

fn realweb_config(extra: &str) -> crate::config::ServerConfig {
    let mut cfg = crate::default_config();
    cfg.realweb = Some(
        toml::from_str(&format!(
            "enabled = true\nsecret = \"{SECRET}\"\npublic_channels = [\"news/*\"]\n{extra}"
        ))
        .unwrap(),
    );
    crate::config::validate(&cfg).unwrap();
    cfg
}

async fn start(cfg: crate::config::ServerConfig) -> SocketAddr {
    let cfg = Arc::new(cfg);
    let listener = crate::server::net::bind_tcp("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let trigger = crate::server::shutdown::ShutdownTrigger::new();
        let live = Arc::new(arc_swap::ArcSwap::from_pointee(
            crate::server::reload::Snapshot::new(cfg).unwrap(),
        ));
        loop {
            let (socket, peer) = listener.accept().await.unwrap();
            let live = live.clone();
            let shutdown = trigger.subscribe();
            tokio::spawn(async move {
                let _ = crate::server::connection::handle_connection(
                    socket, peer, live, shutdown, false,
                )
                .await;
            });
        }
    });
    address
}

async fn try_connect(
    address: SocketAddr,
    origin: Option<&str>,
    subprotocol: bool,
) -> Result<Ws, tungstenite::Error> {
    let mut request = format!("ws://{address}/realweb")
        .into_client_request()
        .unwrap();
    if subprotocol {
        request
            .headers_mut()
            .insert("sec-websocket-protocol", "realweb.v1".parse().unwrap());
    }
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    let stream = crate::server::net::connect_tcp(address).await.unwrap();
    tokio_tungstenite::client_async(request, stream)
        .await
        .map(|(ws, _)| ws)
}

async fn send(ws: &mut Ws, frame: Value) {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
}

/// Next JSON frame, skipping control frames.
async fn recv(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("frame within 5s")
            .expect("connection open")
            .expect("valid frame");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Close code the server ends the connection with, skipping data frames.
async fn close_code(ws: &mut Ws) -> u16 {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => return frame.code.into(),
            Ok(Some(Ok(Message::Close(None)))) | Ok(None) => return 1005,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => panic!("socket error before close: {e}"),
            Err(_) => panic!("no close within 10s"),
        }
    }
}

async fn connect(address: SocketAddr, token: Option<&str>) -> (Ws, Value) {
    let mut ws = try_connect(address, None, true).await.unwrap();
    send(&mut ws, json!({ "t": "hello", "v": 1, "token": token })).await;
    let welcome = recv(&mut ws).await;
    assert_eq!(welcome["t"], "welcome", "{welcome}");
    (ws, welcome)
}

/// Sends a request frame and returns the reply with the same id.
async fn call(ws: &mut Ws, mut frame: Value, id: u64) -> Value {
    frame["id"] = id.into();
    send(ws, frame).await;
    loop {
        let reply = recv(ws).await;
        if reply["id"] == id {
            return reply;
        }
    }
}

async fn http(address: SocketAddr, request: String) -> String {
    let mut stream = crate::server::net::connect_tcp(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_serves_client_and_enforces_subprotocol_and_origin() {
    let address = start(realweb_config("")).await;

    let script = http(
        address,
        format!("GET /realweb/client.js HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(script.starts_with("HTTP/1.1 200"), "{script}");
    assert!(script.contains("text/javascript") && script.contains("realweb.v1"));

    let plain = http(
        address,
        format!("GET /realweb HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(plain.starts_with("HTTP/1.1 426"), "{plain}");

    let status = |result: Result<Ws, tungstenite::Error>| match result {
        Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => 101,
    };
    assert_eq!(status(try_connect(address, None, false).await), 400);
    assert_eq!(
        status(try_connect(address, Some("https://evil.example"), true).await),
        403
    );
    // Same-origin pages may connect without an allowlist.
    assert_eq!(
        status(try_connect(address, Some(&format!("http://{address}")), true).await),
        101
    );

    // A bad token is refused at hello with a specific close code.
    let mut ws = try_connect(address, None, true).await.unwrap();
    send(&mut ws, json!({ "t": "hello", "token": "not.a.token" })).await;
    assert_eq!(close_code(&mut ws).await, 4003);
    // Anything other than hello first is a protocol error.
    let mut ws = try_connect(address, None, true).await.unwrap();
    send(&mut ws, json!({ "t": "sub", "ch": "news/x" })).await;
    assert_eq!(close_code(&mut ws).await, 4000);

    let allowlisted = start(realweb_config(
        "allowed_origins = [\"https://app.example\"]",
    ))
    .await;
    assert_eq!(
        status(try_connect(allowlisted, Some("https://app.example"), true).await),
        101
    );
    assert_eq!(
        status(try_connect(allowlisted, Some(&format!("http://{allowlisted}")), true).await),
        403
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_fanout_presence_resume_and_backend_publish() {
    let address = start(realweb_config("history = 3")).await;
    let exp = unix_now() + 600;
    let alice = token("alice", &["t1/*"], &["t1/*"], exp);
    let bob = token("bob", &["t1/*"], &[], exp);

    let (mut a, welcome) = connect(address, Some(&alice)).await;
    assert_eq!(welcome["uid"], "alice");
    let (mut b, _) = connect(address, Some(&bob)).await;
    let (mut anon, welcome) = connect(address, None).await;
    assert!(welcome["uid"].is_null());

    // Anonymous: public channels only, never publish.
    assert_eq!(
        call(&mut anon, json!({"t":"sub","ch":"news/today"}), 1).await["t"],
        "ok"
    );
    assert_eq!(
        call(&mut anon, json!({"t":"sub","ch":"t1/room"}), 2).await["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut anon, json!({"t":"pub","ch":"news/today","data":1}), 3).await["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut b, json!({"t":"sub","ch":"other/x"}), 1).await["code"],
        "forbidden"
    );
    assert_eq!(
        call(&mut b, json!({"t":"sub","ch":"bad name"}), 2).await["code"],
        "bad_channel"
    );

    let ok = call(&mut b, json!({"t":"sub","ch":"t1/room"}), 3).await;
    assert_eq!(
        (ok["t"].as_str(), ok["seq"].as_u64()),
        (Some("ok"), Some(0))
    );
    let epoch = ok["epoch"].as_str().unwrap().to_string();

    let ack = call(&mut a, json!({"t":"pub","ch":"t1/room","data":{"n":1}}), 1).await;
    assert_eq!(
        (ack["t"].as_str(), ack["seq"].as_u64()),
        (Some("ok"), Some(1))
    );
    let msg = recv(&mut b).await;
    assert_eq!(msg["t"], "msg");
    assert_eq!(msg["seq"], 1);
    assert_eq!(msg["from"], "alice");
    assert_eq!(msg["data"], json!({"n":1}));

    assert_eq!(
        call(&mut b, json!({"t":"pub","ch":"t1/room","data":"x"}), 4).await["code"],
        "forbidden"
    );
    let presence = call(&mut a, json!({"t":"presence","ch":"t1/room"}), 2).await;
    assert_eq!(presence["count"], 1);
    assert_eq!(presence["users"], json!(["bob"]));

    // Bob drops; two messages arrive while he is away; he resumes from seq 1.
    drop(b);
    for n in 2..=3 {
        call(
            &mut a,
            json!({"t":"pub","ch":"t1/room","data":{"n":n}}),
            10 + n,
        )
        .await;
    }
    let (mut b, _) = connect(address, Some(&bob)).await;
    let ok = call(
        &mut b,
        json!({"t":"sub","ch":"t1/room","since":1,"epoch":epoch}),
        1,
    )
    .await;
    assert_eq!(
        (ok["gap"].as_bool(), ok["replayed"].as_u64()),
        (Some(false), Some(2))
    );
    assert_eq!(recv(&mut b).await["seq"], 2);
    assert_eq!(recv(&mut b).await["seq"], 3);

    // More than `history` messages missed: the client is told about the gap
    // and still gets what was retained.
    for n in 4..=7 {
        call(
            &mut a,
            json!({"t":"pub","ch":"t1/room","data":{"n":n}}),
            10 + n,
        )
        .await;
    }
    for seq in 4..=7 {
        assert_eq!(recv(&mut b).await["seq"], seq);
    }
    let (mut late, _) = connect(address, Some(&bob)).await;
    let ok = call(
        &mut late,
        json!({"t":"sub","ch":"t1/room","since":1,"epoch":epoch}),
        1,
    )
    .await;
    assert_eq!(
        (ok["gap"].as_bool(), ok["replayed"].as_u64()),
        (Some(true), Some(3))
    );
    assert_eq!(recv(&mut late).await["seq"], 5);
    // A different epoch (channel recreated, server restarted) is also a gap.
    let (mut other, _) = connect(address, Some(&bob)).await;
    let ok = call(
        &mut other,
        json!({"t":"sub","ch":"t1/room","since":7,"epoch":"stale"}),
        1,
    )
    .await;
    assert_eq!(ok["gap"], true);

    // Backends publish over loopback HTTP.
    let body = r#"{"ch":"t1/room","data":{"from":"backend"}}"#;
    let response = http(
        address,
        format!(
            "POST /flashlin/realweb/publish HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains(r#""seq":8"#), "{response}");
    let msg = recv(&mut b).await;
    assert_eq!((msg["seq"].as_u64(), msg.get("from")), (Some(8), None));
    assert_eq!(msg["data"]["from"], "backend");

    let stats = http(
        address,
        format!("GET /flashlin/realweb HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(stats.contains(r#""connections":"#), "{stats}");

    // Unsubscribed connections stop receiving.
    assert_eq!(
        call(&mut b, json!({"t":"unsub","ch":"t1/room"}), 9).await["t"],
        "ok"
    );
    call(&mut a, json!({"t":"pub","ch":"t1/room","data":9}), 30).await;
    send(&mut b, json!({"t":"ping","id":10})).await;
    assert_eq!(recv(&mut b).await["t"], "pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_refresh_revokes_and_expiry_closes() {
    let address = start(realweb_config("")).await;
    let now = unix_now();
    // Valid only through the 30s skew leeway, which runs out in ~2s.
    let short = token("carol", &["t2/*"], &[], now - 28);
    let (mut ws, _) = connect(address, Some(&short)).await;
    assert_eq!(
        call(&mut ws, json!({"t":"sub","ch":"t2/a"}), 1).await["t"],
        "ok"
    );

    // Refreshing with another user's token is refused.
    let mallory = token("mallory", &["*"], &["*"], now + 600);
    assert_eq!(
        call(&mut ws, json!({"t":"auth","token":mallory}), 2).await["code"],
        "uid_mismatch"
    );
    // A refresh with narrower rights ends subscriptions it no longer covers.
    let narrower = token("carol", &["t2/b"], &[], now + 600);
    send(&mut ws, json!({"t":"auth","token":narrower,"id":3})).await;
    let kicked = recv(&mut ws).await;
    assert_eq!(
        (kicked["t"].as_str(), kicked["ch"].as_str()),
        (Some("kicked"), Some("t2/a"))
    );
    assert_eq!(recv(&mut ws).await["t"], "ok");

    // Without a refresh the connection closes when its token expires.
    let (mut expiring, _) = connect(address, Some(&short)).await;
    assert_eq!(close_code(&mut expiring).await, 4001);
    // The refreshed connection outlives the original expiry.
    assert_eq!(call(&mut ws, json!({"t":"ping"}), 4).await["t"], "pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn limits_close_abusive_connections() {
    let address = start(realweb_config(
        "messages_per_second = 1\nmax_message_size = \"1KB\"\nsend_queue = 16",
    ))
    .await;
    let exp = unix_now() + 600;

    // Burst is 2: the third frame is refused, a sustained flood is closed.
    let (mut ws, _) = connect(address, None).await;
    for _ in 0..2 {
        send(&mut ws, json!({"t":"ping"})).await;
        assert_eq!(recv(&mut ws).await["t"], "pong");
    }
    send(&mut ws, json!({"t":"ping","id":1})).await;
    assert_eq!(recv(&mut ws).await["code"], "rate_limited");
    for _ in 0..4 {
        send(&mut ws, json!({"t":"ping"})).await;
    }
    assert_eq!(close_code(&mut ws).await, 4008);

    let (mut ws, _) = connect(address, None).await;
    ws.send(Message::Text("x".repeat(2048).into()))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, 1009);

    let (mut ws, _) = connect(address, None).await;
    ws.send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, 1003);

    // A subscriber that stops reading is disconnected once its queue is full,
    // instead of the server buffering for it without limit. Its session sees
    // the overflow when its blocked socket write completes, so flood first and
    // read afterwards.
    let reader = token("slow", &["t3/*"], &[], exp);
    let (mut slow, _) = connect(address, Some(&reader)).await;
    assert_eq!(
        call(&mut slow, json!({"t":"sub","ch":"t3/flood"}), 1).await["t"],
        "ok"
    );
    let data =
        serde_json::value::RawValue::from_string(json!("y".repeat(60_000)).to_string()).unwrap();
    let retention = crate::realweb::Retention {
        max: 0,
        ttl: Duration::ZERO,
    };
    for _ in 0..1000 {
        crate::realweb::hub()
            .publish("t3/flood", &data, None, retention)
            .unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(close_code(&mut slow).await, 4002);
    assert!(
        crate::realweb::hub().stats()["slow_disconnects"]
            .as_u64()
            .unwrap()
            >= 1
    );
}
