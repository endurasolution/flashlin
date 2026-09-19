use crate::server::reload::{load_config, reload_once, Snapshot};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use std::sync::Arc;

fn fixture() -> std::path::PathBuf {
    let folder = std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("reload-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

#[tokio::test]
async fn watcher_loads_new_domain_and_rejects_broken_domain_files() {
    let folder = fixture();
    std::fs::create_dir_all(folder.join("domains")).unwrap();
    let path = folder.join("server.toml");
    let path = path.to_string_lossy().into_owned();
    let mut cfg = crate::default_config();
    cfg.server.domains_dir = Some("domains".into());
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        Snapshot::new(Arc::new(load_config(&path).unwrap())).unwrap(),
    ));
    let task = tokio::spawn(crate::server::reload::watch(live.clone(), path.clone()));
    let domain = folder.join("domains/site.toml");
    std::fs::write(&domain, "[domain]\nname = 'example.com'\n").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while live.load().config.virtual_hosts.len() != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    let _ = task.await;
    let stable = live.load_full();
    std::fs::write(&domain, "[broken").unwrap();
    assert!(reload_once(&live, &path).is_err());
    assert!(Arc::ptr_eq(&stable, &live.load_full()));
    std::fs::remove_file(&domain).unwrap();
    assert!(reload_once(&live, &path).unwrap());
    assert!(live.load().config.virtual_hosts.is_empty());
    std::fs::remove_dir_all(folder).unwrap();
}

#[cfg(feature = "acme")]
#[test]
fn acme_rejects_public_cache_and_wildcard_domains() {
    let folder = fixture();
    let mut cfg: crate::config::ServerConfig =
        toml::from_str(include_str!("../../examples/automatic-https.toml")).unwrap();
    let tls = cfg.tls.as_mut().unwrap();
    let acme = tls.acme.as_mut().unwrap();
    acme.accept_terms = true;
    acme.cache_dir = folder.join("private").to_string_lossy().into();
    cfg.server.root = Some(folder.to_string_lossy().into());
    // A server document root cannot contain private ACME storage.
    assert!(crate::config::validate(&cfg).is_err());
    cfg.server.root = Some("./public".into());
    assert!(crate::config::validate(&cfg).is_ok());
    cfg.tls.as_mut().unwrap().acme.as_mut().unwrap().domains = vec!["*.example.com".into()];
    assert!(crate::config::validate(&cfg).is_err());
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn reload_updates_keepalive_and_rejects_invalid_or_restart_settings() {
    let folder = fixture();
    std::fs::create_dir_all(folder.join("a")).unwrap();
    std::fs::create_dir_all(folder.join("b")).unwrap();
    std::fs::write(folder.join("a/index.html"), "before").unwrap();
    std::fs::write(folder.join("b/index.html"), "after").unwrap();
    let path = folder.join("server.toml");
    let path = path.to_str().unwrap();
    let mut cfg = crate::default_config();
    cfg.compression = None;
    cfg.server.root = Some(folder.join("a").to_string_lossy().into_owned());
    cfg.routes.as_mut().unwrap()[0].root = cfg.server.root.clone();
    std::fs::write(path, toml::to_string(&cfg).unwrap()).unwrap();
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        Snapshot::new(Arc::new(load_config(path).unwrap())).unwrap(),
    ));
    let old_state = Arc::downgrade(&live.load().config.state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, peer) = listener.accept().await.unwrap();
    let trigger = crate::server::shutdown::ShutdownTrigger::new();
    let task = tokio::spawn(crate::server::connection::handle_connection(
        server,
        peer,
        live.clone(),
        trigger.subscribe(),
        false,
    ));
    let (mut client, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
            .await
            .unwrap();
    let client_task = tokio::spawn(connection);
    for expected in ["before", "after"] {
        let req = hyper::Request::builder()
            .uri("/")
            .header("host", "localhost")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let response = client.send_request(req).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.collect().await.unwrap().to_bytes(), expected);
        if expected == "before" {
            cfg.server.root = Some(folder.join("b").to_string_lossy().into_owned());
            cfg.routes.as_mut().unwrap()[0].root = cfg.server.root.clone();
            cfg.cache.as_mut().unwrap().max_size = "2MB".into();
            std::fs::write(path, toml::to_string(&cfg).unwrap()).unwrap();
            assert!(reload_once(&live, path).unwrap());
            assert!(
                old_state.upgrade().is_none(),
                "Idle keep-alive connection retained obsolete state"
            );
            assert!(!reload_once(&live, path).unwrap());
            let stable = live.load_full();
            std::fs::write(path, "[server\ninvalid").unwrap();
            assert!(reload_once(&live, path).is_err());
            assert!(Arc::ptr_eq(&stable, &live.load_full()));
            cfg.server.port += 1;
            std::fs::write(path, toml::to_string(&cfg).unwrap()).unwrap();
            assert!(reload_once(&live, path).is_err());
            assert!(Arc::ptr_eq(&stable, &live.load_full()));
        }
    }
    client_task.abort();
    trigger.fire();
    task.await.unwrap().unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}

