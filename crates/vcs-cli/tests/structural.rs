//! `vcs merge` / `vcs rebase` use the structural Rust merge for `.rs` files (feature `structural`).
#![cfg_attr(not(feature = "structural"), allow(dead_code))]
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
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}

const BASE: &str = "fn a() -> i32 {\n    1\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n";
const MOVED: &str = "fn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n\nfn a() -> i32 {\n    1\n}\n";
const MERGED: &str = "fn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n\nfn a() -> i32 {\n    10\n}\n";

/// feature branch moves `a` to the end of the file; main edits `a`. Returns the repo dir.
fn setup(t: &Path, file: &str) -> std::path::PathBuf {
    ok(t, &["init", "r"]);
    let d = t.join("r");
    std::fs::write(d.join(file), BASE).unwrap();
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    std::fs::write(d.join(file), MOVED).unwrap();
    commit_all(&d, "move a");
    ok(&d, &["switch", "main"]);
    std::fs::write(d.join(file), BASE.replace("    1\n", "    10\n")).unwrap();
    commit_all(&d, "edit a");
    d
}

#[test]
#[cfg(feature = "structural")]
fn move_versus_edit_merges_cleanly() {
    let t = tempfile::tempdir().unwrap();
    let d = setup(t.path(), "lib.rs");
    let out = ok(&d, &["merge", "feature"]);
    assert!(out.contains("merged, new commit"), "{out}");
    assert_eq!(std::fs::read_to_string(d.join("lib.rs")).unwrap(), MERGED);
}

#[test]
#[cfg(feature = "structural")]
fn rebase_uses_structural_merge() {
    let t = tempfile::tempdir().unwrap();
    let d = setup(t.path(), "lib.rs");
    ok(&d, &["switch", "feature"]);
    ok(&d, &["rebase", "main"]);
    assert_eq!(std::fs::read_to_string(d.join("lib.rs")).unwrap(), MERGED);
}

#[test]
#[cfg(feature = "structural")]
fn no_structural_flag_gives_text_conflict() {
    let t = tempfile::tempdir().unwrap();
    let d = setup(t.path(), "lib.rs");
    let o = vcs(&d, &["merge", "feature", "--no-structural"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("conflict: lib.rs"));
    assert!(std::fs::read_to_string(d.join("lib.rs")).unwrap().contains("<<<<<<<"));
}

#[test]
#[cfg(feature = "structural")]
fn non_rust_file_and_unparsable_rust_fall_back_to_text() {
    let t = tempfile::tempdir().unwrap();
    let d = setup(t.path(), "lib.txt");
    let o = vcs(&d, &["merge", "feature"]);
    assert!(!o.status.success(), "non-.rs path must use the text merge");
    ok(&d, &["merge", "--abort"]);
    // a .rs file that does not parse also falls back to the (conflicting) text merge
    let t2 = tempfile::tempdir().unwrap();
    let d2 = t2.path().join("r");
    ok(t2.path(), &["init", "r"]);
    let bad = |x: &str| format!("fn a( {{\n{x}\n");
    std::fs::write(d2.join("x.rs"), bad("1")).unwrap();
    commit_all(&d2, "base");
    ok(&d2, &["branch", "f"]);
    ok(&d2, &["switch", "f"]);
    std::fs::write(d2.join("x.rs"), bad("2")).unwrap();
    commit_all(&d2, "f");
    ok(&d2, &["switch", "main"]);
    std::fs::write(d2.join("x.rs"), bad("3")).unwrap();
    commit_all(&d2, "m");
    let o = vcs(&d2, &["merge", "f"]);
    assert!(!o.status.success());
    assert!(std::fs::read_to_string(d2.join("x.rs")).unwrap().contains("<<<<<<<"));
}

/// Without the feature the flag is accepted and the result is the plain text conflict.
#[test]
#[cfg(not(feature = "structural"))]
fn feature_off_is_text_merge() {
    let t = tempfile::tempdir().unwrap();
    let d = setup(t.path(), "lib.rs");
    let o = vcs(&d, &["merge", "feature"]);
    assert!(!o.status.success());
    assert!(std::fs::read_to_string(d.join("lib.rs")).unwrap().contains("<<<<<<<"));
}
