use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use hyper::body::Body;
use hyper::Request;

use crate::config::parse_size;
use crate::error::{FlashError, Result};

/// The parsed, fully-buffered incoming request.
#[derive(Clone)]
pub struct ParsedRequest {
    pub method: hyper::Method,
    pub uri: hyper::Uri,
    pub headers: hyper::HeaderMap,
    pub body: Bytes,
    /// Decoded path (percent-decoded, no query string)
    pub path: String,
    /// Raw query string (without leading `?`)
    pub query: String,
    pub peer: Option<std::net::SocketAddr>,
    pub tls: bool,
    pub upgrade: Option<std::sync::Arc<tokio::sync::Mutex<Option<hyper::upgrade::OnUpgrade>>>>,
}

#[derive(Clone)]
pub struct ConnectionInfo(pub std::net::SocketAddr, pub bool);

/// RFC 10008 QUERY is case-sensitive, safe and idempotent, and carries content.
pub fn is_query(method: &hyper::Method) -> bool {
    method.as_str() == "QUERY"
}

pub fn query_content_type_valid(headers: &hyper::HeaderMap) -> bool {
    let mut values = headers.get_all(hyper::header::CONTENT_TYPE).iter();
    let media_type = values
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<mime::Mime>().ok());
    values.next().is_none()
        && media_type.is_some_and(|m| m.type_() != mime::STAR && m.subtype() != mime::STAR)
}

impl ParsedRequest {
    /// Consume the hyper request and buffer the body up to `max_body_bytes`.
    pub async fn from_hyper<B>(mut req: Request<B>, max_body_bytes: u64) -> Result<Self>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let info = req.extensions().get::<ConnectionInfo>().cloned();
        let upgrade = if req
            .headers()
            .get("upgrade")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        {
            Some(hyper::upgrade::on(&mut req))
        } else {
            None
        };
        let (parts, body) = req.into_parts();

        // Apply body size limit.
        let limited = Limited::new(body, max_body_bytes as usize);
        let collected = limited
            .collect()
            .await
            .map_err(|_| FlashError::BodyTooLarge)?;
        let body_bytes = collected.to_bytes();

        let raw_path_and_query = parts
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let (raw_path, query) = raw_path_and_query
            .split_once('?')
            .map(|(p, q)| (p.to_string(), q.to_string()))
            .unwrap_or_else(|| (raw_path_and_query.to_string(), String::new()));

        // Most paths carry no escapes; decoding them still allocated a second
        // copy, so the already-owned string is reused when nothing changes.
        let path = if raw_path.contains('%') {
            percent_encoding::percent_decode_str(&raw_path)
                .decode_utf8_lossy()
                .into_owned()
        } else {
            raw_path
        };

        Ok(ParsedRequest {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            body: body_bytes,
            path,
            query,
            peer: info.as_ref().map(|info| info.0),
            tls: info.is_some_and(|info| info.1),
            // Ordinary requests never use upgrade state. Allocate its shared
            // lock only when a WebSocket handshake actually requested it.
            upgrade: upgrade
                .map(|upgrade| std::sync::Arc::new(tokio::sync::Mutex::new(Some(upgrade)))),
        })
    }
}

/// Extract the value of a header as a &str, or empty string.
pub fn header_str<'a>(headers: &'a hyper::HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// Return true when the client accepts gzip encoding.
pub fn accepts_gzip(headers: &hyper::HeaderMap) -> bool {
    encoding_quality(headers, "gzip") > 0
}

/// Return true when the client accepts brotli encoding.
pub fn accepts_brotli(headers: &hyper::HeaderMap) -> bool {
    encoding_quality(headers, "br") > 0
}

pub fn encoding_quality(headers: &hyper::HeaderMap, encoding: &str) -> u16 {
    let mut wildcard = 0;
    for value in headers.get_all("accept-encoding") {
        let Ok(value) = value.to_str() else { continue };
        for token in value.split(',') {
            let mut parts = token.trim().split(';');
            let name = parts.next().unwrap_or("").trim();
            let mut quality = 1000;
            for parameter in parts {
                if let Some((key, value)) = parameter.trim().split_once('=') {
                    if key.eq_ignore_ascii_case("q") {
                        quality = value
                            .trim()
                            .parse::<f64>()
                            .ok()
                            .filter(|q| q.is_finite() && (0.0..=1.0).contains(q))
                            .map(|q| (q * 1000.0) as u16)
                            .unwrap_or(0);
                    }
                }
            }
            if name.eq_ignore_ascii_case(encoding) {
                return quality;
            }
            if name == "*" {
                wildcard = quality;
            }
        }
    }
    wildcard
}

/// Resolve the effective body-size limit from config (defaults to 20 MB).
pub fn body_limit_bytes(cfg: &crate::config::ServerConfig) -> u64 {
    cfg.security
        .as_ref()
        .and_then(|s| parse_size(&s.max_body_size))
        .unwrap_or(20 * 1024 * 1024)
}
