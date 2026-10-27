use crate::error::{FlashError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Top-level config ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(skip)]
    pub state: std::sync::Arc<crate::http::router::HostStates>,
    pub server: ServerSection,
    pub http3: Option<Http3Config>,
    pub tls: Option<TlsConfig>,
    pub security: Option<SecurityConfig>,
    pub compression: Option<CompressionConfig>,
    pub cache: Option<CacheConfig>,
    pub php: Option<PhpConfig>,
    pub logging: Option<LoggingConfig>,
    pub routes: Option<Vec<RouteConfig>>,
    pub static_dirs: Option<Vec<StaticDirConfig>>,
    /// Populated at runtime from `server.domains_dir`; not read from server.toml.
    #[serde(skip)]
    pub virtual_hosts: Vec<VirtualHostConfig>,
    /// Virtual hosts merged with this config once per loaded generation, so
    /// requests look them up instead of deep-cloning the config each time.
    #[serde(skip)]
    pub vhost_index: std::sync::Arc<crate::http::router::VhostIndex>,
    /// Background applications FlashLin starts, supervises and proxies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<AppConfig>,
}

// ─── Virtual host (per-domain .toml) ────────────────────────────────────────

/// Top-level structure of a `domains/<name>.toml` file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VirtualHostConfig {
    pub domain: DomainSection,
    pub tls: Option<TlsConfig>,
    pub security: Option<SecurityConfig>,
    pub compression: Option<CompressionConfig>,
    pub cache: Option<CacheConfig>,
    pub php: Option<PhpConfig>,
    pub logging: Option<LoggingConfig>,
    pub routes: Option<Vec<RouteConfig>>,
    pub static_dirs: Option<Vec<StaticDirConfig>>,
}

/// `[domain]` section inside a virtual-host TOML file.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DomainSection {
    /// Primary hostname, e.g. `"example.com"`.
    pub name: String,
    /// Extra hostnames that map to this virtual host (e.g. `["www.example.com"]`).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Document root for this domain (overrides `[server].root`).
    pub root: Option<String>,
    /// Index files for this domain (overrides `[server].index`).
    pub index: Option<Vec<String>>,
    /// Name shown in error pages; defaults to `name`.
    pub server_name: Option<String>,
}

// ─── [server] ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerSection {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub workers: Option<usize>,
    #[serde(default = "default_server_name")]
    pub server_name: String,
    /// e.g. "30s", "60s"
    #[serde(default = "default_read_timeout")]
    pub read_timeout: String,
    #[serde(default = "default_write_timeout")]
    pub write_timeout: String,
    #[serde(default = "default_true")]
    pub keep_alive: bool,
    #[serde(default = "default_keepalive_timeout")]
    pub keep_alive_timeout: String,
    /// Default document root used when no route matches
    pub root: Option<String>,
    /// Default index files
    pub index: Option<Vec<String>>,
    /// Maximum simultaneous connections before FlashLin returns 111.
    /// 0 = unlimited (default).
    #[serde(default)]
    pub max_connections: usize,
    /// Directory containing per-domain `*.toml` virtual-host configs.
    /// All `.toml` files found here are loaded at startup as virtual hosts.
    pub domains_dir: Option<String>,
    /// Serve static files directly on the async worker instead of handing
    /// each request to the blocking thread pool, removing one executor
    /// scheduling round trip per request. Safe only when the document root
    /// is backed by fast, OS-cached local storage: a slow read (cold disk,
    /// network filesystem, contended remote storage) then stalls every other
    /// request already queued on that worker thread. Default: false.
    #[serde(default)]
    pub direct_static_io: bool,
    /// Serve plain (non-TLS) GET/HEAD requests for static files with a
    /// minimal HTTP/1.1 fast path that transfers file bytes via Linux
    /// `sendfile(2)` — the kernel copies straight from the page cache to the
    /// socket, so this process's memory never holds the response body at
    /// all. Requests this path cannot safely handle (compression, ranges,
    /// virtual hosts, rewrite rules, non-GET/HEAD methods, anything with a
    /// body) fall back to the ordinary connection handler for the rest of
    /// that connection, so enabling this never removes functionality — it
    /// only accelerates the common case. Linux-only; ignored elsewhere.
    /// Default: false.
    #[serde(default)]
    pub sendfile: bool,
    /// Maximum simultaneous connections from one client (IPv4 address or
    /// IPv6 /64). Excess connections are closed at accept, before any TLS or
    /// HTTP work. 0 = unlimited (default).
    #[serde(default)]
    pub max_connections_per_ip: usize,
    /// When set, the loopback control endpoints (`/flashlin/...`, used by
    /// `flashlin purge cache` and `flashlin apps`) also require
    /// `Authorization: Bearer <token>`. The CLI reads it from the same file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_token: Option<String>,
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    8080
}
fn default_server_name() -> String {
    "FlashLin".into()
}
fn default_read_timeout() -> String {
    "30s".into()
}
fn default_write_timeout() -> String {
    "60s".into()
}
fn default_keepalive_timeout() -> String {
    "75s".into()
}
fn default_true() -> bool {
    true
}

