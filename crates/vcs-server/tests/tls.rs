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
            role: Role::Admin,
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
    assert_eq!(b.whoami().unwrap(), "alice admin *");
    conformance(&b); // admin: the conformance suite deletes
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

// ---- mutual TLS and certificate reload ----

use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};

struct Ca {
    cert: rcgen::Certificate,
    key: KeyPair,
}
fn make_ca(name: &str) -> Ca {
    let mut p = CertificateParams::new(vec![]).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, name);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().unwrap();
    Ca {
        cert: p.self_signed(&key).unwrap(),
        key,
    }
}
fn leaf(ca: &Ca, names: &[&str], client: bool) -> Cert {
    let mut p = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    p.extended_key_usages = vec![if client {
        ExtendedKeyUsagePurpose::ClientAuth
    } else {
        ExtendedKeyUsagePurpose::ServerAuth
    }];
    let key = KeyPair::generate().unwrap();
    let c = p.signed_by(&key, &ca.cert, &ca.key).unwrap();
    Cert {
        cert_pem: c.pem(),
        key_pem: key.serialize_pem(),
    }
}

fn start_cfg(cfg_tls: Arc<rustls::ServerConfig>) -> (Server, u16) {
    let mut cfg = Config::new(TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]));
    cfg.tls = Some(cfg_tls);
    cfg.header_timeout = Duration::from_secs(2);
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let port = srv.local_addr().port();
    (srv, port)
}

#[test]
fn mtls_requires_a_client_certificate_from_the_configured_ca() {
    let ca = make_ca("test ca");
    let server = leaf(&ca, &["localhost"], false);
    let client = leaf(&ca, &["alice"], true);
    let other_ca = make_ca("other ca");
    let rogue = leaf(&other_ca, &["alice"], true);
    let ca_pem = ca.cert.pem();
    let (_s, port) =
        start_cfg(vcs_server::tls::server_config_mtls(server.cert_pem.as_bytes(), server.key_pem.as_bytes(), ca_pem.as_bytes()).unwrap());
    let url = format!("https://localhost:{port}");
    // with a certificate signed by the client CA: works, and the bearer token is still checked
    let b = HttpBackend::with_tls(
        &url,
        "t-alice",
        Some(ca_pem.as_bytes()),
        Some((client.cert_pem.as_bytes(), client.key_pem.as_bytes())),
    )
    .unwrap();
    assert_eq!(b.whoami().unwrap(), "alice write *");
    let bad = HttpBackend::with_tls(
        &url,
        "wrong-token",
        Some(ca_pem.as_bytes()),
        Some((client.cert_pem.as_bytes(), client.key_pem.as_bytes())),
    )
    .unwrap();
    assert!(bad.whoami().unwrap_err().to_string().contains("401"));
    // no client certificate: the handshake is refused even with a valid token
    let none = HttpBackend::with_ca(&url, "t-alice", Some(ca_pem.as_bytes())).unwrap();
    assert!(none.whoami().is_err());
    // a certificate from another CA is refused
    let r = HttpBackend::with_tls(
        &url,
        "t-alice",
        Some(ca_pem.as_bytes()),
        Some((rogue.cert_pem.as_bytes(), rogue.key_pem.as_bytes())),
    )
    .unwrap();
    assert!(r.whoami().is_err());
    // a client certificate without a CA bundle is a configuration error, not silently web roots
    assert!(HttpBackend::with_tls(&url, "t", None, Some((client.cert_pem.as_bytes(), client.key_pem.as_bytes()))).is_err());
}

#[test]
fn reloaded_certificate_serves_new_connections() {
    let ca1 = make_ca("ca one");
    let ca2 = make_ca("ca two");
    let s1 = leaf(&ca1, &["localhost"], false);
    let s2 = leaf(&ca2, &["localhost"], false);
    let (srv, port) = start_cfg(server_config(s1.cert_pem.as_bytes(), s1.key_pem.as_bytes()).unwrap());
    let url = format!("https://localhost:{port}");
    let (pem1, pem2) = (ca1.cert.pem(), ca2.cert.pem());
    let c1 = || HttpBackend::with_ca(&url, "t-alice", Some(pem1.as_bytes())).unwrap();
    let c2 = || HttpBackend::with_ca(&url, "t-alice", Some(pem2.as_bytes())).unwrap();
    assert!(c1().whoami().is_ok());
    assert!(c2().whoami().is_err());
    srv.reload_tls(server_config(s2.cert_pem.as_bytes(), s2.key_pem.as_bytes()).unwrap());
    assert!(c2().whoami().is_ok(), "new certificate is served after reload");
    assert!(c1().whoami().is_err(), "old certificate is gone");
}

// ---- certificate revocation and subject-to-principal mapping ----

fn ca_with_crl_sign(name: &str) -> Ca {
    let mut p = CertificateParams::new(vec![]).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, name);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
    let key = KeyPair::generate().unwrap();
    Ca {
        cert: p.self_signed(&key).unwrap(),
        key,
    }
}

