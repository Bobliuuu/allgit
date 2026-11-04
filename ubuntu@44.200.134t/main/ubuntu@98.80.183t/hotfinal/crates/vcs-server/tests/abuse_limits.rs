//! Security review finding 14: failed-authentication throttle per address, and a per-address
//! connection cap. Both are opt-in (library default off).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use vcs_remote::*;
use vcs_server::{Config, Principal, RateLimit, Role, Server, TokenTable};

fn start(f: impl FnOnce(&mut Config)) -> (Server, String, Arc<AtomicU64>) {
    let table = TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let c = clock.clone();
    cfg.clock = Arc::new(move || c.load(Ordering::SeqCst));
    f(&mut cfg);
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let addr = srv.local_addr().to_string();
    (srv, addr, clock)
}

fn status(addr: &str, tok: &str) -> u16 {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /v1/whoami HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {tok}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.strip_prefix("HTTP/1.1 ")
        .and_then(|r| r.get(..3))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

#[test]
fn failed_auth_is_throttled_per_address_and_recovers() {
    let (_srv, addr, clock) = start(|c| c.auth_fail_limit = Some(RateLimit { burst: 3, per_sec: 1.0 }));
    assert_eq!(status(&addr, "t-alice"), 200);
    for _ in 0..3 {
        assert_eq!(status(&addr, "guess"), 401);
    }
    // blocked: even the right token is refused until the level leaks, so guessing cannot continue
    assert_eq!(status(&addr, "guess"), 429);
    assert_eq!(status(&addr, "t-alice"), 429);
    // liveness probes are not affected
    let mut s = TcpStream::connect(&addr).unwrap();
    write!(s, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut o = String::new();
    s.read_to_string(&mut o).unwrap();
    assert!(o.starts_with("HTTP/1.1 200"), "{o}");
    clock.fetch_add(2_000, Ordering::SeqCst);
    assert_eq!(status(&addr, "t-alice"), 200, "recovered after the level leaked");
}

#[test]
fn throttle_is_off_by_default() {
    let (_srv, addr, _) = start(|_| {});
    for _ in 0..40 {
        assert_eq!(status(&addr, "guess"), 401);
    }
    assert_eq!(status(&addr, "t-alice"), 200);
}

#[test]
fn per_address_connection_cap_refuses_extra_connections_and_releases_slots() {
    let (_srv, addr, _) = start(|c| c.max_conns_per_ip = 2);
    let hold = |a: &str| {
        let mut s = TcpStream::connect(a).unwrap();
        // open a keep-alive connection and finish one request so the server has surely accepted it
        write!(s, "GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut b = [0u8; 12];
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.read_exact(&mut b).unwrap();
        (s, String::from_utf8_lossy(&b).into_owned())
    };
    let (c1, r1) = hold(&addr);
    let (c2, r2) = hold(&addr);
    assert!(r1.starts_with("HTTP/1.1 200") && r2.starts_with("HTTP/1.1 200"));
    let (_c3, r3) = hold(&addr);
    assert!(r3.starts_with("HTTP/1.1 503"), "third connection from one address is refused: {r3}");
    drop(c1);
    drop(c2);
    // slots are released when connections close (poll briefly: the server notices on its own thread)
    let mut ok = false;
    for _ in 0..50 {
        let (_c, r) = hold(&addr);
        if r.starts_with("HTTP/1.1 200") {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ok, "slot never released");
}
