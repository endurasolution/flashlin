use crate::{
    config::TlsConfig,
    server::{reload::LiveConfig, shutdown::ShutdownSignal},
};
#[cfg(feature = "acme")]
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;

#[derive(Clone)]
pub enum ManagedTls {
    Manual,
    #[cfg(feature = "acme")]
    Automatic {
        normal: Arc<rustls::ServerConfig>,
        challenge: Arc<rustls::ServerConfig>,
    },
}

impl ManagedTls {
    pub fn new(_tls: &TlsConfig, _shutdown: ShutdownSignal) -> anyhow::Result<Self> {
        #[cfg(feature = "acme")]
        if let Some(acme) = &_tls.acme {
            let mut shutdown = _shutdown;
            use futures_util::StreamExt;
            let directory = std::path::PathBuf::from(&acme.cache_dir);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&directory)?;
                anyhow::ensure!(
                    std::fs::metadata(&directory)?.permissions().mode() & 0o077 == 0,
                    "ACME cache directory must have owner-only permissions (0700)"
                );
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&directory)?;
            let mut state = rustls_acme::AcmeConfig::new(acme.domains.clone())
                .contact([format!("mailto:{}", acme.email)])
                .directory_lets_encrypt(acme.production)
                .cache(rustls_acme::caches::DirCache::new(directory))
                .state();
            let challenge = state.challenge_rustls_config();
            let mut normal = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(state.resolver());
            normal.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
            tracing::info!(
                production = acme.production,
                "Automatic HTTPS enabled; certificate issuance/renewal runs in background"
            );
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        event = state.next() => match event {
                            Some(Ok(event)) => tracing::info!(?event, "ACME certificate lifecycle"),
                            Some(Err(error)) => tracing::warn!(%error, "ACME operation failed; automatic retry scheduled"),
                            None => break,
                        },
                        _ = shutdown.wait() => break,
                    }
                }
            });
            return Ok(Self::Automatic {
                normal: Arc::new(normal),
                challenge,
            });
        }
        Ok(Self::Manual)
    }

    pub async fn accept(
        &self,
        stream: TcpStream,
        live: &LiveConfig,
    ) -> std::io::Result<Option<TlsStream<TcpStream>>> {
        match self {
            Self::Manual => {
                let acceptor = live
                    .load()
                    .acceptor
                    .clone()
                    .ok_or_else(|| std::io::Error::other("Missing manual TLS configuration"))?;
                acceptor.accept(stream).await.map(Some)
            }
            #[cfg(feature = "acme")]
            Self::Automatic { normal, challenge } => {
                let start = tokio_rustls::LazyConfigAcceptor::new(
                    rustls::server::Acceptor::default(),
                    stream,
                )
                .await?;
                let is_challenge = rustls_acme::is_tls_alpn_challenge(&start.client_hello());
                let selected = if is_challenge { challenge } else { normal };
                let stream = start.into_stream(selected.clone()).await?;
                // Validation-only TLS connections must never enter the HTTP router.
                if is_challenge {
                    Ok(None)
                } else {
                    Ok(Some(stream))
                }
            }
        }
    }
}
