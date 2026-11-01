//! `vcs-server (--root <dir> | --backend <s3://bucket/prefix?endpoint=URL|dir>) --tokens <file> [--listen 127.0.0.1:8480] [options]`
//!
//! * `vcs-server hash-token <token>` prints the hash to put in the tokens file.
//! * `vcs-server audit verify <file> [--key <keyfile>]` checks an audit log's hash chain (exit 0 ok, 1 tampered).
//!
//! Options: `--audit-log <file> [--audit-key <keyfile>]`, `--rate-limit <burst>,<per-sec>`,
//! `--max-connections <n>`, `--max-body <bytes>`, and with the `tls` feature `--tls-cert <pem> --tls-key <pem>`.
//! Policy options: `--policy <file> [--max-lease-secs N] [--skew-epsilon-ms N]` (docs/deploy.md).
//! Operations: `--config <server.toml>` (flags override it), `--log-format json|text`, `--log-level <level>`,
//! `--shutdown-grace <secs>`, `--metrics-token <token|@file>`, `--backend-retries N`, `--backend-timeout <secs>`,
//! `--breaker-threshold N`, `--breaker-cooldown <secs>`. SIGTERM/SIGINT drain and exit 0; SIGHUP reloads.
//! Without TLS the server speaks plain HTTP (docs/server.md).

use std::path::Path;
use std::sync::Arc;
use vcs_server::audit::{parse_key, verify_file};
use vcs_server::log::{Format, Level, Logger, V};
use vcs_server::{auth, backend_url, AuditLog, Config, Policy, RateLimit, Server, TokenTable};

