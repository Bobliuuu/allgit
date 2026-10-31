//! End-to-end: two clones diverge, sync reports the divergence, `vcs merge` reconciles them.
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
/// (stdout, stderr) of a command that must fail.
fn fail(dir: &Path, args: &[&str]) -> (String, String) {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}

#[test]
fn two_clones_diverge_and_merge() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    let remote = root.join("remote");
    let a = root.join("a");
    ok(root, &["init", "a"]);
    write(&a, "f.txt", "1\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&a, "init");
    ok(&a, &["remote", "set", remote.to_str().unwrap()]);
    ok(&a, &["publish"]);
    ok(root, &["clone", remote.to_str().unwrap(), "b"]);
    let b = root.join("b");

    // disjoint edits on both sides: sync reports divergence with a merge hint
    write(&a, "f.txt", "ONE\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&a, "a: first line");
    ok(&a, &["publish"]);
    write(&b, "f.txt", "1\n2\n3\n4\n5\n6\n7\nEIGHT\n");
    write(&b, "b-only.txt", "b\n");
    commit_all(&b, "b: last line");
    let out = ok(&b, &["sync"]);
    assert!(out.contains("diverged") && out.contains("vcs merge --remote main"), "{out}");
    let out = ok(&b, &["merge", "--remote", "main"]);
    assert!(out.contains("merged, new commit"), "{out}");
    assert_eq!(read(&b, "f.txt"), "ONE\n2\n3\n4\n5\n6\n7\nEIGHT\n");
    assert!(ok(&b, &["status"]).contains("clean"));
    ok(&b, &["fsck"]);
    assert!(ok(&b, &["merge", "--remote", "main"]).contains("already up to date"));
    ok(&b, &["publish"]);

    // a fast-forwards to b's merge
    let out = ok(&a, &["sync"]);
    assert!(out.contains("fast-forwarded"), "{out}");
    assert_eq!(read(&a, "b-only.txt"), "b\n");

    // conflicting edits of the same line
    write(&a, "f.txt", "ONE\n2\n3\nA4\n5\n6\n7\nEIGHT\n");
    commit_all(&a, "a: line 4");
    ok(&a, &["publish"]);
    write(&b, "f.txt", "ONE\n2\n3\nB4\n5\n6\n7\nEIGHT\n");
    commit_all(&b, "b: line 4");
    assert!(ok(&b, &["sync"]).contains("diverged"));
    let (stdout, stderr) = fail(&b, &["merge", "--remote", "main"]);
    assert!(
        stdout.contains("conflict: f.txt") && stderr.contains("conflicts"),
        "{stdout}{stderr}"
    );
    let marked = read(&b, "f.txt");
    assert!(
        marked.contains("<<<<<<<") && marked.contains("A4") && marked.contains("B4"),
        "{marked}"
    );
    let st = ok(&b, &["status"]);
    assert!(st.contains("merging origin/main") && st.contains("unresolved"), "{st}");
    assert!(ok(&b, &["conflict", "list"]).contains("f.txt"));
    assert!(ok(&b, &["conflict", "show", "f.txt"]).contains("modify/modify"));
    let (_, err) = fail(&b, &["commit", "-m", "too early"]);
    assert!(err.contains("unresolved"), "{err}");

    // abort restores exactly, then redo and resolve
    ok(&b, &["merge", "--abort"]);
    assert_eq!(read(&b, "f.txt"), "ONE\n2\n3\nB4\n5\n6\n7\nEIGHT\n");
    assert!(ok(&b, &["status"]).contains("clean"));
    fail(&b, &["merge", "--remote", "main"]);
    write(&b, "f.txt", "ONE\n2\n3\nAB4\n5\n6\n7\nEIGHT\n");
    ok(&b, &["add", "f.txt"]);
    assert!(ok(&b, &["conflict", "list"]).contains("resolved"));
    ok(&b, &["commit", "-m", "merge a"]);
    assert!(ok(&b, &["status"]).contains("clean"));
    ok(&b, &["fsck"]);
    ok(&b, &["publish"]);
    ok(&a, &["sync"]);
    assert_eq!(read(&a, "f.txt"), "ONE\n2\n3\nAB4\n5\n6\n7\nEIGHT\n");
    ok(&a, &["fsck"]);
}