/// A client leaf with subject CN `cn` and serial `serial`.
fn client_leaf(ca: &Ca, cn: &str, serial: u64) -> Cert {
    let mut p = CertificateParams::new(vec![]).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, cn);
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    p.serial_number = Some(rcgen::SerialNumber::from(serial));
    let key = KeyPair::generate().unwrap();
    let c = p.signed_by(&key, &ca.cert, &ca.key).unwrap();
    Cert {
        cert_pem: c.pem(),
        key_pem: key.serialize_pem(),
    }
}

fn crl_pem(ca: &Ca, revoked: &[u64]) -> String {
    let now = time::OffsetDateTime::now_utc();
    let params = rcgen::CertificateRevocationListParams {
        this_update: now - time::Duration::hours(1),
        next_update: now + time::Duration::days(30),
        crl_number: rcgen::SerialNumber::from(1u64),
        issuing_distribution_point: None,
        revoked_certs: revoked
            .iter()
            .map(|s| rcgen::RevokedCertParams {
                serial_number: rcgen::SerialNumber::from(*s),
                revocation_time: now - time::Duration::minutes(5),
                reason_code: Some(rcgen::RevocationReason::KeyCompromise),
                invalidity_date: None,
            })
            .collect(),
        key_identifier_method: rcgen::KeyIdMethod::Sha256,
    };
    params.signed_by(&ca.cert, &ca.key).unwrap().pem().unwrap()
}

#[test]
fn revoked_client_certificate_is_refused_and_a_reload_applies_a_new_crl() {
    let ca = ca_with_crl_sign("crl ca");
    let server = leaf(&ca, &["localhost"], false);
    let good = client_leaf(&ca, "alice", 1001);
    let bad = client_leaf(&ca, "alice", 1002);
    let ca_pem = ca.cert.pem();
    let build = |crl: &str| {
        vcs_server::tls::server_config_mtls_crl(
            server.cert_pem.as_bytes(),
            server.key_pem.as_bytes(),
            ca_pem.as_bytes(),
            crl.as_bytes(),
        )
        .unwrap()
    };
    let (srv, port) = start_cfg(build(&crl_pem(&ca, &[1002])));
    let url = format!("https://localhost:{port}");
    let with = |c: &Cert| {
        HttpBackend::with_tls(
            &url,
            "t-alice",
            Some(ca_pem.as_bytes()),
            Some((c.cert_pem.as_bytes(), c.key_pem.as_bytes())),
        )
        .unwrap()
    };
    assert_eq!(with(&good).whoami().unwrap(), "alice write *");
    assert!(with(&bad).whoami().is_err(), "revoked certificate must not connect");
    // SIGHUP path: a CRL that also lists the good certificate takes effect for new connections
    srv.reload_tls(build(&crl_pem(&ca, &[1001, 1002])));
    assert!(with(&good).whoami().is_err());
    // junk in the CRL file is a configuration error, not "no revocation"
    assert!(vcs_server::tls::server_config_mtls_crl(
        server.cert_pem.as_bytes(),
        server.key_pem.as_bytes(),
        ca_pem.as_bytes(),
        b"not a crl"
    )
    .is_err());
}

#[test]
fn client_certificate_subject_must_map_to_the_token_principal() {
    let ca = make_ca("map ca");
    let server = leaf(&ca, &["localhost"], false);
    let alice = client_leaf(&ca, "alice-laptop", 7);
    let mallory = client_leaf(&ca, "mallory-laptop", 8);
    let nameless = {
        // a certificate with two CNs is ambiguous and never maps
        let mut p = CertificateParams::new(vec![]).unwrap();
        p.distinguished_name.push(rcgen::DnType::CommonName, "alice-laptop");
        p.distinguished_name.push(rcgen::DnType::CommonName, "other");
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let key = KeyPair::generate().unwrap();
        let c = p.signed_by(&key, &ca.cert, &ca.key).unwrap();
        Cert {
            cert_pem: c.pem(),
            key_pem: key.serialize_pem(),
        }
    };
    let ca_pem = ca.cert.pem();
    let mut cfg = Config::new(TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]));
    cfg.tls = Some(vcs_server::tls::server_config_mtls(server.cert_pem.as_bytes(), server.key_pem.as_bytes(), ca_pem.as_bytes()).unwrap());
    cfg.client_cert_map = Some(Arc::new(
        vcs_server::certmap::ClientCertMap::parse("alice-laptop alice\nmallory-laptop mallory\n").unwrap(),
    ));
    cfg.header_timeout = Duration::from_secs(2);
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url = format!("https://localhost:{}", srv.local_addr().port());
    let with = |c: &Cert| {
        HttpBackend::with_tls(
            &url,
            "t-alice",
            Some(ca_pem.as_bytes()),
            Some((c.cert_pem.as_bytes(), c.key_pem.as_bytes())),
        )
        .unwrap()
    };
    assert_eq!(with(&alice).whoami().unwrap(), "alice write *");
    // mallory's certificate is valid and mapped, but not to the principal that owns this token
    let e = with(&mallory).whoami().unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
    let e = with(&nameless).whoami().unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
}
