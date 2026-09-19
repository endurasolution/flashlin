//! Validated immutable generations: in-flight requests retain the old generation.
use crate::config::{self, ServerConfig};
use arc_swap::ArcSwap;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub struct Snapshot {
    pub config: Arc<ServerConfig>,
    #[cfg(feature = "tls")]
    pub acceptor: Option<tokio_rustls::TlsAcceptor>,
    fingerprint: Vec<u8>,
}
pub type LiveConfig = Arc<ArcSwap<Snapshot>>;

impl Snapshot {
    pub fn new(config: Arc<ServerConfig>) -> anyhow::Result<Self> {
        let fingerprint = fingerprint(&config)?;
        #[cfg(feature = "tls")]
        let acceptor = config
            .tls
            .as_ref()
            .filter(|t| t.enabled && t.acme.is_none())
            .map(crate::tls::build_tls_config)
            .transpose()?;
        anyhow::ensure!(
            fingerprint == self::fingerprint(&config)?,
            "Certificate files changed during reload; retrying"
        );
        Ok(Self {
            config,
            fingerprint,
            #[cfg(feature = "tls")]
            acceptor,
        })
    }
}

pub fn load_config(path: &str) -> anyhow::Result<ServerConfig> {
    let mut cfg = config::load(path)?;
    if let Some(dir) = &cfg.server.domains_dir {
        cfg.virtual_hosts = config::load_domains(
            &crate::resolve_config_relative_path(path, dir).to_string_lossy(),
        )?;
    }
    crate::promote_domain_tls(&mut cfg);
    config::validate(&cfg)?;
    let mut names = std::collections::HashSet::new();
    for host in &cfg.virtual_hosts {
        for name in std::iter::once(&host.domain.name).chain(&host.domain.aliases) {
            anyhow::ensure!(
                !name.is_empty() && names.insert(name.to_ascii_lowercase()),
                "Empty or duplicate virtual hostname: {name}"
            );
        }
        let merged = crate::http::router::merge_vhost(&cfg, host);
        config::validate(&merged)?;
        #[cfg(feature = "acme")]
        if let Some(acme) = cfg.tls.as_ref().and_then(|t| t.acme.as_ref()) {
            config::validation::validate_acme_cache(&merged, &acme.cache_dir)?;
        }
    }
    #[cfg(not(feature = "tls"))]
    anyhow::ensure!(
        !cfg.tls.as_ref().is_some_and(|t| t.enabled),
        "TLS requires --features tls"
    );
    #[cfg(feature = "tls")]
    if let Some(tls) = cfg.tls.as_ref().filter(|t| t.enabled && t.acme.is_none()) {
        crate::tls::build_tls_config(tls)?;
    }
    Ok(cfg)
}

fn fingerprint(cfg: &ServerConfig) -> anyhow::Result<Vec<u8>> {
    let mut hash = Sha256::new();
    hash.update(toml::to_string(cfg)?);
    for host in &cfg.virtual_hosts {
        hash.update(toml::to_string(host)?);
    }
    if let Some(tls) = cfg.tls.as_ref().filter(|t| t.enabled && t.acme.is_none()) {
        for path in [&tls.cert, &tls.key].into_iter().flatten() {
            hash.update(std::fs::read(path)?);
        }
    }
    Ok(hash.finalize().to_vec())
}

fn restart_key(cfg: &ServerConfig) -> String {
    let t = cfg.tls.as_ref().filter(|t| t.enabled);
    format!(
        "{:?}",
        (
            &cfg.server.host,
            cfg.server.port,
            cfg.server.workers,
            cfg.server.max_connections,
            &cfg.http3,
            cfg.logging.as_ref().map(|l| toml::to_string(l).unwrap()),
            t.map(|t| (t.http_port, t.redirect_http_to_https, &t.acme))
        )
    )
}

pub fn reload_once(live: &LiveConfig, path: &str) -> anyhow::Result<bool> {
    let current = live.load_full();
    let config = load_config(path)?;
    anyhow::ensure!(restart_key(&current.config) == restart_key(&config),
        "Listener, worker, connection-limit, logging or ACME changes require a restart; keeping previous configuration");
    let next = Snapshot::new(Arc::new(config))?;
    if next.fingerprint == current.fingerprint {
        return Ok(false);
    }
    live.store(Arc::new(next));
    Ok(true)
}

pub async fn watch(live: LiveConfig, path: String) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut last_error = String::new();
    loop {
        interval.tick().await;
        let state = live.clone();
        let file = path.clone();
        match tokio::task::spawn_blocking(move || reload_once(&state, &file)).await {
            Ok(Ok(changed)) => {
                if changed {
                    tracing::info!("Configuration and manual certificates reloaded successfully");
                }
                last_error.clear();
            }
            result => {
                let error = format!("{result:?}");
                if error != last_error {
                    tracing::warn!(%error, "Reload rejected; previous configuration remains active");
                    last_error = error;
                }
            }
        }
    }
}
