//! The `vcs` CLI over `https://` against the real `vcs-server --tls-cert --tls-key`, with a generated
//! CA and leaf certificates (rcgen): clone, publish, fetch, lock, the `--ca-pem` flag and the
//! `remote.cainfo` config key, a wrong CA, certificate reload on SIGHUP and optional mutual TLS.
//! Needs `--features tls` (the server binary is built with it too).
#![cfg(all(feature = "tls", unix))]

use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, SanType};
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

fn server_bin() -> PathBuf {
    let vcs = PathBuf::from(env!("CARGO_BIN_EXE_vcs"));
    let p = vcs.with_file_name("vcs-server");
    // the default `cargo test -p vcs-cli` does not build other packages' binaries, and the one
    // that exists may lack TLS: (re)build it with the feature (a no-op when it is current)
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let st = Command::new(cargo)
        .args(["build", "-p", "vcs-server", "--bin", "vcs-server", "--features", "tls"])
        .status()
        .unwrap();
    assert!(st.success() && p.exists(), "could not build vcs-server with TLS");
    p
}

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
/// (cert pem, key pem). Server leaves are valid for 127.0.0.1.
fn leaf(ca: &Ca, client: bool) -> (String, String) {
    let mut p = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    p.subject_alt_names.push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    p.extended_key_usages = vec![if client {
        ExtendedKeyUsagePurpose::ClientAuth
    } else {
        ExtendedKeyUsagePurpose::ServerAuth
    }];
    let key = KeyPair::generate().unwrap();
    let c = p.signed_by(&key, &ca.cert, &ca.key).unwrap();
    (c.pem(), key.serialize_pem())
}

struct Srv {
    child: Child,
    url: String,
    _stderr: BufReader<std::process::ChildStderr>, // keep the pipe open: a closed stderr must not matter, but should not be needed either
}
impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hash(tok: &str) -> String {
    let o = Command::new(server_bin()).args(["hash-token", tok]).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_owned()
}

fn start(dir: &Path, extra: &[&str]) -> Srv {
    let tokens = dir.join("tokens");
    std::fs::write(
        &tokens,
        format!("alice write * {}\nbob write * {}\n", hash("tok-alice"), hash("tok-bob")),
    )
    .unwrap();
    let mut child = Command::new(server_bin())
        .args(["--root", dir.join("data").to_str().unwrap()])
        .args(["--tokens", tokens.to_str().unwrap(), "--listen", "127.0.0.1:0"])
        .args(extra)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut rd = BufReader::new(child.stderr.take().unwrap());
    rd.read_line(&mut line).unwrap();
    let url = line
        .split_whitespace()
        .find(|w| w.starts_with("https://"))
        .unwrap_or_else(|| panic!("server did not report an https address: {line:?}"))
        .to_owned();
    Srv { child, url, _stderr: rd }
}

