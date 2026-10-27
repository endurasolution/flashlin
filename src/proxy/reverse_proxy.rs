//! Hyper handles framing, chunk decoding, keep-alive and response streaming.
use crate::{
    config::{parse_duration_secs, ServerConfig},
    http::{
        request::ParsedRequest,
        response::{error_502, error_504, HyperResponse, ResponseBody},
    },
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    header::{self, HeaderName, HeaderValue},
    HeaderMap, Method, Request, Response, Uri,
};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::{TokioExecutor, TokioIo, TokioTimer},
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

#[cfg(feature = "tls")]
type Connector = hyper_rustls::HttpsConnector<HttpConnector>;
#[cfg(not(feature = "tls"))]
type Connector = HttpConnector;
type ProxyClient = Client<Connector, Full<Bytes>>;

fn client() -> &'static ProxyClient {
    static CLIENT: OnceLock<ProxyClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(Duration::from_secs(10)));
        http.set_nodelay(true);
        #[cfg(feature = "tls")]
        let connector = {
            http.enforce_http(false);
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_webpki_roots()
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .wrap_connector(http)
        };
        #[cfg(not(feature = "tls"))]
        let connector = http;
        Client::builder(TokioExecutor::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(16)
            .retry_canceled_requests(false)
            .build(connector)
    })
}

pub fn validate_upstream(value: &str) -> Result<Uri, &'static str> {
    let uri = value.parse::<Uri>().map_err(|_| "invalid upstream URI")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.host().is_none()
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        || uri.query().is_some()
    {
        return Err("upstream must be an http(s) URL without credentials or query");
    }
    #[cfg(not(feature = "tls"))]
    if uri.scheme_str() == Some("https") {
        return Err("HTTPS upstreams require --features tls");
    }
    Ok(uri)
}

fn strip_hop_headers(headers: &mut HeaderMap) {
    let nominated: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "proxy-connection",
    ] {
        headers.remove(name);
    }
}

pub async fn balanced_request(
    req: &ParsedRequest,
    method: &Method,
    fallback: &str,
    upstreams: &[String],
    config: &Arc<ServerConfig>,
) -> HyperResponse {
    if upstreams.is_empty() {
        return proxy_request(req, method, fallback, config).await;
    }
    let group = super::upstream::UpstreamGroup::shared(upstreams, config);
    let retry_safe = (crate::http::request::is_query(method)
        || (matches!(*method, Method::GET | Method::HEAD) && req.body.is_empty()))
        && !req.headers.contains_key(header::UPGRADE);
    let candidates = group.candidates(upstreams.len());
    let mut last = None;
    for index in candidates.into_iter().take(if retry_safe { 3 } else { 1 }) {
        let response = proxy_request(req, method, &upstreams[index], config).await;
        let failed = matches!(response.status().as_u16(), 502 | 503 | 504);
        group.record(index, failed);
        if !failed {
            return response;
        }
        last = Some(response);
    }
    last.unwrap_or_else(|| error_502(&config.server.server_name))
}

pub async fn proxy_request(
    req: &ParsedRequest,
    method: &Method,
    proxy_pass: &str,
    config: &Arc<ServerConfig>,
) -> HyperResponse {
    let name = config.server.server_name.as_str();
    let base = match validate_upstream(proxy_pass) {
        Ok(uri) => uri,
        Err(reason) => {
            tracing::warn!(reason, "Invalid upstream");
            return error_502(name);
        }
    };
    // Preserve the original escaped URI: decoding %2F or %0D before forwarding
    // changes routing semantics and can corrupt a request line.
    let path = format!(
        "{}{}",
        base.path().trim_end_matches('/'),
        req.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
    );
    let uri = match format!(
        "{}://{}{}",
        base.scheme_str().unwrap(),
        base.authority().unwrap(),
        path
    )
    .parse::<Uri>()
    {
        Ok(uri) => uri,
        Err(_) => return error_502(name),
    };
    let websocket = method == Method::GET
        && req
            .headers
            .get(header::UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && req
            .headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case("upgrade"));
    let mut outgoing = Request::builder()
        .method(method)
        .uri(uri)
        .body(Full::new(req.body.clone()))
        .unwrap();
    *outgoing.headers_mut() = req.headers.clone();
    let headers = outgoing.headers_mut();
    strip_hop_headers(headers);
    for name in [
        "host",
        "content-length",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
    ] {
        headers.remove(name);
    }
    headers.insert(
        header::HOST,
        HeaderValue::from_str(base.authority().unwrap().as_str()).unwrap(),
    );
    headers.insert(header::CONTENT_LENGTH, req.body.len().into());
    if let Some(peer) = req.peer {
        headers.insert("x-forwarded-for", peer.ip().to_string().parse().unwrap());
    }
    headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_static(if req.tls { "https" } else { "http" }),
    );
    if let Some(host) = req.headers.get(header::HOST).cloned() {
        headers.insert("x-forwarded-host", host);
    }
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    let idle = Duration::from_secs(parse_duration_secs(&config.server.read_timeout).unwrap_or(30));
    let mut upstream = match tokio::time::timeout(idle, client().request(outgoing)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(%error, "Upstream request failed");
            return error_502(name);
        }
        Err(_) => return error_504(name),
    };
    if upstream.status() == hyper::StatusCode::SWITCHING_PROTOCOLS {
        if !websocket
            || !upstream
                .headers()
                .get(header::UPGRADE)
                .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"))
        {
            return error_502(name);
        }
        let Some(upgrade) = req.upgrade.as_ref() else {
            return error_502(name);
        };
        let Some(downstream) = upgrade.lock().await.take() else {
            return error_502(name);
        };
        let backend = hyper::upgrade::on(&mut upstream);
        // Bound upgraded tunnels separately because Hyper releases the HTTP
        // connection once it has handed off the transport.
        static TUNNELS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
        let Ok(permit) = TUNNELS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1024)))
            .clone()
            .try_acquire_owned()
        else {
            return crate::http::response::error_503(name);
        };
        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(Ok((downstream, backend))) =
                tokio::time::timeout(Duration::from_secs(10), async {
                    tokio::try_join!(downstream, backend)
                })
                .await
            {
                let mut downstream = TokioIo::new(downstream);
                let mut backend = TokioIo::new(backend);
                let _ = tokio::io::copy_bidirectional(&mut downstream, &mut backend).await;
            }
        });
        let (mut parts, _) = upstream.into_parts();
        parts.version = hyper::Version::HTTP_11;
        strip_hop_headers(&mut parts.headers);
        parts
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        parts
            .headers
            .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        return Response::from_parts(parts, ResponseBody::new(Bytes::new()));
    }
    let (mut parts, body) = upstream.into_parts();
    // The upstream's HTTP version describes the upstream hop only. Passing an
    // HTTP/1.0 one on (Python's http.server, some WSGI servers) made Hyper
    // answer HTTP/1.1 clients with HTTP/1.0, which rules out chunked encoding
    // and so closed keep-alive connections after streamed bodies. Hyper still
    // downgrades the response itself for clients that really are HTTP/1.0.
    parts.version = hyper::Version::HTTP_11;
    strip_hop_headers(&mut parts.headers);
    Response::from_parts(parts, ResponseBody::incoming(body, idle))
}
