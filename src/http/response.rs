pub use super::body::ResponseBody;
use bytes::Bytes;
use hyper::header::{self, HeaderValue};
use hyper::Response;

pub type HyperResponse = Response<ResponseBody>;

// ─── Generic builders ─────────────────────────────────────────────────────────

pub fn html_response(status: u16, body: impl Into<Bytes>) -> HyperResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(ResponseBody::new(body.into()))
        .unwrap()
}

pub fn text_response(status: u16, body: impl Into<Bytes>) -> HyperResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(ResponseBody::new(body.into()))
        .unwrap()
}

pub fn bytes_response(status: u16, content_type: &str, body: Bytes) -> HyperResponse {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(ResponseBody::new(body))
        .unwrap()
}

pub fn empty_response(status: u16) -> HyperResponse {
    Response::builder()
        .status(status)
        .body(ResponseBody::new(Bytes::new()))
        .unwrap()
}

pub fn redirect_response(location: &str, permanent: bool) -> HyperResponse {
    let status = if permanent { 301 } else { 302 };
    Response::builder()
        .status(status)
        .header(header::LOCATION, location)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(ResponseBody::new(
            format!("<a href=\"{location}\">Moved</a>").into(),
        ))
        .unwrap()
}

// ─── Styled preset error pages ────────────────────────────────────────────────
//
// All error pages call into `crate::error_pages` which returns a fully-styled,
// self-contained HTML document with the FlashLin logo in the footer.

/// 111 Server Overloaded — FlashLin's own internal overload signal.
/// Returns the styled 111 page with connection stats and an auto-retry timer.
pub fn error_111(server_name: &str, active: usize, max: usize) -> HyperResponse {
    let mut resp = html_response(111, crate::error_pages::page_111(server_name, active, max));
    // Tell the client not to cache this response and to retry soon.
    resp.headers_mut()
        .insert("Retry-After", HeaderValue::from_static("10"));
    resp.headers_mut()
        .insert("Cache-Control", HeaderValue::from_static("no-store"));
    resp.headers_mut()
        .insert("Connection", HeaderValue::from_static("close"));
    resp
}

pub fn error_404(server_name: &str) -> HyperResponse {
    html_response(404, crate::error_pages::page_404(server_name))
}

pub fn error_403(server_name: &str) -> HyperResponse {
    html_response(403, crate::error_pages::page_403(server_name))
}

pub fn error_500(server_name: &str) -> HyperResponse {
    html_response(500, crate::error_pages::page_500(server_name))
}

pub fn error_502(server_name: &str) -> HyperResponse {
    html_response(502, crate::error_pages::page_502(server_name))
}

pub fn error_503(server_name: &str) -> HyperResponse {
    html_response(503, crate::error_pages::page_503(server_name))
}

pub fn error_504(server_name: &str) -> HyperResponse {
    html_response(504, crate::error_pages::page_504(server_name))
}

pub fn error_429() -> HyperResponse {
    // Rate-limit pages don't have server_name context; use a default.
    let mut resp = html_response(429, crate::error_pages::page_429("FlashLin"));
    resp.headers_mut()
        .insert("Retry-After", HeaderValue::from_static("60"));
    resp
}

pub fn error_413(server_name: &str) -> HyperResponse {
    html_response(413, crate::error_pages::page_413(server_name))
}

pub fn method_not_allowed(server_name: &str) -> HyperResponse {
    html_response(405, crate::error_pages::page_405(server_name))
}

pub fn method_not_allowed_with_allow(server_name: &str, allowed: &str) -> HyperResponse {
    let mut response = method_not_allowed(server_name);
    if let Ok(value) = HeaderValue::from_str(allowed) {
        response.headers_mut().insert(header::ALLOW, value);
    }
    response
}
