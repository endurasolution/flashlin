use std::time::SystemTime;

use hyper::header::{self, HeaderMap, HeaderValue};

/// Build ETag from file size + modified time.
///
/// Size and nanosecond mtime are hex-encoded directly rather than hashed: an
/// ETag is an opaque cache validator, not a digest of the content, so the
/// per-request SHA-256 bought nothing. Same inputs, same change detection,
/// same weak-validator semantics — this is what nginx emits too.
pub fn make_etag(size: u64, modified: SystemTime) -> String {
    let nanos = modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("W/\"{size:x}-{nanos:x}\"")
}

/// Format a SystemTime as an HTTP date string.
pub fn http_date(t: SystemTime) -> String {
    httpdate::fmt_http_date(t)
}

/// Check whether the client's conditional request headers indicate we should
/// send 304 Not Modified.
pub fn is_not_modified(req_headers: &HeaderMap, etag: &str, last_modified: SystemTime) -> bool {
    // If-None-Match wins over If-Modified-Since when both present.
    if let Some(inm) = req_headers.get(header::IF_NONE_MATCH) {
        if let Ok(s) = inm.to_str() {
            return s.split(',').any(|item| {
                item.trim() == "*"
                    || item.trim().trim_start_matches("W/") == etag.trim_start_matches("W/")
            });
        }
    }
    if let Some(ims) = req_headers.get(header::IF_MODIFIED_SINCE) {
        if let Ok(s) = ims.to_str() {
            if let Ok(client_time) = httpdate::parse_http_date(s) {
                // Truncate to seconds for comparison.
                let file_secs = last_modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let client_secs = client_time
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                return file_secs <= client_secs;
            }
        }
    }
    false
}

/// Apply ETag, Last-Modified, and Cache-Control to a response's header map.
pub fn apply_cache_headers(
    headers: &mut HeaderMap,
    etag: &str,
    last_modified: SystemTime,
    cache_control: Option<&str>,
) {
    if let Ok(v) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, v);
    }
    if let Ok(v) = HeaderValue::from_str(&http_date(last_modified)) {
        headers.insert(header::LAST_MODIFIED, v);
    }
    let cc = cache_control.unwrap_or("public, max-age=3600");
    if let Ok(v) = HeaderValue::from_str(cc) {
        headers.insert(header::CACHE_CONTROL, v);
    }
}
