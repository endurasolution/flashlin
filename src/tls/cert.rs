#![cfg(feature = "tls")]
use std::fs::File;
use std::io::BufReader;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::{certs, private_key};

use crate::error::{FlashError, Result};

/// Load PEM-encoded certificate chain from a file.
pub fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)
        .map_err(|e| FlashError::Tls(format!("Cannot open cert file '{path}': {e}")))?;
    let mut reader = BufReader::new(file);

    let cert_chain = certs(&mut reader)
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| FlashError::Tls(format!("Failed to parse cert '{path}': {e}")))?;

    if cert_chain.is_empty() {
        return Err(FlashError::Tls(format!(
            "No certificates found in '{path}'"
        )));
    }

    Ok(cert_chain)
}

/// Load a PEM-encoded private key from a file.
pub fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let file = File::open(path)
        .map_err(|e| FlashError::Tls(format!("Cannot open key file '{path}': {e}")))?;
    let mut reader = BufReader::new(file);

    private_key(&mut reader)
        .map_err(|e| FlashError::Tls(format!("Failed to parse key '{path}': {e}")))?
        .ok_or_else(|| FlashError::Tls(format!("No private key found in '{path}'")))
}