const USAGE: &str = "usage: vcs-server (--root <dir> | --backend <s3://...|dir>) --tokens <file> [--listen host:port] [--audit-log <file> [--audit-key <file>]] \
[--rate-limit <burst>,<per-sec>] [--max-connections <n>] [--max-body <bytes>] [--policy <file>] [--max-lease-secs N] [--skew-epsilon-ms N] [--tls-cert <pem> --tls-key <pem> [--tls-client-ca <pem>]] \
[--config <file>] [--log-format json|text] [--log-level error|warn|info|debug] [--shutdown-grace <secs>] [--metrics-token <token|@file>] \
[--backend-retries N] [--backend-timeout <secs>] [--breaker-threshold N] [--breaker-cooldown <secs>]\n       \
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
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
    // `--config FILE`: its entries become leading flags, so real flags (later) win.
    if let Some(i) = args.iter().position(|a| a == "--config") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| die("--config needs a value"));
        args.drain(i..i + 2);
        if args.iter().any(|a| a == "--config") {
            die("--config given twice");
        }
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| die(&format!("cannot read {path}: {e}")));
        let entries = vcs_server::config::parse(&text, &path).unwrap_or_else(|e| die(&e));
        let mut merged: Vec<String> = entries.into_iter().flat_map(|(k, v)| [format!("--{k}"), v]).collect();
        merged.extend(args);
        args = merged;
    }
    let (mut root, mut tokens, mut listen) = (None, None, "127.0.0.1:8480".to_owned());
    let (mut audit_log, mut audit_key, mut rate, mut maxc, mut maxb) = (None, None, None, None, None);
    let (mut tls_cert, mut tls_key, mut tls_client_ca) = (None, None, None);
    let (mut backend_arg, mut policy_path, mut max_lease, mut eps) = (None, None, None, None);
    let (mut log_format, mut log_level, mut grace, mut metrics_tok) = (None, None, None, None);
    let (mut retries, mut timeout, mut brk_n, mut brk_cool) = (None, None, None, None);
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
            "--log-format" => log_format = Some(val("--log-format")),
            "--log-level" => log_level = Some(val("--log-level")),
            "--shutdown-grace" => grace = Some(val("--shutdown-grace")),
            "--metrics-token" => metrics_tok = Some(val("--metrics-token")),
            "--backend-retries" => retries = Some(val("--backend-retries")),
            "--backend-timeout" => timeout = Some(val("--backend-timeout")),
            "--breaker-threshold" => brk_n = Some(val("--breaker-threshold")),
            "--breaker-cooldown" => brk_cool = Some(val("--breaker-cooldown")),
            _ => die(USAGE),
        }
    }
    let backend_url = match (root, backend_arg) {
        (Some(_), Some(_)) => die("give either --root or --backend, not both"),
        (Some(r), None) | (None, Some(r)) => r,
        (None, None) => die("--root or --backend is required"),
    };
    let fmt = match log_format.as_deref() {
        None => Format::Text,
        Some(f) => Format::parse(f).unwrap_or_else(|| die("--log-format expects json or text")),
    };
    let level = match log_level.as_deref() {
        Some(l) => Level::parse(l).unwrap_or_else(|| die("--log-level expects error, warn, info or debug")),
        None => Level::from_env().unwrap_or(Level::Info),
    };
    let log = Arc::new(Logger::new(fmt, level));
    let secs = |n: &str, v: &str| -> std::time::Duration {
        match v.parse::<f64>() {
            Ok(x) if x.is_finite() && (0.0..=86_400.0).contains(&x) => std::time::Duration::from_secs_f64(x),
            _ => die(&format!("{n} expects a number of seconds (0..86400)")),
        }
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
    cfg.log = log.clone();
    if let Some(t) = metrics_tok {
        let t = match t.strip_prefix('@') {
            Some(f) => std::fs::read_to_string(f)
                .unwrap_or_else(|e| die(&format!("cannot read {f}: {e}")))
                .trim()
                .to_owned(),
            None => t,
        };
        if t.len() < 16 {
            die("--metrics-token must be at least 16 characters");
        }
        cfg.metrics_token_hash = Some(auth::hash_token(&t));
    }
    let count = |n: &str, v: Option<String>| v.map(|v| v.parse::<u32>().unwrap_or_else(|_| die(&format!("{n} expects a number"))));
    if let Some(n) = count("--backend-retries", retries) {
        cfg.resilience.retries = n;
    }
    if let Some(t) = timeout {
        let d = secs("--backend-timeout", &t);
        cfg.resilience.request_deadline = (!d.is_zero()).then_some(d);
    }
    if let Some(n) = count("--breaker-threshold", brk_n) {
        cfg.resilience.breaker_threshold = n;
    }
    if let Some(c) = brk_cool {
        cfg.resilience.breaker_cooldown = secs("--breaker-cooldown", &c);
    }
    let grace = grace.map_or(std::time::Duration::from_secs(30), |g| secs("--shutdown-grace", &g));
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
    let mut srv = Server::start(backend, cfg, &listen).unwrap_or_else(|e| die(&format!("cannot listen on {listen}: {e}")));
    log.info(
        &format!("vcs-server listening on {scheme}://{}", srv.local_addr()),
        &[
            (
                "tls",
                V::S(if scheme == "https" {
                    "yes"
                } else {
                    "no (use a TLS proxy beyond loopback)"
                }),
            ),
            ("version", V::S(env!("CARGO_PKG_VERSION"))),
        ],
    );
    #[cfg(unix)]
    {
        let h = srv.handle();
        let reload = Reload {
            tokens: cfg_tokens,
            policy: cfg_policy,
            log: log.clone(),
            #[cfg(feature = "tls")]
            tls: tls_files,
        };
        signals::install(move || reload.run(&h));
        sd_notify("READY=1");
        while !signals::terminated() {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        sd_notify("STOPPING=1");
        log.info(
            "shutting down: draining in-flight requests",
            &[("grace_s", V::F(grace.as_secs_f64()))],
        );
        let clean = srv.shutdown_graceful(grace);
        if clean {
            log.info("shutdown complete", &[]);
        } else {
            log.warn("shutdown grace expired with requests still in flight", &[]);
        }
        std::process::exit(0);
    }
    #[cfg(not(unix))]
    {
        let _ = (grace, &mut srv);
        srv.join();
    }
}

/// systemd `Type=notify`: send a state line to `$NOTIFY_SOCKET` (no-op when unset). Best effort.
#[cfg(unix)]
fn sd_notify(state: &str) {
    use std::os::unix::net::UnixDatagram;
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else { return };
    let Ok(sock) = UnixDatagram::unbound() else { return };
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    #[cfg(target_os = "linux")]
    if let Some(name) = bytes.strip_prefix(b"@") {
        use std::os::linux::net::SocketAddrExt;
        if let Ok(addr) = std::os::unix::net::SocketAddr::from_abstract_name(name) {
            let _ = sock.send_to_addr(state.as_bytes(), &addr);
        }
        return;
    }
    let _ = sock.send_to(state.as_bytes(), std::path::Path::new(&path));
}

/// What SIGHUP re-reads. A file that fails to parse leaves the running value in place.
#[cfg(unix)]
struct Reload {
    tokens: String,
    policy: Option<String>,
    log: Arc<Logger>,
    #[cfg(feature = "tls")]
    tls: Option<(String, String, Option<String>)>,
}

#[cfg(unix)]
impl Reload {
    fn run(&self, h: &vcs_server::Handle) {
        let say = |m: &str| self.log.info(m, &[]);
        let keep = |what: &str, e: &str| self.log.warn(&format!("keeping old {what}"), &[("error", V::S(e))]);
        say("SIGHUP: reloading");
        match std::fs::read_to_string(&self.tokens)
            .map_err(|e| e.to_string())
            .and_then(|t| TokenTable::parse(&t))
        {
            Ok(t) if !t.is_empty() => {
                h.reload_tokens(t);
                say("reloaded tokens");
            }
            Ok(_) => keep("tokens", &format!("{} is empty", self.tokens)),
            Err(e) => keep("tokens", &e),
        }
        if let Some(p) = &self.policy {
            match std::fs::read_to_string(p)
                .map_err(|e| e.to_string())
                .and_then(|t| Policy::parse(&t))
            {
                Ok(pol) => {
                    h.reload_policy(pol);
                    say("reloaded policy");
                }
                Err(e) => keep("policy", &e),
            }
        }
        #[cfg(feature = "tls")]
        if let Some((c, k, ca)) = &self.tls {
            match build_tls(c, k, ca.as_deref()) {
                Ok(cfg) => {
                    h.reload_tls(cfg);
                    say("reloaded tls certificate");
                }
                Err(e) => keep("certificate", &e),
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

/// SIGHUP -> run a callback on a helper thread; SIGTERM/SIGINT -> `terminated()`. The handlers only
/// set flags (async-signal-safe).
#[cfg(unix)]
mod signals {
    use std::sync::atomic::{AtomicBool, Ordering};
    static HUP: AtomicBool = AtomicBool::new(false);
    static TERM: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_hup(_: libc::c_int) {
        HUP.store(true, Ordering::SeqCst);
    }
    extern "C" fn on_term(_: libc::c_int) {
        TERM.store(true, Ordering::SeqCst);
    }
    pub fn terminated() -> bool {
        TERM.load(Ordering::SeqCst)
    }
    pub fn install(f: impl Fn() + Send + 'static) {
        // SAFETY: the handlers only store to atomics, which is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGHUP, on_hup as extern "C" fn(libc::c_int) as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if HUP.swap(false, Ordering::SeqCst) {
                f();
            }
        });
    }
}
