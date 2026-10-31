use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::http::response::ResponseBody;
use bytes::Bytes;
use hyper::body::Body;
use hyper::{Method, Request, Response};
use tracing::{info, warn};

use crate::cache::ResponseCache;
use crate::config::{RouteConfig, ServerConfig, VirtualHostConfig};
use crate::flashweb;
use crate::http::request::{body_limit_bytes, ParsedRequest};
use crate::http::response::*;
use crate::middleware::compression::maybe_compress;
use crate::security::{
    access_control::check_ip_access, headers::apply_security_headers, rate_limit::RateLimiter,
    sensitive_files::is_sensitive_path,
};
use crate::static_files::file_server::{
    resolve_directory_index, serve_static_cached, DirectoryIndex,
};
use crate::static_files::redirect_rules::{find_rewrite_action, RewriteAction};

use dashmap::DashMap;
use std::sync::OnceLock;

/// Stateful controls must be scoped to the selected virtual host.  A process-wide
/// singleton silently applied the first host's cache and rate-limit policy to every
/// other host, which is especially surprising on shared installations.
#[derive(Default)]
pub struct HostStates {
    default_cache: OnceLock<Arc<ResponseCache>>,
    pub(crate) upstreams: DashMap<Vec<String>, Arc<crate::proxy::upstream::UpstreamGroup>>,
    limiters: DashMap<String, Arc<RateLimiter>>,
    caches: DashMap<String, Arc<ResponseCache>>,
}
impl std::fmt::Debug for HostStates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStates").finish_non_exhaustive()
    }
}
impl HostStates {
    /// Clears every response cache this process holds — the default host's
    /// and every virtual host's. Returns the total entries removed.
    pub fn clear_all_caches(&self) -> usize {
        let mut removed = self.default_cache.get().map(|c| c.clear()).unwrap_or(0);
        for entry in self.caches.iter() {
            removed += entry.value().clear();
        }
        removed
    }
}
static METRICS: OnceLock<Arc<crate::metrics::prometheus::Metrics>> = OnceLock::new();
static REQUEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Virtual hosts merged with the global config, keyed by lowercase hostname
/// (name and aliases). Built once per config generation by
/// [`build_vhost_index`]; merging on every request deep-cloned the whole config.
#[derive(Default)]
pub struct VhostIndex(std::collections::HashMap<String, (Arc<str>, Arc<ServerConfig>)>);

impl std::fmt::Debug for VhostIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// Precomputes every virtual host's merged config. The merged configs share
/// this generation's `state` and hold an empty index themselves, so there is
/// no reference cycle between a config and its index.
pub fn build_vhost_index(config: &mut ServerConfig) {
    let mut index = std::collections::HashMap::new();
    for vhost in &config.virtual_hosts {
        let key: Arc<str> = format!("vhost:{}", vhost.domain.name.to_ascii_lowercase()).into();
        let merged = Arc::new(merge_vhost(config, vhost));
        for name in std::iter::once(&vhost.domain.name).chain(&vhost.domain.aliases) {
            index
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| (key.clone(), merged.clone()));
        }
    }
    config.vhost_index = Arc::new(VhostIndex(index));
}

/// Selects the virtual host for a request: its state key (for per-host cache
/// and rate-limit state) and its effective config.
fn resolve_host<B>(
    req: &Request<B>,
    config: &Arc<ServerConfig>,
) -> (Option<Arc<str>>, Arc<ServerConfig>) {
    // Single-host servers need neither authority parsing nor a lookup.
    if config.virtual_hosts.is_empty() {
        return (None, Arc::clone(config));
    }
    let authority = req
        .uri()
        .authority()
        .map(|a| a.as_str())
        .or_else(|| req.headers().get("host").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    let host = authority
        .parse::<::http::uri::Authority>()
        .ok()
        .map(|a| a.host().to_ascii_lowercase())
        .unwrap_or_default();
    if host.is_empty() {
        return (None, Arc::clone(config));
    }
    if let Some((key, merged)) = config.vhost_index.0.get(&host) {
        return (Some(key.clone()), merged.clone());
    }
    // Configs built without load_config (tests, embedders) have no index.
    config
        .virtual_hosts
        .iter()
        .find(|vh| {
            vh.domain.name.eq_ignore_ascii_case(&host)
                || vh
                    .domain
                    .aliases
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(&host))
        })
        .map(|vh| {
            (
                Some(format!("vhost:{}", vh.domain.name.to_ascii_lowercase()).into()),
                Arc::new(merge_vhost(config, vh)),
            )
        })
        .unwrap_or_else(|| (None, Arc::clone(config)))
}

