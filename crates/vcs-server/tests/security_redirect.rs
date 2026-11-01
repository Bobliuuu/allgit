//! Security regression (docs/security-review.md): the bearer token must not follow a redirect.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use vcs_remote::*;
use vcs_server::HttpBackend;

/// A redirect must never carry the bearer token to another host.
#[test]
fn http_client_never_forwards_the_token_on_redirect() {
    let evil = TcpListener::bind("127.0.0.1:0").unwrap();
    let evil_addr = evil.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in evil.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            s2.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let front = TcpListener::bind("127.0.0.1:0").unwrap();
    let front_addr = front.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in front.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let r = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{evil_addr}/v1/o/k/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = c.write_all(r.as_bytes());
        }
    });
    let c = HttpBackend::new(&format!("http://{front_addr}"), "secret-token-123").unwrap();
    let _ = c.get("k/x");
    let _ = c.put_if_absent("k/x", b"data");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let got = seen.lock().unwrap().clone();
    assert!(!got.contains("secret-token-123"), "token leaked to redirect target:\n{got}");
}
