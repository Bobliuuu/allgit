//! `vcs watch` hands over to the separate `vcs-watchd` binary (so `vcs` itself does not link the
//! platform file-event API); both spellings must start a daemon that publishes its pid and that
//! stops by itself with `--idle-exit`.
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn vcs(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .output()
        .unwrap()
}

#[test]
fn vcs_watch_runs_the_daemon_and_idle_exit_stops_it() {
    let d = tempfile::tempdir().unwrap();
    assert!(vcs(d.path(), &["init"]).status.success());
    std::fs::write(d.path().join("a.txt"), "a").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(["watch", "--idle-exit", "2"])
        .current_dir(d.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = d.path().join(".vcs/fsmon/pid");
    let t = Instant::now();
    while !pid.exists() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pid.exists(), "daemon did not publish its pid");
    let t = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            assert!(st.success(), "watch --idle-exit exited with {st}");
            break;
        }
        if t.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("watch --idle-exit 2 did not stop");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn watchd_binary_runs_directly() {
    let d = tempfile::tempdir().unwrap();
    assert!(vcs(d.path(), &["init"]).status.success());
    let st = Command::new(env!("CARGO_BIN_EXE_vcs-watchd"))
        .args(["--idle-exit", "1"])
        .current_dir(d.path())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
    let st = Command::new(env!("CARGO_BIN_EXE_vcs-watchd"))
        .arg("--bogus")
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(st.code(), Some(2));
}

#[test]
fn watch_daemonize_returns_with_a_running_detached_watcher() {
    let d = tempfile::tempdir().unwrap();
    assert!(vcs(d.path(), &["init"]).status.success());
    let out = vcs(d.path(), &["watch", "--daemonize", "--idle-exit", "2"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let pid = d.path().join(".vcs/fsmon/pid");
    assert!(pid.exists(), "journal and pid must exist when --daemonize returns");
    // a second call finds it
    let again = vcs(d.path(), &["watch", "--daemonize"]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("already running"));
    // it stops by itself (idle exit) and removes its pid file
    let t = Instant::now();
    while pid.exists() && t.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!pid.exists(), "detached watcher did not idle-exit");
}
