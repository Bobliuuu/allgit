//! `vcs-server (--root <dir> | --backend <s3://bucket/prefix?endpoint=URL|dir>) --tokens <file> [--listen 127.0.0.1:8480] [options]`
//!
//! * `vcs-server hash-token <token>` prints the hash to put in the tokens file.
//! * `vcs-server audit verify <file> [--key <keyfile>]` checks an audit log's hash chain (exit 0 ok, 1 tampered).
//!
//! Options: `--audit-log <file> [--audit-key <keyfile>]`, `--rate-limit <burst>,<per-sec>`,
//! `--max-connections <n>`, `--max-body <bytes>`, and with the `tls` feature `--tls-cert <pem> --tls-key <pem>`.
//! Policy options: `--policy <file> [--max-lease-secs N] [--skew-epsilon-ms N]` (docs/deploy.md).
//! Without TLS the server speaks plain HTTP (docs/server.md).

use std::path::Path;
use std::sync::Arc;
use vcs_server::audit::{parse_key, verify_file};
use vcs_server::{auth, backend_url, AuditLog, Config, Policy, RateLimit, Server, TokenTable};

const USAGE: &str = "usage: vcs-server (--root <dir> | --backend <s3://...|dir>) --tokens <file> [--listen host:port] [--audit-log <file> [--audit-key <file>]] \
[--rate-limit <burst>,<per-sec>] [--max-connections <n>] [--max-body <bytes>] [--policy <file>] [--max-lease-secs N] [--skew-epsilon-ms N] [--tls-cert <pem> --tls-key <pem> [--tls-client-ca <pem>]]\n       \
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
    let (mut tls_cert, mut tls_key, mut tls_client_ca) = (None, None, None);
    let (mut backend_arg, mut policy_path, mut max_lease, mut eps) = (None, None, None, None);
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
            "--tls-client-ca" => tls_client_ca = Some(val("--tls-client-ca")),
            "--backend" => backend_arg = Some(val("--backend")),
            "--policy" => policy_path = Some(val("--policy")),
            "--max-lease-secs" => max_lease = Some(val("--max-lease-secs")),
            "--skew-epsilon-ms" => eps = Some(val("--skew-epsilon-ms")),
            _ => die(USAGE),
        }
    }
    let backend_url = match (root, backend_arg) {
        (Some(_), Some(_)) => die("give either --root or --backend, not both"),
        (Some(r), None) | (None, Some(r)) => r,
        (None, None) => die("--root or --backend is required"),
    };
    let tokens = tokens.unwrap_or_else(|| die("--tokens is required"));
    let text = std::fs::read_to_string(&tokens).unwrap_or_else(|e| die(&format!("cannot read {tokens}: {e}")));
    let table = TokenTable::parse(&text).unwrap_or_else(|e| die(&e));
    if table.is_empty() {
        die("tokens file has no entries");
    }
    let backend = backend_url::open(&backend_url).unwrap_or_else(|e| die(&e));
    let mut cfg = Config::new(table);
    #[cfg(unix)]
    let (cfg_tokens, cfg_policy) = (tokens.clone(), policy_path.clone());
    if let Some(p) = policy_path {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}")));
        cfg.validator.policy = Policy::parse(&text).unwrap_or_else(|e| die(&e));
    }
    let num = |n: &str, v: Option<String>| v.map(|v| v.parse::<u64>().unwrap_or_else(|_| die(&format!("{n} must be a number"))));
    if let Some(s) = num("--max-lease-secs", max_lease) {
        cfg.validator.max_lease_ms = s.saturating_mul(1000);
    }
    if let Some(ms) = num("--skew-epsilon-ms", eps) {
        cfg.validator.skew_epsilon_ms = ms;
    }
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
    #[cfg(feature = "tls")]
    let tls_files = tls_cert.clone().zip(tls_key.clone()).map(|(c, k)| (c, k, tls_client_ca.clone()));
    if tls_client_ca.is_some() && (tls_cert.is_none() || tls_key.is_none()) {
        die("--tls-client-ca needs --tls-cert and --tls-key");
    }
    match (tls_cert, tls_key) {
        (None, None) => {}
        #[cfg(feature = "tls")]
        (Some(_), Some(_)) => {
            let (c, k, ca) = tls_files.clone().unwrap_or_else(|| die("internal: tls files"));
            cfg.tls = Some(build_tls(&c, &k, ca.as_deref()).unwrap_or_else(|e| die(&e)));
            scheme = "https";
        }
        #[cfg(not(feature = "tls"))]
        (Some(_), Some(_)) => die("this build has no TLS support: rebuild with `--features tls`"),
        _ => die("--tls-cert and --tls-key must be given together"),
    }
    let srv = Server::start(backend, cfg, &listen).unwrap_or_else(|e| die(&format!("cannot listen on {listen}: {e}")));
    eprintln!(
        "vcs-server listening on {scheme}://{}{}",
        srv.local_addr(),
        if scheme == "http" {
            " (no TLS: use a TLS proxy beyond loopback)"
        } else {
            ""
        }
    );
    #[cfg(unix)]
    {
        let h = srv.handle();
        let reload = Reload {
            tokens: cfg_tokens,
            policy: cfg_policy,
            #[cfg(feature = "tls")]
            tls: tls_files,
        };
        sighup::install(move || reload.run(&h));
    }
    srv.join();
}

