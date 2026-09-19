/// .flashweb — FlashLin per-directory configuration file
///
/// Place a `.flashweb` file in any directory inside the web root to override
/// behaviour for that path.  Rules are inherited from parent directories
/// (root-level lowest priority, nearest directory highest).
///
/// Example .flashweb file:
///
///   [[redirect]]
///   from = "/old-page"
///   to   = "/new-page"
///   code = 301
///
///   [[rewrite]]
///   from = "/blog/*"
///   to   = "/index.php"
///
///   [[header]]
///   name  = "X-Powered-By"
///   value = "FlashLin"
///
///   [cache]
///   max_age = 86400
///
///   [cors]
///   origin  = "*"
///   methods = ["GET", "POST", "OPTIONS"]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use http::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use std::net::IpAddr;

static FLASHWEB_CACHE: std::sync::OnceLock<DashMap<PathBuf, (Instant, Option<Arc<FlashWebFile>>)>> =
    std::sync::OnceLock::new();

fn fw_cache() -> &'static DashMap<PathBuf, (Instant, Option<Arc<FlashWebFile>>)> {
    FLASHWEB_CACHE.get_or_init(DashMap::new)
}

const CACHE_TTL: Duration = Duration::from_secs(60);

// ─── Config structs ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FlashWebFile {
    #[serde(default)]
    pub redirect: Vec<FlashWebRedirect>,
    #[serde(default)]
    pub rewrite: Vec<FlashWebRewrite>,
    #[serde(default)]
    pub header: Vec<FlashWebHeader>,
    pub cache: Option<FlashWebCache>,
    pub cors: Option<FlashWebCors>,
    pub access: Option<FlashWebAccess>,
    #[serde(default)]
    pub error_page: Vec<FlashWebErrorPage>,
}

/// Redirect the browser to a different URL.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebRedirect {
    /// URL path to match (exact or glob with `*`).
    pub from: String,
    /// Destination URL or path.
    pub to: String,
    /// HTTP status code: 301 (permanent) or 302 (temporary, default).
    #[serde(default = "default_redirect_code")]
    pub code: u16,
}

fn default_redirect_code() -> u16 {
    302
}

/// Rewrite the request path internally (no browser redirect).
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebRewrite {
    /// URL path to match (exact or glob with `*`).
    pub from: String,
    /// New internal path to serve.
    pub to: String,
    /// Stop processing further rewrite rules after this one.
    #[serde(default)]
    pub last: bool,
}

/// Add, replace, or remove a response header.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebHeader {
    pub name: String,
    /// Value to set. Omit (or set to null) to remove the header.
    pub value: Option<String>,
    /// When true, append instead of replacing.
    #[serde(default)]
    pub append: bool,
}

/// Cache-control override for this directory.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebCache {
    /// `max-age` in seconds (sets `Cache-Control: public, max-age=N`).
    pub max_age: Option<u64>,
    /// Emit `Cache-Control: no-cache`.
    #[serde(default)]
    pub no_cache: bool,
    /// Emit `Cache-Control: no-store`.
    #[serde(default)]
    pub no_store: bool,
    /// Append `must-revalidate`.
    #[serde(default)]
    pub must_revalidate: bool,
}

/// CORS policy for this directory.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebCors {
    /// `Access-Control-Allow-Origin` value (e.g. `"*"` or `"https://example.com"`).
    pub origin: Option<String>,
    pub methods: Option<Vec<String>>,
    pub headers: Option<Vec<String>>,
    #[serde(default)]
    pub credentials: bool,
    /// `Access-Control-Max-Age` in seconds.
    pub max_age: Option<u64>,
}

/// IP access-control override for this directory.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebAccess {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
}

/// Serve a custom HTML file for a specific error code.
#[derive(Debug, Clone, Deserialize)]
pub struct FlashWebErrorPage {
    pub code: u16,
    pub file: String,
}

// ─── File loading ─────────────────────────────────────────────────────────────