// ─── [tls] ──────────────────────────────────────────────────────────────────

/// Optional UDP listener. HTTP/1.1 and HTTP/2 continue on the TLS TCP port.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Http3Config {
    #[serde(default)]
    pub enabled: bool,
    /// Defaults to server.port. Must also be the externally reachable UDP port.
    pub port: Option<u16>,
    #[serde(default = "default_h3_streams")]
    pub max_concurrent_streams: u32,
}

fn default_h3_streams() -> u32 {
    32
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TlsConfig {
    pub acme: Option<AcmeConfig>,
    #[serde(default)]
    pub enabled: bool,
    pub cert: Option<String>,
    pub key: Option<String>,
    #[serde(default)]
    pub redirect_http_to_https: bool,
    /// HTTP port for redirect listener (only used when redirect_http_to_https = true)
    #[serde(default = "default_http_port")]
    pub http_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AcmeConfig {
    pub domains: Vec<String>,
    pub email: String,
    pub cache_dir: String,
    #[serde(default)]
    pub production: bool,
    #[serde(default)]
    pub accept_terms: bool,
}

fn default_http_port() -> u16 {
    80
}

// ─── [security] ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    /// e.g. "20MB"
    #[serde(default = "default_body_size")]
    pub max_body_size: String,
    /// e.g. "32KB"
    #[serde(default = "default_header_size")]
    pub max_header_size: String,
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default = "default_true")]
    pub hide_server_header: bool,
    #[serde(default = "default_true")]
    pub block_dotfiles: bool,
    #[serde(default = "default_true")]
    pub block_sensitive_files: bool,
    pub allow_ips: Option<Vec<String>>,
    pub deny_ips: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub security_headers: bool,
    pub csp: Option<String>,
}

fn default_body_size() -> String {
    "20MB".into()
}
fn default_header_size() -> String {
    "32KB".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitConfig {
    /// e.g. "100/min", "500/hour"
    pub limit: String,
    /// Burst size above the limit
    pub burst: Option<u32>,
}

// ─── [cache] ────────────────────────────────────────────────────────────────

/// In-memory response cache for static files.
///
/// ```toml
/// [cache]
/// enabled      = true
/// max_size     = "128MB"
/// ttl          = "300s"
/// max_file_size = "10MB"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheConfig {
    /// Enable/disable the in-memory response cache (default: true).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Total cache capacity (default: "128MB").
    #[serde(default = "default_cache_size")]
    pub max_size: String,
    /// Time-to-live for cached entries (default: "300s").
    #[serde(default = "default_cache_ttl")]
    pub ttl: String,
    /// Optional static response freshness window; 0s validates every request.
    #[serde(default = "default_cache_revalidate_after")]
    pub revalidate_after: String,
    /// Maximum individual file size to cache (default: "10MB").
    #[serde(default = "default_cache_entry_size")]
    pub max_file_size: String,
}

