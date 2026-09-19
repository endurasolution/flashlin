//! QUERY transport, discovery, validation and replay over actual sockets.
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, Response};
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
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[test]
fn query_metadata_and_method_tokens_are_validated() {
    use crate::http::request::{is_query, query_content_type_valid};
    assert!(is_query(&Method::from_bytes(b"QUERY").unwrap()));
    assert!(!is_query(&Method::from_bytes(b"query").unwrap()));
    let mut headers = hyper::HeaderMap::new();
    assert!(!query_content_type_valid(&headers));
    headers.insert(
        "content-type",
        "application/json; charset=utf-8".parse().unwrap(),
    );
    assert!(query_content_type_valid(&headers));
    headers.append("content-type", "application/json".parse().unwrap());
    assert!(!query_content_type_valid(&headers));
    let mut cfg = crate::default_config();
    cfg.routes.as_mut().unwrap()[0].methods = Some(vec!["QUERY\r\ninjected: value".into()]);
    assert!(crate::config::validate(&cfg).is_err());
}

#[test]
fn query_cgi_metadata_preserves_method_media_type_and_body_length() {
    let mut req = crate::http::request::ParsedRequest {
        method: Method::from_bytes(b"QUERY").unwrap(),
        uri: "/search.php?limit=2".parse().unwrap(),
        headers: hyper::HeaderMap::new(),
        body: Bytes::new(),
        path: "/search.php".into(),
        query: "limit=2".into(),
        peer: None,
        tls: false,
        upgrade: Default::default(),
    };
    req.headers
        .insert("content-type", "application/json".parse().unwrap());
    for body in ["", "{\"filter\":1}"] {
        req.body = Bytes::copy_from_slice(body.as_bytes());
        let params = crate::php::params::build_cgi_params(&req, "/srv/search.php", "/srv", None);
        assert_eq!(params["REQUEST_METHOD"], "QUERY");
        assert_eq!(params["CONTENT_TYPE"], "application/json");
        assert_eq!(params["CONTENT_LENGTH"], body.len().to_string());
        assert_eq!(params["QUERY_STRING"], "limit=2");
    }
}

