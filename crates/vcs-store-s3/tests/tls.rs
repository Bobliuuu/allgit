//! HTTPS endpoint tests against an in-process rustls server (feature `tls`).
#![cfg(feature = "tls")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use vcs_remote::{Backend, BackendError};
use vcs_store_s3::{S3Backend, S3Config};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Serves `conns` TLS connections, answering every request with 200 and an ETag, and sends
/// each request head back on the channel.
fn serve(cert_der: Vec<u8>, key_der: Vec<u8>, conns: usize) -> (u16, std::sync::mpsc::Receiver<String>) {
    let cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert_der)],
            rustls::pki_types::PrivateKeyDer::try_from(key_der).unwrap(),
        )
        .unwrap();
    let cfg = Arc::new(cfg);
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..conns {
            let Ok((mut tcp, _)) = l.accept() else { return };
            let mut conn = rustls::ServerConnection::new(cfg.clone()).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
            let mut buf = vec![0u8; 8192];
            let Ok(n) = tls.read(&mut buf) else { continue }; // handshake failure
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nETag: \"abc\"\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = tls.flush();
        }
    });
    (port, rx)
}

fn cert() -> (String, Vec<u8>, Vec<u8>) {
    let c = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    (c.cert.pem(), c.cert.der().to_vec(), c.key_pair.serialize_der())
}

#[test]
fn https_with_trusted_ca_and_session_token() {
    let (pem, cert_der, key_der) = cert();
    let (port, rx) = serve(cert_der, key_der, 1);
    let mut cfg = S3Config::new(&format!("https://localhost:{port}"), "bkt", "AK", "SK");
    cfg.ca_pem = Some(pem.into_bytes());
    cfg.session_token = Some("SESSION".into());
    let b = S3Backend::new_unchecked(cfg).unwrap();
    let got = b.get("k").unwrap().unwrap();
    assert_eq!(got.data, b"hi");
    let head = rx.recv().unwrap().to_ascii_lowercase();
    assert!(head.contains("x-amz-security-token: session"), "{head}");
    assert!(head.contains("x-amz-security-token") && head.contains("authorization:"));
}

#[test]
fn https_with_untrusted_cert_is_an_error() {
    let (_, cert_der, key_der) = cert();
    let (other_pem, _, _) = cert(); // a different CA than the server's
    let (port, _rx) = serve(cert_der, key_der, 1);
    let mut cfg = S3Config::new(&format!("https://localhost:{port}"), "bkt", "AK", "SK");
    cfg.ca_pem = Some(other_pem.into_bytes());
    let b = S3Backend::new_unchecked(cfg).unwrap();
    let e = b.get("k").unwrap_err();
    assert!(matches!(e, BackendError::Unavailable(_)), "{e:?}");
}

#[test]
fn https_with_default_roots_rejects_self_signed() {
    let (_, cert_der, key_der) = cert();
    let (port, _rx) = serve(cert_der, key_der, 1);
    let b = S3Backend::new_unchecked(S3Config::new(&format!("https://localhost:{port}"), "bkt", "AK", "SK")).unwrap();
    assert!(b.get("k").is_err());
}

#[test]
fn garbage_ca_pem_is_rejected() {
    let mut cfg = S3Config::new("https://localhost:1", "b", "a", "s");
    cfg.ca_pem = Some(b"not a cert".to_vec());
    assert!(S3Backend::new_unchecked(cfg).is_err());
}
