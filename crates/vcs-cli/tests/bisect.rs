//! `vcs bisect`: manual marking, `run` exit codes, dirty-tree refusal, reset.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
        .output()
        .unwrap()
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

/// Ten commits; the file says BAD from commit 6 on. Returns (tempdir, repo dir).
fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    for i in 1..=10 {
        let body = if i >= 6 { format!("BAD {i}\n") } else { format!("fine {i}\n") };
        std::fs::write(d.join("f.txt"), body).unwrap();
        ok(&d, &["add", "f.txt"]);
        ok(&d, &["commit", "-m", &format!("c{i}")]);
    }
    (t, d)
}

#[test]
fn run_finds_first_bad() {
    let (_t, d) = repo();
    ok(&d, &["bisect", "start", "HEAD", "HEAD~9"]);
    let out = ok(&d, &["bisect", "run", "! grep -q BAD f.txt"]);
    assert!(out.contains("is the first bad commit"), "{out}");
    assert!(out.contains("c6"), "{out}");
    assert!(d.join(".vcs/bisect").exists());
    ok(&d, &["bisect", "reset"]);
    assert!(!d.join(".vcs/bisect").exists());
    assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), "BAD 10\n");
    assert!(ok(&d, &["branch"]).contains("main"));
}

#[test]
fn manual_marks_and_skip() {
    let (_t, d) = repo();
    ok(&d, &["bisect", "start"]);
    assert!(ok(&d, &["bisect", "bad"]).contains("waiting for a good"));
    let out = ok(&d, &["bisect", "good", "HEAD~9"]);
    assert!(out.contains("Bisecting:"), "{out}");
    // the checked-out commit is a midpoint; judge it from the file
    let mut last = out;
    for _ in 0..8 {
        if last.contains("first bad commit") {
            break;
        }
        let text = std::fs::read_to_string(d.join("f.txt")).unwrap();
        last = ok(&d, &["bisect", if text.starts_with("BAD") { "bad" } else { "good" }]);
    }
    assert!(last.contains("first bad commit") && last.contains("c6"), "{last}");
    ok(&d, &["bisect", "reset"]);
}

#[test]
fn run_exit_codes_skip_and_abort() {
    let (_t, d) = repo();
    ok(&d, &["bisect", "start", "HEAD", "HEAD~9"]);
    // 125 for every commit: nothing can be decided
    let out = ok(&d, &["bisect", "run", "exit 125"]);
    assert!(out.contains("only skipped commits"), "{out}");
    ok(&d, &["bisect", "reset"]);
    ok(&d, &["bisect", "start", "HEAD", "HEAD~9"]);
    let o = vcs(&d, &["bisect", "run", "exit 130"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("130"));
    assert!(d.join(".vcs/bisect").exists(), "state kept after abort");
    ok(&d, &["bisect", "reset"]);
}

#[test]
fn refuses_dirty_tree_and_double_start() {
    let (_t, d) = repo();
    std::fs::write(d.join("f.txt"), "dirty\n").unwrap();
    let o = vcs(&d, &["bisect", "start", "HEAD", "HEAD~9"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("uncommitted"));
    assert!(!d.join(".vcs/bisect").exists(), "failed start leaves no state");
    ok(&d, &["restore", "f.txt"]);
    ok(&d, &["bisect", "start", "HEAD", "HEAD~9"]);
    assert!(!vcs(&d, &["bisect", "start"]).status.success());
    ok(&d, &["bisect", "reset"]);
    assert!(!vcs(&d, &["bisect", "good"]).status.success(), "no bisection after reset");
}
