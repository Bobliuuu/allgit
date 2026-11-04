//! `vcs resolve`: side picks, --file, --mark and --check, for both merge and rebase state.
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
fn fail(dir: &Path, args: &[&str]) -> (String, String) {
    let o = vcs(dir, args);
    assert!(!o.status.success(), "vcs {args:?} unexpectedly succeeded");
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}

const BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n";
fn body(x: &str) -> String {
    format!("1\n2\n3\n4\n5\n6\n7\n{x}\n")
}

/// main edits `f` (and `g`), feature edits them differently; leaves a merge of main into feature in conflict.
fn merging(t: &Path) -> std::path::PathBuf {
    let d = t.join("r");
    ok(t, &["init", "r"]);
    write(&d, "f", BASE);
    write(&d, "g", BASE);
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    write(&d, "f", &body("FEAT"));
    write(&d, "g", &body("FEAT"));
    commit_all(&d, "feature");
    ok(&d, &["switch", "main"]);
    write(&d, "f", &body("MAIN"));
    write(&d, "g", &body("MAIN"));
    commit_all(&d, "main");
    ok(&d, &["switch", "feature"]);
    let (out, _) = fail(&d, &["merge", "main"]);
    assert!(out.contains("conflict: f") && out.contains("conflict: g"), "{out}");
    d
}

#[test]
fn ours_theirs_base_and_file_in_a_merge() {
    let t = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    // ours = the checked-out branch (feature)
    ok(&d, &["resolve", "f", "--ours"]);
    assert_eq!(read(&d, "f"), body("FEAT"));
    assert!(ok(&d, &["conflict", "list"]).contains("resolved"));
    // still one left: cannot commit
    let (_, e) = fail(&d, &["commit", "-m", "x"]);
    assert!(e.contains("unresolved"), "{e}");
    // theirs, then replace by base, then by --file
    ok(&d, &["resolve", "g", "--theirs"]);
    assert_eq!(read(&d, "g"), body("MAIN"));
    ok(&d, &["resolve", "g", "--base"]);
    assert_eq!(read(&d, "g"), BASE);
    let src = d.join("..").join("custom.txt");
    std::fs::write(&src, "custom\n").unwrap();
    let out = ok(&d, &["resolve", "g", "--file", src.to_str().unwrap()]);
    assert!(out.contains("0 conflict(s) left"), "{out}");
    assert_eq!(read(&d, "g"), "custom\n");
    ok(&d, &["resolve", "--check"]);
    ok(&d, &["commit", "-m", "merged"]);
    assert_eq!(read(&d, "f"), body("FEAT"));
    ok(&d, &["fsck"]);
}

#[test]
fn mark_and_check_catch_leftover_markers() {
    let t = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    // --mark on the untouched marker file is refused
    let (_, e) = fail(&d, &["resolve", "f", "--mark"]);
    assert!(e.contains("conflict markers"), "{e}");
    let (out, _) = fail(&d, &["resolve", "--check"]);
    assert!(out.contains("unresolved: f") && out.contains("unresolved: g"), "{out}");

    // hand edit that keeps some markers: --mark accepts, --check fails
    let marked = read(&d, "f");
    write(&d, "f", &format!("{marked}extra\n"));
    ok(&d, &["resolve", "f", "--mark"]);
    ok(&d, &["resolve", "g", "--theirs"]);
    let (out, _) = fail(&d, &["resolve", "--check"]);
    assert!(out.contains("conflict markers remain: f") && !out.contains("g"), "{out}");

    // clean hand edit passes
    write(&d, "f", "hand edited\n");
    ok(&d, &["resolve", "f", "--mark"]);
    ok(&d, &["resolve", "--check"]);
    ok(&d, &["commit", "-m", "merged"]);
    assert_eq!(read(&d, "f"), "hand edited\n");
}

#[test]
fn resolve_rejects_bad_use() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "f", BASE);
    commit_all(&d, "base");
    let (_, e) = fail(&d, &["resolve", "f", "--ours"]);
    assert!(e.contains("no merge or rebase in progress"), "{e}");
    fail(&d, &["resolve", "f"]);
    fail(&d, &["resolve", "f", "--ours", "--theirs"]);
}

#[test]
fn not_conflicted_path_is_an_error() {
    let t = tempfile::tempdir().unwrap();
    let d = merging(t.path());
    write(&d, "other", "x\n");
    let (_, e) = fail(&d, &["resolve", "other", "--ours"]);
    assert!(e.contains("is not conflicted"), "{e}");
}

#[test]
fn delete_modify_side_removes_or_keeps_the_file() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "f", BASE);
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    std::fs::remove_file(d.join("f")).unwrap();
    commit_all(&d, "feature deletes");
    ok(&d, &["switch", "main"]);
    write(&d, "f", &body("MAIN"));
    commit_all(&d, "main edits");
    ok(&d, &["switch", "feature"]);
    fail(&d, &["merge", "main"]);
    ok(&d, &["resolve", "f", "--ours"]); // ours deleted it
    assert!(!d.join("f").exists());
    ok(&d, &["resolve", "--check"]);
    ok(&d, &["resolve", "f", "--theirs"]); // change of mind: keep their edit
    assert_eq!(read(&d, "f"), body("MAIN"));
    ok(&d, &["commit", "-m", "merged"]);
    assert_eq!(read(&d, "f"), body("MAIN"));
}

#[test]
fn works_during_a_rebase() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "f", BASE);
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    write(&d, "f", &body("FEAT"));
    commit_all(&d, "feature edit");
    ok(&d, &["switch", "main"]);
    write(&d, "f", &body("MAIN"));
    commit_all(&d, "main edit");
    ok(&d, &["switch", "feature"]);
    fail(&d, &["rebase", "main"]);
    let (out, _) = fail(&d, &["resolve", "--check"]);
    assert!(out.contains("unresolved: f"), "{out}");
    // in a rebase "ours" is the branch being rebased onto, "theirs" the replayed commit
    ok(&d, &["resolve", "f", "--theirs"]);
    ok(&d, &["resolve", "--check"]);
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(read(&d, "f"), body("FEAT"));

    // second round with --ours
    ok(&d, &["op", "undo"]);
    fail(&d, &["rebase", "main"]);
    ok(&d, &["resolve", "f", "--ours"]);
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(read(&d, "f"), body("MAIN"));
}
