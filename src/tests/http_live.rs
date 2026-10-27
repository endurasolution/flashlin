//! Real TCP coverage of the page-delivery path and a controlled upstream.
use crate::http::response::ResponseBody;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{header, Method, Request, Response};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo},
};
use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn request(
    client: &Client<HttpConnector, Full<Bytes>>,
    address: std::net::SocketAddr,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> Response<hyper::body::Incoming> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("http://{address}{path}"));
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        client.request(builder.body(Full::new(Bytes::new())).unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_delivery_proxy_pool_streaming_and_websocket_over_tcp() {
    let folder = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("http-fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(folder.join("nested")).unwrap();
    std::fs::write(folder.join("app.txt"), b"hello page").unwrap();
    std::fs::write(folder.join("large.bin"), vec![7u8; 3 * 1024 * 1024]).unwrap();
    std::fs::write(folder.join("nested/index.html"), b"nested page").unwrap();
    std::fs::write(
        folder.join("compress.txt"),
        "compress me quickly!".repeat(2000),
    )
    .unwrap();
    {
        use std::io::Write;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(b"hello page").unwrap();
        std::fs::write(folder.join("app.txt.gz"), gzip.finish().unwrap()).unwrap();
    }
    let backend = crate::server::net::bind_tcp("127.0.0.1:0").unwrap();
    let backend_addr = backend.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let backend_task = tokio::spawn(async move {
        loop {
            let (socket, _) = backend.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let gate = gate.clone();
            tokio::spawn(async move {
                let service =
                    hyper::service::service_fn(move |mut req: Request<hyper::body::Incoming>| {
                        let gate = gate.clone();
                        async move {
                            let response = if req.uri().path() == "/proxy/socket" {
                                let upgrade = hyper::upgrade::on(&mut req);
                                tokio::spawn(async move {
                                    let mut io = TokioIo::new(upgrade.await.unwrap());
                                    let mut bytes = [0; 4];
                                    io.read_exact(&mut bytes).await.unwrap();
                                    io.write_all(&bytes).await.unwrap();
                                });
                                Response::builder()
                                    .status(101)
                                    .header("connection", "upgrade")
                                    .header("upgrade", "websocket")
                                    .header("sec-websocket-accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
                                    .body(ResponseBody::new(Bytes::new()))
                                    .unwrap()
                            } else if req.uri().path() == "/proxy/stream" {
                                let stream = futures_util::stream::unfold(
                                    (0, gate),
                                    |(state, gate)| async move {
                                        match state {
                                            0 => {
                                                Some((Ok(Bytes::from_static(b"first")), (1, gate)))
                                            }
                                            1 => {
                                                gate.notified().await;
                                                Some((Ok(Bytes::from_static(b"second")), (2, gate)))
                                            }
                                            _ => None,
                                        }
                                    },
                                );
                                Response::builder()
                                    .header("content-type", "text/event-stream")
                                    .body(ResponseBody::stream(stream, None))
                                    .unwrap()
                            } else {
                                assert_eq!(req.headers().get_all("host").iter().count(), 1);
                                assert!(!req.headers().contains_key("x-remove-me"));
                                assert_eq!(req.headers()["x-forwarded-for"], "127.0.0.1");
                                assert_eq!(req.headers()["x-forwarded-proto"], "http");
                                Response::new(ResponseBody::new(Bytes::from(req.uri().to_string())))
                            };
                            Ok::<_, Infallible>(response)
                        }
                    });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(socket), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    let mut cfg = crate::default_config();
    cfg.server.root = Some(folder.to_string_lossy().into_owned());
    cfg.server.read_timeout = "1s".into();
    // Exercise uncached streaming for large.bin even when the configured
    // default cache limit permits multi-megabyte entries.
    cfg.cache.as_mut().unwrap().max_file_size = "1MB".into();
    cfg.routes.as_mut().unwrap()[0].root = cfg.server.root.clone();
    cfg.static_dirs = Some(vec![crate::config::parser::StaticDirConfig {
        url_path: "/assets".into(),
        root: folder.clone(),
        cache_control: None,
    }]);
    let mut proxy_route = cfg.routes.as_ref().unwrap()[0].clone();
    proxy_route.path = "/proxy".into();
    proxy_route.root = None;
    proxy_route.proxy_pass = Some(format!("http://{backend_addr}"));
    cfg.routes.as_mut().unwrap().push(proxy_route);
    let unavailable = crate::server::net::bind_tcp("127.0.0.1:0").unwrap();
    let unavailable_addr = unavailable.local_addr().unwrap();
    // Hold the listening port without accepting: a deterministic unresponsive
    // backend, without racing another process for a recently released port.
    let mut balanced = cfg.routes.as_ref().unwrap()[1].clone();
    balanced.path = "/balanced".into();
    balanced.proxy_pass = None;
    balanced.proxy_upstreams = vec![
        format!("http://{unavailable_addr}"),
        format!("http://{backend_addr}"),
    ];
    cfg.routes.as_mut().unwrap().push(balanced);
    let mut write_route = cfg.routes.as_ref().unwrap()[2].clone();
    write_route.path = "/no-retry".into();
    write_route.proxy_upstreams[1].push_str("/write");
    cfg.routes.as_mut().unwrap().push(write_route);
    let mut rotating = cfg.routes.as_ref().unwrap()[2].clone();
    rotating.path = "/rotate".into();
    rotating.proxy_upstreams = vec![
        format!("http://{backend_addr}/one"),
        format!("http://{backend_addr}/two"),
    ];
    cfg.routes.as_mut().unwrap().push(rotating);
    let cfg = Arc::new(cfg);
    let listener = crate::server::net::bind_tcp("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let trigger = Arc::new(crate::server::shutdown::ShutdownTrigger::new());
    let signal = trigger.clone();
    let server_task = tokio::spawn(async move {
        loop {
            let (socket, peer) = listener.accept().await.unwrap();
            let cfg = cfg.clone();
            let shutdown = signal.subscribe();
            tokio::spawn(async move {
                crate::server::connection::handle_connection(
                    socket,
                    peer,
                    Arc::new(arc_swap::ArcSwap::from_pointee(
                        crate::server::reload::Snapshot::new(cfg).unwrap(),
                    )),
                    shutdown,
                    false,
                )
                .await
                .unwrap();
            });
        }
    });
    let client = Client::builder(TokioExecutor::new()).build_http();
    let response = request(&client, address, Method::GET, "/assets/app.txt", &[]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-cache"], "MISS");
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "hello page"
    );
    let response = request(&client, address, Method::GET, "/assets/app.txt", &[]).await;
    assert_eq!(response.headers()["x-cache"], "HIT");
    response.into_body().collect().await.unwrap();
    let response = request(
        &client,
        address,
        Method::GET,
        "/assets/app.txt",
        &[("if-none-match", &format!("\"other\", {etag}"))],
    )
    .await;
    assert_eq!(response.status(), 304);
    let response = request(
        &client,
        address,
        Method::GET,
        "/assets/app.txt",
        &[("range", "bytes=1-4")],
    )
    .await;
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 1-4/10");
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "ello"
    );
    let response = request(
        &client,
        address,
        Method::GET,
        "/assets/app.txt",
        &[("range", "bytes=-4")],
    )
    .await;
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "page"
    );
    let response = request(
        &client,
        address,
        Method::GET,
        "/assets/app.txt",
        &[("range", "bytes=99-")],
    )
    .await;
    assert_eq!(response.status(), 416);
    let response = request(
        &client,
        address,
        Method::GET,
        "/assets/app.txt",
        &[("range", "bytes=0-1"), ("if-range", "\"stale\"")],
    )
    .await;
    assert_eq!(response.status(), 200);
    response.into_body().collect().await.unwrap();
    let response = request(&client, address, Method::HEAD, "/large.bin", &[]).await;
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        (3 * 1024 * 1024).to_string()
    );
    assert!(response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());
    let response = request(&client, address, Method::GET, "/large.bin", &[]).await;
    assert_eq!(response.headers()["x-cache"], "BYPASS");
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(first.len() < 3 * 1024 * 1024);
    assert_eq!(
        body.collect().await.unwrap().to_bytes().len() + first.len(),
        3 * 1024 * 1024
    );
    let response = request(&client, address, Method::GET, "/assets/nested", &[]).await;
    assert_eq!(response.headers()[header::LOCATION], "/assets/nested/");
    response.into_body().collect().await.unwrap();
    let response = request(
        &client,
        address,
        Method::GET,
        "/app.txt",
        &[("accept-encoding", "gzip")],
    )
    .await;
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    let compressed = response.into_body().collect().await.unwrap().to_bytes();
    let mut decoded = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(&compressed[..]),
        &mut decoded,
    )
    .unwrap();
    assert_eq!(decoded, "hello page");
    let response = request(
        &client,
        address,
        Method::GET,
        "/compress.txt",
        &[("accept-encoding", "br;q=0,gzip;q=0")],
    )
    .await;
    assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
    assert!(response.headers()[header::VARY]
        .to_str()
        .unwrap()
        .contains("Accept-Encoding"));
    response.into_body().collect().await.unwrap();
    let response = request(
        &client,
        address,
        Method::GET,
        "/compress.txt",
        &[("accept-encoding", "br;q=0,gzip;q=1")],
    )
    .await;
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    response.into_body().collect().await.unwrap();
    std::fs::write(folder.join("app.txt"), "a newly deployed page").unwrap();
    let response = request(&client, address, Method::GET, "/app.txt", &[]).await;
    assert_ne!(response.headers()[header::ETAG], etag);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "a newly deployed page"
    );
    let h2 = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build_http();
    let response = request(&h2, address, Method::GET, "/assets/app.txt", &[]).await;
    assert_eq!(response.version(), hyper::Version::HTTP_2);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "a newly deployed page"
    );
    drop(h2);
    for _ in 0..2 {
        let response = request(
            &client,
            address,
            Method::GET,
            "/proxy/a%2Fb?x=1",
            &[
                ("connection", "x-remove-me"),
                ("x-remove-me", "secret"),
                ("x-forwarded-for", "forged"),
            ],
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "/proxy/a%2Fb?x=1"
        );
    }
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "upstream connection must be reused"
    );
    let response = request(&client, address, Method::GET, "/balanced/test", &[]).await;
    assert_eq!(
        response.status(),
        200,
        "GET should fail over from an unavailable backend"
    );
    response.into_body().collect().await.unwrap();
    let response = request(&client, address, Method::GET, "/proxy/stream", &[]).await;
    // The stream has sent headers; its second chunk remains gated below.
    let mut stream = response.into_body();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(500), stream.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap(),
        "first"
    );
    release.notify_one();
    assert_eq!(stream.collect().await.unwrap().to_bytes(), "second");
    let response = request(&client, address, Method::POST, "/no-retry/test", &[]).await;
    assert!(
        matches!(response.status().as_u16(), 502 | 504),
        "writes must not be replayed on another backend"
    );
    response.into_body().collect().await.unwrap();
    for expected in ["/one/rotate/test", "/two/rotate/test"] {
        let response = request(&client, address, Method::GET, "/rotate/test", &[]).await;
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected
        );
    }
    let mut socket = crate::server::net::connect_tcp(address).await.unwrap();
    socket.write_all(format!("GET /proxy/socket HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").as_bytes()).await.unwrap();
    let mut handshake = Vec::new();
    while !handshake.ends_with(b"\r\n\r\n") {
        handshake.push(
            tokio::time::timeout(Duration::from_secs(3), socket.read_u8())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(String::from_utf8(handshake)
        .unwrap()
        .starts_with("HTTP/1.1 101"));
    socket.write_all(b"ping").await.unwrap();
    let mut echo = [0; 4];
    tokio::time::timeout(Duration::from_secs(3), socket.read_exact(&mut echo))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&echo, b"ping");
    drop(socket);
    let mut slow = crate::server::net::connect_tcp(address).await.unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\nHost:").await.unwrap();
    let mut error = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), slow.read_to_end(&mut error))
        .await
        .unwrap()
        .unwrap();
    assert!(error.is_empty() || String::from_utf8_lossy(&error).contains("408"));
    let response = request(&client, address, Method::GET, "/metrics", &[]).await;
    assert_eq!(response.status(), 200);
    assert!(
        String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
            .contains("flashlin_requests_total")
    );
    drop(client);
    trigger.fire();
    server_task.abort();
    backend_task.abort();
    // Remove only the unique fixture directory created at the top of this test.
    let _ = std::fs::remove_dir_all(folder);
}
