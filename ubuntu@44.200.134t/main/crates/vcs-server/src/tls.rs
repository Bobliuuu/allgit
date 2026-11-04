//! rustls configuration helpers (feature `tls`). Certificate verification on the client is never disabled.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, CertificateRevocationListDer, PrivateKeyDer};
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

/// Like [`server_config`], but every client must present a certificate chaining to a CA in
/// `client_ca_pem` (mutual TLS). The handshake fails without one; bearer-token authentication
/// still applies on top (the certificate only admits the connection, it does not name a principal).
pub fn server_config_mtls(cert_pem: &[u8], key_pem: &[u8], client_ca_pem: &[u8]) -> Result<Arc<rustls::ServerConfig>, String> {
    server_config_mtls_crl(cert_pem, key_pem, client_ca_pem, b"")
}

/// [`server_config_mtls`] plus certificate revocation: `crl_pem` holds one or more PEM CRLs
/// (`X509 CRL`) signed by the client CA. The client's end-entity certificate is checked against
/// them; a certificate whose issuer has no CRL here is refused (fail closed), so list one CRL per
/// issuing CA. An empty `crl_pem` means no revocation checking.
pub fn server_config_mtls_crl(
    cert_pem: &[u8],
    key_pem: &[u8],
    client_ca_pem: &[u8],
    crl_pem: &[u8],
) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("certificate file contains no certificates".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid private key PEM: {e}"))?;
    let roots = roots_from_pem(client_ca_pem)?;
    let crls = CertificateRevocationListDer::pem_slice_iter(crl_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid CRL PEM: {e}"))?;
    if !crl_pem.is_empty() && crls.is_empty() && crl_pem.iter().any(|b| !b.is_ascii_whitespace()) {
        return Err("CRL file contains no `X509 CRL` blocks".into());
    }
    let mut vb = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots));
    if !crls.is_empty() {
        vb = vb.with_crls(crls).only_check_end_entity_revocation();
    }
    let verifier = vb.build().map_err(|e| format!("client CA rejected: {e}"))?;
    let cfg = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate/key rejected: {e}"))?;
    Ok(Arc::new(cfg))
}

fn roots_from_pem(ca_pem: &[u8]) -> Result<rustls::RootCertStore, String> {
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
    Ok(roots)
}

/// Client configuration trusting exactly `ca_pem` and presenting the client certificate chain and
/// key (PEM) for mutual TLS. A CA bundle is required: the built-in web roots are not reachable here.
pub fn client_config_with_identity(ca_pem: &[u8], cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<rustls::ClientConfig>, String> {
    let roots = roots_from_pem(ca_pem)?;
    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid client certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("client certificate file contains no certificates".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid client key PEM: {e}"))?;
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| format!("client certificate/key rejected: {e}"))?;
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
