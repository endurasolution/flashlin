#![cfg(feature = "tls")]
use std::sync::Arc;

use rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

use super::cert::{load_certs, load_private_key};
use crate::config::TlsConfig;
use crate::error::{FlashError, Result};

/// Build a rustls ServerConfig from the FlashLin TLS configuration.
pub fn build_tls_config(tls: &TlsConfig) -> Result<TlsAcceptor> {
    let cert_path = tls
        .cert
        .as_deref()
        .ok_or_else(|| FlashError::Tls("tls.cert is required".into()))?;
    let key_path = tls
        .key
        .as_deref()
        .ok_or_else(|| FlashError::Tls("tls.key is required".into()))?;

    let certs = load_certs(cert_path)?;
    let key = load_private_key(key_path)?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| FlashError::Tls(format!("TLS config error: {e}")))?;

    // Advertise HTTP/2 (h2) and HTTP/1.1 via ALPN so the hyper auto-builder
    // can negotiate the right protocol for each connection.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(config)))
}
