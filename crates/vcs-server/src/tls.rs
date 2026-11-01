//! rustls configuration helpers (feature `tls`). Certificate verification on the client is never disabled.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;

/// Server configuration from a PEM certificate chain and a PEM private key (PKCS#8, PKCS#1 or SEC1).
pub fn server_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("certificate file contains no certificates".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid private key PEM: {e}"))?;
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate/key rejected: {e}"))?;
    Ok(Arc::new(cfg))
}

/// Client configuration trusting exactly the certificates in `ca_pem` (instead of the built-in web roots).
pub fn client_config(ca_pem: &[u8]) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    let mut n = 0;
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
        let cert = cert.map_err(|e| format!("invalid CA PEM: {e}"))?;
        roots.add(cert).map_err(|e| format!("invalid CA certificate: {e}"))?;
        n += 1;
    }
    if n == 0 {
        return Err("CA bundle contains no certificates".into());
    }
    Ok(Arc::new(
        rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth(),
    ))
}
