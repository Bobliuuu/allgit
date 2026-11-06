//! `vcs-server (--root <dir> | --backend <s3://bucket/prefix?endpoint=URL|dir>) --tokens <file> [--listen 127.0.0.1:8480] [options]`
//!
//! * `vcs-server hash-token <token>` prints the hash to put in the tokens file.
//! * `vcs-server audit verify <file> [--key <keyfile>]` checks an audit log's hash chain (exit 0 ok, 1 tampered).
//!
//! Options: `--audit-log <file> [--audit-key <keyfile>]`, `--rate-limit <burst>,<per-sec>`,
//! `--max-connections <n>`, `--max-body <bytes>`, and with the `tls` feature `--tls-cert <pem> --tls-key <pem>`.
//! Policy options: `--policy <file> [--allowed-signers <file>] [--revoked-signers <file>] [--max-lease-secs N] [--skew-epsilon-ms N]` (docs/deploy.md).
//! Operations: `--config <server.toml>` (flags override it), `--log-format json|text`, `--log-level <level>`,
//! `--shutdown-grace <secs>`, `--metrics-token <token|@file>`, `--backend-retries N`, `--backend-timeout <secs>`,
//! `--breaker-threshold N`, `--breaker-cooldown <secs>`, `--cache-dir <dir> [--cache-size <bytes>]` (disk cache of immutable reads), `--upstream <url> --upstream-token <t>` (read-through peer, docs/peering.md). SIGTERM/SIGINT drain and exit 0; SIGHUP reloads.
//! Without TLS the server speaks plain HTTP (docs/server.md).

use std::path::Path;
use std::sync::Arc;
use vcs_server::audit::{parse_key, verify_file};
use vcs_server::log::{Format, Level, Logger, V};
use vcs_server::{auth, backend_url, AuditLog, Config, Policy, RateLimit, Server, TokenTable};

const USAGE: &str = "usage: vcs-server (--root <dir> | --backend <s3://...|dir>) --tokens <file> [--listen host:port] [--audit-log <file> [--audit-key <file>]] \
[--rate-limit <burst>,<per-sec>] [--max-connections <n>] [--max-conns-per-ip <n>] [--auth-fail-limit <burst>,<per-sec>] [--max-body <bytes>] [--max-inflight-body <bytes>] [--cache-dir <dir> [--cache-size <bytes>]] [--upstream <url> --upstream-token <token|@file> [--upstream-ca <pem>] [--upstream-manifest-ttl <secs>] [--upstream-stale-if-error <secs>]] [--policy <file>] [--allowed-signers <file>] [--revoked-signers <file>] [--max-lease-secs N] [--skew-epsilon-ms N] [--tls-cert <pem> --tls-key <pem> [--tls-client-ca <pem> [--tls-client-crl <pem>] [--tls-client-map <file>]]] \
[--config <file>] [--log-format json|text] [--log-level error|warn|info|debug] [--shutdown-grace <secs>] [--metrics-token <token|@file>] \
[--backend-retries N] [--backend-timeout <secs>] [--breaker-threshold N] [--breaker-cooldown <secs>] [--check-client-cannot-write-manifest] [--no-group-commit]\n       \
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

/// glibc's defaults suit a program with a few threads, not a thread per connection that buffers whole packs: every
/// thread gets its own malloc arena (up to 8 per core) and keeps what it freed, and the mmap threshold grows to 32 MiB
/// as big buffers are freed, so the next ones are carved from arena heaps that never shrink. Under 24 concurrent
/// downloads of an 80 MB pack the resident size peaked at 2.3 to 2.6 GiB that way, against 0.9 to 1.2 GiB with four
/// arenas and a fixed 1 MiB threshold (docs/known-limits.md). An explicit `MALLOC_ARENA_MAX` or
/// `MALLOC_MMAP_THRESHOLD_` in the environment wins.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_malloc() {
    // SAFETY: mallopt only sets allocator parameters; it is called before any other thread exists.
    unsafe {
        if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
            libc::mallopt(libc::M_ARENA_MAX, 4);
        }
        if std::env::var_os("MALLOC_MMAP_THRESHOLD_").is_none() {
            libc::mallopt(libc::M_MMAP_THRESHOLD, 1 << 20);
        }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn tune_malloc() {}

