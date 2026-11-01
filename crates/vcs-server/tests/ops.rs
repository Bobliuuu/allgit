//! Operability: health/readiness, metrics, structured logs, backend resilience (docs/deploy.md).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vcs_remote::*;
use vcs_server::log::{Format, Level, Logger};
use vcs_server::metrics::parse;
use vcs_server::{AuditLog, Config, Principal, RateLimit, Resilience, Role, Server, TokenTable};

fn who(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}

const ADMIN: &str = "t-admin-SECRET-0123456789";
const WRITER: &str = "t-writer-SECRET-0123456789";
const METRICS: &str = "t-metrics-SECRET-0123456789";

fn table() -> TokenTable {
    TokenTable::from_entries(vec![(WRITER, who("wendy", Role::Write)), (ADMIN, who("root", Role::Admin))])
}

/// Backend that fails the next `fail` calls with `Unavailable` (or all calls while `fail` is huge) and counts calls.
struct Fault {
    inner: MemBackend,
    fail: AtomicUsize,
    calls: AtomicUsize,
}

impl Fault {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MemBackend::new(),
            fail: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        })
    }
    fn gate(&self) -> Result<(), BackendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let f = self.fail.load(Ordering::SeqCst);
        if f > 0 {
            if f != usize::MAX {
                self.fail.fetch_sub(1, Ordering::SeqCst);
            }
            return Err(BackendError::Unavailable("injected fault".into()));
        }
        Ok(())
    }
}

impl Backend for Fault {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.gate()?;
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.gate()?;
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.gate()?;
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.gate()?;
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.gate()?;
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.gate()?;
        self.inner.list(p)
    }
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);
impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Buf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn fast_cfg() -> Config {
    let mut cfg = Config::new(table());
    cfg.resilience = Resilience {
        retries: 2,
        base_backoff: Duration::from_millis(2),
        max_backoff: Duration::from_millis(10),
        request_deadline: Some(Duration::from_secs(5)),
        breaker_threshold: 0,
        breaker_cooldown: Duration::from_millis(300),
    };
    cfg
}

/// Raw HTTP/1.1 exchange; returns (status, headers lowercased, body).
fn http(addr: std::net::SocketAddr, method: &str, path: &str, tok: Option<&str>, extra: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let auth = tok.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}{extra}\r\n").unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let t = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = t.split_once("\r\n\r\n").unwrap_or((&t, ""));
    let code = head.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (code, head.to_ascii_lowercase(), body.to_owned())
}

fn metrics(addr: std::net::SocketAddr) -> std::collections::BTreeMap<String, f64> {
    let (c, _, b) = http(addr, "GET", "/metrics", Some(ADMIN), "");
    assert_eq!(c, 200);
    parse(&b).unwrap()
}

fn req_count(m: &std::collections::BTreeMap<String, f64>, method: &str, status: u16, role: &str) -> f64 {
    m.get(&format!(
        "vcs_server_requests_total{{method=\"{method}\",status=\"{status}\",role=\"{role}\"}}"
    ))
    .copied()
    .unwrap_or(0.0)
}

