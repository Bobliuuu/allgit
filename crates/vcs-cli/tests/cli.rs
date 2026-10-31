//! End-to-end: drives the real `vcs` binary through the README workflow.
use std::path::Path;
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

#[test]
fn first_demo_workflow() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "demo"]);
    let d = t.path().join("demo");
    std::fs::write(d.join("hello.txt"), "hello\n").unwrap();
    ok(&d, &["add", "hello.txt"]);
    assert!(ok(&d, &["commit", "-m", "Initial commit"]).contains("Initial commit"));
    std::fs::write(d.join("hello.txt"), "hello\nworld\n").unwrap();
    assert!(ok(&d, &["status"]).contains("modified:  hello.txt"));
    assert!(ok(&d, &["diff"]).contains("+world"));
    ok(&d, &["add", "hello.txt"]);
    ok(&d, &["commit", "-m", "Update greeting"]);
    let log = ok(&d, &["log"]);
    assert!(log.find("Update greeting").unwrap() < log.find("Initial commit").unwrap());
    assert!(!d.join(".git").exists());
    assert!(ok(&d, &["fsck"]).contains("ok"));
}

#[test]
fn zone_workflow_and_errors() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    std::fs::create_dir_all(d.join("engine")).unwrap();
    std::fs::write(d.join("engine/a.rs"), "1\n").unwrap();
    ok(&d, &["zone", "create", "engine", "--path", "engine"]);
    ok(&d, &["add", "."]);
    ok(&d, &["commit", "-m", "init"]);
    ok(&d, &["zone", "branch", "engine", "feature"]);
    ok(&d, &["zone", "checkout", "engine", "feature"]);
    std::fs::write(d.join("engine/a.rs"), "2\n").unwrap();
    ok(&d, &["add", "engine"]);
    ok(&d, &["commit", "-m", "feat"]);
    let list = ok(&d, &["zone", "list"]);
    assert!(list.contains("subscribed, on feature"), "{list}");
    assert!(list.contains("subscribed, on main"), "{list}");
    // overlapping zone -> nonzero exit + message
    let o = vcs(&d, &["zone", "create", "x", "--path", "engine/sub"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("overlaps"));
    // outside a repo
    let o = vcs(t.path(), &["status"]);
    assert!(!o.status.success());
    // log from inside the zone dir defaults to that zone
    assert!(ok(&d.join("engine"), &["log"]).contains("zone engine"));
}