fn get_cache(config: &ServerConfig, key: &str) -> Arc<ResponseCache> {
    if key == "default" {
        return Arc::clone(
            config
                .state
                .default_cache
                .get_or_init(|| Arc::new(ResponseCache::from_config(config))),
        );
    }
    let caches = &config.state.caches;
    // Hit path first: `entry` needs an owned key, so it allocated a String on
    // every request even though the entry almost always already exists.
    if let Some(found) = caches.get(key) {
        return found.clone();
    }
    caches
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(ResponseCache::from_config(config)))
        .clone()
}

pub(crate) fn get_rate_limiter(config: &ServerConfig, key: &str) -> Arc<RateLimiter> {
    let limiters = &config.state.limiters;
    if let Some(found) = limiters.get(key) {
        return found.clone();
    }
    limiters
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(RateLimiter::from_config(config)))
        .clone()
}

pub fn metrics() -> &'static Arc<crate::metrics::prometheus::Metrics> {
    METRICS.get_or_init(crate::metrics::prometheus::Metrics::new)
}

/// Called by the connection service after *every* dispatch path, including
/// early security failures, so the exported counters represent actual traffic.
pub fn record_response(response: &Response<ResponseBody>) {
    let bytes = response
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    metrics().record(response.status().as_u16(), bytes);
}