#[cfg(feature = "tls")]
#[test]
fn manual_certificates_rotate_and_bad_keys_preserve_snapshot() {
    let folder = fixture();
    let cert_path = folder.join("cert.pem");
    let key_path = folder.join("key.pem");
    let config_path = folder.join("server.toml");
    let mut cfg = crate::default_config();
    cfg.tls = Some(crate::config::TlsConfig {
        enabled: true,
        cert: Some(cert_path.to_string_lossy().into()),
        key: Some(key_path.to_string_lossy().into()),
        acme: None,
        redirect_http_to_https: false,
        http_port: 80,
    });
    let first = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(&cert_path, first.cert.pem()).unwrap();
    std::fs::write(&key_path, first.key_pair.serialize_pem()).unwrap();
    std::fs::write(&config_path, toml::to_string(&cfg).unwrap()).unwrap();
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        Snapshot::new(Arc::new(cfg)).unwrap(),
    ));
    let previous = live.load_full();
    let second = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(&cert_path, second.cert.pem()).unwrap();
    // Mixed generations of cert/key cannot replace the active pair.
    assert!(reload_once(&live, config_path.to_str().unwrap()).is_err());
    assert!(Arc::ptr_eq(&previous, &live.load_full()));
    std::fs::write(&key_path, second.key_pair.serialize_pem()).unwrap();
    assert!(reload_once(&live, config_path.to_str().unwrap()).unwrap());
    assert!(!Arc::ptr_eq(&previous, &live.load_full()));
    std::fs::remove_dir_all(folder).unwrap();
}

#[cfg(feature = "acme")]
#[tokio::test]
async fn acme_loads_cached_certificate_and_serves_verified_tls_offline() {
    use rustls_acme::CertCache;
    let folder = fixture();
    let private = folder.join("private");
    let cert = rcgen::generate_simple_self_signed(vec!["example.com".into()]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&private)
            .unwrap();
    }
    let cache = rustls_acme::caches::DirCache::new(private.clone());
    let domains = vec!["example.com".to_string()];
    cache
        .store_cert(
            &domains,
            "https://acme-staging-v02.api.letsencrypt.org/directory",
            format!("{}{}", cert.key_pair.serialize_pem(), cert.cert.pem()).as_bytes(),
        )
        .await
        .unwrap();
    let mut cfg = crate::default_config();
    cfg.tls = Some(crate::config::TlsConfig {
        enabled: true,
        cert: None,
        key: None,
        redirect_http_to_https: false,
        http_port: 80,
        acme: Some(crate::config::parser::AcmeConfig {
            domains,
            email: "admin@example.com".into(),
            cache_dir: private.to_string_lossy().into(),
            production: false,
            accept_terms: true,
        }),
    });
    assert!(crate::config::validate(&cfg).is_ok());
    cfg.tls
        .as_mut()
        .unwrap()
        .acme
        .as_mut()
        .unwrap()
        .accept_terms = false;
    assert!(crate::config::validate(&cfg).is_err());
    cfg.tls
        .as_mut()
        .unwrap()
        .acme
        .as_mut()
        .unwrap()
        .accept_terms = true;
    let trigger = crate::server::shutdown::ShutdownTrigger::new();
    let trigger = Arc::new(trigger);
    #[cfg(feature = "http3")]
    {
        cfg.http3 = Some(crate::config::parser::Http3Config {
            enabled: true,
            port: None,
            max_concurrent_streams: 4,
        });
    }
    let endpoint =
        crate::tls::managed::ManagedTls::new(cfg.tls.as_ref().unwrap(), trigger.subscribe())
            .unwrap();
    let live = Arc::new(arc_swap::ArcSwap::from_pointee(
        Snapshot::new(Arc::new(cfg)).unwrap(),
    ));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let mut client = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let socket = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (server, _) = listener.accept().await.unwrap();
            let (accepted, connected) = tokio::join!(
                endpoint.accept(server, &live),
                connector.connect("example.com".try_into().unwrap(), socket)
            );
            if let (Ok(Some(_)), Ok(client)) = (accepted, connected) {
                assert_eq!(client.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    #[cfg(feature = "http3")]
    {
        // The ACME cache is now loaded. Verify that QUIC uses that same resolver,
        // without disabling certificate checks or contacting a CA for issuance.
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let quic = crate::server::http3::endpoint(socket, live.clone(), endpoint.clone()).unwrap();
        let addr = quic.local_addr().unwrap();
        let task = tokio::spawn(crate::server::http3::accept_loop(
            quic.clone(),
            live.clone(),
            crate::server::overload::ConnectionLimiter::new(4),
            trigger.clone(),
        ));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).unwrap();
        let mut crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        crypto.alpn_protocols = vec![b"h3".to_vec()];
        let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
        )));
        let connection = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.connect(addr, "example.com").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let (mut driver, mut sender) =
            h3::client::new(h3_quinn::Connection::new(connection.clone()))
                .await
                .unwrap();
        let driver =
            tokio::spawn(
                async move { futures_util::future::poll_fn(|cx| driver.poll_close(cx)).await },
            );
        let mut stream = sender
            .send_request(
                http::Request::builder()
                    .uri("https://example.com/metrics")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap();
        stream.finish().await.unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), stream.recv_response())
                .await
                .unwrap()
                .unwrap()
                .status(),
            200
        );
        while stream.recv_data().await.unwrap().is_some() {}
        connection.close(0_u32.into(), b"finished");
        driver.abort();
        trigger.fire();
        task.await.unwrap().unwrap();
        quic.close(0_u32.into(), b"finished");
        client.wait_idle().await;
    }
    trigger.fire();
    tokio::task::yield_now().await;
    std::fs::remove_dir_all(folder).unwrap();
}
