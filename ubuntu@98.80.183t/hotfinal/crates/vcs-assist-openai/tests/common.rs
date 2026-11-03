#![allow(dead_code)]
//! A tiny scripted HTTP server on 127.0.0.1 standing in for the OpenAI-compatible API. No real network.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const KEY: &str = "sk-test-SECRET-0123456789";

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay_ms: u64,
    pub headers: Vec<(String, String)>,
}

pub fn ok_text(text: &str) -> Reply {
    ok_usage(text, 100, 50)
}

pub fn ok_usage(text: &str, p: u64, c: u64) -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({"id":"x","object":"chat.completion","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":p,"completion_tokens":c,"total_tokens":p+c}})
            .to_string(),
        delay_ms: 0,
        headers: vec![],
    }
}

pub fn status(code: u16, body: &str) -> Reply {
    Reply {
        status: code,
        body: body.into(),
        delay_ms: 0,
        headers: vec![],
    }
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub headers: String,
    pub body: String,
}

pub struct Mock {
    pub url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    addr: String,
}

impl Mock {
    /// Replies are served in order; the last one repeats.
    pub fn start(replies: Vec<Reply>) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0;
            for conn in l.incoming() {
                if st2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut c) = conn else { continue };
                let r = replies[n.min(replies.len() - 1)].clone();
                n += 1;
                let s3 = s2.clone();
                std::thread::spawn(move || serve(&mut c, &r, &s3));
            }
        });
        Mock {
            url: format!("http://{addr}/v1/chat/completions"),
            seen,
            stop,
            addr,
        }
    }
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
    }
}

fn serve(c: &mut TcpStream, r: &Reply, seen: &Mutex<Vec<Seen>>) {
    c.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let (head_end, len) = loop {
        let Ok(n) = c.read(&mut chunk) else { return };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            break (p + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let Ok(n) = c.read(&mut chunk) else { return };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    seen.lock().unwrap().push(Seen {
        headers: String::from_utf8_lossy(&buf[..head_end]).into_owned(),
        body: String::from_utf8_lossy(&buf[head_end..]).into_owned(),
    });
    std::thread::sleep(Duration::from_millis(r.delay_ms));
    let mut out = format!(
        "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
        r.status,
        r.body.len()
    );
    for (k, v) in &r.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&r.body);
    let _ = c.write_all(out.as_bytes());
}

pub fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs-assist-openai"));
    for k in [
        "OPENAI_API_KEY",
        "VCS_ASSIST_MODEL",
        "VCS_ASSIST_API_URL",
        "VCS_ASSIST_API_TIMEOUT_SECS",
        "VCS_ASSIST_MAX_RETRIES",
        "VCS_ASSIST_BACKOFF_MS",
        "VCS_ASSIST_MAX_REQUESTS",
        "VCS_ASSIST_MAX_TOTAL_TOKENS",
        "VCS_ASSIST_MAX_OUTPUT_TOKENS",
        "VCS_ASSIST_TOKEN_PARAM",
    ] {
        c.env_remove(k);
    }
    c
}

/// Run the assistant against `url` with the given request JSON; fast backoff by default.
pub fn run(url: &str, req: &str, extra: &[(&str, &str)]) -> Output {
    let mut c = bin();
    c.env("OPENAI_API_KEY", KEY)
        .env("VCS_ASSIST_API_URL", url)
        .env("VCS_ASSIST_BACKOFF_MS", "10");
    for (k, v) in extra {
        c.env(k, v);
    }
    let mut ch = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    ch.stdin.take().unwrap().write_all(req.as_bytes()).unwrap();
    ch.wait_with_output().unwrap()
}

pub const BASE: &str = "fn a() -> i32 {\n    1\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n";
pub const OURS: &str = "fn a() -> i32 {\n    10\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n";
pub const THEIRS: &str = "fn a() -> i32 {\n    11\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    30\n}\n";
pub const GOOD: &str = "fn a() -> i32 {\n    21\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    30\n}\n";

pub fn request(path: &str, base: Option<&str>, ours: &str, theirs: &str) -> String {
    serde_json::json!({"version":1,"path":path,"language":"rust","base":base,"ours":ours,"theirs":theirs}).to_string()
}

pub fn good_request() -> String {
    request("src/a.rs", Some(BASE), OURS, THEIRS)
}

pub fn resolved(file: &str) -> String {
    format!("<explanation>merged</explanation>\n<resolved_file>\n{file}\n</resolved_file>")
}

pub fn all_output(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
