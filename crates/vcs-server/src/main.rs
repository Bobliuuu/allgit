//! `vcs-server --root <dir> --tokens <file> [--listen 127.0.0.1:8480] [options]`
//!
//! * `vcs-server hash-token <token>` prints the hash to put in the tokens file.
//! * `vcs-server audit verify <file> [--key <keyfile>]` checks an audit log's hash chain (exit 0 ok, 1 tampered).
//!
//! Options: `--audit-log <file> [--audit-key <keyfile>]`, `--rate-limit <burst>,<per-sec>`,
//! `--max-connections <n>`, `--max-body <bytes>`, and with the `tls` feature `--tls-cert <pem> --tls-key <pem>`.
//! Without TLS the server speaks plain HTTP (docs/server.md).

use std::path::Path;
use std::sync::Arc;
use vcs_remote::FsBackend;
use vcs_server::audit::{parse_key, verify_file};
use vcs_server::{auth, AuditLog, Config, RateLimit, Server, TokenTable};

const USAGE: &str = "usage: vcs-server --root <dir> --tokens <file> [--listen host:port] [--audit-log <file> [--audit-key <file>]] \
[--rate-limit <burst>,<per-sec>] [--max-connections <n>] [--max-body <bytes>] [--tls-cert <pem> --tls-key <pem>]\n       \
vcs-server hash-token <token>\n       vcs-server audit verify <file> [--key <file>]";

fn die(m: &str) -> ! {
    eprintln!("vcs-server: {m}");
    std::process::exit(2)
}

fn read_key(path: &str) -> [u8; 32] {
    let t = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("cannot read {path}: {e}")));
    parse_key(&t).unwrap_or_else(|e| die(&e))
}

fn audit_verify(args: &[String]) -> ! {
    let (mut file, mut key) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => key = Some(read_key(it.next().unwrap_or_else(|| die("--key needs a value")))),
            f if file.is_none() => file = Some(f.to_owned()),
            _ => die(USAGE),
        }
    }
    let file = file.unwrap_or_else(|| die(USAGE));
    match verify_file(Path::new(&file), key.as_ref()) {
        Ok(r) => {
            println!("ok: {} records, head {}", r.records, r.head);
            std::process::exit(0)
        }
        Err(e) => {
            println!("TAMPERED OR INVALID: {e}");
            std::process::exit(1)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hash-token") => {
            match args.get(1) {
                Some(t) => println!("{}", auth::hash_token(t)),
                None => die("usage: vcs-server hash-token <token>"),
            }
            return;
        }
        Some("audit") if args.get(1).map(String::as_str) == Some("verify") => audit_verify(&args[2..]),
        _ => {}
    }
    let (mut root, mut tokens, mut listen) = (None, None, "127.0.0.1:8480".to_owned());
    let (mut audit_log, mut audit_key, mut rate, mut maxc, mut maxb) = (None, None, None, None, None);
    let (mut tls_cert, mut tls_key) = (None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().unwrap_or_else(|| die(&format!("{n} needs a value")));
        match a.as_str() {
            "--root" => root = Some(val("--root")),
            "--tokens" => tokens = Some(val("--tokens")),
            "--listen" => listen = val("--listen"),
            "--audit-log" => audit_log = Some(val("--audit-log")),
            "--audit-key" => audit_key = Some(val("--audit-key")),
            "--rate-limit" => rate = Some(val("--rate-limit")),
            "--max-connections" => maxc = Some(val("--max-connections")),
            "--max-body" => maxb = Some(val("--max-body")),
            "--tls-cert" => tls_cert = Some(val("--tls-cert")),
            "--tls-key" => tls_key = Some(val("--tls-key")),
            _ => die(USAGE),
        }
    }
    let root = root.unwrap_or_else(|| die("--root is required"));
    let tokens = tokens.unwrap_or_else(|| die("--tokens is required"));
    let text = std::fs::read_to_string(&tokens).unwrap_or_else(|e| die(&format!("cannot read {tokens}: {e}")));
    let table = TokenTable::parse(&text).unwrap_or_else(|e| die(&e));
    if table.is_empty() {
        die("tokens file has no entries");
    }
    let backend = FsBackend::open(&root).unwrap_or_else(|e| die(&format!("cannot open {root}: {e}")));
    let mut cfg = Config::new(table);
    cfg.tokens_file = Some(tokens.into());
    if let Some(r) = rate {
        let parsed = r.split_once(',').and_then(|(b, p)| {
            Some(RateLimit {
                burst: b.parse().ok()?,
                per_sec: p.parse().ok()?,
            })
        });
        cfg.rate_limit = Some(parsed.unwrap_or_else(|| die("--rate-limit expects <burst>,<per-sec> (e.g. 20,5)")));
    }
    if let Some(n) = maxc {
        cfg.max_connections = n.parse().unwrap_or_else(|_| die("--max-connections expects a number"));
    }
    if let Some(n) = maxb {
        cfg.max_body = n.parse().unwrap_or_else(|_| die("--max-body expects a number of bytes"));
    }
    if let Some(p) = audit_log {
        let key = audit_key.as_deref().map(read_key);
        cfg.audit = Some(Arc::new(
            AuditLog::open(Path::new(&p), key).unwrap_or_else(|e| die(&format!("cannot open audit log {p}: {e}"))),
        ));
    } else if audit_key.is_some() {
        die("--audit-key needs --audit-log");
    }
    #[allow(unused_mut)]
    let mut scheme = "http";
    match (tls_cert, tls_key) {
        (None, None) => {}
        #[cfg(feature = "tls")]
        (Some(c), Some(k)) => {
            let cert = std::fs::read(&c).unwrap_or_else(|e| die(&format!("cannot read {c}: {e}")));
            let key = std::fs::read(&k).unwrap_or_else(|e| die(&format!("cannot read {k}: {e}")));
            cfg.tls = Some(vcs_server::tls::server_config(&cert, &key).unwrap_or_else(|e| die(&e)));
            scheme = "https";
        }
        #[cfg(not(feature = "tls"))]
        (Some(_), Some(_)) => die("this build has no TLS support: rebuild with `--features tls`"),
        _ => die("--tls-cert and --tls-key must be given together"),
    }
    let srv = Server::start(Arc::new(backend), cfg, &listen).unwrap_or_else(|e| die(&format!("cannot listen on {listen}: {e}")));
    eprintln!(
        "vcs-server listening on {scheme}://{}{}",
        srv.local_addr(),
        if scheme == "http" {
            " (no TLS: use a TLS proxy beyond loopback)"
        } else {
            ""
        }
    );
    srv.join();
}
