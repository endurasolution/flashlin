use std::net::SocketAddr;
use std::time::Duration;

use tracing::info;

pub fn log_request(
    request_id: &str,
    remote_addr: SocketAddr,
    method: &str,
    path: &str,
    status: u16,
    elapsed: Duration,
    user_agent: &str,
) {
    info!(
        request_id = %request_id,
        ip = %remote_addr,
        method = method,
        path = path,
        status = status,
        ms = elapsed.as_millis(),
        ua = user_agent,
        "access"
    );
}