pub async fn dispatch<B>(
    req: Request<B>,
    remote_addr: SocketAddr,
    config: Arc<ServerConfig>,
) -> Response<ResponseBody>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let start = Instant::now();
    // A per-request UUID meant random-number generation and an allocation on
    // every request, including the overwhelming majority that log nothing at
    // this level. A process-local counter correlates log lines just as well.
    let request_id = REQUEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let (host_key, config) = resolve_host(&req, &config);
    let state_key = host_key.as_deref().unwrap_or("default");
    let server_name = &config.server.server_name;

    // Security: IP access control
    if check_ip_access(&remote_addr.ip(), &config).is_err() {
        warn!(request_id = request_id, ip = %remote_addr, "IP denied");
        return error_403(server_name);
    }

    // Security: rate limiting. The limiter is only looked up when one is
    // configured; fetching it unconditionally allocated a key and touched the
    // shared map on every request of every server that has rate limiting off.
    if config
        .security
        .as_ref()
        .and_then(|s| s.rate_limit.as_ref())
        .is_some()
        && !get_rate_limiter(&config, state_key).check(remote_addr.ip())
    {
        warn!(request_id = request_id, ip = %remote_addr, "Rate limit exceeded");
        return error_429();
    }

    // Parse request
    let max_body = body_limit_bytes(&config);
    let method = req.method().clone();
    let header_bytes: usize = req
        .headers()
        .iter()
        .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
        .sum();
    let header_limit = config
        .security
        .as_ref()
        .and_then(|s| crate::config::parse_size(&s.max_header_size))
        .unwrap_or(32768);
    if header_bytes as u64 > header_limit {
        return empty_response(431);
    }
    let read_timeout = std::time::Duration::from_secs(
        crate::config::parse_duration_secs(&config.server.read_timeout).unwrap_or(30),
    );
    // A request with no body has nothing to wait for, so it does not need a
    // timer registered and cancelled around an already-complete read.
    let parsed_result = if req.body().size_hint().exact() == Some(0) {
        ParsedRequest::from_hyper(req, max_body).await
    } else {
        match tokio::time::timeout(read_timeout, ParsedRequest::from_hyper(req, max_body)).await {
            Ok(result) => result,
            Err(_) => return empty_response(408),
        }
    };
    let mut parsed = match parsed_result {
        Ok(p) => p,
        Err(crate::error::FlashError::BodyTooLarge) => return error_413(server_name),
        Err(_) => return error_500(server_name),
    };

    // Keep the endpoint intentionally small and dependency-free.  It is useful
    // both for Prometheus scraping and inexpensive liveness monitoring.
    if parsed.path == "/metrics" && method == Method::GET {
        if !remote_addr.ip().to_canonical().is_loopback() {
            return error_403(server_name);
        }
        return Response::builder()
            .status(200)
            .header("content-type", "text/plain; version=0.0.4; charset=utf-8")
            .body(ResponseBody::new(Bytes::from(
                metrics().render() + &crate::realweb::hub().render_metrics(),
            )))
            .unwrap_or_else(|_| error_500(server_name));
    }

    // Loopback-only control endpoints backing `flashlin purge cache` and
    // `flashlin apps`.
    if let Some(action) = parsed.path.strip_prefix("/flashlin/") {
        if let Some(response) = control_endpoint(action, &method, &parsed, remote_addr, &config) {
            return response;
        }
    }

    // RealWeb endpoint and its client script, ahead of route matching.
    if let Some(rw) = config.realweb.as_ref().filter(|rw| rw.enabled) {
        if let Some(response) =
            crate::realweb::handle(&parsed, &method, rw, remote_addr, &config).await
        {
            return response;
        }
    }

    // Security: block dotfiles and sensitive paths
    if crate::static_files::path_security::validate_url_path(&parsed.path).is_err() {
        return error_403(server_name);
    }
    let sec = config.security.as_ref();
    if sec.map(|s| s.block_dotfiles).unwrap_or(true) {
        if parsed
            .path
            .split('/')
            .any(|seg| seg.starts_with('.') && seg.len() > 1)
        {
            return error_403(server_name);
        }
    }
    if sec.map(|s| s.block_sensitive_files).unwrap_or(true) {
        if is_sensitive_path(&parsed.path) {
            return error_403(server_name);
        }
    }

    // Static mounts have precedence over a broad fallback route such as `/`.
    // Their URL prefix is removed before filesystem resolution, so
    // `url_path = "/assets"` with `root = "./public/assets"` maps
    // `/assets/app.js` to `./public/assets/app.js` as users expect.
    let request_path = parsed.path.clone();
    let static_dir = find_static_dir(&request_path, &config);
    let mounted_route = static_dir.map(|dir| RouteConfig {
        path: dir.url_path.clone(),
        root: Some(dir.root.to_string_lossy().into_owned()),
        index: None,
        proxy_pass: None,
        proxy_upstreams: vec![],
        php_handler: None,
        php_enabled: false,
        cache_control: dir.cache_control.clone(),
        methods: None,
        directory_listing: false,
        app: None,
    });
    if let Some(dir) = static_dir {
        parsed.path = static_relative_path(&request_path, &dir.url_path);
    }
    let route = mounted_route
        .as_ref()
        .or_else(|| find_route(&request_path, &config));

    // Method check
    if let Some(route) = &route {
        if let Some(allowed) = &route.methods {
            if !allowed.is_empty() && !allowed.iter().any(|m| m == method.as_str()) {
                return method_not_allowed_with_allow(server_name, &allowed.join(", "));
            }
        }
    }

    // =========================================================================
    // Unified rewrite / redirect pipeline
    //
    // Priority (highest -> lowest):
    //   1. .flashweb  — FlashLin native format
    //   2. .htaccess  — Apache-compatible rules
    //   3. nginx.conf / redirects.conf — nginx-compatible rules
    //
    // Steps 2-3 fire for ALL requests, even when a physical file with the same
    // name exists — matching real Apache/nginx behaviour where Redirect / return
    // directives take precedence over static files.
    //
    // Proxy routes are excluded: the upstream handles its own path routing.
    // =========================================================================

    let root = route
        .and_then(|r| r.root.as_deref())
        .or_else(|| config.server.root.as_deref())
        .unwrap_or("./public");

    // --- Step 1: .flashweb ---------------------------------------------------
    let flashweb_files = flashweb::load_flashweb(&parsed.path, root);

    if !flashweb::allows_ip(&flashweb_files, &remote_addr.ip()) {
        warn!(request_id = request_id, ip = %remote_addr, "IP denied by .flashweb");
        return error_403(server_name);
    }

    let is_proxy_route = route.and_then(|r| r.proxy_target()).is_some();
    if crate::http::request::is_query(&method)
        && !crate::http::request::query_content_type_valid(&parsed.headers)
    {
        let mut response = empty_response(400);
        flashweb::apply_cors(&flashweb_files, response.headers_mut());
        apply_security_headers(&mut response, &config);
        return response;
    }

    // Query formats/Accept-Query belong to the upstream application. Forward
    // OPTIONS discovery and preflight unless this host explicitly owns CORS.
    if method == Method::OPTIONS
        && (!is_proxy_route || flashweb_files.iter().any(|file| file.cors.is_some()))
    {
        let mut response = empty_response(204);
        flashweb::apply_headers(&flashweb_files, response.headers_mut());
        if let Some(allowed) = route
            .and_then(|r| r.methods.as_ref())
            .filter(|m| !m.is_empty())
        {
            if let Ok(value) = allowed.join(", ").parse() {
                response.headers_mut().insert(hyper::header::ALLOW, value);
            }
        }
        flashweb::apply_cors(&flashweb_files, response.headers_mut());
        apply_security_headers(&mut response, &config);
        return response;
    }

    if let Some((location, permanent)) = flashweb::apply_redirects(&flashweb_files, &parsed.path) {
        return redirect_response(&location, permanent);
    }

    if let Some(new_path) = flashweb::apply_rewrites(&flashweb_files, &parsed.path) {
        parsed.path = new_path;
    }

    // --- Step 2 & 3: .htaccess / nginx.conf / redirects.conf ----------------
    // Applied on the possibly-rewritten path from step 1.
    // Not applied to proxy routes (the upstream owns its routing).
    if !is_proxy_route {
        match find_rewrite_action(&parsed.path, root) {
            Ok(Some(RewriteAction::Redirect(rule))) => {
                return redirect_response(&rule.location, rule.permanent);
            }
            Ok(Some(RewriteAction::Rewrite(path))) => {
                parsed.path = path;
            }
            Ok(None) => {}
            Err(crate::error::FlashError::AccessDenied)
            | Err(crate::error::FlashError::PathTraversal) => {
                return error_403(server_name);
            }
            Err(_) => {} // Non-fatal (file not found, parse error, etc.)
        }
    }

    // =========================================================================

    let cache = get_cache(&config, state_key);
    let mut response =
        route_to_handler(&parsed, &parsed.headers, &method, route, &config, &cache).await;

    if response.status().is_client_error() || response.status().is_server_error() {
        if let Some(body) =
            flashweb::custom_error_page(&flashweb_files, response.status().as_u16(), root)
        {
            let (mut parts, _) = response.into_parts();
            parts.headers.insert(
                hyper::header::CONTENT_TYPE,
                hyper::header::HeaderValue::from_static("text/html; charset=utf-8"),
            );
            if let Ok(length) = body.len().to_string().parse() {
                parts.headers.insert(hyper::header::CONTENT_LENGTH, length);
            }
            response = Response::from_parts(parts, ResponseBody::new(body));
        }
    }

    // Apply .flashweb response-phase rules (headers, CORS, cache-control)
    flashweb::apply_headers(&flashweb_files, response.headers_mut());
    flashweb::apply_cors(&flashweb_files, response.headers_mut());

    if let Some(cc) = flashweb::get_cache_override(&flashweb_files) {
        if let Ok(v) = cc.parse() {
            response
                .headers_mut()
                .insert(hyper::header::CACHE_CONTROL, v);
        }
    }

    // Add security headers
    apply_security_headers(&mut response, &config);

    // Hide / set server header
    if sec.map(|s| s.hide_server_header).unwrap_or(true) {
        response.headers_mut().remove("server");
        response.headers_mut().insert(
            "server",
            server_name
                .parse()
                .unwrap_or_else(|_| "FlashLin".parse().unwrap()),
        );
    }

    // Compression
    let response = maybe_compress(response, &parsed.headers, &config).await;

    // Access log
    let status = response.status().as_u16();
    let elapsed = start.elapsed().as_millis();
    info!(
        request_id = request_id,
        ip = %remote_addr,
        method = %method,
        path = %parsed.path,
        status = status,
        ms = elapsed,
        "request"
    );

    response
}

