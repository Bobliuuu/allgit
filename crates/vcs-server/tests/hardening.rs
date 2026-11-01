//! Hardening: object hash verification, rate limiting, audit log, token reload and expiry,
//! connection cap, request limits and malformed input.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vcs_objects::Object;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::audit::verify_file;
use vcs_server::{AuditLog, Config, HttpBackend, Principal, RateLimit, Role, Server, TokenTable};

fn who(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}

fn table() -> TokenTable {
    TokenTable::from_entries(vec![("t-alice", who("alice", Role::Write)), ("t-bob", who("bob", Role::Write))])
}

fn start_cfg(cfg: Config) -> (Server, Arc<MemBackend>, String) {
    let mem = Arc::new(MemBackend::new());
    let srv = Server::start(mem.clone(), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, mem, url)
}

/// Send raw bytes, read whatever comes back until close/timeout; returns the status code if any.
fn raw(addr: &str, bytes: &[u8]) -> (Option<u16>, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(bytes);
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let t = String::from_utf8_lossy(&out).into_owned();
    let code = t.strip_prefix("HTTP/1.1 ").and_then(|r| r.get(..3)).and_then(|c| c.parse().ok());
    (code, t)
}

fn addr(url: &str) -> &str {
    url.strip_prefix("http://").unwrap()
}

#[test]
fn objects_put_must_hash_to_key() {
    let (_s, mem, url) = start_cfg(Config::new(table()));
    let c = HttpBackend::new(&url, "t-alice").unwrap();
    let obj = Object::Blob(b"hello".to_vec());
    let key = object_key(&obj.id());
    assert!(c.put_if_absent(&key, &obj.encode()).unwrap());
    // wrong content under another object's key is refused even though the key exists
    let other = Object::Blob(b"evil".to_vec());
    assert!(c.put_if_absent(&key, &other.encode()).unwrap_err().to_string().contains("422"));
    let k2 = format!("objects/ab/{}", "c".repeat(62));
    let e = c.put_if_absent(&k2, &other.encode()).unwrap_err().to_string();
    assert!(e.contains("422") && e.contains("hash mismatch"), "{e}");
    assert_eq!(mem.get(&k2).unwrap(), None, "nothing stored");
    for bad in [
        "objects/x",
        "objects/ab/short",
        &format!("objects/AB/{}", "c".repeat(62)),
        &format!("objects/ab/{}/x", "c".repeat(62)),
    ] {
        let e = c.put_if_absent(bad, b"x").unwrap_err().to_string();
        assert!(e.contains("422"), "{bad}: {e}");
    }
    // CAS path is verified too
    let tag = c.get(&key).unwrap().unwrap().tag;
    assert!(c.put_if_match(&key, &other.encode(), &tag).unwrap_err().to_string().contains("422"));
    // non-objects keys are unaffected
    assert!(c.put_if_absent("tx/whatever", b"free-form").unwrap());
}

#[test]
fn rate_limit_per_principal_with_retry_after() {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let mut cfg = Config::new(table());
    let c2 = clock.clone();
    cfg.clock = Arc::new(move || c2.load(Ordering::SeqCst));
    cfg.rate_limit = Some(RateLimit { burst: 3, per_sec: 1.0 });
    let (_s, _m, url) = start_cfg(cfg);
    #[allow(clippy::result_large_err)]
    let get = |tok: &str| {
        ureq::get(&format!("{url}/v1/whoami"))
            .set("Authorization", &format!("Bearer {tok}"))
            .call()
    };
    for _ in 0..3 {
        assert_eq!(get("t-alice").unwrap().status(), 200);
    }
    match get("t-alice") {
        Err(ureq::Error::Status(429, r)) => assert_eq!(r.header("Retry-After"), Some("1")),
        other => panic!("{other:?}"),
    }
    assert_eq!(get("t-bob").unwrap().status(), 200, "other principals are unaffected");
    // unauthenticated requests do not consume anyone's bucket
    let _ = ureq::get(&format!("{url}/v1/whoami")).call();
    clock.fetch_add(1000, Ordering::SeqCst);
    assert_eq!(get("t-alice").unwrap().status(), 200);
    assert!(matches!(get("t-alice"), Err(ureq::Error::Status(429, _))));
    // the HttpBackend client surfaces 429 as Unavailable
    let c = HttpBackend::new(&url, "t-alice").unwrap();
    assert!(matches!(c.get("k/x"), Err(BackendError::Unavailable(m)) if m.contains("429")));
}

