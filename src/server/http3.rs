//! Optional HTTP/3 origin listener. Routing and bounded body collection are shared
//! with HTTP/1.1 and HTTP/2; upstream proxy connections remain HTTP/1.1 / HTTP/2.
use super::{overload::ConnectionLimiter, reload::LiveConfig, shutdown::ShutdownTrigger};
use crate::{
    config::{parse_duration_secs, parse_size},
    tls::managed::ManagedTls,
};
use bytes::{Buf, Bytes};
use h3::error::Code;
use http_body_util::BodyExt;
use hyper::body::{Body, Frame};
use std::{
    net::UdpSocket,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

struct CertificateResolver {
    live: LiveConfig,
    tls: ManagedTls,
}
impl std::fmt::Debug for CertificateResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveQuicCertificateResolver")
            .finish_non_exhaustive()
    }
}
impl rustls::server::ResolvesServerCert for CertificateResolver {
    fn resolve(
        &self,
        hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        match &self.tls {
            ManagedTls::Manual => self
                .live
                .load()
                .acceptor
                .as_ref()?
                .config()
                .cert_resolver
                .resolve(hello),
            #[cfg(feature = "acme")]
            ManagedTls::Automatic { normal, .. } => normal.cert_resolver.resolve(hello),
        }
    }
}

pub fn endpoint(
    socket: UdpSocket,
    live: LiveConfig,
    tls: ManagedTls,
) -> anyhow::Result<quinn::Endpoint> {
    let cfg = live.load().config.clone();
    let h3 = cfg
        .http3
        .as_ref()
        .filter(|h| h.enabled)
        .ok_or_else(|| anyhow::anyhow!("HTTP/3 is not enabled"))?;
    let mut crypto =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(CertificateResolver { live, tls }));
    crypto.alpn_protocols = vec![b"h3".to_vec()];
    // Do not accept replayable 0-RTT application requests.
    crypto.max_early_data_size = 0;
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto)?,
    ));
    // IP access/rate policies use the established peer address, so do not allow
    // active migration to a different address in this first implementation.
    server.migration(false);
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(h3.max_concurrent_streams.into())
        .max_concurrent_uni_streams(3_u32.into())
        .stream_receive_window((256_u32 * 1024).into())
        .receive_window((4_u32 * 1024 * 1024).into())
        .send_window(4 * 1024 * 1024)
        .datagram_receive_buffer_size(None)
        .max_idle_timeout(Some(
            Duration::from_secs(parse_duration_secs(&cfg.server.keep_alive_timeout).unwrap_or(75))
                .try_into()?,
        ));
    server.transport_config(Arc::new(transport));
    socket.set_nonblocking(true)?;
    Ok(quinn::Endpoint::new(
        Default::default(),
        Some(server),
        socket,
        Arc::new(quinn::TokioRuntime),
    )?)
}

pub async fn accept_loop(
    endpoint: quinn::Endpoint,
    live: LiveConfig,
    limiter: ConnectionLimiter,
    trigger: Arc<ShutdownTrigger>,
) -> anyhow::Result<()> {
    let mut shutdown = trigger.subscribe();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                // Validate the source before allocating a connection slot / TLS work.
                if !incoming.remote_address_validated() {
                    let _ = incoming.retry();
                    continue;
                }
                let Some(slot) = limiter.try_acquire() else { incoming.refuse(); continue; };
                let live = live.clone();
                let shutdown = trigger.subscribe();
                tokio::spawn(async move {
                    let _slot = slot;
                    match tokio::time::timeout(Duration::from_secs(10), incoming).await {
                        Ok(Ok(conn)) => {
                            if let Err(error) = serve(conn, live, shutdown).await {
                                tracing::debug!(%error, "HTTP/3 connection ended");
                            }
                        }
                        error => tracing::debug!(?error, "QUIC handshake failed or timed out"),
                    }
                });
            }
            _ = shutdown.wait() => break,
        }
    }
    // Stop new handshakes; existing connections receive the same shutdown signal.
    // Their endpoint handles remain alive while requests drain.
    endpoint.set_server_config(None);
    Ok(())
}

