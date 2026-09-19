use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::http::response::ResponseBody;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::debug;

use super::shutdown::ShutdownSignal;
use crate::config::{parse_duration_secs, parse_size};
use crate::http::router::dispatch;

pub async fn handle_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    remote_addr: SocketAddr,
    live: super::reload::LiveConfig,
    mut shutdown: ShutdownSignal,
    tls: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = live.load().config.clone();
    let read_idle = std::time::Duration::from_secs(
        parse_duration_secs(&config.server.keep_alive_timeout).unwrap_or(75),
    );
    let write_idle = std::time::Duration::from_secs(
        parse_duration_secs(&config.server.write_timeout).unwrap_or(60),
    );
    let io = TokioIo::new(super::timed_io::TimedIo::new(stream, read_idle, write_idle));

    let service = service_fn(move |mut req: Request<Incoming>| {
        req.extensions_mut()
            .insert(crate::http::request::ConnectionInfo(remote_addr, tls));
        let cfg = live.load().config.clone();
        let addr = remote_addr;
        async move {
            let head = req.method() == hyper::Method::HEAD;
            let mut resp = dispatch(req, addr, Arc::clone(&cfg)).await;
            advertise_http3(&mut resp, &cfg, tls);
            if head {
                *resp.body_mut() = ResponseBody::new(bytes::Bytes::new());
            }
            crate::http::router::record_response(&resp);
            Ok::<Response<ResponseBody>, Infallible>(resp)
        }
    });

    // Auto builder handles both HTTP/1.1 and HTTP/2 (h2c + h2 over TLS).
    let mut builder = auto::Builder::new(TokioExecutor::new());
    let max_header_buffer = config
        .security
        .as_ref()
        .and_then(|security| parse_size(&security.max_header_size))
        .unwrap_or(32 * 1024)
        .clamp(8 * 1024, 16 * 1024 * 1024) as usize;
    let header_timeout = std::time::Duration::from_secs(
        parse_duration_secs(&config.server.read_timeout).unwrap_or(30),
    );
    builder
        .http1()
        .timer(TokioTimer::new())
        .keep_alive(config.server.keep_alive)
        .max_buf_size(max_header_buffer)
        .header_read_timeout(header_timeout);
    builder
        .http2()
        .max_concurrent_streams(128)
        .max_header_list_size(max_header_buffer as u32);

    // Transport settings have been copied; idle keep-alive connections must not
    // retain the original generation's cache/rate-limit state after reload.
    drop(config);
    let conn = builder.serve_connection_with_upgrades(io, service);
    tokio::pin!(conn);

    tokio::select! {
        result = &mut conn => {
            if let Err(e) = result {
                debug!("HTTP connection error from {}: {}", remote_addr, e);
            }
        }
        _ = shutdown.wait() => {
            debug!("Graceful shutdown: closing connection from {}", remote_addr);
            // Initiate graceful HTTP/2 GOAWAY + HTTP/1.1 connection close.
            conn.as_mut().graceful_shutdown();
            // Drain the connection so in-flight requests complete.
            let _ = conn.await;
        }
    }

    Ok(())
}

pub(crate) fn advertise_http3(
    response: &mut Response<ResponseBody>,
    cfg: &crate::config::ServerConfig,
    tls: bool,
) {
    if cfg!(feature = "http3") && tls {
        if let Some(h3) = cfg.http3.as_ref().filter(|h| h.enabled) {
            let value = format!("h3=\":{}\"; ma=300", h3.port.unwrap_or(cfg.server.port));
            response
                .headers_mut()
                .insert("alt-svc", value.parse().expect("numeric HTTP/3 port"));
        }
    }
}