#[test]
fn audit_log_records_every_authenticated_put_and_detects_tampering() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("audit.jsonl");
    let mut cfg = Config::new(table());
    cfg.audit = Some(Arc::new(AuditLog::open(&path, None).unwrap()));
    let clock = Arc::new(AtomicU64::new(42));
    let c2 = clock.clone();
    cfg.clock = Arc::new(move || c2.load(Ordering::SeqCst));
    let (_s, _m, url) = start_cfg(cfg);
    let c = HttpBackend::new(&url, "t-alice").unwrap();
    c.put_if_absent("k/one", b"1").unwrap(); // 201
    c.put_if_absent("k/one", b"2").unwrap(); // 412
    let _ = c.get("k/one"); // reads are not audited
    let _ = ureq::put(&format!("{url}/v1/o/k/two"))
        .set("Authorization", "Bearer t-bob")
        .send_bytes(b"x"); // 428
    let _ = ureq::put(&format!("{url}/v1/o/k/three")).send_bytes(b"x"); // unauthenticated: not audited
    let _ = HttpBackend::new(&url, "bad-token").unwrap().put_if_absent("k/four", b"x"); // 401: not audited
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].contains("\"principal\":\"alice\"") && lines[0].contains("\"key\":\"k/one\"") && lines[0].contains("\"outcome\":\"201\"")
    );
    assert!(lines[0].contains("\"ts_ms\":42") && lines[0].contains("\"op\":\"PUT\""));
    assert!(lines[1].contains("\"outcome\":\"412\""));
    assert!(lines[2].contains("\"principal\":\"bob\"") && lines[2].contains("\"outcome\":\"428\""));
    assert_eq!(verify_file(&path, None).unwrap().records, 3);
    // tamper: change the principal on a record
    std::fs::write(&path, text.replace("\"bob\"", "\"alice\"")).unwrap();
    assert!(verify_file(&path, None).is_err());
}

#[test]
fn audit_verify_cli_exit_codes() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("a.jsonl");
    let l = AuditLog::open(&path, None).unwrap();
    l.record(1, "alice", "PUT", "k", "201").unwrap();
    l.record(2, "alice", "PUT", "k2", "201").unwrap();
    let bin = env!("CARGO_BIN_EXE_vcs-server");
    let run = |p: &std::path::Path| std::process::Command::new(bin).args(["audit", "verify"]).arg(p).output().unwrap();
    let ok = run(&path);
    assert!(ok.status.success(), "{ok:?}");
    assert!(String::from_utf8_lossy(&ok.stdout).contains("2 records"));
    let t = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, t.replace("\"k2\"", "\"k3\"")).unwrap();
    let bad = run(&path);
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("line 2"), "{bad:?}");
}

#[test]
fn token_reload_from_file_and_expiry() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("tokens");
    let h = |t: &str| vcs_server::auth::hash_token(t);
    std::fs::write(&f, format!("alice write * {}\\n", h("old")).replace("\\n", "\n")).unwrap();
    let mut cfg = Config::new(TokenTable::parse(&std::fs::read_to_string(&f).unwrap()).unwrap());
    cfg.tokens_file = Some(f.clone());
    cfg.token_reload_interval = Duration::from_millis(50);
    let (_s, _m, url) = start_cfg(cfg);
    let ok = |t: &str| HttpBackend::new(&url, t).unwrap().whoami();
    assert!(ok("old").is_ok());
    assert!(ok("new").unwrap_err().to_string().contains("401"));
    // rotate: revoke old, add new
    std::fs::write(&f, format!("alice write * {}\n# rotated\n", h("new"))).unwrap();
    let t0 = Instant::now();
    while ok("new").is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "reload never happened");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        ok("old").unwrap_err().to_string().contains("401"),
        "revoked token must stop working"
    );
    // a broken file keeps the previous table
    std::fs::write(&f, "this is not a tokens file at all\n").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(ok("new").is_ok());
    // an empty file is ignored too
    std::fs::write(&f, "# nothing\n").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(ok("new").is_ok());
}

#[test]
fn reload_tokens_api_and_expiry_with_injected_clock() {
    let clock = Arc::new(AtomicU64::new(1_000_000)); // ms
    let mut cfg = Config::new(TokenTable::from_entries_expiring(vec![
        ("short", who("sam", Role::Write), Some(1_010)), // expires at 1_010_000 ms
        ("forever", who("fay", Role::Write), None),
    ]));
    let c2 = clock.clone();
    cfg.clock = Arc::new(move || c2.load(Ordering::SeqCst));
    let (s, _m, url) = start_cfg(cfg);
    let w = |t: &str| HttpBackend::new(&url, t).unwrap().whoami();
    assert!(w("short").is_ok());
    clock.store(1_009_999, Ordering::SeqCst);
    assert!(w("short").is_ok());
    clock.store(1_010_000, Ordering::SeqCst);
    let e = w("short").unwrap_err().to_string();
    assert!(e.contains("401") && e.contains("expired"), "{e}");
    assert!(w("forever").is_ok());
    // immediate reload
    s.reload_tokens(TokenTable::from_entries(vec![("fresh", who("fran", Role::Read))]));
    assert!(w("forever").unwrap_err().to_string().contains("401"));
    assert_eq!(w("fresh").unwrap(), "fran read *");
    // file syntax
    let h = vcs_server::auth::hash_token("x");
    let t = TokenTable::parse(&format!("a write * {h} expires=100\n")).unwrap();
    assert!(t.authenticate_at("x", 99_999).is_ok());
    assert_eq!(t.authenticate_at("x", 100_000), Err(vcs_server::auth::AuthError::Expired));
    assert!(TokenTable::parse(&format!("a write * {h} expires=soon\n")).is_err());
    assert!(TokenTable::parse(&format!("a write * {h} nope=1\n")).is_err());
}

