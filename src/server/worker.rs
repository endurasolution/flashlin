use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::error;

use super::connection::handle_connection;
use super::overload::ConnectionSlot;
use super::shutdown::ShutdownSignal;

pub struct WorkerPool {
    pub(crate) config: super::reload::LiveConfig,
    pub(crate) tls: bool,
}

impl WorkerPool {
    pub fn new(config: super::reload::LiveConfig) -> Self {
        WorkerPool { config, tls: false }
    }

    /// Spawn an async task to handle a single accepted connection.
    /// `slot` is held for the connection's lifetime and released on drop.
    pub fn spawn(
        &self,
        stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
        remote_addr: std::net::SocketAddr,
        slot: ConnectionSlot,
        shutdown: ShutdownSignal,
    ) {
        let config = Arc::clone(&self.config);
        let tls = self.tls;
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, remote_addr, config, shutdown, tls).await {
                if !is_benign_io_error(&e.to_string()) {
                    error!("Connection error from {}: {}", remote_addr, e);
                }
            }
            drop(slot); // explicit: slot lives until here
        });
    }

    /// Spawn a plain (non-TLS) TCP connection, letting the sendfile fast
    /// path in `server::fastpath` decide per-connection whether it can
    /// accelerate it; it falls back to the same handler `spawn` uses for
    /// anything outside its narrow scope. TLS connections always use `spawn`
    /// directly since the fast path never applies to them.
    pub fn spawn_http(
        &self,
        stream: tokio::net::TcpStream,
        remote_addr: std::net::SocketAddr,
        slot: ConnectionSlot,
        shutdown: ShutdownSignal,
    ) {
        let config = Arc::clone(&self.config);
        tokio::spawn(async move {
            if let Err(e) =
                super::fastpath::handle_plain_connection(stream, remote_addr, config, shutdown)
                    .await
            {
                if !is_benign_io_error(&e.to_string()) {
                    error!("Connection error from {}: {}", remote_addr, e);
                }
            }
            drop(slot); // explicit: slot lives until here
        });
    }
}

fn is_benign_io_error(msg: &str) -> bool {
    msg.contains("connection reset")
        || msg.contains("broken pipe")
        || msg.contains("connection aborted")
        || msg.contains("os error 10054") // Windows: connection reset
        || msg.contains("os error 32") // Unix: broken pipe
}
