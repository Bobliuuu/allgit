//! #566: /readyz fails while S3 answers 403 (expired credentials) and recovers when requests succeed again.
//! Single test: the rejected-credentials flag is process-wide.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;
use vcs_server::{Config, Server, TokenTable};
use vcs_store_s3::{S3Backend, S3Config};

fn fake_s3(deny: Arc<AtomicBool>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let deny = deny.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let r = if deny.load(SeqCst) {
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    url
}

fn readyz(addr: std::net::SocketAddr) -> u16 {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(s, "GET /readyz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out[9..12].parse().unwrap()
}

#[test]
fn readyz_fails_on_403_from_expired_credentials_and_recovers() {
    let deny = Arc::new(AtomicBool::new(true));
    let url = fake_s3(deny.clone());
    let mut c = S3Config::new(&url, "b", "AK", "SK");
    c.retries = 0;
    let backend = S3Backend::new_unchecked(c).unwrap();
    let mut cfg = Config::new(TokenTable::from_entries(vec![]));
    cfg.ready_cache = Duration::from_millis(0);
    let srv = Server::start(Arc::new(backend), cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr();
    assert_eq!(readyz(a), 503, "403 on the manifest probe means every call would fail");
    deny.store(false, SeqCst);
    assert_eq!(readyz(a), 200);
}
