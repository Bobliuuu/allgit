//! The real `vcs-server` binary: signals, config file, JSON logs (unix only).
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "proc-test-SECRET-token-0123456789";

struct Srv {
    child: Child,
    addr: String,
    stderr: Vec<String>,
    rx: std::sync::mpsc::Receiver<String>,
    dir: tempfile::TempDir,
}

fn spawn(extra: &[&str], config: Option<&str>) -> Srv {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin admin * {}\n", vcs_server::auth::hash_token(TOKEN))).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vcs-server"));
    cmd.env_remove("RUST_LOG").env_remove("VCS_SERVER_LOG");
    match config {
        Some(c) => {
            let cfg = dir.path().join("server.toml");
            std::fs::write(&cfg, c.replace("@DIR", dir.path().to_str().unwrap())).unwrap();
            cmd.arg("--config").arg(&cfg);
        }
        None => {
            cmd.args(["--root", dir.path().join("data").to_str().unwrap(), "--listen", "127.0.0.1:0"]);
        }
    }
    cmd.args(["--tokens", tokens.to_str().unwrap()]);
    for a in extra {
        cmd.arg(a.replace("@DIR", dir.path().to_str().unwrap()));
    }
    let mut child = cmd.stderr(Stdio::piped()).spawn().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let err = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(err).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    let mut stderr = vec![];
    let addr = loop {
        let l = rx.recv_timeout(Duration::from_secs(10)).expect("server did not start");
        stderr.push(l.clone());
        if let Some(i) = l.find("http://") {
            let rest = &l[i + 7..];
            break rest.split(['"', ' ']).next().unwrap().to_owned();
        }
    };
    Srv {
        child,
        addr,
        stderr,
        rx,
        dir,
    }
}

impl Srv {
    fn get(&self, path: &str) -> String {
        let mut s = TcpStream::connect(&self.addr).unwrap();
        write!(
            s,
            "GET {path} HTTP/1.1\r\nConnection: close\r\nAuthorization: Bearer {TOKEN}\r\n\r\n"
        )
        .unwrap();
        let mut o = String::new();
        s.read_to_string(&mut o).unwrap();
        o
    }
    fn kill(&self, sig: &str) {
        assert!(Command::new("kill")
            .args([sig, &self.child.id().to_string()])
            .status()
            .unwrap()
            .success());
    }
    fn wait_exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let t0 = Instant::now();
        while t0.elapsed() < within {
            if let Some(s) = self.child.try_wait().unwrap() {
                while let Ok(l) = self.rx.try_recv() {
                    self.stderr.push(l);
                }
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("did not exit in {within:?}");
    }
}

impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn raw_put(addr: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "PUT /v1/o/misc/a HTTP/1.1\r\nConnection: close\r\nAuthorization: Bearer {TOKEN}\r\nIf-None-Match: *\r\nContent-Length: 2\r\n\r\nhi"
    )
    .unwrap();
    let mut o = String::new();
    s.read_to_string(&mut o).unwrap();
    o
}

#[test]
fn sigterm_and_sigint_exit_zero_and_flush_audit() {
    for sig in ["-TERM", "-INT"] {
        let mut s = spawn(&["--log-format", "json", "--audit-log", "@DIR/audit.jsonl"], None);
        assert!(raw_put(&s.addr).starts_with("HTTP/1.1 201"));
        s.kill(sig);
        let st = s.wait_exit(Duration::from_secs(5));
        assert_eq!(st.code(), Some(0), "{sig}: {:?}", s.stderr);
        let log = s.stderr.join("\n");
        assert!(log.contains("shutdown complete"), "{log}");
        assert!(!log.contains(TOKEN), "token leaked in logs");
        assert!(s.stderr.iter().all(|l| l.starts_with('{') && l.ends_with('}')), "json lines: {log}");
        assert!(log.contains("\"msg\":\"request\"") && log.contains("\"request_id\":"));
        let out = Command::new(env!("CARGO_BIN_EXE_vcs-server"))
            .args(["audit", "verify", s.dir.path().join("audit.jsonl").to_str().unwrap()])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 records"), "{out:?}");
    }
}

#[test]
fn sigterm_lets_in_flight_requests_finish() {
    let mut s = spawn(&["--shutdown-grace", "5"], None);
    let mut c = TcpStream::connect(&s.addr).unwrap();
    write!(
        c,
        "GET /v1/whoami HTTP/1.1\r\nConnection: close\r\nAuthorization: Bearer {TOKEN}\r\n"
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    s.kill("-TERM");
    std::thread::sleep(Duration::from_millis(400));
    assert!(s.child.try_wait().unwrap().is_none(), "still draining while a request is in flight");
    assert!(TcpStream::connect(&s.addr).is_err(), "no new connections while draining");
    c.write_all(b"\r\n").unwrap();
    let mut o = String::new();
    c.read_to_string(&mut o).unwrap();
    assert!(o.starts_with("HTTP/1.1 200") && o.contains("admin admin"), "{o}");
    assert_eq!(s.wait_exit(Duration::from_secs(5)).code(), Some(0));
}

#[test]
fn shutdown_grace_expiry_still_exits_zero() {
    let mut s = spawn(&["--shutdown-grace", "0.3"], None);
    let mut c = TcpStream::connect(&s.addr).unwrap();
    c.write_all(b"GET /healthz HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    s.kill("-TERM");
    assert_eq!(s.wait_exit(Duration::from_secs(5)).code(), Some(0));
    assert!(s.stderr.join("\n").contains("grace expired"));
}

#[test]
fn config_file_is_merged_and_flags_override() {
    let cfg = "# server\nroot = \"@DIR/data\"\nlisten = \"127.0.0.1:0\"\nlog-format = \"json\"\nlog_level = \"info\"\n";
    let s = spawn(&[], Some(cfg));
    assert!(s.stderr.last().unwrap().starts_with('{'), "json from the file");
    assert!(s.get("/v1/whoami").contains("200 OK"));
    drop(s);
    let s = spawn(&["--log-format", "text"], Some(cfg));
    assert!(!s.stderr.last().unwrap().starts_with('{'), "flag overrides file: {:?}", s.stderr);
}

#[test]
fn bad_config_gives_file_line_errors_and_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    for (text, want) in [
        ("listen = \"127.0.0.1:0\"\nbogus = 1\n", "server.toml:2: unknown key `bogus`"),
        ("listen = \n", "server.toml:1: missing value"),
        ("[tls]\n", "server.toml:1: tables"),
        ("\u{0}\u{0}", "server.toml:1:"),
    ] {
        let f = dir.path().join("server.toml");
        std::fs::write(&f, text).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_vcs-server"))
            .arg("--config")
            .arg(&f)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{text:?}");
        let e = String::from_utf8_lossy(&out.stderr);
        assert!(
            e.contains(&f.to_str().unwrap().replace("server.toml", "")) && e.contains(want),
            "{text:?}: {e}"
        );
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vcs-server"))
        .args(["--config", "/nonexistent/x.toml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