#[test]
fn connection_cap_refuses_extra_connections_then_recovers() {
    let mut cfg = Config::new(table());
    cfg.max_connections = 2;
    cfg.header_timeout = Duration::from_secs(20);
    let (s, _m, url) = start_cfg(cfg);
    // two idle connections that never send a request occupy the slots
    let hold: Vec<TcpStream> = (0..2).map(|_| TcpStream::connect(addr(&url)).unwrap()).collect();
    let t0 = Instant::now();
    while s.active_connections() < 2 {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    let (code, resp) = raw(addr(&url), b"GET /healthz HTTP/1.1\r\n\r\n");
    assert_eq!(code, Some(503), "{resp}");
    assert!(resp.to_ascii_lowercase().contains("retry-after"));
    drop(hold);
    let t0 = Instant::now();
    loop {
        let (code, _) = raw(addr(&url), b"GET /healthz HTTP/1.1\r\n\r\n");
        if code == Some(200) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "slots never freed");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn size_limits_give_clean_errors() {
    let mut cfg = Config::new(table());
    cfg.max_body = 1000;
    cfg.max_manifest_body = 100;
    cfg.max_header_bytes = 2048;
    let (_s, mem, url) = start_cfg(cfg);
    let a = addr(&url);
    let put = |key: &str, n: usize| {
        let mut r = format!("PUT /v1/o/{key} HTTP/1.1\r\nAuthorization: Bearer t-alice\r\nIf-None-Match: *\r\nContent-Length: {n}\r\n\r\n")
            .into_bytes();
        r.extend(std::iter::repeat_n(b'x', n));
        raw(a, &r).0
    };
    assert_eq!(put("k/ok", 1000), Some(201));
    assert_eq!(put("k/big", 1001), Some(413));
    assert_eq!(mem.get("k/big").unwrap(), None);
    assert_eq!(put("manifest", 101), Some(413), "manifest has its own, smaller limit");
    // announced huge body, none sent: rejected from the header alone, no allocation, no stall
    let t0 = Instant::now();
    let (code, _) = raw(
        a,
        b"PUT /v1/o/k/h HTTP/1.1\r\nAuthorization: Bearer t-alice\r\nIf-None-Match: *\r\nContent-Length: 99999999999\r\n\r\n",
    );
    assert_eq!(code, Some(413));
    assert!(t0.elapsed() < Duration::from_secs(5));
    // one huge header line, and many medium ones exceeding the total
    let long = format!("GET /healthz HTTP/1.1\r\nX-Big: {}\r\n\r\n", "a".repeat(5000));
    assert_eq!(raw(a, long.as_bytes()).0, Some(431));
    let mut many = String::from("GET /healthz HTTP/1.1\r\n");
    for i in 0..60 {
        many.push_str(&format!("X-{i}: {}\r\n", "b".repeat(100)));
    }
    many.push_str("\r\n");
    assert_eq!(raw(a, many.as_bytes()).0, Some(431));
    let long_line = format!("GET /{} HTTP/1.1\r\n\r\n", "p".repeat(4000));
    assert_eq!(raw(a, long_line.as_bytes()).0, Some(431));
    // too many headers
    let mut hdrs = String::from("GET /healthz HTTP/1.1\r\n");
    for i in 0..150 {
        hdrs.push_str(&format!("A{i}: 1\r\n"));
    }
    hdrs.push_str("\r\n");
    assert_eq!(raw(a, hdrs.as_bytes()).0, Some(431));
    assert_eq!(raw(a, b"GET /healthz HTTP/1.1\r\n\r\n").0, Some(200), "server still healthy");
}

#[test]
fn slowloris_stalls_time_out() {
    let mut cfg = Config::new(table());
    cfg.header_timeout = Duration::from_millis(500);
    cfg.io_timeout = Duration::from_secs(30);
    cfg.body_timeout = Duration::from_millis(700);
    let (s, _m, url) = start_cfg(cfg);
    let a = addr(&url);
    // 1. connect and say nothing
    let t0 = Instant::now();
    let (code, _) = raw(a, b"");
    assert!(t0.elapsed() < Duration::from_secs(5), "silent connection must be dropped");
    assert!(code.is_none() || code == Some(408));
    // 2. drip one header byte every 100 ms: the per-read timeout never fires, the header deadline does
    let t0 = Instant::now();
    let mut c = TcpStream::connect(a).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for b in b"GET /healthz HTTP/1.1\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
        if c.write_all(&[*b]).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        if t0.elapsed() > Duration::from_secs(4) {
            break;
        }
    }
    let mut out = String::new();
    let _ = c.read_to_string(&mut out);
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "drip-feeding must hit the overall header deadline"
    );
    assert!(out.is_empty() || out.starts_with("HTTP/1.1 408"), "{out}");
    // 3. complete headers, announce a body, stall
    let t0 = Instant::now();
    let (code, _) = raw(
        a,
        b"PUT /v1/o/k/s HTTP/1.1\r\nAuthorization: Bearer t-alice\r\nIf-None-Match: *\r\nContent-Length: 10\r\n\r\nabc",
    );
    assert_eq!(code, Some(408));
    assert!(t0.elapsed() < Duration::from_secs(5));
    // all those connections were released
    let t0 = Instant::now();
    while s.active_connections() > 0 {
        assert!(t0.elapsed() < Duration::from_secs(5), "leaked connections");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(raw(a, b"GET /healthz HTTP/1.1\r\n\r\n").0, Some(200));
}

#[test]
fn malformed_requests_never_panic_and_never_authenticate() {
    let mut cfg = Config::new(table());
    cfg.header_timeout = Duration::from_secs(1);
    cfg.body_timeout = Duration::from_secs(1);
    let (s, _m, url) = start_cfg(cfg);
    let a = addr(&url);
    let cases: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"\r\n\r\n".to_vec(),
        b"GARBAGE\r\n\r\n".to_vec(),
        b"GET\r\n\r\n".to_vec(),
        b"GET /healthz\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1 extra\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/9.9\r\n\r\n".to_vec(),
        b"get /healthz HTTP/1.1\r\n\r\n".to_vec(),
        b"GET healthz HTTP/1.1\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1\r\nno-colon-header\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1\r\n: empty-name\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1\r\nBad Name: x\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1\r\nX: \xff\xfe\r\n\r\n".to_vec(),
        b"GET /healthz HTTP/1.1\r\nX: a\0b\r\n\r\n".to_vec(),
        b"GET /\xff\xfe HTTP/1.1\r\n\r\n".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: -1\r\n\r\n".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: +5\r\n\r\nhello".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello!".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: abc\r\n\r\n".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: 99999999999999999999999\r\n\r\n".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n".to_vec(),
        b"PUT /v1/o/k HTTP/1.1\r\nContent-Length: 50\r\n\r\nshort".to_vec(),
        b"GET /v1/o/%zz HTTP/1.1\r\nAuthorization: Bearer t-alice\r\n\r\n".to_vec(),
        b"GET /v1/o/%ff HTTP/1.1\r\nAuthorization: Bearer t-alice\r\n\r\n".to_vec(),
        b"GET /v1/o/../../etc/passwd HTTP/1.1\r\nAuthorization: Bearer t-alice\r\n\r\n".to_vec(),
        b"GET /v1/o/k?offset=a&len=b HTTP/1.1\r\nAuthorization: Bearer t-alice\r\n\r\n".to_vec(),
        b"GET /v1/o/k?offset=99999999999999999999999&len=1 HTTP/1.1\r\nAuthorization: Bearer t-alice\r\n\r\n".to_vec(),
        b"GET /v1/whoami HTTP/1.1\r\nAuthorization: Bearer\r\n\r\n".to_vec(),
        b"GET /v1/whoami HTTP/1.1\r\nAuthorization: Basic dDph\r\n\r\n".to_vec(),
        b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03 (a TLS hello sent to plain HTTP)".to_vec(),
        vec![0u8; 5000],
        (0..=255u8).cycle().take(20000).collect(),
    ];
    for (i, c) in cases.iter().enumerate() {
        let (code, resp) = raw(a, c);
        if let Some(code) = code {
            assert!((400..600).contains(&code) || code == 200, "case {i}: {code} {resp}");
            assert!(code != 200 || c.starts_with(b"GET /healthz"), "case {i} must not succeed: {resp}");
        }
    }
    // still alive and correct afterwards, and no thread/slot leak
    assert_eq!(raw(a, b"GET /healthz HTTP/1.1\r\n\r\n").0, Some(200));
    assert_eq!(HttpBackend::new(&url, "t-alice").unwrap().whoami().unwrap(), "alice write *");
    let t0 = Instant::now();
    while s.active_connections() > 0 {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
}