// ---------------------------------------------------------------------------
// Control endpoints
// ---------------------------------------------------------------------------

/// The app name in `apps/<name>/restart`.
fn restart_target(action: &str) -> Option<&str> {
    action
        .strip_prefix("apps/")?
        .strip_suffix("/restart")
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

/// Handles `/flashlin/<action>`; `None` lets unknown paths fall through to
/// normal routing.
///
/// Every control request must come from loopback, must not carry headers a
/// proxy adds (a reverse proxy on the same host would otherwise make internet
/// traffic look local), and must present `server.control_token` when one is set.
fn control_endpoint(
    action: &str,
    method: &Method,
    req: &ParsedRequest,
    remote_addr: SocketAddr,
    config: &ServerConfig,
) -> Option<Response<ResponseBody>> {
    let restart = restart_target(action);
    let known = match *method {
        Method::POST => {
            action == "purge-cache" || action == "realweb/publish" || restart.is_some()
        }
        Method::GET => action == "apps" || action == "realweb",
        _ => false,
    };
    if !known {
        return None;
    }
    let server_name = &config.server.server_name;
    if !control_allowed(remote_addr, &req.headers, config) {
        warn!(ip = %remote_addr, path = %req.path, "Control request refused");
        return Some(error_403(server_name));
    }
    let respond = |status: u16, content_type: &'static str, body: Vec<u8>| {
        Response::builder()
            .status(status)
            .header("content-type", content_type)
            .header("cache-control", "no-store")
            .body(ResponseBody::new(Bytes::from(body)))
            .unwrap_or_else(|_| error_500(server_name))
    };
    let text =
        |status: u16, body: String| respond(status, "text/plain; charset=utf-8", body.into());
    if action == "purge-cache" {
        // Clears every in-memory response cache this process holds (default
        // host and every virtual host); does not touch files on disk.
        let removed = config.state.clear_all_caches();
        return Some(text(200, format!("purged {removed} cache entries\n")));
    }
    if action.starts_with("realweb") {
        let (status, body) = if *method == Method::POST {
            crate::realweb::control_publish(&req.body, config)
        } else {
            crate::realweb::control_stats(config)
        };
        let content_type = if status == 200 {
            "application/json"
        } else {
            "text/plain; charset=utf-8"
        };
        return Some(respond(status, content_type, body.into()));
    }
    let Some(manager) = crate::apps::current() else {
        return Some(text(
            404,
            "no [[apps]] are configured on this server\n".into(),
        ));
    };
    let Some(name) = restart else {
        let body = serde_json::to_vec(&manager.status()).unwrap_or_default();
        return Some(respond(200, "application/json", body));
    };
    Some(if manager.restart(name) {
        info!(app = name, "App restart requested via control endpoint");
        text(200, format!("restarting {name}\n"))
    } else {
        text(404, format!("no app named '{name}'\n"))
    })
}

