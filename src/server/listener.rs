use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};

use super::overload::{ConnectionLimiter, Refused};
use super::shutdown::{wait_for_ctrl_c, ShutdownTrigger};
use super::worker::WorkerPool;
use crate::config::ServerConfig;

pub struct FlashLinServer {
    config: Arc<ServerConfig>,
    live: super::reload::LiveConfig,
    watch_path: Option<String>,
}

impl FlashLinServer {
    pub fn new(config: Arc<ServerConfig>, watch_path: Option<String>) -> anyhow::Result<Self> {
        let live = Arc::new(arc_swap::ArcSwap::from_pointee(
            super::reload::Snapshot::new(config.clone())?,
        ));
        // Listener settings must not retain an obsolete generation's populated caches.
        let mut startup = (*config).clone();
        startup.state = Default::default();
        Ok(FlashLinServer {
            config: Arc::new(startup),
            live,
            watch_path,
        })
    }

    /// Starts the configured `[[apps]]`, serves until shutdown, then stops
    /// the apps — after connections have drained, so in-flight proxied
    /// requests can still reach them.
    pub async fn run(self) -> anyhow::Result<()> {
        let apps = crate::apps::AppManager::start(&self.config.apps);
        apps.register();
        let result = self.serve().await;
        apps.stop().await;
        result
    }

    async fn serve(self) -> anyhow::Result<()> {
        if let Some(path) = self.watch_path.clone() {
            tokio::spawn(super::reload::watch(self.live.clone(), path));
            info!("Hot reload enabled; checking configuration every 2 seconds");
        }
        let trigger = Arc::new(ShutdownTrigger::new());
        let limiter = ConnectionLimiter::with_per_client(
            self.config.server.max_connections,
            self.config.server.max_connections_per_ip,
        );
        let max = self.config.server.max_connections;
        let draining = limiter.clone();

        if self.config.tls.as_ref().map(|t| t.enabled).unwrap_or(false) {
            return self.run_with_tls(trigger, limiter, max).await;
        }

        let addr = format!("{}:{}", self.config.server.host, self.config.server.port);
        let listener = build_listener(&addr)?;
        let pool = WorkerPool::new(Arc::clone(&self.live));

        info!(
            server_name = %self.config.server.server_name,
            address = %addr,
            max_connections = if max == 0 { "unlimited".to_string() } else { max.to_string() },
            "FlashLin HTTP listening"
        );

        tokio::select! {
            result = accept_http_loop(
                listener,
                pool,
                limiter,
                Arc::clone(&trigger),
                self.config.server.server_name.clone(),
                "http",
            ) => result?,
            _ = wait_for_ctrl_c() => {
                info!("Shutdown signal received - stopping gracefully");
                trigger.fire();
            }
        }

        trigger.fire();
        drain_connections(&draining).await;
        info!("FlashLin shut down cleanly");
        Ok(())
    }

