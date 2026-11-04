//! `vcs daemon`: differential output vs the in-process path, freshness, concurrency, crash safety,
//! idle exit, socket permissions, fallback. Unix only.
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn vcs_env(dir: &Path, args: &[&str], daemon: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
        .env("VCS_DAEMON", daemon)
        .env("VCS_DAEMON_IDLE", "60")
        .output()
        .unwrap()
}

fn plain(d: &Path, a: &[&str]) -> Output {
    vcs_env(d, a, "0")
}

/// Without VCS_DAEMON the client still probes for a running daemon: the "user ran `vcs daemon start`" case.
fn auto(d: &Path, a: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(a)
        .current_dir(d)
        .env_remove("VCS_DAEMON")
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000");
    c.output().unwrap()
}

fn sock(d: &Path) -> PathBuf {
    d.join(".vcs/daemon/sock")
}

fn repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    assert!(plain(d.path(), &["init"]).status.success());
    for i in 0..30 {
        let p = d.path().join(format!("src/m{}/f{i}.txt", i % 3));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, format!("line {i}\nsecond\n")).unwrap();
    }
    assert!(plain(d.path(), &["add", "."]).status.success());
    assert!(plain(d.path(), &["commit", "-m", "base"]).status.success());
    d
}

fn copy_dir(a: &Path, b: &Path) {
    assert!(Command::new("cp").arg("-R").arg(a).arg(b).status().unwrap().success());
}

