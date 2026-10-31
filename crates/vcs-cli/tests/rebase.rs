//! End-to-end `vcs rebase`: replay, conflict stop, resolve + continue, undo.
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
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}
/// "change <id>" lines of `vcs log`, newest first.
fn changes(dir: &Path) -> Vec<String> {
    ok(dir, &["log"])
        .lines()
        .filter_map(|l| l.strip_prefix("change "))
        .map(|l| l.split_whitespace().next().unwrap().to_owned())
        .collect()
}

#[test]
fn rebase_conflict_continue_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "f", "1\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    write(&d, "f", "1\n2\n3\n4\n5\n6\n7\nFEAT\n");
    commit_all(&d, "feature edit");
    let feat_changes = changes(&d);
    ok(&d, &["switch", "main"]);
    write(&d, "f", "1\n2\n3\n4\n5\n6\n7\nMAIN\n");
    commit_all(&d, "main edit");
    ok(&d, &["switch", "feature"]);

    let o = vcs(&d, &["rebase", "main"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("conflict: f"));
    assert!(ok(&d, &["status"]).contains("rebase in progress"));
    assert!(std::fs::read_to_string(d.join("f")).unwrap().contains("<<<<<<<"));
    let c = vcs(&d, &["commit", "-m", "x"]);
    assert!(String::from_utf8_lossy(&c.stderr).contains("rebase --continue"));

    write(&d, "f", "1\n2\n3\n4\n5\n6\n7\nBOTH\n");
    ok(&d, &["add", "f"]);
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(changes(&d)[0], feat_changes[0], "change id survives the rebase");
    assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "1\n2\n3\n4\n5\n6\n7\nBOTH\n");
    assert!(ok(&d, &["op", "log"]).contains("rebase root/feature onto main"));

    ok(&d, &["op", "undo"]);
    assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "1\n2\n3\n4\n5\n6\n7\nFEAT\n");
    assert_eq!(changes(&d), feat_changes);

    // abort path
    let o = vcs(&d, &["rebase", "main"]);
    assert!(!o.status.success());
    ok(&d, &["rebase", "--abort"]);
    assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "1\n2\n3\n4\n5\n6\n7\nFEAT\n");
}