async fn serve(
    conn: quinn::Connection,
    live: LiveConfig,
    mut shutdown: super::shutdown::ShutdownSignal,
) -> anyhow::Result<()> {
    let peer = conn.remote_address();
    let cfg = live.load().config.clone();
    let max_streams = cfg.http3.as_ref().unwrap().max_concurrent_streams as usize;
    let header_limit = cfg
        .security
        .as_ref()
        .and_then(|s| parse_size(&s.max_header_size))
        .unwrap_or(32 * 1024)
        .clamp(1024, 16 * 1024 * 1024);
    let mut builder = h3::server::builder();
    builder.max_field_section_size(header_limit);
    let setup_timeout =
        Duration::from_secs(parse_duration_secs(&cfg.server.read_timeout).unwrap_or(30));
    let mut connection = tokio::time::timeout(
        setup_timeout,
        builder.build(h3_quinn::Connection::new(conn.clone())),
    )
    .await??;
    drop(cfg);
    let mut requests = tokio::task::JoinSet::new();
    let mut draining = false;
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            result = connection.accept(), if requests.len() < max_streams => {
                match result? {
                    Some(resolver) => {
                        let live = live.clone();
                        requests.spawn(async move {
                            if let Err(error) = handle_request(resolver, peer, live).await {
                                tracing::debug!(%error, "HTTP/3 request ended");
                            }
                        });
                    }
                    None => break,
                }
            }
            result = requests.join_next(), if !requests.is_empty() => {
                if let Some(Err(error)) = result { tracing::warn!(%error, "HTTP/3 request task failed"); }
            }
            _ = shutdown.wait(), if !draining => {
                draining = true;
                deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(30));
                tokio::time::timeout(Duration::from_secs(1), connection.shutdown(0)).await??;
            }
            _ = &mut deadline, if draining => break,
        }
        // Keep the control-stream driver alive while the client reads completed
        // responses. Closing QUIC immediately after send_data can discard data
        // still awaiting acknowledgement. The client GOAWAY or deadline ends drain.
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    Ok(())
}

struct RequestBody {
    stream: h3::server::RequestStream<h3_quinn::RecvStream, Bytes>,
    remaining: Option<u64>,
    data_done: bool,
    done: bool,
    malformed: Arc<std::sync::atomic::AtomicBool>,
}
impl RequestBody {
    fn invalid(&mut self) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        self.malformed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.done = true;
        Poll::Ready(Some(Err(std::io::Error::other(
            "Malformed HTTP/3 request body",
        ))))
    }
}
impl Body for RequestBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }
        if !self.data_done {
            match std::task::ready!(self.stream.poll_recv_data(cx)) {
                Ok(Some(mut data)) => {
                    let count = data.remaining();
                    if let Some(left) = self.remaining {
                        if count as u64 > left {
                            return self.invalid();
                        }
                        self.remaining = Some(left - count as u64);
                    }
                    return Poll::Ready(Some(Ok(Frame::data(data.copy_to_bytes(count)))));
                }
                Ok(None) => self.data_done = true,
                Err(_) => return self.invalid(),
            }
        }
        match std::task::ready!(self.stream.poll_recv_trailers(cx)) {
            Ok(trailers) => {
                if self.remaining.is_some_and(|left| left != 0) {
                    return self.invalid();
                }
                self.done = true;
                Poll::Ready(trailers.map(|h| Ok(Frame::trailers(h))))
            }
            Err(_) => self.invalid(),
        }
    }
}
// Let the underlying Quinn receive handle cancel unread data when dropped.
// h3-quinn 0.0.10's explicit stop_sending is not cancellation-safe while its
// read future is pending, which is exactly the request-body timeout case.