async fn send(
    client: &Client<HttpConnector, Full<Bytes>>,
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    content_type: Option<&str>,
) -> Response<hyper::body::Incoming> {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("http://{addr}{path}"));
    if let Some(kind) = content_type {
        request = request.header("content-type", kind);
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.request(
            request
                .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_body_discovery_limits_static_rejection_and_failover() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("query-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("cors")).unwrap();
    std::fs::write(root.join("index.html"), "must not become a query result").unwrap();
    std::fs::write(root.join("cors/.flashweb"), "[cors]\norigin = 'https://client.example'\nmethods = ['QUERY', 'OPTIONS']\nheaders = ['Content-Type']\n[[header]]\nname = 'Accept-Query'\nvalue = 'application/json'\n").unwrap();
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let backend = tokio::spawn(async move {
        loop {
            let (stream, _) = upstream.accept().await.unwrap();
            let counter = counter.clone();
            tokio::spawn(async move {
                let service =
                    hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                        let counter = counter.clone();
                        async move {
                            counter.fetch_add(1, Ordering::SeqCst);
                            let method = req.method().clone();
                            let uri = req.uri().to_string();
                            let content_type = req.headers().get("content-type").cloned();
                            let body = req.collect().await.unwrap().to_bytes();
                            let response = if method == Method::OPTIONS {
                                Response::builder()
                                    .status(204)
                                    .header("allow", "QUERY, OPTIONS")
                                    .header("accept-query", "application/json")
                                    .header("access-control-allow-methods", "QUERY, OPTIONS")
                                    .body(Full::new(Bytes::new()))
                                    .unwrap()
                            } else {
                                assert_eq!(method.as_str(), "QUERY");
                                if content_type.unwrap() != "application/json" {
                                    Response::builder()
                                        .status(415)
                                        .header("accept-query", "application/json")
                                        .body(Full::new(Bytes::new()))
                                        .unwrap()
                                } else if uri.starts_with("/fail/") {
                                    assert_eq!(body, r#"{"retry":true}"#);
                                    Response::builder()
                                        .status(503)
                                        .body(Full::new(Bytes::new()))
                                        .unwrap()
                                } else {
                                    Response::builder()
                                        .header("content-type", "application/json")
                                        .header("accept-query", "application/json")
                                        .header("content-location", "/result/42")
                                        .header("x-original-uri", uri)
                                        .body(Full::new(body))
                                        .unwrap()
                                }
                            };
                            Ok::<_, Infallible>(response)
                        }
                    });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let mut cfg = crate::default_config();
    cfg.server.root = Some(root.to_string_lossy().into());
    cfg.compression = None;
    cfg.security.as_mut().unwrap().max_body_size = "128B".into();
    cfg.routes.as_mut().unwrap()[0].root = cfg.server.root.clone();
    let mut route = cfg.routes.as_ref().unwrap()[0].clone();
    route.path = "/query".into();
    route.root = None;
    route.php_enabled = false;
    route.proxy_pass = Some(format!("http://{upstream_addr}"));
    route.methods = Some(vec!["QUERY".into(), "OPTIONS".into()]);
    cfg.routes.as_mut().unwrap().push(route.clone());
    route.path = "/retry".into();
    route.proxy_pass = None;
    route.proxy_upstreams = vec![
        format!("http://{upstream_addr}/fail"),
        format!("http://{upstream_addr}/ok"),
    ];
    cfg.routes.as_mut().unwrap().push(route.clone());
    route.path = "/cors".into();
    route.proxy_upstreams.clear();
    route.proxy_pass = Some(format!("http://{upstream_addr}"));
    cfg.routes.as_mut().unwrap().push(route);
    let cfg = Arc::new(cfg);
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        crate::server::reload::Snapshot::new(cfg).unwrap(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let trigger = Arc::new(crate::server::shutdown::ShutdownTrigger::new());
    let signal = trigger.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, peer) = listener.accept().await.unwrap();
            tokio::spawn(crate::server::connection::handle_connection(
                stream,
                peer,
                live.clone(),
                signal.subscribe(),
                false,
            ));
        }
    });
    let client = Client::builder(TokioExecutor::new()).build_http();
    let h2 = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build_http();
    for (client, body) in [
        (&client, r#"{"filter":"first"}"#),
        (&client, r#"{"filter":"second"}"#),
        (&h2, r#"{"filter":"h2"}"#),
    ] {
        let response = send(
            client,
            addr,
            "QUERY",
            "/query/a%2Fb?limit=2",
            body,
            Some("application/json"),
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["accept-query"], "application/json");
        assert_eq!(response.headers()["content-location"], "/result/42");
        assert_eq!(response.headers()["x-original-uri"], "/query/a%2Fb?limit=2");
        assert!(!response.headers().contains_key("x-cache"));
        assert_eq!(response.collect().await.unwrap().to_bytes(), body);
    }
    let response = send(&client, addr, "OPTIONS", "/query", "", None).await;
    assert_eq!(response.status(), 204);
    assert_eq!(response.headers()["allow"], "QUERY, OPTIONS");
    assert_eq!(response.headers()["accept-query"], "application/json");
    let unsupported = send(&client, addr, "QUERY", "/query", "test", Some("text/plain")).await;
    assert_eq!(unsupported.status(), 415);
    assert_eq!(unsupported.headers()["accept-query"], "application/json");
    for content_type in [None, Some(""), Some("not-a-media-type"), Some("*/*")] {
        let before = calls.load(Ordering::SeqCst);
        assert_eq!(
            send(&client, addr, "QUERY", "/query", "{}", content_type)
                .await
                .status(),
            400
        );
        assert_eq!(calls.load(Ordering::SeqCst), before);
    }
    assert_eq!(
        send(
            &client,
            addr,
            "QUERY",
            "/query",
            &"x".repeat(129),
            Some("application/json")
        )
        .await
        .status(),
        413
    );
    let response = send(
        &client,
        addr,
        "query",
        "/query",
        "{}",
        Some("application/json"),
    )
    .await;
    assert_eq!(response.status(), 405);
    assert_eq!(response.headers()["allow"], "QUERY, OPTIONS");
    for path in ["/index.html", "/"] {
        let response = send(&client, addr, "QUERY", path, "{}", Some("application/json")).await;
        assert_eq!(response.status(), 405);
        assert_eq!(response.headers()["allow"], "GET, HEAD, OPTIONS");
    }
    let before = calls.load(Ordering::SeqCst);
    let response = send(&client, addr, "OPTIONS", "/cors/test", "", None).await;
    assert_eq!(response.status(), 204);
    assert_eq!(response.headers()["accept-query"], "application/json");
    assert_eq!(
        response.headers()["access-control-allow-methods"],
        "QUERY, OPTIONS"
    );
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let response = send(
        &client,
        addr,
        "QUERY",
        "/retry",
        r#"{"retry":true}"#,
        Some("application/json"),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.collect().await.unwrap().to_bytes(),
        r#"{"retry":true}"#
    );
    assert_eq!(calls.load(Ordering::SeqCst), before + 2);
    // A chunked HTTP/1 body is decoded and forwarded without losing content.
    let mut socket = TcpStream::connect(addr).await.unwrap();
    socket.write_all(format!("QUERY /query HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\n{{\r\n1\r\n}}\r\n0\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        socket.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(b"{}"));
    trigger.fire();
    server.abort();
    backend.abort();
    std::fs::remove_dir_all(root).unwrap();
}
