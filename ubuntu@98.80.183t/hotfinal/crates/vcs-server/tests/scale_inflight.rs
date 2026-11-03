//! Scalability limits: the server-wide budget of request-body bytes in flight (503 + Retry-After instead of
//! unbounded memory), and a high-connection-count smoke test.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vcs_remote::*;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

/// The descriptor-exhaustion test lowers a process-wide limit, so the tests here run one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn start(f: impl FnOnce(&mut Config)) -> (Server, String) {
    let table = TokenTable::from_entries(vec![(
        "t-a",
        Principal {
            name: "a".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    f(&mut cfg);
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr().to_string();
    (srv, a)
}

fn put_head(len: usize) -> String {
    format!("PUT /v1/o/objects/zz HTTP/1.1\r\nAuthorization: Bearer t-a\r\nContent-Length: {len}\r\n\r\n")
}

fn status_of(addr: &str, req: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let _ = s.write_all(req.as_bytes());
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    let t = String::from_utf8_lossy(&out).into_owned();
    (t.get(9..12).and_then(|c| c.parse().ok()).unwrap_or(0), t)
}

#[test]
fn inflight_body_budget_sheds_load_with_retry_after_and_recovers() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (srv, addr) = start(|c| {
        c.max_inflight_body = 1 << 20;
        c.body_timeout = Duration::from_secs(30);
    });
    // A announces 800 KiB and stalls after the headers: its reservation is held.
    let mut a = TcpStream::connect(&addr).unwrap();
    a.write_all(put_head(800 << 10).as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (code, text) = status_of(&addr, &(put_head(800 << 10) + "Connection: close\r\n"));
    assert_eq!(code, 503, "{text}");
    assert!(text.to_ascii_lowercase().contains("retry-after"), "{text}");
    // small requests still pass (health) while the budget is full
    let (code, _) = status_of(&addr, "GET /healthz HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert_eq!(code, 200);
    drop(a);
    let t0 = Instant::now();
    loop {
        let mut req = put_head(800 << 10).replace("\r\n\r\n", "\r\nConnection: close\r\n\r\n");
        req.push_str(&"x".repeat(800 << 10));
        let (code, _) = status_of(&addr, &req);
        // a full body that does not hash to its key is refused with 4xx, never 503
        if code != 503 {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "budget never released");
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(srv);
}

#[test]
fn many_idle_connections_do_not_block_requests_and_cap_holds() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (srv, addr) = start(|c| {
        c.max_connections = 300;
        c.header_timeout = Duration::from_secs(30);
    });
    let hold: Vec<TcpStream> = (0..250).filter_map(|_| TcpStream::connect(&addr).ok()).collect();
    let t0 = Instant::now();
    while srv.active_connections() < hold.len() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    let t = Instant::now();
    for _ in 0..20 {
        let (code, _) = status_of(&addr, "GET /healthz HTTP/1.1\r\nConnection: close\r\n\r\n");
        assert_eq!(code, 200);
    }
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "requests slowed by idle connections: {:?}",
        t.elapsed()
    );
    drop(hold);
}

/// With the descriptor table full, `accept` fails with EMFILE and leaves the connection queued. The accept
/// loop must back off, not spin a core.
#[cfg(unix)]
#[test]
fn accept_loop_does_not_spin_when_descriptors_are_exhausted() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (srv, addr) = start(|_| {});
    let cpu = || unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1e6 + ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1e6
    };
    // Fill the descriptor table: lower the limit just above what is open now, then open files.
    let mut files = vec![];
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    let old = lim;
    lim.rlim_cur = 256.min(old.rlim_max);
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    while let Ok(f) = std::fs::File::open("/dev/null") {
        files.push(f);
    }
    // Free exactly one descriptor for the client socket; the server's accept() then has none left.
    files.pop();
    let c = std::net::TcpStream::connect(&addr);
    let c0 = cpu();
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_millis(1000));
    let used = cpu() - c0;
    let wall = t0.elapsed().as_secs_f64();
    drop(files);
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &old) };
    drop(c);
    drop(srv);
    assert!(
        used < 0.3 * wall,
        "accept loop burned {used:.2}s CPU in {wall:.2}s while out of descriptors"
    );
}