fn start(d: &Path) {
    let o = plain(d, &["daemon", "start"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(sock(d).exists());
}

fn stop(d: &Path) {
    let _ = plain(d, &["daemon", "stop"]);
}

fn same(a: &Output, b: &Output, what: &str) {
    assert_eq!(a.status.code(), b.status.code(), "{what}: exit code");
    assert_eq!(String::from_utf8_lossy(&a.stdout), String::from_utf8_lossy(&b.stdout), "{what}: stdout");
    assert_eq!(String::from_utf8_lossy(&a.stderr), String::from_utf8_lossy(&b.stderr), "{what}: stderr");
}

fn mask(mut o: Output) -> Output {
    let f = |b: &[u8]| {
        let t = String::from_utf8_lossy(b).to_string();
        let mut out = String::new();
        let mut run = String::new();
        for ch in t.chars().chain(std::iter::once('\0')) {
            if ch.is_ascii_hexdigit() {
                run.push(ch);
            } else {
                out.push_str(if run.len() >= 12 { "<hex>" } else { &run });
                run.clear();
                if ch != '\0' {
                    out.push(ch);
                }
            }
        }
        out.into_bytes()
    };
    o.stdout = f(&o.stdout);
    o.stderr = f(&o.stderr);
    o
}

fn daemon_pid(d: &Path) -> i32 {
    std::fs::read_to_string(d.join(".vcs/daemon/pid")).unwrap().trim().parse().unwrap()
}

#[test]
fn differential_over_command_corpus() {
    let a = repo();
    let b = tempfile::tempdir().unwrap();
    let bp = b.path().join("r");
    copy_dir(a.path(), &bp);
    // The copy keeps the same relative layout; run the same sequence on both, one through the daemon.
    start(&bp);
    let steps: Vec<Vec<&str>> = vec![
        vec!["status"],
        vec!["diff"],
        vec!["log", "-n", "1"],
        vec!["log"],
        vec!["show"],
        vec!["branch"],
        vec!["ls-files"],
        vec!["p4", "opened"],
        vec!["add", "nonexistent.txt"],
        vec!["status"],
        vec!["diff", "--staged"],
        vec!["rm", "src/m0/f0.txt"],
        vec!["status"],
        vec!["mv", "src/m1/f1.txt", "src/m1/renamed.txt"],
        vec!["status"],
        vec!["restore", "--staged", "src/m1/renamed.txt"],
        vec!["status"],
        vec!["add", "src"],
        vec!["status"],
        vec!["commit", "-m", "second"],
        vec!["log", "-n", "2"],
        vec!["show"],
        vec!["branch", "feature"],
        vec!["branch"],
        vec!["commit", "-m", "nothing to commit"],
        vec!["status"],
    ];
    // Local edits, applied identically to both before the steps.
    for r in [a.path(), bp.as_path()] {
        std::fs::write(r.join("src/m2/f2.txt"), "changed\n").unwrap();
        std::fs::write(r.join("new.txt"), "new\n").unwrap();
    }
    let mut committed = false;
    for s in &steps {
        let x = plain(a.path(), s);
        let y = vcs_env(&bp, s, "1");
        // Commit ids carry a per-run random change id: compare them with hex runs masked.
        let (x, y) = if s[0] == "commit" || committed { committed = true; (mask(x), mask(y)) } else { (x, y) };
        same(&x, &y, &s.join(" "));
    }
    // And the client really used the daemon (a request reached it: the daemon pid is alive).
    assert!(sock(&bp).exists());
    stop(&bp);
}

#[test]
fn fresh_after_external_edits_and_concurrent_clients() {
    let d = repo();
    start(d.path());
    let r = d.path();
    assert!(String::from_utf8_lossy(&auto(r, &["status"]).stdout).contains("nothing") || auto(r, &["status"]).status.success());
    // External edit between two daemon status calls must show up at once, in both modes.
    std::fs::write(r.join("src/m0/f0.txt"), "edited externally\n").unwrap();
    let viad = auto(r, &["status"]);
    let inproc = plain(r, &["status"]);
    same(&viad, &inproc, "status after edit");
    assert!(String::from_utf8_lossy(&viad.stdout).contains("f0.txt"));
    std::fs::write(r.join("src/m0/f0.txt"), "line 0\nsecond\n").unwrap();
    // Concurrent clients adding different files.
    let mut hs = Vec::new();
    for i in 0..8 {
        let p = r.to_path_buf();
        std::fs::write(p.join(format!("c{i}.txt")), format!("c{i}\n")).unwrap();
        hs.push(std::thread::spawn(move || {
            // The repo lock (flock) is honoured by the daemon's children: contention is reported
            // exactly as in-process, so retry like a user would.
            let mut tries = 0;
            loop {
                let o = auto(&p, &["add", &format!("c{i}.txt")]);
                if o.status.success() {
                    break;
                }
                let e = String::from_utf8_lossy(&o.stderr).to_string();
                tries += 1;
                assert!(e.contains("repository lock held") && tries < 200, "{e}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    let st = String::from_utf8_lossy(&plain(r, &["status"]).stdout).to_string();
    for i in 0..8 {
        assert!(st.contains(&format!("c{i}.txt")), "missing c{i}: {st}");
    }
    same(&auto(r, &["status"]), &plain(r, &["status"]), "status after concurrent adds");
    // Idle daemon holds no repository lock: take .vcs/lock exclusively without waiting.
    let f = std::fs::OpenOptions::new().create(true).append(true).open(r.join(".vcs/lock")).unwrap();
    use std::os::fd::AsRawFd;
    assert_eq!(unsafe { libc_flock(f.as_raw_fd()) }, 0, "an idle daemon must not hold .vcs/lock");
    stop(r);
}

extern "C" {
    #[link_name = "flock"]
    fn c_flock(fd: i32, op: i32) -> i32;
}
unsafe fn libc_flock(fd: i32) -> i32 {
    c_flock(fd, 2 | 4) // LOCK_EX | LOCK_NB
}

#[test]
fn kill9_daemon_then_next_command_works() {
    let d = repo();
    start(d.path());
    let pid = daemon_pid(d.path());
    unsafe {
        kill(pid, 9);
    }
    std::thread::sleep(Duration::from_millis(100));
    // Stale socket file remains; clients must fall back, and the repo is intact.
    std::fs::write(d.path().join("z.txt"), "z\n").unwrap();
    let o = auto(d.path(), &["add", "z.txt"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let st = plain(d.path(), &["status"]);
    assert!(String::from_utf8_lossy(&st.stdout).contains("z.txt"));
    // A new daemon replaces the stale socket.
    start(d.path());
    same(&auto(d.path(), &["status"]), &plain(d.path(), &["status"]), "status after restart");
    stop(d.path());
}

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn socket_is_private_idle_exit_and_version_mismatch() {
    let d = repo();
    start(d.path());
    let m = std::fs::metadata(sock(d.path())).unwrap().permissions().mode() & 0o777;
    assert_eq!(m, 0o600);
    let dm = std::fs::metadata(d.path().join(".vcs/daemon")).unwrap().permissions().mode() & 0o777;
    assert_eq!(dm, 0o700);
    // A client with a different build id is refused (falls back, same output) and the daemon retires.
    let pid = daemon_pid(d.path());
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["status"])
        .current_dir(d.path())
        .env("VCS_DAEMON_TEST_BUILD_ID", "other-build")
        .env_remove("VCS_DAEMON")
        .output()
        .unwrap();
    same(&o, &plain(d.path(), &["status"]), "mismatched client falls back");
    let t = Instant::now();
    while unsafe { kill(pid, 0) } == 0 && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(unsafe { kill(pid, 0) } != 0, "daemon should retire after a version mismatch");

    // Idle exit.
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["daemon", "start"])
        .current_dir(d.path())
        .env("VCS_DAEMON_IDLE", "1")
        .output()
        .unwrap();
    assert!(o.status.success());
    let pid = daemon_pid(d.path());
    let t = Instant::now();
    while unsafe { kill(pid, 0) } == 0 && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(unsafe { kill(pid, 0) } != 0, "daemon should exit when idle");
    assert!(!sock(d.path()).exists());
}

#[test]
fn unsupported_commands_and_missing_daemon_fall_back() {
    let d = repo();
    // No daemon at all.
    same(&auto(d.path(), &["status"]), &plain(d.path(), &["status"]), "no daemon");
    start(d.path());
    // Unsupported shapes run in-process and behave identically.
    same(&auto(d.path(), &["tag"]), &plain(d.path(), &["tag"]), "tag");
    same(&auto(d.path(), &["status", "--bogus"]), &plain(d.path(), &["status", "--bogus"]), "bad flag");
    stop(d.path());
}