/// Return all applicable `.flashweb` files for `url_path` under `root`,
/// ordered from outermost directory (lowest priority) to innermost (highest).
pub fn load_flashweb(url_path: &str, root: &str) -> Vec<Arc<FlashWebFile>> {
    let root_path = crate::static_files::path_security::canonical_root(root);

    let request_rel = url_path.trim_start_matches('/');
    let mut files = Vec::new();
    if let Some(file) = load_flashweb_file(&root_path.join(".flashweb")) {
        files.push(file);
    }

    let parent = Path::new(request_rel).parent().unwrap_or(Path::new(""));
    let mut walk = root_path.clone();
    for component in parent.components() {
        walk.push(component.as_os_str());
        if walk.starts_with(&root_path) {
            if let Some(file) = load_flashweb_file(&walk.join(".flashweb")) {
                files.push(file);
            }
        }
    }

    files
}

fn load_flashweb_file(path: &Path) -> Option<Arc<FlashWebFile>> {
    let cache = fw_cache();
    if let Some(entry) = cache.get(path) {
        let (created, cached_val) = &*entry;
        if created.elapsed() < CACHE_TTL {
            return cached_val.clone();
        }
    }
    let result = parse_flashweb_file(path).map(Arc::new);
    if cache.len() >= 4096 && !cache.contains_key(path) {
        cache.retain(|_, (created, _)| created.elapsed() < CACHE_TTL);
        if cache.len() >= 4096 {
            return result;
        }
    }
    cache.insert(path.to_path_buf(), (Instant::now(), result.clone()));
    result
}

fn parse_flashweb_file(path: &Path) -> Option<FlashWebFile> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return None,
    };
    match toml::from_str::<FlashWebFile>(&content) {
        Ok(fw) => Some(fw),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, ".flashweb parse error");
            None
        }
    }
}

// ─── Pattern matching ─────────────────────────────────────────────────────────

/// Match a URL path against a pattern.
/// Supports exact match and single-wildcard glob (`*`).
pub fn path_matches(pattern: &str, url_path: &str) -> bool {
    if pattern == url_path {
        return true;
    }
    if let Some(star) = pattern.find('*') {
        let prefix = &pattern[..star];
        let suffix = &pattern[star + 1..];
        return url_path.starts_with(prefix)
            && url_path.ends_with(suffix)
            && url_path.len() >= prefix.len() + suffix.len();
    }
    false
}

// ─── Rule application ─────────────────────────────────────────────────────────

/// Check redirect rules; returns `(location, permanent)` on first match.
pub fn apply_redirects(files: &[Arc<FlashWebFile>], url_path: &str) -> Option<(String, bool)> {
    for file in files.iter().rev() {
        for rule in &file.redirect {
            if path_matches(&rule.from, url_path) {
                return Some((rule.to.clone(), rule.code == 301));
            }
        }
    }
    None
}

/// Check rewrite rules; returns the new internal path on first match.
pub fn apply_rewrites(files: &[Arc<FlashWebFile>], url_path: &str) -> Option<String> {
    for file in files.iter().rev() {
        for rule in &file.rewrite {
            if path_matches(&rule.from, url_path) {
                return Some(rule.to.clone());
            }
        }
    }
    None
}

/// Apply inherited directory ACLs. A matching deny rule wins; a configured
/// allow-list requires the client address to be present in that list.
pub fn allows_ip(files: &[Arc<FlashWebFile>], ip: &IpAddr) -> bool {
    for file in files {
        let Some(access) = &file.access else {
            continue;
        };
        if access.deny.as_ref().is_some_and(|rules| {
            rules
                .iter()
                .any(|rule| crate::security::access_control::matches_ip_rule(ip, rule))
        }) {
            return false;
        }
        if access.allow.as_ref().is_some_and(|rules| {
            !rules.is_empty()
                && !rules
                    .iter()
                    .any(|rule| crate::security::access_control::matches_ip_rule(ip, rule))
        }) {
            return false;
        }
    }
    true
}

/// Apply custom response headers from `.flashweb` files.
pub fn apply_headers(files: &[Arc<FlashWebFile>], headers: &mut HeaderMap) {
    for file in files {
        for h in &file.header {
            let Ok(name) = HeaderName::from_bytes(h.name.as_bytes()) else {
                continue;
            };
            match &h.value {
                Some(val) => {
                    if let Ok(v) = HeaderValue::from_str(val) {
                        if h.append {
                            headers.append(name, v);
                        } else {
                            headers.insert(name, v);
                        }
                    }
                }
                None => {
                    headers.remove(&name);
                }
            }
        }
    }
}

