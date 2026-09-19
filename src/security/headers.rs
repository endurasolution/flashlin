use hyper::header::HeaderValue;

use crate::config::ServerConfig;
use crate::http::response::HyperResponse;

/// Inject security headers into a response.
pub fn apply_security_headers(resp: &mut HyperResponse, config: &ServerConfig) {
    let enabled = config
        .security
        .as_ref()
        .map(|s| s.security_headers)
        .unwrap_or(true);

    if !enabled {
        return;
    }

    let headers = resp.headers_mut();

    // Prevent MIME-type sniffing.
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );

    // Block clickjacking.
    headers.insert("x-frame-options", HeaderValue::from_static("SAMEORIGIN"));

    // Referrer policy — don't leak the full URL to third parties.
    headers.insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );

    // Disable browser features we don't need.
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static(
            "accelerometer=(), camera=(), geolocation=(), gyroscope=(), \
             magnetometer=(), microphone=(), payment=(), usb=()",
        ),
    );

    // Content-Security-Policy — use custom value if configured, else a safe default.
    let csp = config
        .security
        .as_ref()
        .and_then(|s| s.csp.as_deref())
        .unwrap_or("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'");
    if let Ok(v) = HeaderValue::from_str(csp) {
        headers.insert("content-security-policy", v);
    }

    // HSTS — only set if TLS is enabled.
    if config.tls.as_ref().map(|t| t.enabled).unwrap_or(false) {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }

    // Cross-origin policies.
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
}
