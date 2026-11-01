//! HTTPS with a locally generated self-signed certificate (feature `tls`).
#![cfg(feature = "tls")]

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;
use vcs_remote::backend::conformance;
use vcs_remote::*;
use vcs_server::tls::server_config;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};

struct Cert {
    cert_pem: String,
    key_pem: String,
}

fn cert(names: &[&str]) -> Cert {
    let c = rcgen::generate_simple_self_signed(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    Cert {
        cert_pem: c.cert.pem(),
        key_pem: c.key_pair.serialize_pem(),
    }
}

fn start(c: &Cert) -> (Server, u16) {
    let mut cfg = Config::new(TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]));
    cfg.tls = Some(server_config(c.cert_pem.as_bytes(), c.key_pem.as_bytes()).unwrap());
    cfg.header_timeout = Duration::from_secs(1);
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let port = srv.local_addr().port();
    (srv, port)
}

#[test]
fn https_roundtrip_with_trusted_ca_passes_conformance() {
    let c = cert(&["localhost"]);
    let (_s, port) = start(&c);
    let b = HttpBackend::with_ca(&format!("https://localhost:{port}"), "t-alice", Some(c.cert_pem.as_bytes())).unwrap();
    assert_eq!(b.whoami().unwrap(), "alice write *");
    conformance(&b);
}

#[test]
fn untrusted_certificate_is_a_hard_error() {
    let c = cert(&["localhost"]);
    let (_s, port) = start(&c);
    let other = cert(&["localhost"]);
    let b = HttpBackend::with_ca(&format!("https://localhost:{port}"), "t-alice", Some(other.cert_pem.as_bytes())).unwrap();
    let e = b.whoami().unwrap_err();
    assert!(matches!(e, BackendError::Unavailable(_)), "{e}");
    // no CA bundle: the built-in web roots do not know this certificate either
    let b = HttpBackend::new(&format!("https://localhost:{port}"), "t-alice").unwrap();
    assert!(b.whoami().is_err());
}

#[test]
fn hostname_mismatch_is_rejected() {
    let c = cert(&["not-localhost.example"]);
    let (_s, port) = start(&c);
    let b = HttpBackend::with_ca(&format!("https://localhost:{port}"), "t-alice", Some(c.cert_pem.as_bytes())).unwrap();
    assert!(b.whoami().is_err(), "certificate for another name must not be accepted");
}

#[test]
fn plain_http_to_tls_port_and_tls_garbage_do_not_hurt_the_server() {
    let c = cert(&["localhost"]);
    let (_s, port) = start(&c);
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = s.write_all(b"GET /healthz HTTP/1.1\r\n\r\n");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    assert!(
        !String::from_utf8_lossy(&out).contains("200 OK"),
        "no plaintext service on the TLS port"
    );
    // a stalled handshake is cut off by the header deadline
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let t0 = std::time::Instant::now();
    let _ = s.write_all(&[0x16, 0x03, 0x01]);
    let _ = s.read_to_end(&mut out);
    assert!(t0.elapsed() < Duration::from_secs(4));
    let b = HttpBackend::with_ca(&format!("https://localhost:{port}"), "t-alice", Some(c.cert_pem.as_bytes())).unwrap();
    assert!(b.whoami().is_ok());
}

#[test]
fn client_config_validation() {
    assert!(HttpBackend::with_ca("https://h:1", "t", Some(b"not pem")).is_err());
    assert!(
        HttpBackend::with_ca("http://h:1", "t", Some(b"x")).is_err(),
        "CA bundle with http:// is a mistake"
    );
    assert!(HttpBackend::new("ftp://h", "t").is_err());
    assert!(server_config(b"junk", b"junk").is_err());
}
