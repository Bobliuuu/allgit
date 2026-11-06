//! `vcs remote verify | backup | restore | safe-point` with the real binary, including the kill -9 drill
//! (docs/backup-restore.md).
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com");
    c
}
fn vcs(dir: &Path, args: &[&str]) -> Output {
    cmd(dir, args).output().unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}

/// A repo with `commits` published commits of `files` files each, pushed to a directory remote.
fn published(root: &Path, files: usize, commits: usize) -> (std::path::PathBuf, String) {
    let remote = root.join("remote");
    let r = remote.to_str().unwrap().to_owned();
    ok(root, &["init", "a"]);
    let a = root.join("a");
    ok(&a, &["remote", "set", &r]);
    for c in 0..commits {
        for f in 0..files {
            write(&a, &format!("d{}/f{f}.txt", f % 7), &format!("commit {c} file {f}\n").repeat(20));
        }
        ok(&a, &["add", "."]);
        ok(&a, &["commit", "-m", &format!("c{c}")]);
        ok(&a, &["publish"]);
    }
    (a, r)
}

#[test]
fn verify_backup_restore_and_clone_round_trip() {
    let t = tempfile::tempdir().unwrap();
    let (a, r) = published(t.path(), 20, 3);
    let out = ok(&a, &["remote", "verify"]);
    assert!(out.contains("verify: OK"), "{out}");
    assert!(ok(&a, &["remote", "verify", "--deep"]).contains("deep:"));

    let bak = t.path().join("bak");
    let b = bak.to_str().unwrap();
    let out = ok(&a, &["remote", "backup", b]);
    assert!(out.contains("GC safe point held"), "{out}");
    assert!(out.contains("verify: OK"), "{out}");
    assert!(ok(t.path(), &["remote", "verify", b, "--deep"]).contains("verify: OK"));
    // incremental: nothing new to copy
    assert!(ok(&a, &["remote", "backup", b]).contains("0 key(s) copied"));

    // restore from the backup into a fresh store, then clone from it
    let fresh = t.path().join("fresh");
    let f = fresh.to_str().unwrap();
    let out = ok(t.path(), &["remote", "restore", b, f]);
    assert!(out.contains("verify: OK"), "{out}");
    assert!(fail(t.path(), &["remote", "restore", b, f]).contains("not empty"));
    ok(t.path(), &["clone", f, "c"]);
    assert_eq!(
        std::fs::read_to_string(t.path().join("c/d3/f3.txt")).unwrap(),
        std::fs::read_to_string(a.join("d3/f3.txt")).unwrap()
    );
    let _ = r;
}

#[test]
fn verify_fails_on_damage_and_names_it() {
    let t = tempfile::tempdir().unwrap();
    let (a, r) = published(t.path(), 5, 2);
    let packs: Vec<_> = std::fs::read_dir(Path::new(&r).join("packs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    assert!(!packs.is_empty());
    std::fs::remove_file(&packs[0]).unwrap();
    let out = fail(&a, &["remote", "verify"]);
    assert!(
        out.contains("verify: FAILED") || out.contains("CORRUPT") || out.contains("MISSING") || out.contains("verification failed"),
        "{out}"
    );
    // a backup of a damaged remote refuses to install a manifest
    let bak = t.path().join("bak");
    let out = fail(&a, &["remote", "backup", bak.to_str().unwrap()]);
    assert!(out.contains("NOT written") || out.contains("damaged"), "{out}");
    assert!(!bak.join("manifest").exists());
}

#[test]
fn safe_point_shows_holds_and_gc_waits_for_them() {
    let t = tempfile::tempdir().unwrap();
    let (a, r) = published(t.path(), 3, 1);
    assert!(ok(&a, &["remote", "safe-point"]).contains("no destructive gc"));
    // plant a hold exactly as a running (or killed) backup would
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let hold = Path::new(&r).join("gc/hold");
    std::fs::create_dir_all(&hold).unwrap();
    std::fs::write(hold.join("deadbeefdeadbeefdeadbeef"), format!("vcs-backup pid 1 {}", now + 120_000)).unwrap();
    let out = ok(&a, &["remote", "safe-point"]);
    assert!(out.contains("HOLD"), "{out}");
    let out = fail(&a, &["gc", "--remote", "--yes"]);
    assert!(out.contains("safe point"), "{out}");
    // an expired hold is cleared by the next gc
    std::fs::write(hold.join("deadbeefdeadbeefdeadbeef"), format!("vcs-backup pid 1 {}", now - 120_000)).unwrap();
    ok(&a, &["gc", "--remote", "--yes"]);
    assert!(!hold.join("deadbeefdeadbeefdeadbeef").exists());
}

/// The drill: SIGKILL `vcs remote backup` at assorted moments, then verify the destination. Whatever the
/// moment, the destination is either "no manifest" (nothing claimed) or a complete, verifiable backup,
/// and re-running the backup finishes it.
#[test]
fn kill_9_mid_backup_then_verify_and_resume() {
    let t = tempfile::tempdir().unwrap();
    let (a, _r) = published(t.path(), 60, 4);
    let mut killed_midway = 0;
    for (i, delay_ms) in [0u64, 3, 8, 15, 30, 60, 120].iter().enumerate() {
        let bak = t.path().join(format!("bak{i}"));
        let b = bak.to_str().unwrap();
        let mut child = cmd(&a, &["remote", "backup", b, "--hold-secs", "2"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(*delay_ms));
        let _ = child.kill(); // SIGKILL
        let st = child.wait().unwrap();
        let v = vcs(t.path(), &["remote", "verify", b, "--deep"]);
        let text = format!("{}{}", String::from_utf8_lossy(&v.stdout), String::from_utf8_lossy(&v.stderr));
        if st.success() {
            assert!(v.status.success(), "finished backup must verify: {text}");
        } else {
            killed_midway += 1;
            if v.status.success() {
                assert!(text.contains("verify: OK"), "{text}"); // killed after the manifest: complete
            } else {
                assert!(
                    text.contains("NO MANIFEST"),
                    "a killed backup must never leave a manifest without its data: {text}"
                );
                assert!(!bak.join("manifest").exists());
            }
        }
        // resume: the same command completes and the result verifies
        ok(&a, &["remote", "backup", b]);
        assert!(ok(t.path(), &["remote", "verify", b, "--deep"]).contains("verify: OK"));
    }
    assert!(killed_midway > 0, "the drill never actually killed a running backup");
}