/// Apply CORS headers from `.flashweb` files (nearest directory wins).
pub fn apply_cors(files: &[Arc<FlashWebFile>], headers: &mut HeaderMap) {
    for file in files.iter().rev() {
        let Some(cors) = &file.cors else { continue };

        if let Some(origin) = &cors.origin {
            if let Ok(v) = HeaderValue::from_str(origin) {
                headers.insert("access-control-allow-origin", v);
            }
        }
        if let Some(methods) = &cors.methods {
            if let Ok(v) = HeaderValue::from_str(&methods.join(", ")) {
                headers.insert("access-control-allow-methods", v);
            }
        }
        if let Some(hdrs) = &cors.headers {
            if let Ok(v) = HeaderValue::from_str(&hdrs.join(", ")) {
                headers.insert("access-control-allow-headers", v);
            }
        }
        if cors.credentials {
            headers.insert(
                "access-control-allow-credentials",
                HeaderValue::from_static("true"),
            );
        }
        if let Some(max_age) = cors.max_age {
            if let Ok(v) = HeaderValue::from_str(&max_age.to_string()) {
                headers.insert("access-control-max-age", v);
            }
        }
        break; // innermost CORS config wins
    }
}

/// Build a `Cache-Control` header value from `.flashweb` cache settings.
/// Returns `None` if no cache override is configured.
pub fn get_cache_override(files: &[Arc<FlashWebFile>]) -> Option<String> {
    for file in files.iter().rev() {
        let Some(cache) = &file.cache else { continue };

        let mut parts: Vec<String> = Vec::new();
        if cache.no_store {
            parts.push("no-store".into());
        } else if cache.no_cache {
            parts.push("no-cache".into());
        } else if let Some(max_age) = cache.max_age {
            parts.push("public".into());
            parts.push(format!("max-age={max_age}"));
        }
        if cache.must_revalidate {
            parts.push("must-revalidate".into());
        }
        if !parts.is_empty() {
            return Some(parts.join(", "));
        }
    }
    None
}

/// Resolve the closest custom error document for a response status. Files are
/// joined through the same traversal-safe resolver as normal static assets.
pub fn custom_error_page(
    files: &[Arc<FlashWebFile>],
    status: u16,
    root: &str,
) -> Option<bytes::Bytes> {
    for file in files.iter().rev() {
        let Some(rule) = file.error_page.iter().find(|rule| rule.code == status) else {
            continue;
        };
        let Ok(path) = crate::static_files::path_security::safe_join(root, &rule.file) else {
            continue;
        };
        if let Ok(contents) = std::fs::read(path) {
            return Some(bytes::Bytes::from(contents));
        }
    }
    None
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(path_matches("/about", "/about"));
        assert!(!path_matches("/about", "/contact"));
    }

    #[test]
    fn glob_prefix() {
        assert!(path_matches("/api/*", "/api/users"));
        assert!(path_matches("/api/*", "/api/"));
        assert!(!path_matches("/api/*", "/other/users"));
    }

    #[test]
    fn glob_suffix() {
        assert!(path_matches("*.php", "/index.php"));
        assert!(!path_matches("*.php", "/index.html"));
    }

    #[test]
    fn access_rules_allow_only_configured_networks() {
        let file = Arc::new(FlashWebFile {
            access: Some(FlashWebAccess {
                allow: Some(vec!["10.0.0.0/8".into()]),
                deny: Some(vec!["10.0.0.9".into()]),
            }),
            ..Default::default()
        });
        let allowed: IpAddr = "10.0.0.8".parse().unwrap();
        let denied: IpAddr = "10.0.0.9".parse().unwrap();
        let outside: IpAddr = "192.168.1.10".parse().unwrap();

        assert!(allows_ip(&[file.clone()], &allowed));
        assert!(!allows_ip(&[file.clone()], &denied));
        assert!(!allows_ip(&[file], &outside));
    }
}
