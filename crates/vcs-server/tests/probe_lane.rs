//! #568: health and readiness probes must be answered while the connection cap is full.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;
use vcs_remote::MemBackend;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    (out.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0), out)
}

#[test]
fn probes_answer_when_the_connection_cap_is_full_but_other_routes_do_not() {
    let t = TokenTable::from_entries(vec![(
        "t",
        Principal {
            name: "root".into(),
            role: Role::Admin,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(t);
    cfg.max_connections = 4;
    cfg.probe_reserve = 2;
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let addr = srv.local_addr();
    // fill the cap with idle sockets
    let idle: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(addr).unwrap()).collect();
    let t0 = std::time::Instant::now();
    while srv.active_connections() < 4 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(srv.active_connections(), 4);
    assert_eq!(get(addr, "/healthz").0, 200);
    assert_eq!(get(addr, "/readyz").0, 200);
    // anything else on the overflow lane is refused
    assert_eq!(get(addr, "/v1/o/manifest").0, 503);
    drop(idle);
}