fn vcs_env(dir: &Path, who: &str, envs: &[(&str, &str)], args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", who)
        .env("VCS_AUTHOR_EMAIL", format!("{who}@example.com"))
        .env("VCS_PRINCIPAL", who)
        .env("VCS_TOKEN", format!("tok-{who}"))
        .env_remove("VCS_CAINFO")
        .env_remove("VCS_CLIENT_CERT")
        .env_remove("VCS_CLIENT_KEY")
        .env("VCS_CONFIG_HOME", dir.join("no-user-config"));
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn ok_env(dir: &Path, who: &str, envs: &[(&str, &str)], args: &[&str]) -> String {
    let o = vcs_env(dir, who, envs, args);
    assert!(
        o.status.success(),
        "vcs {args:?} as {who} failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail_env(dir: &Path, who: &str, envs: &[(&str, &str)], args: &[&str]) -> String {
    let o = vcs_env(dir, who, envs, args);
    assert!(!o.status.success(), "vcs {args:?} as {who} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

struct Pki {
    ca: Ca,
    ca_pem: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}
fn pki(dir: &Path, name: &str) -> Pki {
    let ca = make_ca(name);
    let (c, k) = leaf(&ca, false);
    let d = dir.join(name.replace(' ', "-"));
    std::fs::create_dir_all(&d).unwrap();
    let out = Pki {
        ca_pem: d.join("ca.pem"),
        cert: d.join("server.pem"),
        key: d.join("server.key"),
        ca,
    };
    std::fs::write(&out.ca_pem, out.ca.cert.pem()).unwrap();
    std::fs::write(&out.cert, c).unwrap();
    std::fs::write(&out.key, k).unwrap();
    out
}

#[test]
fn cli_clone_publish_fetch_lock_over_https_and_wrong_ca_fails() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let good = pki(root, "good ca");
    let other = pki(root, "other ca");
    let srv = start(root, &["--tls-cert", p(&good.cert), "--tls-key", p(&good.key)]);
    assert!(srv.url.starts_with("https://127.0.0.1:"), "{}", srv.url);
    let ca = p(&good.ca_pem);
    let env = [("VCS_CAINFO", ca)];

    let (a, b) = (root.join("a"), root.join("b"));
    ok_env(root, "alice", &[], &["init", "a"]);
    write(&a, "art/hero.bin", "v1\n");
    write(&a, "notes.txt", "n1\n");
    ok_env(&a, "alice", &[], &["add", "."]);
    ok_env(&a, "alice", &[], &["commit", "-m", "init"]);
    ok_env(&a, "alice", &[], &["remote", "set", &srv.url]);

    // the server certificate is verified: without the private CA (web roots only) and with the wrong CA it is refused
    let e = fail_env(&a, "alice", &[], &["publish"]);
    assert!(
        e.contains("ertificate") || e.contains("tls") || e.contains("TLS") || e.contains("unavailable"),
        "{e}"
    );
    let e = fail_env(&a, "alice", &[("VCS_CAINFO", p(&other.ca_pem))], &["publish"]);
    assert!(
        e.contains("ertificate") || e.contains("unavailable") || e.contains("Invalid"),
        "{e}"
    );
    assert!(!e.contains("tok-alice"), "token leaked: {e}");
    assert!(
        std::fs::read_dir(root.join("data")).map(|d| d.count()).unwrap_or(0) == 0 || !root.join("data/manifest").exists(),
        "nothing may be published through an unverified connection"
    );

    // publish and clone (clone using the --ca-pem flag instead of the environment)
    assert!(ok_env(&a, "alice", &env, &["publish"]).contains("published"));
    ok_env(root, "bob", &[], &["--ca-pem", ca, "clone", &srv.url, "b"]);
    assert_eq!(std::fs::read_to_string(b.join("art/hero.bin")).unwrap(), "v1\n");

    // the config key works instead of the env var / flag, and a login over https verifies the token
    ok_env(&b, "bob", &[], &["config", "set", "remote.cainfo", ca]);
    assert!(ok_env(&b, "bob", &[], &["fetch"]).contains("fetched"));
    let who = ok_env(&b, "bob", &[], &["auth", "status", &srv.url]);
    assert!(who.contains("bob"), "{who}");

    // locks across clients
    let out = ok_env(
        &a,
        "alice",
        &env,
        &["lock", "art/hero.bin", "--lease-secs", "3600", "--reason", "repaint"],
    );
    assert!(out.contains("locked art/hero.bin"), "{out}");
    let listing = ok_env(&b, "bob", &[], &["locks"]);
    assert!(listing.contains("art/hero.bin") && listing.contains("alice"), "{listing}");
    write(&b, "art/hero.bin", "bob's\n");
    ok_env(&b, "bob", &[], &["add", "."]);
    ok_env(&b, "bob", &[], &["commit", "-m", "bob"]);
    let e = fail_env(&b, "bob", &[], &["publish"]);
    assert!(e.contains("art/hero.bin") && e.contains("alice"), "{e}");
    write(&a, "art/hero.bin", "v2\n");
    ok_env(&a, "alice", &[], &["add", "."]);
    ok_env(&a, "alice", &[], &["commit", "-m", "v2"]);
    assert!(ok_env(&a, "alice", &env, &["publish"]).contains("published"));
    ok_env(&a, "alice", &env, &["unlock", "art/hero.bin"]);
    assert!(ok_env(&b, "bob", &[], &["fetch"]).contains("fetched"));

    // a wrong CA configured as remote.cainfo fails closed for an existing clone
    ok_env(&b, "bob", &[], &["config", "set", "remote.cainfo", p(&other.ca_pem)]);
    fail_env(&b, "bob", &[], &["fetch"]);
}

#[test]
fn sighup_reloads_the_certificate_without_a_restart() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let one = pki(root, "ca one");
    let two = pki(root, "ca two");
    let srv = start(root, &["--tls-cert", p(&one.cert), "--tls-key", p(&one.key)]);
    ok_env(root, "alice", &[], &["init", "a"]);
    let a = root.join("a");
    write(&a, "f.txt", "x\n");
    ok_env(&a, "alice", &[], &["add", "."]);
    ok_env(&a, "alice", &[], &["commit", "-m", "init"]);
    ok_env(&a, "alice", &[], &["remote", "set", &srv.url]);
    let e1 = [("VCS_CAINFO", p(&one.ca_pem))];
    let e2 = [("VCS_CAINFO", p(&two.ca_pem))];
    ok_env(&a, "alice", &e1, &["publish"]);
    fail_env(&a, "alice", &e2, &["fetch"]);

    // swap the files on disk: nothing changes until the signal
    std::fs::copy(&two.cert, &one.cert).unwrap();
    std::fs::copy(&two.key, &one.key).unwrap();
    ok_env(&a, "alice", &e1, &["fetch"]);
    fail_env(&a, "alice", &e2, &["fetch"]);
    let st = Command::new("kill").args(["-HUP", &srv.child.id().to_string()]).status().unwrap();
    assert!(st.success());
    let mut reloaded = false;
    for _ in 0..50 {
        if vcs_env(&a, "alice", &e2, &["fetch"]).status.success() {
            reloaded = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(reloaded, "the new certificate was not served within 5 s of SIGHUP");
    fail_env(&a, "alice", &e1, &["fetch"]);

    // a broken certificate file on the next SIGHUP keeps the working one
    std::fs::write(&one.cert, "garbage").unwrap();
    Command::new("kill").args(["-HUP", &srv.child.id().to_string()]).status().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    ok_env(&a, "alice", &e2, &["fetch"]);
}

#[test]
fn mutual_tls_needs_a_client_certificate_and_still_a_token() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let good = pki(root, "good ca");
    let client_ca = make_ca("client ca");
    let client_ca_pem = root.join("client-ca.pem");
    std::fs::write(&client_ca_pem, client_ca.cert.pem()).unwrap();
    let (cc, ck) = leaf(&client_ca, true);
    let (client_cert, client_key) = (root.join("client.pem"), root.join("client.key"));
    std::fs::write(&client_cert, cc).unwrap();
    std::fs::write(&client_key, ck).unwrap();
    let rogue_ca = make_ca("rogue ca");
    let (rc, rk) = leaf(&rogue_ca, true);
    let (rogue_cert, rogue_key) = (root.join("rogue.pem"), root.join("rogue.key"));
    std::fs::write(&rogue_cert, rc).unwrap();
    std::fs::write(&rogue_key, rk).unwrap();

    let srv = start(
        root,
        &[
            "--tls-cert",
            p(&good.cert),
            "--tls-key",
            p(&good.key),
            "--tls-client-ca",
            p(&client_ca_pem),
        ],
    );
    ok_env(root, "alice", &[], &["init", "a"]);
    let a = root.join("a");
    write(&a, "f.txt", "x\n");
    ok_env(&a, "alice", &[], &["add", "."]);
    ok_env(&a, "alice", &[], &["commit", "-m", "init"]);
    ok_env(&a, "alice", &[], &["remote", "set", &srv.url]);
    let ca = p(&good.ca_pem);

    // no client certificate: refused at the handshake
    fail_env(&a, "alice", &[("VCS_CAINFO", ca)], &["publish"]);
    // certificate from the wrong CA: refused
    fail_env(
        &a,
        "alice",
        &[
            ("VCS_CAINFO", ca),
            ("VCS_CLIENT_CERT", p(&rogue_cert)),
            ("VCS_CLIENT_KEY", p(&rogue_key)),
        ],
        &["publish"],
    );
    // cert without key is a configuration error
    let e = fail_env(
        &a,
        "alice",
        &[("VCS_CAINFO", ca), ("VCS_CLIENT_CERT", p(&client_cert))],
        &["publish"],
    );
    assert!(e.contains("remote.sslcert") && e.contains("remote.sslkey"), "{e}");
    let good_env = [
        ("VCS_CAINFO", ca),
        ("VCS_CLIENT_CERT", p(&client_cert)),
        ("VCS_CLIENT_KEY", p(&client_key)),
    ];
    assert!(ok_env(&a, "alice", &good_env, &["publish"]).contains("published"));
    // the certificate does not replace the token
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["fetch"])
        .current_dir(&a)
        .env("VCS_TOKEN", "wrong")
        .env("VCS_CAINFO", ca)
        .env("VCS_CLIENT_CERT", &client_cert)
        .env("VCS_CLIENT_KEY", &client_key)
        .env("VCS_CONFIG_HOME", root.join("no-user-config"))
        .output()
        .unwrap();
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("401"));
}
