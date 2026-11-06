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

#[test]
fn rebase_merges_flag_recreates_merges_and_rejects_bad_combinations() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "f", "base\n");
    commit_all(&d, "base");
    ok(&d, &["branch", "feature"]);
    ok(&d, &["switch", "feature"]);
    write(&d, "a", "a\n");
    commit_all(&d, "add a");
    ok(&d, &["branch", "side"]);
    ok(&d, &["switch", "side"]);
    write(&d, "s", "s\n");
    commit_all(&d, "add s");
    ok(&d, &["switch", "feature"]);
    write(&d, "b", "b\n");
    commit_all(&d, "add b");
    ok(&d, &["merge", "side"]);
    ok(&d, &["switch", "main"]);
    write(&d, "h", "h\n");
    commit_all(&d, "main h");
    ok(&d, &["switch", "feature"]);

    // the linear rebase still refuses, and says what to do
    let o = vcs(&d, &["rebase", "main"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--rebase-merges"));
    // bad combinations and modes are refused before anything changes
    for args in [
        &["rebase", "--rebase-merges", "-i", "main"][..],
        &["rebase", "--rebase-merges", "--continue"][..],
        &["rebase", "--rebase-merges=bogus", "main"][..],
    ] {
        assert!(!vcs(&d, args).status.success(), "{args:?}");
    }
    let out = ok(&d, &["rebase", "--rebase-merges", "main"]);
    assert!(out.contains("rebased 4 commit(s)"), "{out}");
    for f in ["a", "b", "s", "h"] {
        assert!(d.join(f).exists(), "{f}");
    }
    assert!(ok(&d, &["op", "log"]).contains("rebase root/feature onto main"));
    ok(&d, &["op", "undo"]);
    assert!(!d.join("h").exists());
    // a mode that is accepted
    ok(&d, &["rebase", "--rebase-merges=rebase-cousins", "main"]);
}