    #[cfg(feature = "tls")]
    async fn run_with_tls(
        self,
        trigger: Arc<ShutdownTrigger>,
        limiter: ConnectionLimiter,
        max: usize,
    ) -> anyhow::Result<()> {
        let draining = limiter.clone();
        let tls = self
            .config
            .tls
            .as_ref()
            .expect("TLS config checked before run_with_tls");
        let https_addr = format!("{}:{}", self.config.server.host, self.config.server.port);
        let https_listener = build_listener(&https_addr)?;
        let http_addr = format!("{}:{}", self.config.server.host, tls.http_port);
        let http_listener = if tls.http_port != self.config.server.port {
            Some(build_listener(&http_addr)?)
        } else {
            None
        };
        // Bind all required ports before starting CA activity.
        #[cfg(feature = "http3")]
        let quic_socket = self
            .config
            .http3
            .as_ref()
            .filter(|h| h.enabled)
            .map(|h| {
                std::net::UdpSocket::bind((
                    self.config.server.host.as_str(),
                    h.port.unwrap_or(self.config.server.port),
                ))
            })
            .transpose()?;
        let acceptor = crate::tls::managed::ManagedTls::new(tls, trigger.subscribe())?;
        #[cfg(feature = "http3")]
        let quic_endpoint = quic_socket
            .map(|s| super::http3::endpoint(s, self.live.clone(), acceptor.clone()))
            .transpose()?;
        #[cfg(feature = "http3")]
        let quic_task_endpoint = quic_endpoint.clone();
        let https_pool = WorkerPool::new(Arc::clone(&self.live));

        info!(
            server_name = %self.config.server.server_name,
            address = %https_addr,
            max_connections = if max == 0 { "unlimited".to_string() } else { max.to_string() },
            "FlashLin HTTPS listening"
        );

        let https_task = accept_https_loop(
            https_listener,
            https_pool,
            limiter.clone(),
            Arc::clone(&trigger),
            acceptor,
        );
        #[cfg(feature = "http3")]
        let https_task = async {
            if let Some(endpoint) = quic_task_endpoint {
                info!(address = %endpoint.local_addr()?, "FlashLin HTTP/3 listening (UDP / QUIC)");
                tokio::try_join!(
                    https_task,
                    super::http3::accept_loop(
                        endpoint,
                        self.live.clone(),
                        limiter.clone(),
                        trigger.clone()
                    )
                )?;
                Ok::<(), anyhow::Error>(())
            } else {
                https_task.await
            }
        };

        if let Some(http_listener) = http_listener {
            info!(
                server_name = %self.config.server.server_name,
                address = %http_addr,
                redirect = tls.redirect_http_to_https,
                "FlashLin HTTP companion listener"
            );

            if tls.redirect_http_to_https {
                tokio::select! {
                    result = https_task => result?,
                    result = accept_redirect_loop(http_listener, limiter.clone(), Arc::clone(&trigger)) => result?,
                    _ = wait_for_ctrl_c() => {
                        info!("Shutdown signal received - stopping gracefully");
                        trigger.fire();
                    }
                }
            } else {
                let http_pool = WorkerPool::new(Arc::clone(&self.live));
                tokio::select! {
                    result = https_task => result?,
                    result = accept_http_loop(
                        http_listener,
                        http_pool,
                        limiter.clone(),
                        Arc::clone(&trigger),
                        self.config.server.server_name.clone(),
                        "http",
                    ) => result?,
                    _ = wait_for_ctrl_c() => {
                        info!("Shutdown signal received - stopping gracefully");
                        trigger.fire();
                    }
                }
            }
        } else {
            tokio::select! {
                result = https_task => result?,
                _ = wait_for_ctrl_c() => {
                    info!("Shutdown signal received - stopping gracefully");
                    trigger.fire();
                }
            }
        }

        #[cfg(feature = "http3")]
        if let Some(endpoint) = &quic_endpoint {
            endpoint.set_server_config(None);
        }
        trigger.fire();
        drain_connections(&draining).await;
        #[cfg(feature = "http3")]
        if let Some(endpoint) = quic_endpoint {
            endpoint.close(0_u32.into(), b"Server shutdown");
        }
        info!("FlashLin shut down cleanly");
        Ok(())
    }

    #[cfg(not(feature = "tls"))]
    async fn run_with_tls(
        self,
        _trigger: Arc<ShutdownTrigger>,
        _limiter: ConnectionLimiter,
        _max: usize,
    ) -> anyhow::Result<()> {
        anyhow::bail!(
            "TLS is enabled in config, but this FlashLin binary was built without --features tls"
        )
    }
}

async fn accept_http_loop(
    listener: TcpListener,
    pool: WorkerPool,
    limiter: ConnectionLimiter,
    trigger: Arc<ShutdownTrigger>,
    server_name: String,
    label: &'static str,
) -> anyhow::Result<()> {
    let mut shutdown = trigger.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, remote_addr)) => {
                        if let Err(error) = stream.set_nodelay(true) {
                            warn!(%error, "Unable to enable TCP_NODELAY");
                        }
                        match limiter.try_acquire(remote_addr.ip()) {
                            Ok(slot) => {
                                pool.spawn_http(stream, remote_addr, slot, trigger.subscribe());
                            }
                            Err(Refused::Client) => {
                                tracing::debug!(client = %remote_addr, listener = label, "Per-client connection limit reached - closing");
                            }
                            Err(Refused::Server) => {
                                let active = limiter.active();
                                let cap = limiter.max();
                                warn!(
                                    active,
                                    max = cap,
                                    client = %remote_addr,
                                    listener = label,
                                    "Connection limit reached - sending 111"
                                );
                                let server_name = server_name.clone();
                                tokio::spawn(async move {
                                    let _ = send_111(stream, &server_name, active, cap).await;
                                });
                            }
                        }
                    }
                    Err(e) => {
                        error!(listener = label, "Accept error: {}", e);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            _ = shutdown.wait() => break,
        }
    }

    Ok(())
}

