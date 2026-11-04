//! `vcs gc`: dry run, real local sweep, and the guarded remote sweep, with the real binary.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn backdate(p: &Path, days: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}
/// Every loose object file under `objects` (fan-out dirs only).
fn loose(objects: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for d in std::fs::read_dir(objects).unwrap().flatten() {
        if d.file_name().len() == 2 && d.path().is_dir() {
            v.extend(std::fs::read_dir(d.path()).unwrap().flatten().map(|f| f.path()));
        }
    }
    v.sort();
    v
}

/// A repo with one commit and one orphan blob (staged, then unstaged). Returns the orphan's file.
fn repo_with_orphan(root: &Path) -> (PathBuf, PathBuf) {
    ok(root, &["init", "r"]);
    let r = root.join("r");
    write(&r, "a", "kept\n");
    ok(&r, &["add", "."]);
    ok(&r, &["commit", "-m", "one"]);
    let before = loose(&r.join(".vcs/objects"));
    write(&r, "b", "garbage that nothing references\n");
    ok(&r, &["add", "b"]);
    let after = loose(&r.join(".vcs/objects"));
    ok(&r, &["restore", "--staged", "b"]);
    let orphan = after
        .into_iter()
        .find(|p| !before.contains(p) && std::fs::read(p).unwrap().len() < 200)
        .expect("new blob object");
    (r, orphan)
}

#[test]
fn local_gc_dry_run_reports_and_real_run_deletes_only_old_unreachable() {
    let t = tempfile::tempdir().unwrap();
    let (r, orphan) = repo_with_orphan(t.path());
    let objects = r.join(".vcs/objects");
    // young orphan: kept by both
    assert!(ok(&r, &["gc", "--dry-run"]).contains("nothing was deleted"));
    let out = ok(&r, &["gc"]);
    assert!(out.contains("DESTRUCTIVE") && out.contains("deleted 0 object"), "{out}");
    assert!(orphan.exists());
    // too-short grace is refused for a real sweep, even if the object is old
    backdate(&orphan, 30);
    let e = fail(&r, &["gc", "--grace-secs", "60"]);
    assert!(e.contains("touch-on-dedup"), "{e}");
    assert!(orphan.exists());
    // dry run lists it, deletes nothing
    let out = ok(&r, &["gc", "--dry-run"]);
    assert!(out.contains("would delete") && orphan.exists(), "{out}");
    // real run
    let n_before = loose(&objects).len();
    let out = ok(&r, &["gc"]);
    assert!(out.contains("deleted 1 object"), "{out}");
    assert!(!orphan.exists());
    assert_eq!(loose(&objects).len(), n_before - 1);
    assert!(ok(&r, &["fsck"]).contains("ok"));
    assert!(ok(&r, &["log"]).contains("one"));
}

#[test]
fn remote_gc_needs_yes_dry_run_is_safe_and_yes_deletes() {
    let t = tempfile::tempdir().unwrap();
    let (r, orphan) = repo_with_orphan(t.path());
    let remote = t.path().join("remote");
    ok(&r, &["remote", "set", remote.to_str().unwrap()]);
    ok(&r, &["publish"]);
    // plant the orphan on the remote (same encoding as the local loose file), 30 days old
    let key = orphan.strip_prefix(r.join(".vcs/objects")).unwrap().to_owned();
    let planted = remote.join("objects").join(&key);
    std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
    std::fs::copy(&orphan, &planted).unwrap();
    backdate(&planted, 30);

    let e = fail(&r, &["gc", "--remote"]);
    assert!(e.contains("--yes"), "{e}");
    assert!(planted.exists());
    let out = ok(&r, &["gc", "--remote", "--dry-run"]);
    assert!(out.contains("1 would be deleted") && out.contains("nothing was deleted"), "{out}");
    assert!(planted.exists());
    // an unsafe grace is refused
    let e = fail(&r, &["gc", "--remote", "--yes", "--grace-secs", "3600"]);
    assert!(e.contains("grace"), "{e}");
    assert!(planted.exists());
    let out = ok(&r, &["gc", "--remote", "--yes"]);
    assert!(out.contains("DESTRUCTIVE") && out.contains("deleted 1 object"), "{out}");
    assert!(!planted.exists());
    // everything published is still there: a fresh clone works
    ok(t.path(), &["clone", remote.to_str().unwrap(), "c"]);
    assert!(ok(&t.path().join("c"), &["log"]).contains("one"));
    // no tombstones or lock left behind
    assert!(!remote.join("gc/lock").exists());
    assert!(std::fs::read_dir(remote.join("tomb")).map(|d| d.count() == 0).unwrap_or(true));
}