pub(crate) fn control_allowed(
    remote_addr: SocketAddr,
    headers: &hyper::HeaderMap,
    config: &ServerConfig,
) -> bool {
    if !remote_addr.ip().to_canonical().is_loopback() {
        return false;
    }
    if ["forwarded", "x-forwarded-for", "x-real-ip", "via"]
        .iter()
        .any(|name| headers.contains_key(*name))
    {
        return false;
    }
    match config.server.control_token.as_deref() {
        None => true,
        Some(token) => headers
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.as_bytes().strip_prefix(b"Bearer "))
            .is_some_and(|given| constant_time_eq(given, token.as_bytes())),
    }
}

/// Compares without an early exit on the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Route matching
// ---------------------------------------------------------------------------

/// `path` is `prefix` followed by a `/` segment boundary.
///
/// Written without `format!` because route matching runs for every route on
/// every request, and the temporary string was the allocation there.
pub(crate) fn starts_with_segment(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len() && path.as_bytes()[prefix.len()] == b'/' && path.starts_with(prefix)
}

pub(crate) fn find_route<'a>(path: &str, config: &'a ServerConfig) -> Option<&'a RouteConfig> {
    let routes = config.routes.as_deref()?;
    // Longest-prefix match wins.
    routes
        .iter()
        .filter(|r| path == r.path || starts_with_segment(path, &r.path) || r.path == "/")
        .max_by_key(|r| r.path.len())
}