#[test]
fn health_and_ready_are_unauthenticated_and_leak_nothing() {
    let be = Fault::new();
    let mut cfg = fast_cfg();
    cfg.ready_cache = Duration::from_millis(300);
    let srv = Server::start(be.clone(), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    let (c, _, b) = http(a, "GET", "/healthz", None, "");
    assert_eq!((c, b.trim()), (200, "ok"));
    assert_eq!(http(a, "POST", "/healthz", None, "Content-Length: 0\r\n").0, 405);
    let (c, _, b) = http(a, "GET", "/readyz", None, "");
    assert_eq!((c, b.trim()), (200, "ready"));
    // cached: many probes, one backend call
    let before = be.calls.load(Ordering::SeqCst);
    for _ in 0..20 {
        assert_eq!(http(a, "GET", "/readyz", None, "").0, 200);
    }
    assert_eq!(be.calls.load(Ordering::SeqCst), before, "probe result is cached");
    // backend down: not ready after the cache lapses, body is constant, liveness unaffected
    be.fail.store(usize::MAX, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(350));
    let (c, _, b) = http(a, "GET", "/readyz", None, "");
    assert_eq!((c, b.trim()), (503, "not ready"), "no backend error text leaks");
    assert!(!b.contains("injected"));
    assert_eq!(http(a, "GET", "/healthz", None, "").0, 200);
    be.fail.store(0, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(http(a, "GET", "/readyz", None, "").0, 200);
}

#[test]
fn metrics_auth_and_counters_move() {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let mut cfg = fast_cfg();
    cfg.metrics_token_hash = Some(vcs_server::auth::hash_token(METRICS));
    cfg.rate_limit = Some(RateLimit { burst: 2, per_sec: 0.0 });
    cfg.audit = Some(Arc::new(AuditLog::open(&audit_path, None).unwrap()));
    cfg.clock = Arc::new(|| 1000);
    let be = Fault::new();
    let srv = Server::start(be.clone(), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    // who may scrape
    assert_eq!(http(a, "GET", "/metrics", None, "").0, 401);
    assert_eq!(http(a, "GET", "/metrics", Some("bogus"), "").0, 401);
    assert_eq!(http(a, "GET", "/metrics", Some(WRITER), "").0, 403);
    let (c, h, _) = http(a, "GET", "/metrics", Some(METRICS), "");
    assert_eq!(c, 200);
    assert!(h.contains("text/plain; version=0.0.4"));
    let m0 = metrics(a);
    for k in [
        "vcs_server_uptime_seconds",
        "vcs_server_open_connections",
        "vcs_server_request_duration_seconds_count",
        "vcs_server_request_bytes_total",
        "vcs_server_response_bytes_total",
        "vcs_server_auth_failures_total",
        "vcs_server_rate_limited_total",
        "vcs_server_lock_conflicts_total",
        "vcs_server_audit_appends_total",
        "vcs_server_audit_errors_total",
        "vcs_server_backend_circuit_open",
        "vcs_server_backend_errors_total{kind=\"unavailable\"}",
    ] {
        assert!(m0.contains_key(k), "missing {k}");
    }
    assert!(m0.keys().any(|k| k.starts_with("vcs_server_build_info{version=")));
    assert!(m0["vcs_server_auth_failures_total"] >= 2.0);
    assert!(m0["vcs_server_open_connections"] >= 1.0, "the scrape itself is open");
    // a 404 GET as writer, then a PUT (audited, 2nd of the writer's 2 tokens), then the 3rd is rate limited
    assert_eq!(http(a, "GET", "/v1/o/nokey", Some(WRITER), "").0, 404);
    let put = http(
        a,
        "PUT",
        "/v1/o/misc/x",
        Some(WRITER),
        "If-None-Match: *\r\nContent-Length: 3\r\n\r\nabc",
    );
    assert_eq!(put.0, 201, "{put:?}");
    assert_eq!(http(a, "GET", "/v1/o/misc/x", Some(WRITER), "").0, 429);
    // backend errors: 3 attempts fail (retries=2) -> one unavailable error, 2 retries; admin is not limited
    be.fail.store(3, Ordering::SeqCst);
    assert_eq!(http(a, "GET", "/v1/o/misc/x", Some(ADMIN), "").0, 503);
    let m1 = metrics(a);
    let d = |k: &str| m1[k] - m0.get(k).copied().unwrap_or(0.0);
    assert_eq!(req_count(&m1, "GET", 404, "write") - req_count(&m0, "GET", 404, "write"), 1.0);
    assert_eq!(req_count(&m1, "PUT", 201, "write") - req_count(&m0, "PUT", 201, "write"), 1.0);
    assert_eq!(req_count(&m1, "GET", 503, "admin") - req_count(&m0, "GET", 503, "admin"), 1.0);
    assert_eq!(d("vcs_server_rate_limited_total"), 1.0);
    assert_eq!(d("vcs_server_audit_appends_total"), 1.0);
    assert_eq!(d("vcs_server_request_bytes_total"), 3.0);
    assert_eq!(d("vcs_server_backend_retries_total"), 2.0);
    assert_eq!(d("vcs_server_backend_errors_total{kind=\"unavailable\"}"), 1.0);
    assert!(d("vcs_server_response_bytes_total") > 0.0);
    assert!(d("vcs_server_request_duration_seconds_count") >= 4.0);
    // histogram is cumulative and consistent
    assert_eq!(
        m1["vcs_server_request_duration_seconds_bucket{le=\"+Inf\"}"],
        m1["vcs_server_request_duration_seconds_count"]
    );
    let mut prev = 0.0;
    for b in vcs_server::metrics::BUCKETS {
        let v = m1[&format!("vcs_server_request_duration_seconds_bucket{{le=\"{b}\"}}")];
        assert!(v >= prev);
        prev = v;
    }
    // unauthenticated failures count
    let f0 = m1["vcs_server_auth_failures_total"];
    assert_eq!(http(a, "GET", "/v1/whoami", Some("nope"), "").0, 401);
    assert_eq!(metrics(a)["vcs_server_auth_failures_total"] - f0, 1.0);
}

#[test]
fn json_access_log_has_request_id_and_never_the_token() {
    let buf = Buf::default();
    let mut cfg = fast_cfg();
    cfg.log = Arc::new(Logger::with_sink(Format::Json, Level::Debug, Box::new(buf.clone())));
    cfg.metrics_token_hash = Some(vcs_server::auth::hash_token(METRICS));
    let srv = Server::start(Fault::new(), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    let (c, h, _) = http(a, "GET", "/v1/whoami", Some(WRITER), "");
    assert_eq!(c, 200);
    let rid = h.lines().find_map(|l| l.strip_prefix("x-request-id: ")).unwrap().to_owned();
    assert_eq!(rid.len(), 16);
    // client-supplied ids are echoed, unsafe ones replaced
    let (_, h2, _) = http(a, "GET", "/healthz", None, "X-Request-Id: abc-123\r\n");
    assert!(h2.contains("x-request-id: abc-123"));
    let (_, h3, _) = http(a, "GET", "/healthz", None, "X-Request-Id: a b\"c\r\n");
    assert!(!h3.contains("a b"));
    // wrong and metrics tokens, garbage request
    http(a, "GET", "/v1/whoami", Some("wrong-token-VALUE-xyz"), "");
    http(a, "GET", "/metrics", Some(METRICS), "");
    http(a, "GET", "/v1/whoami", Some(ADMIN), "X-Evil: t-admin-SECRET\r\n");
    let mut s = TcpStream::connect(a).unwrap();
    let _ = s.write_all(b"GARBAGE\r\n\r\n");
    let mut sink = Vec::new();
    let _ = s.read_to_end(&mut sink);
    drop(srv);
    let logs = buf.text();
    for secret in [ADMIN, WRITER, METRICS, "wrong-token-VALUE-xyz", "SECRET"] {
        assert!(!logs.contains(secret), "log leaks {secret}:\n{logs}");
    }
    let line = logs.lines().find(|l| l.contains(&rid)).expect("access line for the request");
    assert!(line.starts_with('{') && line.ends_with('}'));
    for k in [
        "\"msg\":\"request\"",
        "\"method\":\"GET\"",
        "\"path\":\"/v1/whoami\"",
        "\"status\":200",
        "\"principal\":\"wendy\"",
        "\"dur_ms\":",
        "\"bytes_out\":",
        "\"ts\":\"",
    ] {
        assert!(line.contains(k), "{k} in {line}");
    }
    assert!(logs.lines().any(|l| l.contains("\"status\":401")));
    assert!(logs.lines().any(|l| l.contains("\"status\":400")), "parse errors are logged too");
    assert_eq!(
        logs.lines().filter(|l| l.contains("\"msg\":\"request\"")).count(),
        7,
        "one line per request"
    );
}

#[test]
fn text_log_and_level_filter() {
    let buf = Buf::default();
    let mut cfg = fast_cfg();
    cfg.log = Arc::new(Logger::with_sink(Format::Text, Level::Error, Box::new(buf.clone())));
    let srv = Server::start(Fault::new(), cfg, "127.0.0.1:0").unwrap();
    http(srv.local_addr(), "GET", "/healthz", None, "");
    drop(srv);
    assert_eq!(buf.text(), "", "info access lines are filtered at level error");
    let buf = Buf::default();
    let mut cfg = fast_cfg();
    cfg.log = Arc::new(Logger::with_sink(Format::Text, Level::Info, Box::new(buf.clone())));
    let srv = Server::start(Fault::new(), cfg, "127.0.0.1:0").unwrap();
    http(srv.local_addr(), "GET", "/healthz", None, "");
    drop(srv);
    let t = buf.text();
    assert!(
        t.contains(" INFO  request ") && t.contains("status=200") && t.contains("request_id="),
        "{t}"
    );
}

#[test]
fn transient_backend_errors_are_retried_then_surface() {
    let be = Fault::new();
    be.inner.put_if_absent("misc/k", b"v").unwrap();
    let srv = Server::start(be.clone(), fast_cfg(), "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    be.fail.store(2, Ordering::SeqCst);
    let before = be.calls.load(Ordering::SeqCst);
    let (c, _, b) = http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "");
    assert_eq!((c, b.as_str()), (200, "v"), "two transient failures are absorbed");
    assert_eq!(be.calls.load(Ordering::SeqCst) - before, 3);
    be.fail.store(100, Ordering::SeqCst);
    let before = be.calls.load(Ordering::SeqCst);
    let (c, h, _) = http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "");
    assert_eq!(c, 503);
    assert!(h.contains("retry-after:"), "{h}");
    assert_eq!(be.calls.load(Ordering::SeqCst) - before, 3, "bounded: 1 try + 2 retries");
}

#[test]
fn request_deadline_stops_retrying() {
    let be = Fault::new();
    let mut cfg = fast_cfg();
    cfg.resilience.retries = 1000;
    cfg.resilience.base_backoff = Duration::from_millis(20);
    cfg.resilience.max_backoff = Duration::from_millis(20);
    cfg.resilience.request_deadline = Some(Duration::from_millis(300));
    let srv = Server::start(be.clone(), cfg, "127.0.0.1:0").unwrap();
    be.fail.store(usize::MAX, Ordering::SeqCst);
    let t0 = Instant::now();
    let (c, _, _) = http(srv.local_addr(), "GET", "/v1/o/misc/k", Some(ADMIN), "");
    assert_eq!(c, 503);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
    assert!(be.calls.load(Ordering::SeqCst) < 100);
}

#[test]
fn circuit_breaker_opens_returns_retry_after_and_recovers() {
    let be = Fault::new();
    be.inner.put_if_absent("misc/k", b"v").unwrap();
    let mut cfg = fast_cfg();
    cfg.resilience.retries = 0;
    cfg.resilience.breaker_threshold = 3;
    cfg.resilience.breaker_cooldown = Duration::from_millis(500);
    let srv = Server::start(be.clone(), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    be.fail.store(usize::MAX, Ordering::SeqCst);
    for _ in 0..3 {
        assert_eq!(http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "").0, 503);
    }
    // open: fails fast without touching the backend, with Retry-After
    let calls = be.calls.load(Ordering::SeqCst);
    let (c, h, _) = http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "");
    assert_eq!(c, 503);
    assert!(h.contains("retry-after: 1"), "{h}");
    assert_eq!(be.calls.load(Ordering::SeqCst), calls, "no backend call while open");
    let m = metrics(a);
    assert_eq!(m["vcs_server_backend_circuit_open"], 1.0);
    assert_eq!(m["vcs_server_backend_breaker_trips_total"], 1.0);
    assert!(m["vcs_server_backend_errors_total{kind=\"circuit_open\"}"] >= 1.0);
    // non-backend routes still work
    assert_eq!(http(a, "GET", "/v1/whoami", Some(ADMIN), "").0, 200);
    // backend heals; after the cooldown one probe closes the breaker
    be.fail.store(0, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "").0, 200);
    assert_eq!(metrics(a)["vcs_server_backend_circuit_open"], 0.0);
    assert_eq!(http(a, "GET", "/v1/o/misc/k", Some(ADMIN), "").0, 200);
}

#[test]
fn graceful_shutdown_waits_for_in_flight_requests_and_flushes_audit() {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("a.jsonl");
    let mut cfg = fast_cfg();
    cfg.audit = Some(Arc::new(AuditLog::open(&audit_path, None).unwrap()));
    let mut srv = Server::start(Fault::new(), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    // an in-flight request: headers sent, body still to come
    let mut s = TcpStream::connect(a).unwrap();
    write!(
        s,
        "PUT /v1/o/misc/z HTTP/1.1\r\nAuthorization: Bearer {WRITER}\r\nIf-None-Match: *\r\nContent-Length: 4\r\n\r\nab"
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let finisher = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        s.write_all(b"cd").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    });
    let t0 = Instant::now();
    assert!(srv.shutdown_graceful(Duration::from_secs(5)), "drained within grace");
    assert!(t0.elapsed() >= Duration::from_millis(250));
    assert!(finisher.join().unwrap().starts_with("HTTP/1.1 201"), "in-flight request completed");
    assert!(TcpStream::connect(a).is_err(), "listener closed");
    assert_eq!(vcs_server::audit::verify_file(&audit_path, None).unwrap().records, 1);
}

#[test]
fn graceful_shutdown_gives_up_after_grace() {
    let mut srv = Server::start(Fault::new(), fast_cfg(), "127.0.0.1:0").unwrap();
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\n").unwrap(); // never finishes the headers
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    assert!(!srv.shutdown_graceful(Duration::from_millis(200)));
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[test]
fn clock_is_not_needed_for_unrelated_state() {
    // Config::new defaults: logging to stderr text/info, 2 s readiness cache, breaker on.
    let c = Config::new(table());
    assert_eq!(c.ready_cache, Duration::from_secs(2));
    assert_eq!(c.resilience.breaker_threshold, 5);
    let _ = AtomicU64::new(0);
}