fn default_cache_size() -> String {
    "128MB".into()
}
fn default_cache_ttl() -> String {
    "300s".into()
}
fn default_cache_revalidate_after() -> String {
    "0s".into()
}
fn default_cache_entry_size() -> String {
    "10MB".into()
}

// ─── [compression] ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CompressionConfig {
    #[serde(default = "default_true")]
    pub gzip: bool,
    #[serde(default = "default_true")]
    pub brotli: bool,
    /// Minimum response size to compress (bytes)
    #[serde(default = "default_compress_min")]
    pub min_size: u64,
}

fn default_compress_min() -> u64 {
    1024
}

// ─── [php] ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhpConfig {
    #[serde(default)]
    pub enabled: bool,
    pub default_handler: Option<String>,
    #[serde(default)]
    pub handlers: Vec<PhpHandler>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PhpMode {
    Fpm,
    Fastcgi,
    Cgi,
    Cli,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PhpHandler {
    pub name: String,
    pub mode: PhpMode,
    /// Unix socket path (for fpm/fastcgi)
    pub socket: Option<String>,
    /// TCP address (for fastcgi TCP mode) e.g. "127.0.0.1:9000"
    pub address: Option<String>,
    pub document_root: Option<String>,
    #[serde(default = "default_index_php")]
    pub index: String,
    /// e.g. "30s"
    #[serde(default = "default_php_timeout")]
    pub timeout: String,
    /// e.g. "20MB"
    #[serde(default = "default_body_size")]
    pub max_body_size: String,
    /// Extra env vars passed to PHP
    pub env: Option<Vec<(String, String)>>,
}

fn default_index_php() -> String {
    "index.php".into()
}
fn default_php_timeout() -> String {
    "30s".into()
}

// ─── [[routes]] ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RouteConfig {
    /// URL path prefix, e.g. "/"  or "/api"
    pub path: String,
    /// Serve static files from this directory
    pub root: Option<String>,
    /// Override index files for this route
    pub index: Option<String>,
    /// Forward to this upstream URL
    pub proxy_pass: Option<String>,
    /// Alternative to proxy_pass: round-robin backends with passive failure cooldown.
    #[serde(default)]
    pub proxy_upstreams: Vec<String>,
    /// PHP handler name for this route
    pub php_handler: Option<String>,
    #[serde(default = "default_true")]
    pub php_enabled: bool,
    /// Cache-Control header value for static assets
    pub cache_control: Option<String>,
    /// Allowed HTTP methods; empty = all
    pub methods: Option<Vec<String>>,
    #[serde(default)]
    pub directory_listing: bool,
    /// Name of an `[[apps]]` entry to proxy this route to. Resolved at load
    /// time into loopback `proxy_pass` / `proxy_upstreams` targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

// ─── [[static_dirs]] (optional simple static mounts) ───────────────────────
impl RouteConfig {
    pub fn proxy_target(&self) -> Option<&str> {
        self.proxy_pass
            .as_deref()
            .or_else(|| self.proxy_upstreams.first().map(String::as_str))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StaticDirConfig {
    pub url_path: String,
    pub root: PathBuf,
    pub cache_control: Option<String>,
}

// ─── [[apps]] ───────────────────────────────────────────────────────────────

/// A background application FlashLin runs and supervises.
///
/// ```toml
/// [[apps]]
/// name         = "api"
/// runtime      = "node"          # or "python"; or set `command` instead
/// script       = "server.js"
/// cwd          = "/srv/api"
/// port         = 3000            # passed to the app as $PORT
/// instances    = 2               # ports 3000 and 3001, load-balanced
/// health_check = "/health"
///
/// [[routes]]
/// path = "/api"
/// app  = "api"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub name: String,
    /// Picks the interpreter when `command` is not set.
    pub runtime: Option<AppRuntime>,
    /// Program to run; overrides the runtime's interpreter (e.g. a virtualenv's
    /// python, `gunicorn`, `npm`).
    pub command: Option<String>,
    /// Script passed as the first argument (e.g. `server.js`, `app.py`).
    pub script: Option<String>,
    /// Further arguments; `{port}` and `$PORT` are replaced with the port.
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (default: FlashLin's).
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// Loopback port of the first instance; instance N uses `port + N`.
    pub port: u16,
    #[serde(default = "default_instances")]
    pub instances: u16,
    #[serde(default)]
    pub restart: RestartPolicy,
    /// Consecutive crashes (runs shorter than 60s) before giving up; 0 = never.
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    /// HTTP path probed on loopback; the app is restarted when it stops answering.
    pub health_check: Option<String>,
    #[serde(default = "default_health_interval")]
    pub health_interval: String,
    /// How long a new instance may take to pass its first health check.
    #[serde(default = "default_start_timeout")]
    pub start_timeout: String,
    /// Grace period between SIGTERM and SIGKILL when stopping.
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout: String,
    /// Append stdout/stderr here instead of FlashLin's own log.
    pub log_file: Option<String>,
    /// Run as this user (Unix, requires FlashLin to start as root).
    pub user: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AppRuntime {
    Node,
    Python,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Always,
    OnFailure,
    Never,
}

fn default_instances() -> u16 {
    1
}
fn default_max_restarts() -> u32 {
    10
}
fn default_health_interval() -> String {
    "10s".into()
}
fn default_start_timeout() -> String {
    "30s".into()
}
fn default_stop_timeout() -> String {
    "10s".into()
}

// ─── [logging] ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub json: bool,
    pub access_log: Option<String>,
    pub error_log: Option<String>,
}

fn default_log_level() -> String {
    "info".into()
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Parse a human-readable size string like "20MB", "32KB", "1GB" into bytes.
///
/// Runs on the request path, so it neither allocates nor overflows.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, multiplier) = [
        ("GB", 1u64 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
        ("B", 1),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        let split = s.len().checked_sub(suffix.len())?;
        (s.is_char_boundary(split) && s[split..].eq_ignore_ascii_case(suffix))
            .then(|| (&s[..split], multiplier))
    })
    .unwrap_or((s, 1));
    digits.trim().parse::<u64>().ok()?.checked_mul(multiplier)
}

/// Parse "30s" / "2m" into seconds.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix('s') {
        n.trim().parse().ok()
    } else if let Some(n) = s.strip_suffix('m') {
        n.trim().parse::<u64>().ok().map(|v| v * 60)
    } else if let Some(n) = s.strip_suffix('h') {
        n.trim().parse::<u64>().ok().map(|v| v * 3600)
    } else {
        s.parse().ok()
    }
}

/// Parse "100/min", "500/hour" into (count, window_secs).
pub fn parse_rate_limit(s: &str) -> Option<(u32, u64)> {
    let (count_str, window_str) = s.split_once('/')?;
    let count: u32 = count_str.trim().parse().ok()?;
    let window = match window_str.trim().to_lowercase().as_str() {
        "sec" | "second" | "s" => 1,
        "min" | "minute" | "m" => 60,
        "hour" | "h" => 3600,
        other => other.parse::<u64>().ok()?,
    };
    Some((count, window))
}

// ─── Loader ─────────────────────────────────────────────────────────────────

pub fn load(path: &str) -> Result<ServerConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| FlashError::Config(format!("Cannot read {path}: {e}")))?;
    toml::from_str(&content)
        .map_err(|e| FlashError::Config(format!("TOML parse error in {path}: {e}")))
}

pub fn load_virtual_host(path: &str) -> Result<VirtualHostConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| FlashError::Config(format!("Cannot read {path}: {e}")))?;
    toml::from_str(&content)
        .map_err(|e| FlashError::Config(format!("TOML parse error in {path}: {e}")))
}
