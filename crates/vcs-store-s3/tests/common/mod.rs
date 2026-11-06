//! Scripted fault-injecting HTTP server shared by the retry and credential tests.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vcs_store_s3::S3Config;

#[derive(Clone)]
pub enum Step {
    /// Status line, extra headers, body.
    Reply(&'static str, &'static str, &'static str),
    /// Read the request, then say nothing for this long (a stalled connection).
    Stall(Duration),
}

pub struct Script {
    pub url: String,
    pub seen: Arc<Mutex<Vec<String>>>,
    pub hits: Arc<AtomicUsize>,
}

/// Replies follow `steps` in order; once exhausted the last step repeats.
pub fn scripted(steps: Vec<Step>) -> Script {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let hits = Arc::new(AtomicUsize::new(0));
    let (s2, h2) = (seen.clone(), hits.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let (seen, hits, steps) = (s2.clone(), h2.clone(), steps.clone());
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let Ok(n) = s.read(&mut buf) else { return };
                    if n == 0 {
                        return;
                    }
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_lowercase();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text[..h]
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if data.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&data).into_owned());
                let i = hits.fetch_add(1, SeqCst);
                match &steps[i.min(steps.len() - 1)] {
                    Step::Reply(status, headers, body) => {
                        let r = format!(
                            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = s.write_all(r.as_bytes());
                    }
                    Step::Stall(d) => std::thread::sleep(*d),
                }
            });
        }
    });
    Script { url, seen, hits }
}

pub fn cfg(url: &str) -> S3Config {
    let mut c = S3Config::new(url, "b", "AKTEST", "secret");
    c.timeout = Duration::from_secs(5);
    c.retry_base = Duration::from_millis(5);
    c.retry_max = Duration::from_millis(40);
    c
}