/// Log line on stderr that never panics (a closed stderr must not kill the reload thread).
#[cfg(unix)]
fn say(m: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{m}");
}

/// What SIGHUP re-reads. A file that fails to parse leaves the running value in place.
#[cfg(unix)]
struct Reload {
    tokens: String,
    policy: Option<String>,
    #[cfg(feature = "tls")]
    tls: Option<(String, String, Option<String>)>,
}

#[cfg(unix)]
impl Reload {
    fn run(&self, h: &vcs_server::Handle) {
        say("vcs-server: SIGHUP: reloading");
        match std::fs::read_to_string(&self.tokens)
            .map_err(|e| e.to_string())
            .and_then(|t| TokenTable::parse(&t))
        {
            Ok(t) if !t.is_empty() => {
                h.reload_tokens(t);
                say("vcs-server: reloaded tokens");
            }
            Ok(_) => say(&format!("vcs-server: keeping old tokens: {} is empty", self.tokens)),
            Err(e) => say(&format!("vcs-server: keeping old tokens: {e}")),
        }
        if let Some(p) = &self.policy {
            match std::fs::read_to_string(p)
                .map_err(|e| e.to_string())
                .and_then(|t| Policy::parse(&t))
            {
                Ok(pol) => {
                    h.reload_policy(pol);
                    say("vcs-server: reloaded policy");
                }
                Err(e) => say(&format!("vcs-server: keeping old policy: {e}")),
            }
        }
        #[cfg(feature = "tls")]
        if let Some((c, k, ca)) = &self.tls {
            match build_tls(c, k, ca.as_deref()) {
                Ok(cfg) => {
                    h.reload_tls(cfg);
                    say("vcs-server: reloaded tls certificate");
                }
                Err(e) => say(&format!("vcs-server: keeping old certificate: {e}")),
            }
        }
    }
}

#[cfg(feature = "tls")]
fn build_tls(cert: &str, key: &str, client_ca: Option<&str>) -> Result<Arc<rustls::ServerConfig>, String> {
    let rd = |p: &str| std::fs::read(p).map_err(|e| format!("cannot read {p}: {e}"));
    match client_ca {
        Some(ca) => vcs_server::tls::server_config_mtls(&rd(cert)?, &rd(key)?, &rd(ca)?),
        None => vcs_server::tls::server_config(&rd(cert)?, &rd(key)?),
    }
}

/// SIGHUP -> run a callback on a helper thread. The handler only sets a flag (async-signal-safe).
#[cfg(unix)]
mod sighup {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FLAG: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_hup(_: libc::c_int) {
        FLAG.store(true, Ordering::SeqCst);
    }
    pub fn install(f: impl Fn() + Send + 'static) {
        // SAFETY: `on_hup` only stores to an atomic, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGHUP, on_hup as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if FLAG.swap(false, Ordering::SeqCst) {
                f();
            }
        });
    }
}
