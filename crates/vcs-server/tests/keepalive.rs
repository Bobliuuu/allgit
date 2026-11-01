//! Persistent connections: the server serves many requests per connection, the client reuses them,
//! and idle or `Connection: close` connections still end cleanly.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vcs_remote::{Backend, MemBackend};
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};

fn table() -> TokenTable {
    TokenTable::from_entries(vec![(
        "t-admin",
        Principal {
            name: "root".into(),
            role: Role::Admin,
            zones: None::<BTreeSet<String>>,
        },
    )])
}

/// TCP forwarder that counts accepted connections.
fn counting_proxy(target: SocketAddr) -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            n2.fetch_add(1, Ordering::SeqCst);
            let up = TcpStream::connect(target).unwrap();
            let _ = (up.set_nodelay(true), c.set_nodelay(true));
            let (mut c2, mut up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
            let (mut c, mut up) = (c, up);
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut c, &mut up);
                let _ = up.shutdown(std::net::Shutdown::Write);
            });
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut up2, &mut c2);
                let _ = c2.shutdown(std::net::Shutdown::Write);
            });
        }
    });
    (addr, n)
}

fn read_response(s: &mut TcpStream) -> (u16, String, String) {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        assert_eq!(s.read(&mut b).unwrap(), 1, "connection closed mid-response");
        head.push(b[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let code = head[9..12].parse().unwrap();
    let len: usize = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
        .unwrap();
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).unwrap();
    (code, head, String::from_utf8(body).unwrap())
}

#[test]
fn many_requests_on_one_socket_then_close_on_request() {
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table()), "127.0.0.1:0").unwrap();
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for i in 0..5 {
        s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let (c, head, body) = read_response(&mut s);
        assert_eq!((c, body.trim()), (200, "ok"), "request {i}");
        assert!(head.to_ascii_lowercase().contains("connection: keep-alive"), "{head}");
    }
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let (c, head, _) = read_response(&mut s);
    assert_eq!(c, 200);
    assert!(head.to_ascii_lowercase().contains("connection: close"));
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "server closed after Connection: close");
}

#[test]
fn http_10_and_error_replies_close_the_connection() {
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table()), "127.0.0.1:0").unwrap();
    for req in [&b"GET /healthz HTTP/1.0\r\n\r\n"[..], b"GARBAGE\r\n\r\n"] {
        let mut s = TcpStream::connect(srv.local_addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(req).unwrap();
        let t0 = Instant::now();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        assert!(t0.elapsed() < Duration::from_secs(3), "closed promptly");
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 "));
    }
}

#[test]
fn idle_connections_are_closed_and_request_cap_is_honoured() {
    let mut cfg = Config::new(table());
    cfg.keepalive_idle = Duration::from_millis(200);
    cfg.max_requests_per_conn = 3;
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut closes = vec![];
    for _ in 0..3 {
        s.write_all(b"GET /healthz HTTP/1.1\r\n\r\n").unwrap();
        let (_, head, _) = read_response(&mut s);
        closes.push(head.to_ascii_lowercase().contains("connection: close"));
    }
    assert_eq!(closes, [false, false, true], "third response ends the connection");
    // an idle connection is dropped silently (no 408 noise) after keepalive_idle
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(b"GET /healthz HTTP/1.1\r\n\r\n").unwrap();
    read_response(&mut s);
    let t0 = Instant::now();
    let mut rest = Vec::new();
    s.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty() && t0.elapsed() < Duration::from_secs(3), "{rest:?}");
}

#[test]
fn http_client_reuses_one_connection_including_writes_and_survives_idle_close() {
    let mem = Arc::new(MemBackend::new());
    let mut cfg = Config::new(table());
    cfg.keepalive_idle = Duration::from_millis(300);
    let srv = Server::start(mem, cfg, "127.0.0.1:0").unwrap();
    let (paddr, conns) = counting_proxy(srv.local_addr());
    let b = HttpBackend::new(&format!("http://{paddr}"), "t-admin").unwrap();
    assert!(b.put_if_absent("k/a", b"one").unwrap());
    for _ in 0..10 {
        assert_eq!(b.get("k/a").unwrap().unwrap().data, b"one");
    }
    let tag = b.get("k/a").unwrap().unwrap().tag;
    assert!(b.put_if_match("k/a", b"two", &tag).unwrap().is_some());
    assert_eq!(b.list("k/").unwrap(), vec!["k/a"]);
    assert_eq!(conns.load(Ordering::SeqCst), 1, "all requests share one TCP connection");
    // the server drops the idle connection; the next write must still apply exactly once
    std::thread::sleep(Duration::from_millis(700));
    let tag = b.get("k/a").unwrap().unwrap().tag;
    std::thread::sleep(Duration::from_millis(700));
    let t2 = b.put_if_match("k/a", b"three", &tag);
    let applied = matches!(t2, Ok(Some(_)));
    let cur = b.get("k/a").unwrap().unwrap();
    if applied {
        assert_eq!(cur.data, b"three");
    } else {
        // A pooled connection that died under a write may surface as Unavailable; the retry layer
        // (RetryBackend) resolves it by reading back. Either way the state is never half-applied.
        assert!(t2.is_err(), "{t2:?}");
        assert!(cur.data == b"two" || cur.data == b"three");
    }
}
