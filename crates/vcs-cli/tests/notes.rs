//! `vcs notes` end to end through the CLI: add, show, list, append, remove, publish/clone, gc, and the
//! hidden notes ref. vcs never calls git here; the twin comparison with real git is the `notes` scenario
//! of tests/gitmap.rs.
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
fn commit_file(dir: &Path, f: &str, c: &str, msg: &str) {
    std::fs::write(dir.join(f), c).unwrap();
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}

#[test]
fn add_show_list_append_remove() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    ok(d, &["init", "."]);
    commit_file(d, "f.txt", "1\n", "one");
    commit_file(d, "f.txt", "2\n", "two");

    assert_eq!(ok(d, &["notes", "list"]), "");
    assert!(fail(d, &["notes", "show"]).contains("no note"));
    ok(d, &["notes", "add", "-m", "para one", "-m", "para two", "HEAD~1"]);
    assert_eq!(ok(d, &["notes", "show", "HEAD~1"]), "para one\n\npara two\n");
    assert!(fail(d, &["notes", "add", "-m", "x", "HEAD~1"]).contains("existing notes"));
    ok(d, &["notes", "add", "--force", "-m", "replaced", "HEAD~1"]);
    std::fs::write(d.join("note.txt"), "from a file\n\n\n").unwrap();
    ok(d, &["notes", "append", "-F", "note.txt", "HEAD~1"]);
    assert_eq!(ok(d, &["notes", "show", "HEAD~1"]), "replaced\n\nfrom a file\n");
    ok(d, &["notes", "add", "-m", "head note"]);

    let list = ok(d, &["notes"]);
    assert_eq!(list.lines().count(), 2, "{list}");
    for l in list.lines() {
        let cols: Vec<&str> = l.split(' ').collect();
        assert!(cols.len() == 2 && cols.iter().all(|c| c.len() == 64), "{l}");
    }
    let one = ok(d, &["notes", "list", "HEAD"]);
    assert_eq!(one.trim().len(), 64);

    // the notes ref is not a branch
    let branches = ok(d, &["branch"]);
    assert!(!branches.contains("notes"), "{branches}");
    assert!(fail(d, &["branch", "refs/notes/commits"]).contains("invalid branch name"));
    assert!(fail(d, &["switch", "refs/notes/commits"]).contains("invalid branch name"));

    ok(d, &["notes", "remove", "HEAD", "HEAD~1"]);
    assert_eq!(ok(d, &["notes", "list"]), "");
    assert!(fail(d, &["notes", "remove", "HEAD"]).contains("no note"));
    assert!(fail(d, &["notes", "add", "HEAD"]).contains("no note text"));
    assert!(ok(d, &["fsck"]).contains("ok"));
}

#[test]
fn notes_travel_with_publish_clone_fetch_and_survive_gc() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    let url = remote.to_str().unwrap().to_owned();
    let a = t.path().join("a");
    ok(t.path(), &["init", "a"]);
    commit_file(&a, "f.txt", "one\n", "init");
    commit_file(&a, "f.txt", "two\n", "second");
    ok(&a, &["notes", "add", "-m", "annotated in a", "HEAD~1"]);
    ok(&a, &["remote", "set", &url]);
    ok(&a, &["publish"]);
    ok(t.path(), &["clone", &url, "b"]);
    let b = t.path().join("b");
    assert_eq!(ok(&b, &["notes", "show", "HEAD~1"]), "annotated in a\n");

    ok(&b, &["notes", "add", "-m", "from b", "HEAD"]);
    ok(&b, &["notes", "append", "-m", "b again", "HEAD~1"]);
    let out = ok(&b, &["publish"]);
    assert!(out.contains("refs/notes/commits"), "{out}");
    ok(&a, &["fetch"]);
    assert_eq!(ok(&a, &["notes", "show", "HEAD"]), "from b\n");
    assert_eq!(ok(&a, &["notes", "show", "HEAD~1"]), "annotated in a\n\nb again\n");

    // diverge, then publish warns instead of failing, fetch merges, publish succeeds
    ok(&a, &["notes", "append", "-m", "a late", "HEAD"]);
    ok(&b, &["notes", "append", "-m", "b late", "HEAD"]);
    ok(&b, &["publish"]);
    let o = vcs(&a, &["publish"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("notes of zone root were not published"));
    ok(&a, &["fetch"]);
    assert_eq!(ok(&a, &["notes", "show", "HEAD"]), "from b\n\na late\n\nfrom b\n\nb late\n");
    ok(&a, &["publish"]);
    ok(&b, &["fetch"]);
    assert_eq!(ok(&b, &["notes", "show", "HEAD"]), ok(&a, &["notes", "show", "HEAD"]));

    // gc keeps them
    ok(&a, &["gc", "--grace-secs", "1209600"]);
    assert_eq!(ok(&a, &["notes", "show", "HEAD~1"]), "annotated in a\n\nb again\n");
}
