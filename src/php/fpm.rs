/// PHP-FPM connector — routes to FastCGI over Unix socket or TCP.
use std::collections::HashMap;

use super::errors::PhpError;
use super::fastcgi::{send_fastcgi_request, send_fastcgi_unix, FastCgiResponse};
use crate::config::{parse_duration_secs, PhpHandler, PhpMode};

pub async fn run_fpm_request(
    handler: &PhpHandler,
    params: HashMap<String, String>,
    body: &[u8],
) -> Result<FastCgiResponse, PhpError> {
    let timeout_secs = parse_duration_secs(&handler.timeout).unwrap_or(30);

    match handler.mode {
        PhpMode::Fpm | PhpMode::Fastcgi => {
            if let Some(socket) = &handler.socket {
                // Unix socket path (Linux/macOS only; falls back with error on Windows).
                send_fastcgi_unix(socket, params, body, timeout_secs).await
            } else if let Some(addr) = &handler.address {
                // TCP FastCGI.
                send_fastcgi_request(addr, params, body, timeout_secs).await
            } else {
                Err(PhpError::Protocol(
                    "PHP handler has neither socket nor address".into(),
                ))
            }
        }
        PhpMode::Cgi | PhpMode::Cli => Err(PhpError::Protocol(
            "CGI/CLI mode not yet implemented in this version".into(),
        )),
    }
}
