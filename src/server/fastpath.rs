//! A minimal HTTP/1.1 keep-alive loop for plain (non-TLS) static GET/HEAD
//! requests, serving eligible responses via Linux `sendfile(2)` — a true
//! zero-copy kernel transfer from the page cache straight to the socket, so
//! the response body is never copied into this process's memory at all.
//!
//! Hyper has no hook for handing a raw file descriptor to its connection
//! writer mid-response, so getting real zero-copy transfer means not using
//! Hyper to drive these connections. To keep that safe, the scope here is
//! deliberately narrow: only a single-host configuration, only GET/HEAD with
//! no request body, no `Range`, and no header this module doesn't recognize.
//! Every check below either reuses the exact function the ordinary
//! dispatch/`prepare_static` path uses (path containment, symlink
//! verification, IP access, rate limiting, sensitive-file blocking) or, on
//! anything it cannot cleanly decide, falls back immediately to the ordinary
//! Hyper-based connection handler — which then owns the connection, with all
//! its buffered-but-unread bytes replayed intact, for the rest of its
//! lifetime. This module never re-implements a security check; it only
//! orchestrates the existing ones for a narrower set of requests.

use std::net::SocketAddr;

use super::reload::LiveConfig;
use super::shutdown::ShutdownSignal;

/// Entry point used by the plain-HTTP accept loop in place of
/// `connection::handle_connection`. On Linux, when `server.sendfile` is
/// enabled and no virtual hosts are configured, eligible static GET/HEAD
/// requests are served with zero-copy `sendfile(2)`. Everything else,
/// including every non-Linux target, uses the ordinary Hyper connection
/// handler unchanged — enabling this flag can only add a faster path, never
/// remove behavior.
pub async fn handle_plain_connection(
    stream: tokio::net::TcpStream,
    remote_addr: SocketAddr,
    live: LiveConfig,
    shutdown: ShutdownSignal,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "linux")]
    {
        let config = live.load().config.clone();
        if config.server.sendfile && config.virtual_hosts.is_empty() {
            drop(config);
            return linux::serve(stream, remote_addr, live, shutdown).await;
        }
    }
    super::connection::handle_connection(stream, remote_addr, live, shutdown, false).await
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::net::SocketAddr;
    use std::os::unix::io::AsRawFd;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use bytes::{Bytes, BytesMut};
    use hyper::header::{self, HeaderMap, HeaderValue};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
    use tokio::net::TcpStream;

    use super::{LiveConfig, ShutdownSignal};
    use crate::config::{parse_duration_secs, ServerConfig};
    use crate::flashweb;
    use crate::http::request::encoding_quality;
    use crate::http::response::ResponseBody;
    use crate::http::router::{find_route, find_static_dir, get_rate_limiter, static_relative_path};
    use crate::middleware::compression::vary_encoding;
    use crate::security::access_control::check_ip_access;
    use crate::security::headers::apply_security_headers;
    use crate::security::sensitive_files::is_sensitive_path;
    use crate::static_files::cache_headers::{apply_cache_headers, is_not_modified, make_etag};
    use crate::static_files::file_server::{
        is_php_path, is_verified_file, mime_header, open_file_within_root, remember_verified_file,
    };
    use crate::static_files::path_security::{canonical_root, safe_join, validate_url_path};
    use crate::static_files::redirect_rules::find_rewrite_action;

    const MAX_HEADERS: usize = 64;

    /// Mirrors `connection.rs`'s own header-size clamp so this fast path
    /// never allows a larger request head than an operator configured
    /// `security.max_header_size` to permit, and never a smaller one than
    /// any ordinary request needs.
    fn max_request_head(config: &ServerConfig) -> usize {
        config
            .security
            .as_ref()
            .and_then(|security| crate::config::parse_size(&security.max_header_size))
            .unwrap_or(32 * 1024)
            .clamp(8 * 1024, 16 * 1024 * 1024) as usize
    }

    struct Head {
        is_head: bool,
        path: String,
        headers: HeaderMap,
        keep_alive: bool,
    }

    enum ReadOutcome {
        Head(Head, usize),
        /// Buffer left untouched: safe to hand straight to Hyper.
        Fallback,
        /// Clean end of connection between requests.
        Closed,
    }

    enum Outcome {
        Served,
        /// Nothing has been written yet: safe to hand off to Hyper.
        Fallback,
    }

    pub(super) async fn serve(
        mut stream: TcpStream,
        remote_addr: SocketAddr,
        live: LiveConfig,
        mut shutdown: ShutdownSignal,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut buf = BytesMut::with_capacity(4096);
        loop {
            let config = live.load().config.clone();
            let idle = Duration::from_secs(
                parse_duration_secs(&config.server.keep_alive_timeout).unwrap_or(75),
            );

            let max_head = max_request_head(&config);
            let outcome = tokio::select! {
                result = read_head(&mut stream, &mut buf, idle, max_head) => result?,
                _ = shutdown.wait() => return Ok(()),
            };

            let (head, consumed) = match outcome {
                ReadOutcome::Head(head, consumed) => (head, consumed),
                ReadOutcome::Closed => return Ok(()),
                ReadOutcome::Fallback => {
                    let prefixed = PrefixedStream::new(buf.split().freeze(), stream);
                    return super::super::connection::handle_connection(
                        prefixed,
                        remote_addr,
                        live,
                        shutdown,
                        false,
                    )
                    .await;
                }
            };

            match try_serve(&mut stream, &head, remote_addr, &config).await? {
                Outcome::Served => {
                    buf.advance_past(consumed);
                    let keep_alive = head.keep_alive && config.server.keep_alive;
                    if !keep_alive {
                        let _ = stream.shutdown().await;
                        return Ok(());
                    }
                    // Loop again; `buf` may already hold a pipelined request.
                }
                Outcome::Fallback => {
                    // The head we parsed is still intact at the front of
                    // `buf` (nothing here ever calls `advance_past` before a
                    // confirmed `Served`), so Hyper sees exactly the bytes
                    // the client sent.
                    let prefixed = PrefixedStream::new(buf.split().freeze(), stream);
                    return super::super::connection::handle_connection(
                        prefixed,
                        remote_addr,
                        live,
                        shutdown,
                        false,
                    )
                    .await;
                }
            }
        }
    }

    trait AdvancePast {
        fn advance_past(&mut self, n: usize);
    }
    impl AdvancePast for BytesMut {
        fn advance_past(&mut self, n: usize) {
            let _ = self.split_to(n);
        }
    }

    async fn read_head(
        stream: &mut TcpStream,
        buf: &mut BytesMut,
        idle: Duration,
        max_head: usize,
    ) -> io::Result<ReadOutcome> {
        loop {
            {
                let mut storage = [httparse::EMPTY_HEADER; MAX_HEADERS];
                let mut req = httparse::Request::new(&mut storage);
                match req.parse(&buf[..]) {
                    Ok(httparse::Status::Complete(consumed)) => {
                        return Ok(match to_owned_head(&req) {
                            Some(head) => ReadOutcome::Head(head, consumed),
                            None => ReadOutcome::Fallback,
                        });
                    }
                    Ok(httparse::Status::Partial) => {
                        if buf.len() >= max_head {
                            return Ok(ReadOutcome::Fallback);
                        }
                    }
                    Err(_) => return Ok(ReadOutcome::Fallback),
                }
            }
            let mut chunk = [0_u8; 4096];
            let read = match tokio::time::timeout(idle, stream.read(&mut chunk)).await {
                Ok(result) => result?,
                Err(_) => return Ok(ReadOutcome::Closed),
            };
            if read == 0 {
                return Ok(if buf.is_empty() {
                    ReadOutcome::Closed
                } else {
                    ReadOutcome::Fallback
                });
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    }

    fn to_owned_head(req: &httparse::Request) -> Option<Head> {
        if req.version != Some(1) {
            return None;
        }
        let is_head = match req.method? {
            "GET" => false,
            "HEAD" => true,
            _ => return None,
        };
        let raw = req.path?;
        if !raw.starts_with('/') {
            return None;
        }
        let raw_path = raw.split_once('?').map(|(p, _)| p).unwrap_or(raw);
        let path = if raw_path.contains('%') {
            percent_encoding::percent_decode_str(raw_path)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            raw_path.to_string()
        };

        let mut headers = HeaderMap::new();
        let mut keep_alive = true;
        for h in req.headers.iter() {
            let lower = h.name.to_ascii_lowercase();
            match lower.as_str() {
                "content-length" | "transfer-encoding" | "expect" | "range" => return None,
                "connection" => {
                    if let Ok(v) = std::str::from_utf8(h.value) {
                        keep_alive =
                            !v.split(',').any(|tok| tok.trim().eq_ignore_ascii_case("close"));
                    }
                }
                _ => {}
            }
            let Ok(name) = header::HeaderName::from_bytes(lower.as_bytes()) else {
                return None;
            };
            let Ok(value) = HeaderValue::from_bytes(h.value) else {
                return None;
            };
            headers.append(name, value);
        }
        Some(Head {
            is_head,
            path,
            headers,
            keep_alive,
        })
    }

    async fn try_serve(
        stream: &mut TcpStream,
        head: &Head,
        remote_addr: SocketAddr,
        config: &Arc<ServerConfig>,
    ) -> io::Result<Outcome> {
        if check_ip_access(&remote_addr.ip(), config).is_err() {
            return Ok(Outcome::Fallback);
        }
        if validate_url_path(&head.path).is_err() {
            return Ok(Outcome::Fallback);
        }
        let sec = config.security.as_ref();
        if sec.map(|s| s.block_dotfiles).unwrap_or(true)
            && head
                .path
                .split('/')
                .any(|seg| seg.starts_with('.') && seg.len() > 1)
        {
            return Ok(Outcome::Fallback);
        }
        if sec.map(|s| s.block_sensitive_files).unwrap_or(true) && is_sensitive_path(&head.path) {
            return Ok(Outcome::Fallback);
        }

        let method_str = if head.is_head { "HEAD" } else { "GET" };
        let (effective_path, root, cache_control): (String, String, Option<String>) =
            if let Some(dir) = find_static_dir(&head.path, config) {
                (
                    static_relative_path(&head.path, &dir.url_path),
                    dir.root.to_string_lossy().into_owned(),
                    dir.cache_control.clone(),
                )
            } else if let Some(route) = find_route(&head.path, config) {
                if route.proxy_target().is_some() || route.php_enabled {
                    return Ok(Outcome::Fallback);
                }
                if let Some(methods) = route.methods.as_deref() {
                    if !methods.is_empty() && !methods.iter().any(|m| m == method_str) {
                        return Ok(Outcome::Fallback);
                    }
                }
                (
                    head.path.clone(),
                    route.root.clone().unwrap_or_else(|| {
                        config
                            .server
                            .root
                            .clone()
                            .unwrap_or_else(|| "./public".to_string())
                    }),
                    route.cache_control.clone(),
                )
            } else {
                (
                    head.path.clone(),
                    config
                        .server
                        .root
                        .clone()
                        .unwrap_or_else(|| "./public".to_string()),
                    None,
                )
            };

        // Only a plain 200/304 identity response is handled here. Anything
        // that needs rewriting, PHP, a directory index, or a compressed
        // representation falls back to the fully-featured dispatch path.
        if !flashweb::load_flashweb(&effective_path, &root).is_empty() {
            return Ok(Outcome::Fallback);
        }
        match find_rewrite_action(&effective_path, &root) {
            Ok(None) => {}
            _ => return Ok(Outcome::Fallback),
        }
        if let Some(comp) = config.compression.as_ref() {
            let br_ok = comp.brotli && encoding_quality(&head.headers, "br") > 0;
            let gz_ok = comp.gzip && encoding_quality(&head.headers, "gzip") > 0;
            if br_ok || gz_ok {
                return Ok(Outcome::Fallback);
            }
        }

        let Some((file, meta, selected)) = resolve_file(&effective_path, &root) else {
            return Ok(Outcome::Fallback);
        };

        // Checked last, immediately before committing to serve: every earlier
        // `Fallback` return re-runs this same check in the ordinary dispatch
        // path, and the limiter's `check` has a side effect (it consumes a
        // token). Running it any earlier would charge some requests twice.
        if config
            .security
            .as_ref()
            .and_then(|s| s.rate_limit.as_ref())
            .is_some()
            && !get_rate_limiter(config, "default").check(remote_addr.ip())
        {
            return Ok(Outcome::Fallback);
        }

        let size = meta.len();
        let modified = meta
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let etag = make_etag(size, modified);
        let mime = mime_header(&selected);

        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, mime);
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        apply_cache_headers(&mut headers, &etag, modified, cache_control.as_deref());
        vary_encoding(&mut headers);

        let not_modified = is_not_modified(&head.headers, &etag, modified);
        let mut response = hyper::Response::new(ResponseBody::new(Bytes::new()));
        *response.status_mut() = if not_modified {
            hyper::StatusCode::NOT_MODIFIED
        } else {
            hyper::StatusCode::OK
        };
        *response.headers_mut() = headers;
        if !not_modified {
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(size));
        }
        apply_security_headers(&mut response, config);
        if sec.map(|s| s.hide_server_header).unwrap_or(true) {
            response.headers_mut().remove("server");
            response.headers_mut().insert(
                "server",
                config
                    .server
                    .server_name
                    .parse()
                    .unwrap_or_else(|_| HeaderValue::from_static("FlashLin")),
            );
        }
        crate::http::router::record_response(&response);
        let (parts, _) = response.into_parts();

        let mut head_bytes = Vec::with_capacity(256);
        head_bytes.extend_from_slice(if not_modified {
            b"HTTP/1.1 304 Not Modified\r\n"
        } else {
            b"HTTP/1.1 200 OK\r\n"
        });
        for (name, value) in parts.headers.iter() {
            head_bytes.extend_from_slice(name.as_str().as_bytes());
            head_bytes.extend_from_slice(b": ");
            head_bytes.extend_from_slice(value.as_bytes());
            head_bytes.extend_from_slice(b"\r\n");
        }
        head_bytes.extend_from_slice(if head.keep_alive && config.server.keep_alive {
            b"Connection: keep-alive\r\n"
        } else {
            b"Connection: close\r\n"
        });
        head_bytes.extend_from_slice(b"\r\n");

        let write_idle =
            Duration::from_secs(parse_duration_secs(&config.server.write_timeout).unwrap_or(60));
        timeout_io(write_idle, stream.write_all(&head_bytes)).await?;

        if !not_modified && !head.is_head && size > 0 {
            sendfile_all(stream, &file, size, write_idle).await?;
        }

        Ok(Outcome::Served)
    }

    async fn timeout_io<F: std::future::Future<Output = io::Result<T>>, T>(
        idle: Duration,
        fut: F,
    ) -> io::Result<T> {
        tokio::time::timeout(idle, fut)
            .await
            .unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "write timed out")))
    }

    /// Resolves a request path to an already-open file within `root`,
    /// reusing the exact containment and symlink-verification primitives
    /// `prepare_static` uses. Returns `None` for anything this fast path
    /// does not serve directly — a directory, a PHP script, a missing file,
    /// or any error — so the caller falls back to the ordinary path, which
    /// re-resolves independently and handles those cases (including
    /// directory-index lookup) exactly as it always has.
    fn resolve_file(
        url_path: &str,
        root: &str,
    ) -> Option<(std::fs::File, std::fs::Metadata, PathBuf)> {
        let base_path = safe_join(root, url_path).ok()?;
        if is_php_path(&base_path) {
            return None;
        }
        let root_canon = canonical_root(root);
        let selected = if is_verified_file(&base_path) {
            base_path
        } else {
            let meta = std::fs::metadata(&base_path).ok()?;
            if !meta.is_file() {
                return None;
            }
            let canonical = std::fs::canonicalize(&base_path).ok()?;
            if !canonical.starts_with(&root_canon) || is_php_path(&canonical) {
                return None;
            }
            let meta = if canonical == base_path {
                meta
            } else {
                std::fs::metadata(&canonical).ok()?
            };
            if !meta.is_file() {
                return None;
            }
            if canonical == base_path {
                remember_verified_file(&base_path);
            }
            canonical
        };
        let (file, meta) = open_file_within_root(&selected, &root_canon).ok()?;
        Some((file, meta, selected))
    }

    /// Loops `sendfile(2)` to completion, waiting for socket writability
    /// (via Tokio's reactor, never blocking a worker thread) between short
    /// writes. The kernel copies straight from the page cache to the socket;
    /// this process's memory never holds the file's bytes.
    async fn sendfile_all(
        stream: &TcpStream,
        file: &std::fs::File,
        len: u64,
        idle: Duration,
    ) -> io::Result<()> {
        let out_fd = stream.as_raw_fd();
        let in_fd = file.as_raw_fd();
        let mut offset: libc::off_t = 0;
        let mut remaining = len;
        while remaining > 0 {
            timeout_io(idle, stream.writable()).await?;
            let attempt = remaining.min(i32::MAX as u64) as usize;
            let result = stream.try_io(tokio::io::Interest::WRITABLE, || {
                let n = unsafe { libc::sendfile(out_fd, in_fd, &mut offset, attempt) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "sendfile returned 0 with bytes remaining",
                    ))
                }
                // `attempt` bounds the kernel to at most `remaining` bytes,
                // so this never underflows in practice; `saturating_sub`
                // keeps it that way even if that contract were ever violated,
                // instead of wrapping to a huge value under release's
                // unchecked arithmetic.
                Ok(n) => remaining = remaining.saturating_sub(n as u64),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Replays bytes already read from the socket before delegating the rest
    /// of the connection to the ordinary Hyper-based handler, so a fallback
    /// decision never loses data the client already sent.
    struct PrefixedStream {
        prefix: Bytes,
        pos: usize,
        inner: TcpStream,
    }

    impl PrefixedStream {
        fn new(prefix: Bytes, inner: TcpStream) -> Self {
            Self {
                prefix,
                pos: 0,
                inner,
            }
        }
    }

    impl AsyncRead for PrefixedStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.pos < self.prefix.len() {
                let remaining = &self.prefix[self.pos..];
                let n = remaining.len().min(buf.remaining());
                buf.put_slice(&remaining[..n]);
                self.pos += n;
                return Poll::Ready(Ok(()));
            }
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for PrefixedStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }
        fn poll_write_vectored(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bufs: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
        }
        fn is_write_vectored(&self) -> bool {
            self.inner.is_write_vectored()
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn parse<'h>(storage: &'h mut [httparse::Header<'h>], raw: &'h [u8]) -> httparse::Request<'h, 'h> {
            let mut req = httparse::Request::new(storage);
            let status = req.parse(raw).unwrap();
            assert!(matches!(status, httparse::Status::Complete(_)));
            req
        }

        #[test]
        fn accepts_plain_get_and_head_and_decodes_percent_escapes_and_close() {
            let mut storage = [httparse::EMPTY_HEADER; 16];
            let req = parse(
                &mut storage,
                b"GET /a%20b?x=1 HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
            );
            let head = to_owned_head(&req).unwrap();
            assert!(!head.is_head);
            assert_eq!(head.path, "/a b");
            assert!(!head.keep_alive);

            let mut storage = [httparse::EMPTY_HEADER; 16];
            let req = parse(&mut storage, b"HEAD /x HTTP/1.1\r\nHost: h\r\n\r\n");
            let head = to_owned_head(&req).unwrap();
            assert!(head.is_head);
            assert!(head.keep_alive);
        }

        #[test]
        fn rejects_anything_this_fast_path_does_not_safely_handle() {
            let cases: &[&[u8]] = &[
                b"POST /x HTTP/1.1\r\nHost: h\r\n\r\n",
                b"PUT /x HTTP/1.1\r\nHost: h\r\n\r\n",
                b"GET /x HTTP/1.1\r\nHost: h\r\nContent-Length: 3\r\n\r\n",
                b"GET /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n",
                b"GET /x HTTP/1.1\r\nHost: h\r\nExpect: 100-continue\r\n\r\n",
                b"GET /x HTTP/1.1\r\nHost: h\r\nRange: bytes=0-1\r\n\r\n",
                b"GET /x HTTP/1.0\r\nHost: h\r\n\r\n",
                b"GET http://example/y HTTP/1.1\r\nHost: h\r\n\r\n",
                b"OPTIONS * HTTP/1.1\r\nHost: h\r\n\r\n",
            ];
            for raw in cases {
                let mut storage = [httparse::EMPTY_HEADER; 16];
                let req = parse(&mut storage, raw);
                assert!(
                    to_owned_head(&req).is_none(),
                    "expected fallback for {:?}",
                    String::from_utf8_lossy(raw)
                );
            }
        }

        struct Fixture(std::path::PathBuf);
        impl Fixture {
            fn new() -> Self {
                let path = std::env::current_dir()
                    .unwrap()
                    .join("target")
                    .join(format!("fastpath-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(path.join("dir")).unwrap();
                std::fs::write(path.join("a.txt"), b"hi").unwrap();
                std::fs::write(path.join("a.php"), b"<?php").unwrap();
                Self(path)
            }
            fn root(&self) -> &str {
                self.0.to_str().unwrap()
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn resolve_file_serves_plain_files_but_not_directories_php_or_missing_paths() {
            let fixture = Fixture::new();
            assert!(resolve_file("/a.txt", fixture.root()).is_some());
            assert!(resolve_file("/dir", fixture.root()).is_none());
            assert!(resolve_file("/a.php", fixture.root()).is_none());
            assert!(resolve_file("/missing.txt", fixture.root()).is_none());
        }

        // ── Real loopback coverage ──────────────────────────────────────────

        
        use http_body_util::{BodyExt, Full};
        use hyper::{Request, Response};
        use hyper_util::client::legacy::{connect::HttpConnector, Client};
        use hyper_util::rt::TokioExecutor;
        use std::time::Duration;

        async fn spawn(root: &std::path::Path) -> (std::net::SocketAddr, Arc<crate::server::shutdown::ShutdownTrigger>) {
            let mut cfg = crate::default_config();
            cfg.routes = None;
            cfg.compression = None;
            cfg.server.root = Some(root.to_string_lossy().into_owned());
            cfg.server.sendfile = true;
            cfg.cache.as_mut().unwrap().enabled = false;
            let live: LiveConfig = Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::server::reload::Snapshot::new(Arc::new(cfg)).unwrap(),
            ));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let trigger = Arc::new(crate::server::shutdown::ShutdownTrigger::new());
            let accept_shutdown = trigger.clone();
            tokio::spawn(async move {
                let mut shutdown = accept_shutdown.subscribe();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((stream, remote)) = accepted else { break };
                            let live = live.clone();
                            let conn_shutdown = accept_shutdown.subscribe();
                            tokio::spawn(super::super::handle_plain_connection(
                                stream, remote, live, conn_shutdown,
                            ));
                        }
                        _ = shutdown.wait() => break,
                    }
                }
            });
            (addr, trigger)
        }

        async fn body_string(resp: Response<hyper::body::Incoming>) -> (u16, HeaderMap, String) {
            let status = resp.status().as_u16();
            let headers = resp.headers().clone();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (status, headers, String::from_utf8_lossy(&bytes).into_owned())
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn serves_get_head_conditional_and_a_range_fallback_correctly() {
            let fixture = Fixture::new();
            std::fs::write(fixture.0.join("small.bin"), b"hello sendfile").unwrap();
            let (addr, trigger) = spawn(&fixture.0).await;
            let client: Client<HttpConnector, Full<Bytes>> =
                Client::builder(TokioExecutor::new()).build(HttpConnector::new());

            let req = Request::get(format!("http://{addr}/small.bin"))
                .header("accept-encoding", "identity")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let resp = tokio::time::timeout(Duration::from_secs(5), client.request(req))
                .await
                .unwrap()
                .unwrap();
            let (status, headers, body) = body_string(resp).await;
            assert_eq!(status, 200);
            assert_eq!(body, "hello sendfile");
            let etag = headers.get(header::ETAG).unwrap().to_str().unwrap().to_string();

            // HEAD: same headers, no body.
            let req = Request::head(format!("http://{addr}/small.bin"))
                .body(Full::new(Bytes::new()))
                .unwrap();
            let resp = tokio::time::timeout(Duration::from_secs(5), client.request(req))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(resp.status(), 200);
            let (_, _, body) = body_string(resp).await;
            assert!(body.is_empty());

            // Conditional GET matching the ETag: 304, no body.
            let req = Request::get(format!("http://{addr}/small.bin"))
                .header("if-none-match", etag)
                .body(Full::new(Bytes::new()))
                .unwrap();
            let resp = tokio::time::timeout(Duration::from_secs(5), client.request(req))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(resp.status(), 304);

            // A Range request is outside this fast path's scope; it must
            // still be served correctly via the fallback to Hyper.
            let req = Request::get(format!("http://{addr}/small.bin"))
                .header("range", "bytes=0-4")
                .body(Full::new(Bytes::new()))
                .unwrap();
            let resp = tokio::time::timeout(Duration::from_secs(5), client.request(req))
                .await
                .unwrap()
                .unwrap();
            let (status, _, body) = body_string(resp).await;
            assert_eq!(status, 206);
            assert_eq!(body, "hello");

            // A 404 for a missing file must also come from the fallback path
            // with a real body, not an empty/broken response.
            let req = Request::get(format!("http://{addr}/missing.bin"))
                .body(Full::new(Bytes::new()))
                .unwrap();
            let resp = tokio::time::timeout(Duration::from_secs(5), client.request(req))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(resp.status(), 404);
            let _ = resp.into_body().collect().await.unwrap();

            trigger.fire();
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn keep_alive_serves_many_requests_over_one_connection() {
            let fixture = Fixture::new();
            std::fs::write(fixture.0.join("small.bin"), vec![b'z'; 4096]).unwrap();
            let (addr, trigger) = spawn(&fixture.0).await;

            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            for _ in 0..20 {
                stream
                    .write_all(b"GET /small.bin HTTP/1.1\r\nHost: h\r\nAccept-Encoding: identity\r\n\r\n")
                    .await
                    .unwrap();
                let mut header_buf = Vec::new();
                let mut byte = [0_u8; 1];
                loop {
                    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_ne!(n, 0, "connection closed mid keep-alive sequence");
                    header_buf.push(byte[0]);
                    if header_buf.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&header_buf);
                assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
                let content_length: usize = headers
                    .lines()
                    .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_string))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap();
                assert_eq!(content_length, 4096);
                let mut remaining = content_length;
                let mut body = vec![0_u8; remaining];
                while remaining > 0 {
                    let n = tokio::time::timeout(
                        Duration::from_secs(5),
                        stream.read(&mut body[content_length - remaining..]),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_ne!(n, 0, "connection closed mid body");
                    remaining -= n;
                }
                assert!(body.iter().all(|b| *b == b'z'));
            }
            trigger.fire();
        }
    }
}
