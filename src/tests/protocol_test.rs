//! Real, certificate-verifying TLS/TCP and QUIC/UDP protocol regression tests.
use crate::server::{
    reload::{LiveConfig, Snapshot},
    shutdown::ShutdownTrigger,
};
use bytes::{Buf, Bytes};
use http_body_util::{BodyExt, Empty, Full};
use std::{sync::Arc, time::Duration};

struct Fixture {
    root: std::path::PathBuf,
    live: LiveConfig,
    endpoint: quinn::Endpoint,
    tcp: tokio::net::TcpListener,
    trigger: Arc<ShutdownTrigger>,
    limiter: crate::server::overload::ConnectionLimiter,
    listener: tokio::task::JoinHandle<anyhow::Result<()>>,
    certificate: rustls::pki_types::CertificateDer<'static>,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("protocol-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::write(root.join("public/index.html"), "hello over all protocols").unwrap();
        std::fs::write(root.join("public/large.bin"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        std::fs::write(root.join("cert.pem"), cert.cert.pem()).unwrap();
        std::fs::write(root.join("key.pem"), cert.key_pair.serialize_pem()).unwrap();
        let mut cfg = crate::default_config();
        cfg.routes = None;
        cfg.compression = None;
        cfg.cache.as_mut().unwrap().max_file_size = "64KB".into();
        cfg.server.host = "127.0.0.1".into();
        cfg.server.root = Some(root.join("public").to_string_lossy().into());
        cfg.server.read_timeout = "1s".into();
        cfg.server.write_timeout = "2s".into();
        cfg.server.max_connections = 4;
        cfg.security.as_mut().unwrap().max_body_size = "1KB".into();
        cfg.tls = Some(crate::config::TlsConfig {
            enabled: true,
            acme: None,
            cert: Some(root.join("cert.pem").to_string_lossy().into()),
            key: Some(root.join("key.pem").to_string_lossy().into()),
            http_port: 80,
            redirect_http_to_https: false,
        });
        cfg.http3 = Some(crate::config::parser::Http3Config {
            enabled: true,
            port: None,
            max_concurrent_streams: 4,
        });
        // Reserve both transports together. An ephemeral UDP port may be in
        // use or excluded for TCP, particularly when the Windows suite is parallel.
        let mut sockets = None;
        for _ in 0..32 {
            let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            if let Ok(udp) = std::net::UdpSocket::bind(tcp.local_addr().unwrap()) {
                tcp.set_nonblocking(true).unwrap();
                sockets = Some((tokio::net::TcpListener::from_std(tcp).unwrap(), udp));
                break;
            }
        }
        let (tcp, socket) = sockets.expect("no common free TCP/UDP test port");
        cfg.server.port = socket.local_addr().unwrap().port();
        crate::config::validate(&cfg).unwrap();
        std::fs::write(root.join("server.toml"), toml::to_string(&cfg).unwrap()).unwrap();
        let live = Arc::new(arc_swap::ArcSwap::from_pointee(
            Snapshot::new(Arc::new(cfg)).unwrap(),
        ));
        let endpoint = crate::server::http3::endpoint(
            socket,
            live.clone(),
            crate::tls::managed::ManagedTls::Manual,
        )
        .unwrap();
        let trigger = Arc::new(ShutdownTrigger::new());
        let limiter = crate::server::overload::ConnectionLimiter::new(4);
        let listener = tokio::spawn(crate::server::http3::accept_loop(
            endpoint.clone(),
            live.clone(),
            limiter.clone(),
            trigger.clone(),
        ));
        Self {
            root,
            live,
            endpoint,
            tcp,
            trigger,
            limiter,
            listener,
            certificate: cert.cert.der().clone(),
        }
    }
    fn client_config(&self, alpn: &[u8]) -> rustls::ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(self.certificate.clone()).unwrap();
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![alpn.to_vec()];
        config
    }
    async fn connect(&self) -> Client {
        let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(self.client_config(b"h3")).unwrap(),
        )));
        let connection = tokio::time::timeout(
            Duration::from_secs(5),
            endpoint
                .connect(self.endpoint.local_addr().unwrap(), "localhost")
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let handshake = connection
            .handshake_data()
            .unwrap()
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .unwrap();
        assert_eq!(handshake.protocol.as_deref(), Some(b"h3".as_slice()));
        let (mut driver, sender) = h3::client::new(h3_quinn::Connection::new(connection.clone()))
            .await
            .unwrap();
        let task =
            tokio::spawn(
                async move { futures_util::future::poll_fn(|cx| driver.poll_close(cx)).await },
            );
        Client {
            endpoint,
            connection,
            sender,
            task,
        }
    }
    async fn finish(self) {
        self.trigger.fire();
        self.listener.await.unwrap().unwrap();
        self.endpoint.close(0_u32.into(), b"test finished");
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.limiter.active() > 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.endpoint.wait_idle().await;
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
struct Client {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    sender: h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>,
    task: tokio::task::JoinHandle<h3::error::ConnectionError>,
}
impl Client {
    async fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (http::Response<()>, Bytes) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut request = http::Request::builder()
                .method(method)
                .uri(format!("https://localhost{path}"));
            for (k, v) in headers {
                request = request.header(*k, *v);
            }
            let mut stream = self
                .sender
                .send_request(request.body(()).unwrap())
                .await
                .unwrap();
            if !body.is_empty() {
                stream
                    .send_data(Bytes::copy_from_slice(body))
                    .await
                    .unwrap();
            }
            stream.finish().await.unwrap();
            let response = stream.recv_response().await.unwrap();
            let mut body = Vec::new();
            while let Some(mut data) = stream.recv_data().await.unwrap() {
                body.extend_from_slice(&data.copy_to_bytes(data.remaining()));
            }
            (response, body.into())
        })
        .await
        .unwrap()
    }
    async fn close(self) {
        self.connection.close(0_u32.into(), b"test finished");
        self.task.abort();
        let _ = self.task.await;
        self.endpoint.wait_idle().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_static_range_head_security_limits_and_parallel_streams() {
    let fixture = Fixture::new().await;
    let mut client = fixture.connect().await;
    let (response, body) = client.request("GET", "/", &[], b"").await;
    assert_eq!(response.version(), http::Version::HTTP_3);
    assert_eq!(response.status(), 200);
    assert_eq!(body, "hello over all protocols");
    let etag = response.headers()["etag"].to_str().unwrap();
    assert_eq!(
        client
            .request("GET", "/", &[("if-none-match", etag)], b"")
            .await
            .0
            .status(),
        304
    );
    let (response, body) = client.request("HEAD", "/", &[], b"").await;
    assert_eq!(response.headers()["content-length"], "24");
    assert!(body.is_empty());
    let (response, body) = client
        .request("GET", "/large.bin", &[("range", "bytes=2-5")], b"")
        .await;
    assert_eq!(response.status(), 206);
    assert_eq!(body, "xxxx");
    assert_eq!(
        client.request("GET", "/large.bin", &[], b"").await.1.len(),
        2 * 1024 * 1024
    );
    assert_eq!(
        client.request("GET", "/.env", &[], b"").await.0.status(),
        403
    );
    assert_eq!(
        client
            .request("GET", "/%2e%2e/private", &[], b"")
            .await
            .0
            .status(),
        403
    );
    assert_eq!(
        client
            .request("QUERY", "/", &[("content-type", "application/json")], b"{}")
            .await
            .0
            .status(),
        405
    );
    assert_eq!(
        client.request("QUERY", "/", &[], b"{}").await.0.status(),
        400
    );
    assert_eq!(
        client
            .request(
                "QUERY",
                "/",
                &[("content-type", "application/json")],
                &vec![b'x'; 2048]
            )
            .await
            .0
            .status(),
        413
    );
    // One unfinished upload must not block another request on the connection.
    let mut slow = client
        .sender
        .send_request(
            http::Request::builder()
                .method("QUERY")
                .uri("https://localhost/")
                .header("content-type", "application/json")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    slow.send_data(Bytes::from_static(b"{")).await.unwrap();
    assert_eq!(client.request("GET", "/", &[], b"").await.0.status(), 200);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), slow.recv_response())
            .await
            .unwrap()
            .unwrap()
            .status(),
        408
    );
    drop(slow);
    client.close().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http1_http2_tls_and_http3_share_port_with_correct_discovery() {
    let fixture = Fixture::new().await;
    let addr = fixture.tcp.local_addr().unwrap();
    for protocol in [b"http/1.1".as_slice(), b"h2".as_slice()] {
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (server, peer) = fixture.tcp.accept().await.unwrap();
        let live = fixture.live.clone();
        let shutdown = fixture.trigger.subscribe();
        let acceptor = live.load().acceptor.clone().unwrap();
        let task = tokio::spawn(async move {
            let stream = acceptor.accept(server).await.unwrap();
            crate::server::connection::handle_connection(stream, peer, live, shutdown, true)
                .await
                .unwrap();
        });
        let connector = tokio_rustls::TlsConnector::from(Arc::new(fixture.client_config(protocol)));
        let tls = connector
            .connect("localhost".try_into().unwrap(), socket)
            .await
            .unwrap();
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(protocol));
        let request = http::Request::builder()
            .uri("https://localhost/")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let (response, client_task) = if protocol == b"h2" {
            let (mut client, conn) = hyper::client::conn::http2::handshake(
                hyper_util::rt::TokioExecutor::new(),
                hyper_util::rt::TokioIo::new(tls),
            )
            .await
            .unwrap();
            let task = tokio::spawn(async move {
                let _ = conn.await;
            });
            (client.send_request(request).await.unwrap(), task)
        } else {
            let (mut client, conn) =
                hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
                    .await
                    .unwrap();
            let task = tokio::spawn(async move {
                let _ = conn.await;
            });
            (client.send_request(request).await.unwrap(), task)
        };
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["alt-svc"],
            format!("h3=\":{}\"; ma=300", addr.port())
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "hello over all protocols"
        );
        client_task.abort();
        task.await.unwrap();
    }
    let mut plain = crate::http::response::empty_response(200);
    crate::server::connection::advertise_http3(&mut plain, &fixture.live.load().config, false);
    assert!(!plain.headers().contains_key("alt-svc"));
    let mut client = fixture.connect().await;
    assert_eq!(client.request("GET", "/", &[], b"").await.0.status(), 200);
    client.close().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_query_proxy_body_framing_reload_and_certificate_rotation() {
    let mut fixture = Fixture::new().await;
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = tcp.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        loop {
            let (socket, _) = tcp.accept().await.unwrap();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(
                    |req: http::Request<hyper::body::Incoming>| async move {
                        assert_eq!(req.method().as_str(), "QUERY");
                        assert_eq!(req.headers()["x-forwarded-proto"], "https");
                        assert_eq!(req.headers()["content-type"], "application/json");
                        assert_eq!(
                            req.uri().path_and_query().unwrap().as_str(),
                            "/search?limit=2"
                        );
                        let body = req.into_body().collect().await.unwrap().to_bytes();
                        Ok::<_, std::convert::Infallible>(
                            http::Response::builder()
                                .header("accept-query", "application/json")
                                .body(Full::new(body))
                                .unwrap(),
                        )
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    let path = fixture.root.join("server.toml");
    let mut cfg = (*fixture.live.load().config).clone();
    let route: crate::config::RouteConfig = toml::from_str(&format!(
        "path = '/search'\nproxy_pass = 'http://{backend}'\nmethods = ['QUERY', 'OPTIONS']"
    ))
    .unwrap();
    cfg.routes = Some(vec![route]);
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    crate::server::reload::reload_once(&fixture.live, path.to_str().unwrap()).unwrap();
    let mut client = fixture.connect().await;
    for body in [b"{\"n\":1}".as_slice(), b"{\"n\":2}".as_slice()] {
        let (response, received) = client
            .request(
                "QUERY",
                "/search?limit=2",
                &[
                    ("content-type", "application/json"),
                    ("content-length", "7"),
                ],
                body,
            )
            .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["accept-query"], "application/json");
        assert_eq!(received, body);
    }
    // Incorrect content length resets only the bad stream, never reaches upstream.
    let mut bad = client
        .sender
        .send_request(
            http::Request::builder()
                .method("QUERY")
                .uri("https://localhost/search?limit=2")
                .header("content-type", "application/json")
                .header("content-length", "20")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    bad.send_data(Bytes::from_static(b"{}")).await.unwrap();
    bad.finish().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), bad.recv_response())
            .await
            .unwrap()
            .is_err()
    );
    drop(bad);
    assert_eq!(client.request("GET", "/", &[], b"").await.0.status(), 200);
    // Existing QUIC connections observe the next validated routing generation.
    std::fs::create_dir_all(fixture.root.join("next")).unwrap();
    std::fs::write(fixture.root.join("next/index.html"), "reloaded").unwrap();
    cfg.server.root = Some(fixture.root.join("next").to_string_lossy().into());
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    assert!(crate::server::reload::reload_once(&fixture.live, path.to_str().unwrap()).unwrap());
    assert_eq!(client.request("GET", "/", &[], b"").await.1, "reloaded");
    cfg.http3.as_mut().unwrap().port = Some(12345);
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    assert!(crate::server::reload::reload_once(&fixture.live, path.to_str().unwrap()).is_err());
    cfg.http3.as_mut().unwrap().port = None;
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let rotated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(fixture.root.join("cert.pem"), rotated.cert.pem()).unwrap();
    assert!(crate::server::reload::reload_once(&fixture.live, path.to_str().unwrap()).is_err());
    std::fs::write(
        fixture.root.join("key.pem"),
        rotated.key_pair.serialize_pem(),
    )
    .unwrap();
    assert!(crate::server::reload::reload_once(&fixture.live, path.to_str().unwrap()).unwrap());
    fixture.certificate = rotated.cert.der().clone();
    let mut next = fixture.connect().await;
    assert_eq!(next.request("GET", "/", &[], b"").await.1, "reloaded");
    next.close().await;
    client.close().await;
    upstream.abort();
    fixture.finish().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_connection_cap_and_shutdown_drain_preserve_response() {
    let fixture = Fixture::new().await;
    let mut client = fixture.connect().await;
    // Represent three TCP connections competing with this QUIC connection.
    let slots: Vec<_> = (0..3)
        .map(|_| fixture.limiter.try_acquire().unwrap())
        .collect();
    assert_eq!(fixture.limiter.active(), 4);
    let mut extra = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    extra.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(fixture.client_config(b"h3")).unwrap(),
    )));
    assert!(tokio::time::timeout(
        Duration::from_secs(5),
        extra
            .connect(fixture.endpoint.local_addr().unwrap(), "localhost")
            .unwrap()
    )
    .await
    .unwrap()
    .is_err());
    drop(slots);
    let mut stream = client
        .sender
        .send_request(
            http::Request::builder()
                .uri("https://localhost/large.bin")
                .body(())
                .unwrap(),
        )
        .await
        .unwrap();
    stream.finish().await.unwrap();
    assert_eq!(stream.recv_response().await.unwrap().status(), 200);
    fixture.trigger.fire();
    let count = tokio::time::timeout(Duration::from_secs(5), async {
        let mut count = 0;
        while let Some(data) = stream.recv_data().await.unwrap() {
            count += data.remaining();
        }
        count
    })
    .await
    .unwrap();
    assert_eq!(count, 2 * 1024 * 1024);
    drop(stream);
    client.close().await;
    extra.wait_idle().await;
    fixture.finish().await;
}
