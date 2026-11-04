//! `vcs worktree add|list|remove` with the real binary.
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
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn worktree_add_list_commit_remove() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    ok(root, &["init", "a"]);
    let a = root.join("a");
    std::fs::write(a.join("f"), "one").unwrap();
    ok(&a, &["add", "."]);
    ok(&a, &["commit", "-m", "one"]);
    let out = ok(&a, &["worktree", "add", "../wt", "topic"]);
    assert!(out.contains("root/topic"), "{out}");
    let wt = root.join("wt");
    assert_eq!(std::fs::read_to_string(wt.join("f")).unwrap(), "one");
    let list = ok(&a, &["worktree", "list"]);
    assert!(
        list.contains("(main)") && list.contains("root/topic") && list.contains("root/main"),
        "{list}"
    );
    assert!(fail(&a, &["worktree", "add", "../wt2", "topic"]).contains("already checked out"));
    std::fs::write(wt.join("f"), "two").unwrap();
    ok(&wt, &["add", "."]);
    ok(&wt, &["commit", "-m", "two"]);
    assert!(ok(&wt, &["log"]).contains("two"));
    assert!(!ok(&a, &["log"]).contains("two"));
    assert!(ok(&wt, &["fsck"]).contains("ok"));
    std::fs::write(wt.join("f"), "dirty").unwrap();
    assert!(fail(&a, &["worktree", "remove", "../wt"]).contains("not clean"));
    ok(&a, &["worktree", "remove", "../wt", "--force"]);
    assert!(!wt.exists());
    assert!(ok(&a, &["fsck"]).contains("ok"));
}