async fn handle_request(
    resolver: h3::server::RequestResolver<h3_quinn::Connection, Bytes>,
    peer: std::net::SocketAddr,
    live: LiveConfig,
) -> anyhow::Result<()> {
    let read_timeout = Duration::from_secs(
        parse_duration_secs(&live.load().config.server.read_timeout).unwrap_or(30),
    );
    let (req, stream) = tokio::time::timeout(read_timeout, resolver.resolve_request()).await??;
    let (mut send, mut recv) = stream.split();
    let length = req
        .headers()
        .get("content-length")
        .map(|v| {
            let value = v.to_str()?;
            anyhow::ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "Invalid content length"
            );
            Ok::<u64, anyhow::Error>(value.parse()?)
        })
        .transpose();
    let forbidden = [
        "connection",
        "transfer-encoding",
        "keep-alive",
        "proxy-connection",
        "upgrade",
    ];
    if length.is_err()
        || req.headers().get_all("content-length").iter().count() > 1
        || forbidden
            .iter()
            .any(|name| req.headers().contains_key(*name))
        || req
            .headers()
            .get_all("te")
            .iter()
            .any(|v| !v.as_bytes().eq_ignore_ascii_case(b"trailers"))
    {
        recv.stop_sending(Code::H3_MESSAGE_ERROR);
        send.stop_stream(Code::H3_MESSAGE_ERROR);
        return Ok(());
    }
    let malformed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut req = req.map(|()| RequestBody {
        stream: recv,
        remaining: length.unwrap(),
        data_done: false,
        done: false,
        malformed: malformed.clone(),
    });
    req.extensions_mut()
        .insert(crate::http::request::ConnectionInfo(peer, true));
    let head = req.method() == http::Method::HEAD;
    let connect = req.method() == http::Method::CONNECT;
    let cfg = live.load().config.clone();
    let write_timeout =
        Duration::from_secs(parse_duration_secs(&cfg.server.write_timeout).unwrap_or(60));
    let mut response = if connect {
        // Neither generic tunnelling nor extended CONNECT WebSockets are offered.
        drop(req);
        crate::http::response::text_response(501, "HTTP/3 CONNECT is not supported")
    } else {
        crate::http::router::dispatch(req, peer, cfg).await
    };
    if malformed.load(std::sync::atomic::Ordering::Relaxed) {
        send.stop_stream(Code::H3_MESSAGE_ERROR);
        return Ok(());
    }
    if head {
        *response.body_mut() = crate::http::response::ResponseBody::new(Bytes::new());
    }
    crate::http::router::record_response(&response);
    // User-configured / upstream hop-by-hop headers must not reach HTTP/3.
    strip_hop_headers(response.headers_mut());
    let (mut parts, mut body) = response.into_parts();
    parts.version = http::Version::HTTP_3;
    tokio::time::timeout(
        write_timeout,
        send.send_response(http::Response::from_parts(parts, ())),
    )
    .await??;
    while let Some(frame) = tokio::time::timeout(write_timeout, body.frame()).await? {
        let frame = frame?;
        match frame.into_data() {
            Ok(data) => tokio::time::timeout(write_timeout, send.send_data(data)).await??,
            Err(frame) => {
                if let Ok(mut trailers) = frame.into_trailers() {
                    strip_hop_headers(&mut trailers);
                    tokio::time::timeout(write_timeout, send.send_trailers(trailers)).await??;
                    return Ok(());
                }
            }
        }
    }
    tokio::time::timeout(write_timeout, send.finish()).await??;
    Ok(())
}

fn strip_hop_headers(headers: &mut http::HeaderMap) {
    let nominated: Vec<_> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| http::HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "transfer-encoding",
        "keep-alive",
        "proxy-connection",
        "upgrade",
        "te",
        "trailer",
    ] {
        headers.remove(name);
    }
}
