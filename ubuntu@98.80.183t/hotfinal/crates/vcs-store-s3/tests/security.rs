//! Security regressions (docs/security-review.md): S3 credentials must not leak through `Debug`
//! output or a redirect.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vcs_remote::Backend;
use vcs_store_s3::{S3Backend, S3Config};

#[test]
fn config_debug_output_redacts_secrets() {
    let mut cfg = S3Config::new("http://127.0.0.1:9", "bkt", "AKIAEXAMPLEKEYID", "sup3r-secret-key");
    cfg.session_token = Some("session-token-xyz".into());
    let shown = format!("{cfg:?}");
    assert!(!shown.contains("sup3r-secret-key"), "{shown}");
    assert!(!shown.contains("session-token-xyz"), "{shown}");
    assert!(shown.contains("bkt"), "non-secret fields stay visible: {shown}");
}

#[test]
fn redirect_does_not_carry_the_session_token_elsewhere() {
    let evil = TcpListener::bind("127.0.0.1:0").unwrap();
    let evil_addr = evil.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for conn in evil.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut buf = [0u8; 8192];
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
            let mut buf = [0u8; 8192];
            let _ = c.read(&mut buf);
            let r = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{evil_addr}/bkt/k\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = c.write_all(r.as_bytes());
        }
    });
    let mut cfg = S3Config::new(&format!("http://{front_addr}"), "bkt", "AKIAEXAMPLEKEYID", "sup3r-secret-key");
    cfg.session_token = Some("session-token-xyz".into());
    cfg.timeout = Duration::from_secs(5);
    let b = S3Backend::new_unchecked(cfg).unwrap();
    let _ = b.get("k");
    std::thread::sleep(Duration::from_millis(200));
    let got = seen.lock().unwrap().clone();
    assert!(got.is_empty(), "client followed a redirect carrying signed headers:\n{got}");
}