pub(crate) fn find_static_dir<'a>(
    path: &str,
    config: &'a ServerConfig,
) -> Option<&'a crate::config::parser::StaticDirConfig> {
    config
        .static_dirs
        .as_deref()?
        .iter()
        .filter(|dir| {
            let mount = dir.url_path.trim_end_matches('/');
            let mount = if mount.is_empty() { "/" } else { mount };
            path == mount || mount == "/" || starts_with_segment(path, mount)
        })
        .max_by_key(|dir| dir.url_path.len())
}

pub(crate) fn static_relative_path(request_path: &str, mount: &str) -> String {
    let mount = mount.trim_end_matches('/');
    if mount.is_empty() || mount == "/" {
        return request_path.to_string();
    }
    match request_path.strip_prefix(mount) {
        Some("") => "/".to_string(),
        Some(path) if path.starts_with('/') => path.to_string(),
        Some(path) => format!("/{path}"),
        None => request_path.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Handler dispatch
// ---------------------------------------------------------------------------

async fn route_to_handler(
    req: &ParsedRequest,
    req_headers: &hyper::HeaderMap,
    method: &Method,
    route: Option<&RouteConfig>,
    config: &Arc<ServerConfig>,
    cache: &Arc<ResponseCache>,
) -> Response<ResponseBody> {
    let server_name = &config.server.server_name;

    let root = route
        .and_then(|r| r.root.as_deref())
        .or_else(|| config.server.root.as_deref())
        .unwrap_or("./public");

    let index_files: Vec<&str> = route
        .and_then(|r| r.index.as_deref())
        .map(|i| vec![i])
        .unwrap_or_else(|| {
            config
                .server
                .index
                .as_deref()
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_else(|| vec!["index.php", "index.html"])
        });

    let php_enabled = route.map(|r| r.php_enabled).unwrap_or(false)
        && config.php.as_ref().map(|p| p.enabled).unwrap_or(false);

    // All redirect/rewrite rules (.flashweb, .htaccess, nginx.conf) have
    // already been applied in `dispatch` before we reach here.
    let effective_req = req;

    if route.and_then(|r| r.proxy_target()).is_none() {
        if let Some(resp) = redirect_directory_without_slash(effective_req, root) {
            return resp;
        }

        match resolve_directory_index(&effective_req.path, root, &index_files, php_enabled) {
            Ok(Some(DirectoryIndex::Php(_))) if php_enabled => {
                if let Some(handler) = route
                    .and_then(|r| r.php_handler.as_deref())
                    .or_else(|| {
                        config
                            .php
                            .as_ref()
                            .and_then(|p| p.default_handler.as_deref())
                    })
                    .and_then(|name| find_php_handler(name, config))
                {
                    let mut php_req = effective_req.clone();
                    php_req.path = if effective_req.path == "/" {
                        "/index.php".to_string()
                    } else {
                        format!("{}/index.php", effective_req.path.trim_end_matches('/'))
                    };
                    return crate::php::handler::handle_php(
                        &php_req, method, root, handler, config,
                    )
                    .await;
                }
            }
            Ok(Some(DirectoryIndex::Php(_))) => {}
            Ok(Some(DirectoryIndex::Static(_))) | Ok(Some(DirectoryIndex::Missing)) => {
                if crate::http::request::is_query(method) {
                    return static_query_not_allowed(route, server_name);
                }
                let cache_control = route.and_then(|r| r.cache_control.as_deref());
                let dir_listing = route.map(|r| r.directory_listing).unwrap_or(false);
                let mut effective_index_files = index_files.clone();
                if !effective_index_files
                    .iter()
                    .any(|idx| idx.eq_ignore_ascii_case("index.php"))
                {
                    effective_index_files.push("index.php");
                }
                if !effective_index_files
                    .iter()
                    .any(|idx| idx.eq_ignore_ascii_case("index.html"))
                {
                    effective_index_files.push("index.html");
                }
                return match serve_static_cached(
                    &effective_req.path,
                    root,
                    &effective_index_files,
                    cache_control,
                    dir_listing,
                    config,
                    Some(req_headers),
                    cache,
                    method,
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(crate::error::FlashError::NotFound) => error_404(server_name),
                    Err(crate::error::FlashError::AccessDenied)
                    | Err(crate::error::FlashError::PathTraversal) => error_403(server_name),
                    Err(crate::error::FlashError::SensitiveFile) => error_403(server_name),
                    Err(_) => error_500(server_name),
                };
            }
            Ok(None) => {}
            Err(crate::error::FlashError::AccessDenied)
            | Err(crate::error::FlashError::PathTraversal) => {
                return error_403(server_name);
            }
            Err(_) => return error_500(server_name),
        }

        if php_enabled
            && should_dispatch_to_php(&effective_req.path)
            && !path_exists(root, &effective_req.path)
        {
            return error_404(server_name);
        }
    }

    // Proxy route
    if let Some(r) = route {
        if let Some(upstream) = r.proxy_target() {
            return crate::proxy::reverse_proxy::balanced_request(
                effective_req,
                method,
                upstream,
                &r.proxy_upstreams,
                config,
            )
            .await;
        }
    }

    // PHP route
    if let Some(r) = route {
        let php_enabled = r.php_enabled && config.php.as_ref().map(|p| p.enabled).unwrap_or(false);
        if php_enabled && should_dispatch_to_php(&effective_req.path) {
            let handler_name = r
                .php_handler
                .as_deref()
                .or_else(|| {
                    config
                        .php
                        .as_ref()
                        .and_then(|p| p.default_handler.as_deref())
                })
                .unwrap_or("");
            if !handler_name.is_empty() {
                if let Some(handler) = find_php_handler(handler_name, config) {
                    let root = r
                        .root
                        .as_deref()
                        .or_else(|| handler.document_root.as_deref())
                        .unwrap_or("./public");
                    return crate::php::handler::handle_php(
                        effective_req,
                        method,
                        root,
                        handler,
                        config,
                    )
                    .await;
                }
            }
        }
    }

    // Static file fallback
    if crate::http::request::is_query(method) {
        return static_query_not_allowed(route, server_name);
    }
    let root = route
        .and_then(|r| r.root.as_deref())
        .or_else(|| config.server.root.as_deref())
        .unwrap_or("./public");

    let index_files: Vec<&str> = route
        .and_then(|r| r.index.as_deref())
        .map(|i| vec![i])
        .unwrap_or_else(|| {
            config
                .server
                .index
                .as_deref()
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_else(|| vec!["index.html", "index.php"])
        });

    let cache_control = route.and_then(|r| r.cache_control.as_deref());
    let dir_listing = route.map(|r| r.directory_listing).unwrap_or(false);

    match serve_static_cached(
        &effective_req.path,
        root,
        &index_files,
        cache_control,
        dir_listing,
        config,
        Some(req_headers),
        cache,
        method,
    )
    .await
    {
        Ok(resp) => resp,
        Err(crate::error::FlashError::NotFound) => error_404(server_name),
        Err(crate::error::FlashError::AccessDenied)
        | Err(crate::error::FlashError::PathTraversal) => error_403(server_name),
        Err(crate::error::FlashError::SensitiveFile) => error_403(server_name),
        Err(_) => error_500(server_name),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn redirect_directory_without_slash(
    req: &ParsedRequest,
    root: &str,
) -> Option<Response<ResponseBody>> {
    if req.uri.path() == "/" || req.uri.path().ends_with('/') {
        return None;
    }

    let fs_path = crate::static_files::path_security::safe_join(root, &req.path).ok()?;
    // Known regular file: it is not a directory, so skip the stat entirely.
    if crate::static_files::file_server::is_verified_file(&fs_path) {
        return None;
    }
    if !Path::new(&fs_path).is_dir() {
        return None;
    }

    let location = if req.query.is_empty() {
        format!("{}/", req.uri.path())
    } else {
        format!("{}/?{}", req.uri.path(), req.query)
    };

    Some(redirect_response(&location, true))
}

fn static_query_not_allowed(route: Option<&RouteConfig>, server_name: &str) -> HyperResponse {
    let allowed: Vec<_> = ["GET", "HEAD", "OPTIONS"]
        .into_iter()
        .filter(|method| {
            route
                .and_then(|r| r.methods.as_ref())
                .filter(|m| !m.is_empty())
                .is_none_or(|methods| methods.iter().any(|m| m == method))
        })
        .collect();
    method_not_allowed_with_allow(server_name, &allowed.join(", "))
}

fn path_exists(root: &str, url_path: &str) -> bool {
    crate::static_files::path_security::safe_join(root, url_path)
        .ok()
        .map(|path| Path::new(&path).exists())
        .unwrap_or(false)
}

fn find_php_handler<'a>(
    name: &str,
    config: &'a ServerConfig,
) -> Option<&'a crate::config::PhpHandler> {
    config
        .php
        .as_ref()?
        .handlers
        .iter()
        .find(|h| h.name == name)
}

fn should_dispatch_to_php(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    let segment = path.rsplit('/').next().unwrap_or(path);

    if let Some(dot_pos) = segment.rfind('.') {
        let ext = &segment[dot_pos..].to_lowercase();
        ext == ".php"
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// Virtual host merging
// ---------------------------------------------------------------------------

pub(crate) fn merge_vhost(global: &ServerConfig, vhost: &VirtualHostConfig) -> ServerConfig {
    let mut merged = global.clone();
    merged.vhost_index = Default::default();
    merged.server.server_name = vhost
        .domain
        .server_name
        .clone()
        .unwrap_or_else(|| vhost.domain.name.clone());
    if let Some(ref root) = vhost.domain.root {
        merged.server.root = Some(root.clone());
    }
    if let Some(ref idx) = vhost.domain.index {
        merged.server.index = Some(idx.clone());
    }
    if let Some(ref css) = vhost.domain.directory_listing_css {
        merged.server.directory_listing_css = Some(css.clone());
    }
    if let Some(ref tls) = vhost.tls {
        merged.tls = Some(tls.clone());
    }
    if let Some(ref sec) = vhost.security {
        merged.security = Some(sec.clone());
    }
    if let Some(ref comp) = vhost.compression {
        merged.compression = Some(comp.clone());
    }
    if let Some(ref cache) = vhost.cache {
        merged.cache = Some(cache.clone());
    }
    if let Some(ref php) = vhost.php {
        merged.php = Some(php.clone());
    }
    if let Some(ref routes) = vhost.routes {
        merged.routes = Some(routes.clone());
    }
    if let Some(ref dirs) = vhost.static_dirs {
        merged.static_dirs = Some(dirs.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::{get_cache, static_relative_path};
    use crate::cache::CachedEntry;
    use std::time::Duration;

    #[test]
    fn static_mount_removes_its_url_prefix() {
        assert_eq!(static_relative_path("/assets/app.js", "/assets"), "/app.js");
        assert_eq!(static_relative_path("/assets", "/assets"), "/");
        assert_eq!(static_relative_path("/index.html", "/"), "/index.html");
    }

    #[test]
    fn clear_all_caches_purges_the_default_host_and_every_virtual_host() {
        let config = crate::default_config();
        let default_cache = get_cache(&config, "default");
        let vhost_cache = get_cache(&config, "vhost:example.com");
        default_cache.insert(
            "a".into(),
            CachedEntry::new(bytes::Bytes::from_static(b"x"), Duration::from_secs(60)),
        );
        vhost_cache.insert(
            "b".into(),
            CachedEntry::new(bytes::Bytes::from_static(b"y"), Duration::from_secs(60)),
        );

        assert_eq!(config.state.clear_all_caches(), 2);
        assert!(default_cache.get("a").is_none());
        assert!(vhost_cache.get("b").is_none());
        // Safe to call again with nothing left to clear.
        assert_eq!(config.state.clear_all_caches(), 0);
    }
}
