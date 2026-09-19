use std::sync::Arc;

use crate::http::response::ResponseBody;
use bytes::Bytes;
use hyper::header::HeaderValue;
use hyper::{Method, Response};
use tracing::{error, warn};

use super::errors::PhpError;
use super::fpm::run_fpm_request;
use super::params::{build_cgi_params, resolve_script};
use crate::config::{PhpHandler, ServerConfig};
use crate::http::request::ParsedRequest;
use crate::http::response::{error_502, error_504, HyperResponse};

/// Directories where PHP execution is always blocked.
const BLOCKED_PHP_DIRS: &[&str] = &[
    "/uploads/",
    "/upload/",
    "/files/",
    "/media/",
    "/static/",
    "/assets/",
    "/tmp/",
    "/temp/",
    "/cache/",
    "/backups/",
    "/backup/",
    "/logs/",
];

pub async fn handle_php(
    req: &ParsedRequest,
    _method: &Method,
    document_root: &str,
    handler: &PhpHandler,
    config: &Arc<ServerConfig>,
) -> HyperResponse {
    let server_name = &config.server.server_name;

    // Block PHP execution in upload/static directories.
    let lower_path = req.path.to_lowercase();
    for blocked in BLOCKED_PHP_DIRS {
        if lower_path.starts_with(blocked) {
            warn!(path = %req.path, "PHP execution blocked in unsafe directory");
            return error_502(server_name);
        }
    }

    // Resolve the script path.
    let handler_root = handler.document_root.as_deref().unwrap_or(document_root);
    let (script_filename, path_info) = match resolve_script(handler_root, &req.path, &handler.index)
    {
        Some(s) => s,
        None => {
            return crate::http::response::error_404(server_name);
        }
    };

    // Build CGI/FastCGI params.
    let mut params = build_cgi_params(req, &script_filename, handler_root, handler.env.as_ref());
    if !path_info.is_empty() {
        params.insert("PATH_INFO".into(), path_info);
    }

    // Execute via PHP-FPM / FastCGI.
    match run_fpm_request(handler, params, &req.body).await {
        Ok(fcgi_resp) => build_http_response(fcgi_resp),
        Err(PhpError::Timeout) => {
            error!(script = %script_filename, "PHP backend timeout");
            error_504(server_name)
        }
        Err(e) => {
            error!(script = %script_filename, error = %e, "PHP backend error");
            error_502(server_name)
        }
    }
}

fn build_http_response(fcgi: super::fastcgi::FastCgiResponse) -> HyperResponse {
    let mut builder = Response::builder().status(fcgi.status);
    for (name, value) in &fcgi.headers {
        if let Ok(v) = HeaderValue::from_str(value) {
            builder = builder.header(name.as_str(), v);
        }
    }
    builder
        .body(ResponseBody::new(fcgi.body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(500)
                .body(ResponseBody::new(Bytes::new()))
                .unwrap()
        })
}
