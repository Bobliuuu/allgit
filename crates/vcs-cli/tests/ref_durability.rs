//! Deferred ref durability (docs/durability.md, issue #494): ref-only operations skip the per-op
//! fsync like git, yet a kill -9 at any moment leaves a repository that opens and fscks clean.
#![cfg(unix)]
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn vcs(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_CONFIG_HOME", dir)
        .env_remove("VCS_FSYNC_REFS")
        .env_remove("VCS_FSYNC");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args, &[]);
    assert!(o.status.success(), "vcs {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    ok(&d, &["commit", "-m", "init"]);
    (t, d)
}
fn marker(d: &Path) -> bool {
    d.join(".vcs/oplog.dirty").exists()
}

#[test]
fn ref_ops_defer_and_object_writes_flush() {
    let (_t, d) = repo();
    assert!(!marker(&d));
    ok(&d, &["branch", "b1"]);
    assert!(marker(&d), "branch must defer its fsync by default");
    ok(&d, &["tag", "t1"]);
    ok(&d, &["branch", "-d", "b1"]);
    assert!(marker(&d));
    // an object-writing command makes the deferred updates durable
    std::fs::write(d.join("a.txt"), "b\n").unwrap();
    ok(&d, &["add", "a.txt"]);
    assert!(!marker(&d), "the object barrier of `add` must flush deferred ref updates");
    ok(&d, &["branch", "b2"]);
    assert!(marker(&d));
    ok(&d, &["commit", "-m", "two"]); // journalled with a full fdatasync
    assert!(!marker(&d));
}

#[test]
fn knob_restores_per_op_durability() {
    let (_t, d) = repo();
    let o = vcs(&d, &["branch", "e1"], &[("VCS_FSYNC_REFS", "1")]);
    assert!(o.status.success());
    assert!(!marker(&d), "VCS_FSYNC_REFS=1 syncs every ref op");
    ok(&d, &["config", "set", "core.fsync", "refs"]);
    ok(&d, &["tag", "c1"]);
    assert!(!marker(&d), "core.fsync=refs syncs every ref op");
    ok(&d, &["config", "unset", "core.fsync"]);
    ok(&d, &["tag", "c2"]);
    assert!(marker(&d));
    assert!(vcs(&d, &["config", "set", "core.fsync", "bogus"], &[]).status.code() != Some(0));
}

#[test]
fn gc_flushes_deferred_refs() {
    let (_t, d) = repo();
    ok(&d, &["branch", "g1"]);
    assert!(marker(&d));
    ok(&d, &["gc"]);
    assert!(!marker(&d), "gc makes deferred ref updates durable");
    assert!(ok(&d, &["branch"]).contains("g1"));
}

/// kill -9 a ref operation at a random moment, many times; the repo must open and fsck clean and
/// every operation that returned success must be visible.
#[test]
fn kill_9_during_ref_ops_never_corrupts() {
    let (_t, d) = repo();
    let mut done: Vec<String> = Vec::new();
    for i in 0..120u64 {
        let name = format!("k{i}");
        let args: Vec<&str> = match i % 3 {
            0 => vec!["branch", &name],
            1 => vec!["tag", &name],
            _ => vec!["branch", "-d", "k0"],
        };
        let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
        let mut ch = c
            .args(&args)
            .current_dir(&d)
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .env("VCS_CONFIG_HOME", &d)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_micros((i * 997) % 9000));
        let finished = matches!(ch.try_wait(), Ok(Some(s)) if s.success());
        let _ = ch.kill(); // SIGKILL
        let st = ch.wait().unwrap();
        if (finished || st.success()) && i % 3 == 0 {
            done.push(name);
        }
        let f = ok(&d, &["fsck"]);
        assert!(f.contains("ok"), "iteration {i}: fsck: {f}");
    }
    let list = ok(&d, &["branch"]);
    for n in done {
        if n != "k0" {
            assert!(list.contains(&n), "completed branch {n} lost: {list}");
        }
    }
}