#[cfg(feature = "tls")]
async fn accept_https_loop(
    listener: TcpListener,
    pool: WorkerPool,
    limiter: ConnectionLimiter,
    trigger: Arc<ShutdownTrigger>,
    acceptor: crate::tls::managed::ManagedTls,
) -> anyhow::Result<()> {
    let mut shutdown = trigger.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, remote_addr)) => {
                        if let Err(error) = stream.set_nodelay(true) {
                            warn!(%error, "Unable to enable TCP_NODELAY");
                        }
                        match limiter.try_acquire(remote_addr.ip()) {
                            Ok(slot) => {
                                let acceptor = acceptor.clone();
                                let config = Arc::clone(&pool.config);
                                let shutdown = trigger.subscribe();
                                tokio::spawn(async move {
                                    match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream, &config)).await {
                                        Ok(Ok(Some(tls_stream))) => {
                                            let mut worker = WorkerPool::new(config);
                                            worker.tls = true;
                                            worker.spawn(tls_stream, remote_addr, slot, shutdown);
                                        }
                                        Ok(Ok(None)) => { drop(slot); }
                                        error => {
                                            warn!(client = %remote_addr, ?error, "TLS handshake failed or timed out");
                                            drop(slot);
                                        }
                                    }
                                });
                            }
                            Err(Refused::Client) => {
                                tracing::debug!(client = %remote_addr, "Per-client connection limit reached on HTTPS listener - closing");
                            }
                            Err(Refused::Server) => {
                                warn!(
                                    active = limiter.active(),
                                    max = limiter.max(),
                                    client = %remote_addr,
                                    "Connection limit reached on HTTPS listener"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!(listener = "https", "Accept error: {}", e);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            _ = shutdown.wait() => break,
        }
    }

    Ok(())
}

async fn accept_redirect_loop(
    listener: TcpListener,
    limiter: ConnectionLimiter,
    trigger: Arc<ShutdownTrigger>,
) -> anyhow::Result<()> {
    let mut shutdown = trigger.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, remote_addr)) => {
                        if let Err(error) = stream.set_nodelay(true) {
                            warn!(%error, "Unable to enable TCP_NODELAY");
                        }
                        if let Ok(slot) = limiter.try_acquire(remote_addr.ip()) {
                            tokio::spawn(async move {
                                let _ = send_https_redirect(stream).await;
                                drop(slot);
                            });
                        } else {
                            warn!(
                                active = limiter.active(),
                                max = limiter.max(),
                                client = %remote_addr,
                                "Connection limit reached on HTTP redirect listener"
                            );
                        }
                    }
                    Err(e) => {
                        error!(listener = "http_redirect", "Accept error: {}", e);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
            _ = shutdown.wait() => break,
        }
    }

    Ok(())
}

/// Write a raw HTTP/1.1 111 response and close the socket.
async fn send_111(
    mut stream: TcpStream,
    server_name: &str,
    active: usize,
    max: usize,
) -> std::io::Result<()> {
    let _ = (active, max);
    let body = crate::error_pages::page_503(server_name);
    let header = format!(
        "HTTP/1.1 503 Service Unavailable\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Retry-After: 10\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn send_https_redirect(mut stream: TcpStream) -> std::io::Result<()> {
    let mut buffer = [0_u8; 2048];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .unwrap_or(Ok(0))?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let mut lines = request.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let host = request
        .lines()
        .find_map(|line| {
            line.strip_prefix("Host:")
                .or_else(|| line.strip_prefix("host:"))
        })
        .map(str::trim)
        .map(|host| host.split(':').next().unwrap_or(host))
        .unwrap_or("");
    let location = if host.is_empty() {
        format!("https://{}", path)
    } else {
        format!("https://{}{}", host, path)
    };
    let response = format!(
        "HTTP/1.1 301 Moved Permanently\r\n\
         Location: {}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n",
        location
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn build_listener(addr: &str) -> anyhow::Result<TcpListener> {
    let socket_addr: std::net::SocketAddr = addr.parse()?;
    let domain = if socket_addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    // SO_REUSEPORT: kernel-level load balancing — best-effort, non-fatal.
    // Uses the same platform gate as socket2 internally.
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    if let Err(e) = socket.set_reuse_port(true) {
        tracing::warn!("SO_REUSEPORT unavailable: {e}");
    }
    socket.set_nonblocking(true)?;
    socket.bind(&socket_addr.into())?;
    // Large accept backlog prevents drops under connection bursts.
    socket.listen(65535)?;
    let std_listener: std::net::TcpListener = socket.into();
    Ok(TcpListener::from_std(std_listener)?)
}

async fn drain_connections(limiter: &ConnectionLimiter) {
    if tokio::time::timeout(Duration::from_secs(30), async {
        while limiter.active() > 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .is_err()
    {
        warn!(active = limiter.active(), "Shutdown drain deadline reached");
    }
}
