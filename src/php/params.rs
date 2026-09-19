use std::collections::HashMap;
use std::path::Path;

use crate::http::request::ParsedRequest;

/// Build the FastCGI / CGI parameter map for a PHP request.
pub fn build_cgi_params(
    req: &ParsedRequest,
    script_filename: &str,
    document_root: &str,
    extra_env: Option<&Vec<(String, String)>>,
) -> HashMap<String, String> {
    let mut params = HashMap::new();

    // Required FastCGI/CGI variables.
    params.insert("GATEWAY_INTERFACE".into(), "CGI/1.1".into());
    params.insert("SERVER_SOFTWARE".into(), "FlashLin/0.3".into());
    params.insert("SERVER_PROTOCOL".into(), "HTTP/1.1".into());

    params.insert("SCRIPT_FILENAME".into(), script_filename.to_string());
    params.insert("DOCUMENT_ROOT".into(), document_root.to_string());

    params.insert("REQUEST_METHOD".into(), req.method.as_str().to_string());
    params.insert("REQUEST_URI".into(), {
        let uri = req.uri.to_string();
        uri
    });
    params.insert("SCRIPT_NAME".into(), req.path.clone());
    params.insert("PATH_INFO".into(), String::new());
    params.insert("QUERY_STRING".into(), req.query.clone());

    // Content headers.
    // Representation metadata remains significant even for an empty QUERY body.
    {
        let ct = req
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        params.insert("CONTENT_TYPE".into(), ct);
        params.insert("CONTENT_LENGTH".into(), req.body.len().to_string());
    }

    // Forward HTTP headers as HTTP_* variables.
    for (name, value) in req.headers.iter() {
        if let Ok(v) = value.to_str() {
            let key = format!("HTTP_{}", name.as_str().to_uppercase().replace('-', "_"));
            params.insert(key, v.to_string());
        }
    }

    // SERVER_NAME from Host header.
    let host = req
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    params.insert("SERVER_NAME".into(), host.to_string());
    params.insert("HTTP_HOST".into(), host.to_string());

    // Forwarded IP.
    let remote_addr = req
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");
    params.insert("REMOTE_ADDR".into(), remote_addr.to_string());

    // Merge caller-supplied extra env vars.
    if let Some(env) = extra_env {
        for (k, v) in env {
            params.insert(k.clone(), v.clone());
        }
    }

    params
}

/// Given the document root and the request path, resolve the PHP script path.
/// Returns `(script_filename, path_info)` or `None` (→ 404) if not found.
///
/// Two modes:
///
/// 1. **Explicit `.php` request** (`/foo.php`, `/admin/login.php`):
///    Only the exact file is tried. If it does not exist → 404.
///    No silent fallback to index.php (that would mask typos / missing files).
///
/// 2. **Extensionless / directory** (`/`, `/about`, `/api/v1/users`):
///    CMS / framework routing — tries `<path>/index.php` then the root
///    `index.php` with `PATH_INFO` set so the app can handle the route.
pub fn resolve_script(
    document_root: &str,
    url_path: &str,
    index: &str,
) -> Option<(String, String)> {
    let root = Path::new(document_root);
    let clean = url_path.trim_start_matches('/');

    if url_path.to_lowercase().ends_with(".php") {
        // ── Explicit .php file ────────────────────────────────────────────────
        // Serve the exact file; return None (→ 404) if it does not exist.
        let script = root.join(clean);
        if script.exists() {
            return Some((script.to_string_lossy().into_owned(), String::new()));
        }
        return None;
    }

    // ── Extensionless / directory — CMS / framework URL routing ──────────────
    // 1. <root>/<path>/index.php   (e.g. /blog/ → /var/www/public/blog/index.php)
    let dir_index = root.join(clean).join(index);
    if dir_index.exists() {
        return Some((dir_index.to_string_lossy().into_owned(), String::new()));
    }

    // 2. <root>/index.php  with PATH_INFO set  (WordPress, Laravel, etc.)
    let root_index = root.join(index);
    if root_index.exists() {
        return Some((
            root_index.to_string_lossy().into_owned(),
            url_path.to_string(),
        ));
    }

    None
}