fn main() {
    tune_malloc();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("hash-token") => {
            match args.get(1) {
                Some(t) => println!("{}", auth::hash_token(t)),
                None => die("usage: vcs-server hash-token <token>"),
            }
            return;
        }
        Some("migrate-sharded") => {
            // one-way opt-in: per-zone sharded manifest (docs/manifest-scaling.md). Stop writers first or not:
            // the freeze makes concurrent publishers retry, and a crash mid-way is resumed by running it again.
            let url = match (args.get(1).map(String::as_str), args.get(2)) {
                (Some("--backend"), Some(u)) => u.clone(),
                _ => die("usage: vcs-server migrate-sharded --backend <dir|s3://...>"),
            };
            let b = backend_url::open_raw(&url).unwrap_or_else(|e| die(&e));
            match vcs_remote::sharded::migrate_to_sharded(b.as_ref()) {
                Ok(r) if r.already => println!("already sharded"),
                Ok(r) => println!("migrated to sharded manifests: {} zones", r.zones),
                Err(e) => die(&format!("migration failed (safe to re-run): {e}")),
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
    let mut maxib: Option<String> = None;
    let (mut maxc_ip, mut auth_fail): (Option<String>, Option<String>) = (None, None);
    let (mut tls_cert, mut tls_key, mut tls_client_ca) = (None, None, None);
    let (mut tls_client_crl, mut tls_client_map): (Option<String>, Option<String>) = (None, None);
    let (mut backend_arg, mut policy_path, mut max_lease, mut eps) = (None, None, None, None);
    let (mut log_format, mut log_level, mut grace, mut metrics_tok) = (None, None, None, None);
    let (mut retries, mut timeout, mut brk_n, mut brk_cool) = (None, None, None, None);
    let (mut allowed_signers, mut revoked_signers) = (None, None);
    let mut check_client = false;
    let mut no_group = false;
    let (mut cache_dir, mut cache_size): (Option<String>, Option<String>) = (None, None);
    let (mut upstream, mut up_token, mut up_ca, mut up_ttl, mut up_stale) =
        (None::<String>, None::<String>, None::<String>, None::<String>, None::<String>);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().unwrap_or_else(|| die(&format!("{n} needs a value")));
        match a.as_str() {
            "--check-client-cannot-write-manifest" => check_client = true,
            "--no-group-commit" => no_group = true,
            "--root" => root = Some(val("--root")),
            "--tokens" => tokens = Some(val("--tokens")),
            "--listen" => listen = val("--listen"),
            "--audit-log" => audit_log = Some(val("--audit-log")),
            "--audit-key" => audit_key = Some(val("--audit-key")),
            "--rate-limit" => rate = Some(val("--rate-limit")),
            "--max-connections" => maxc = Some(val("--max-connections")),
            "--max-conns-per-ip" => maxc_ip = Some(val("--max-conns-per-ip")),
            "--auth-fail-limit" => auth_fail = Some(val("--auth-fail-limit")),
            "--max-body" => maxb = Some(val("--max-body")),
            "--max-inflight-body" => maxib = Some(val("--max-inflight-body")),
            "--tls-cert" => tls_cert = Some(val("--tls-cert")),
            "--tls-key" => tls_key = Some(val("--tls-key")),
            "--tls-client-ca" => tls_client_ca = Some(val("--tls-client-ca")),
            "--tls-client-crl" => tls_client_crl = Some(val("--tls-client-crl")),
            "--tls-client-map" => tls_client_map = Some(val("--tls-client-map")),
            "--cache-dir" => cache_dir = Some(val("--cache-dir")),
            "--cache-size" => cache_size = Some(val("--cache-size")),
            "--upstream" => upstream = Some(val("--upstream")),
            "--upstream-token" => up_token = Some(val("--upstream-token")),
            "--upstream-ca" => up_ca = Some(val("--upstream-ca")),
            "--upstream-manifest-ttl" => up_ttl = Some(val("--upstream-manifest-ttl")),
            "--upstream-stale-if-error" => up_stale = Some(val("--upstream-stale-if-error")),
            "--backend" => backend_arg = Some(val("--backend")),
            "--policy" => policy_path = Some(val("--policy")),
            "--allowed-signers" => allowed_signers = Some(val("--allowed-signers")),
            "--revoked-signers" => revoked_signers = Some(val("--revoked-signers")),
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
    let backend_url = match (root, backend_arg, &upstream) {
        (Some(_), Some(_), _) => die("give either --root or --backend, not both"),
        (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => die("--upstream replaces --root/--backend: a peer keeps only a cache"),
        (Some(r), None, None) | (None, Some(r), None) => r,
        (None, None, Some(_)) => String::new(),
        (None, None, None) => die("--root, --backend or --upstream is required"),
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
    if check_client && upstream.is_none() {
        backend_url::check_client_cannot_write_manifest(&backend_url, &|k| std::env::var(k).ok()).unwrap_or_else(|e| die(&e));
    }
    let peer_mode = upstream.is_some();
    let backend: std::sync::Arc<dyn vcs_remote::Backend> = if let Some(up) = upstream {
        // Peer mode: the cache directory holds verified immutable reads; every write and every
        // freshness decision goes to the upstream server (docs/peering.md).
        let dir = cache_dir.unwrap_or_else(|| die("--upstream needs --cache-dir"));
        let tok = up_token
            .or_else(|| std::env::var("VCS_UPSTREAM_TOKEN").ok())
            .unwrap_or_else(|| die("--upstream needs --upstream-token <token|@file> or VCS_UPSTREAM_TOKEN"));
        let tok = match tok.strip_prefix('@') {
            Some(f) => std::fs::read_to_string(f)
                .unwrap_or_else(|e| die(&format!("cannot read {f}: {e}")))
                .trim()
                .to_owned(),
            None => tok,
        };
        let ca = up_ca.map(|p| std::fs::read(&p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}"))));
        let http = vcs_server::HttpBackend::with_ca(&up, &tok, ca.as_deref()).unwrap_or_else(|e| die(&format!("--upstream: {e}")));
        let mut pc = vcs_server::peer::PeerConfig::new(dir);
        if let Some(n) = cache_size {
            pc.cache_size = n.parse().unwrap_or_else(|_| die("--cache-size expects a number of bytes"));
        }
        let d = |n: &str, v: Option<String>| {
            v.map(|v| match v.parse::<f64>() {
                Ok(x) if x.is_finite() && (0.0..=86_400.0).contains(&x) => std::time::Duration::from_secs_f64(x),
                _ => die(&format!("{n} expects a number of seconds (0..86400)")),
            })
        };
        if let Some(t) = d("--upstream-manifest-ttl", up_ttl) {
            pc.manifest_ttl = t;
        }
        if let Some(t) = d("--upstream-stale-if-error", up_stale) {
            pc.stale_if_error = t;
        }
        std::sync::Arc::new(
            vcs_server::peer::PeerBackend::new(std::sync::Arc::new(http), pc)
                .unwrap_or_else(|e| die(&format!("cannot use --cache-dir: {e}"))),
        )
    } else {
        let backend = backend_url::open(&backend_url).unwrap_or_else(|e| die(&e));
        // Persistent cache of immutable reads (indexes, pack blocks, objects): a restarted server
        // answers a warm clone from disk and asks the store only for the manifest and the listing.
        match cache_dir {
            None => backend,
            Some(d) => {
                let cap = cache_size.map_or(8u64 << 30, |n| {
                    n.parse().unwrap_or_else(|_| die("--cache-size expects a number of bytes"))
                });
                std::sync::Arc::new(
                    vcs_remote::DiskCacheBackend::new(backend, d, cap).unwrap_or_else(|e| die(&format!("cannot use --cache-dir: {e}"))),
                )
            }
        }
    };
    let mut cfg = Config::new(table);
    if peer_mode {
        // the peer has its own fast-fail on upstream; a breaker above it would also block cache hits
        cfg.resilience.breaker_threshold = 0;
    }
    #[cfg(unix)]
    let (cfg_tokens, cfg_policy) = (tokens.clone(), policy_path.clone());
    if let Some(p) = policy_path {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}")));
        cfg.validator.policy = Policy::parse(&text).unwrap_or_else(|e| die(&e));
    }
    if let Some(p) = allowed_signers {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}")));
        cfg.validator.signers.parse_allowed(&text).unwrap_or_else(|e| die(&e));
    }
    if let Some(p) = revoked_signers {
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}")));
        cfg.validator.signers.parse_revoked(&text).unwrap_or_else(|e| die(&e));
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
    if let Some(n) = maxc_ip {
        cfg.max_conns_per_ip = n
            .parse()
            .unwrap_or_else(|_| die("--max-conns-per-ip expects a number (0 = unlimited)"));
    }
    if let Some(r) = auth_fail {
        let parsed = r.split_once(',').and_then(|(b, p)| {
            Some(RateLimit {
                burst: b.parse().ok()?,
                per_sec: p.parse().ok()?,
            })
        });
        cfg.auth_fail_limit = Some(parsed.unwrap_or_else(|| die("--auth-fail-limit expects <burst>,<per-sec> (e.g. 20,0.5)")));
    }
    if let Some(n) = maxc {
        cfg.max_connections = n.parse().unwrap_or_else(|_| die("--max-connections expects a number"));
    }
    if let Some(n) = maxb {
        cfg.max_body = n.parse().unwrap_or_else(|_| die("--max-body expects a number of bytes"));
    }
    if no_group {
        cfg.group_commit = false;
    }
    if let Some(n) = maxib {
        cfg.max_inflight_body = n
            .parse()
            .unwrap_or_else(|_| die("--max-inflight-body expects a number of bytes (0 = unlimited)"));
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
    let tls_files = tls_cert
        .clone()
        .zip(tls_key.clone())
        .map(|(c, k)| (c, k, tls_client_ca.clone(), tls_client_crl.clone()));
    if tls_client_ca.is_some() && (tls_cert.is_none() || tls_key.is_none()) {
        die("--tls-client-ca needs --tls-cert and --tls-key");
    }
    if (tls_client_crl.is_some() || tls_client_map.is_some()) && tls_client_ca.is_none() {
        die("--tls-client-crl and --tls-client-map need --tls-client-ca");
    }
    if let Some(p) = &tls_client_map {
        let t = std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("cannot read {p}: {e}")));
        cfg.client_cert_map = Some(Arc::new(vcs_server::certmap::ClientCertMap::parse(&t).unwrap_or_else(|e| die(&e))));
    }
    match (tls_cert, tls_key) {
        (None, None) => {}
        #[cfg(feature = "tls")]
        (Some(_), Some(_)) => {
            let (c, k, ca, crl) = tls_files.clone().unwrap_or_else(|| die("internal: tls files"));
            cfg.tls = Some(build_tls(&c, &k, ca.as_deref(), crl.as_deref()).unwrap_or_else(|e| die(&e)));
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
    tls: Option<(String, String, Option<String>, Option<String>)>,
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
        if let Some((c, k, ca, crl)) = &self.tls {
            match build_tls(c, k, ca.as_deref(), crl.as_deref()) {
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
fn build_tls(cert: &str, key: &str, client_ca: Option<&str>, client_crl: Option<&str>) -> Result<Arc<rustls::ServerConfig>, String> {
    let rd = |p: &str| std::fs::read(p).map_err(|e| format!("cannot read {p}: {e}"));
    match client_ca {
        Some(ca) => match client_crl {
            Some(crl) => vcs_server::tls::server_config_mtls_crl(&rd(cert)?, &rd(key)?, &rd(ca)?, &rd(crl)?),
            None => vcs_server::tls::server_config_mtls(&rd(cert)?, &rd(key)?, &rd(ca)?),
        },
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
