//! End-to-end `vcs rebase -i` with non-interactive editors (the path scripts and CI use).
use std::path::Path;
use std::process::{Command, Output};

fn vcs_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env_remove("GIT_EDITOR")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .env_remove("VCS_EDITOR")
        .env_remove("GIT_SEQUENCE_EDITOR")
        .env_remove("VCS_SEQUENCE_EDITOR");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn ok_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let o = vcs_env(dir, args, env);
    assert!(
        o.status.success(),
        "vcs {args:?} failed: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    ok_env(dir, args, &[])
}
fn write(dir: &Path, f: &str, c: &str) {
    std::fs::write(dir.join(f), c).unwrap();
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}
fn subjects(dir: &Path) -> Vec<String> {
    ok(dir, &["log", "--format=%s"]).lines().map(str::to_owned).collect()
}
fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p.to_str().unwrap().to_owned()
}
fn setup(n: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("r");
    ok(t.path(), &["init", "r"]);
    write(&d, "base", "base\n");
    commit_all(&d, "base");
    for i in 1..=n {
        write(&d, &format!("c{i}"), &format!("{i}\n"));
        commit_all(&d, &format!("commit {i}"));
    }
    (t, d)
}

#[test]
fn reverse_with_sequence_editor_and_undo() {
    let (t, d) = setup(3);
    let before = ok(&d, &["log", "-n", "1", "--format=%H"]);
    let rev = script(
        t.path(),
        "rev.sh",
        r#"grep '^pick' "$1" | sed '1!G;h;$!d' > "$1.new"; mv "$1.new" "$1""#,
    );
    ok_env(&d, &["rebase", "-i", "HEAD~3"], &[("GIT_SEQUENCE_EDITOR", &rev)]);
    assert_eq!(subjects(&d), ["commit 1", "commit 2", "commit 3", "base"]);
    assert!(d.join("c1").exists() && d.join("c3").exists());
    ok(&d, &["op", "undo"]);
    assert_eq!(subjects(&d), ["commit 3", "commit 2", "commit 1", "base"]);
    assert_eq!(ok(&d, &["log", "-n", "1", "--format=%H"]), before);
}

#[test]
fn drop_squash_fixup_reword() {
    let (t, d) = setup(5);
    let ed = script(
        t.path(),
        "ed.sh",
        r#"awk 'NR==1{$1="reword"} NR==2{$1="drop"} NR==3{$1="pick"} NR==4{$1="squash"} NR==5{$1="fixup"} {print}' "$1" > "$1.new"; mv "$1.new" "$1""#,
    );
    let msg = script(
        t.path(),
        "msg.sh",
        r#"if grep -q 'commit 3' "$1"; then printf 'combined 3-4\n' > "$1"; else printf 'renamed 1\n' > "$1"; fi"#,
    );
    ok_env(
        &d,
        &["rebase", "-i", "HEAD~5"],
        &[("GIT_SEQUENCE_EDITOR", &ed), ("GIT_EDITOR", &msg)],
    );
    assert_eq!(subjects(&d), ["combined 3-4", "renamed 1", "base"]);
    assert!(d.join("c1").exists() && !d.join("c2").exists() && d.join("c3").exists() && d.join("c4").exists() && d.join("c5").exists());
}

#[test]
fn autosquash_with_noop_editor() {
    let (_t, d) = setup(2);
    write(&d, "c1", "1 fixed\n");
    commit_all(&d, "fixup! commit 1");
    ok_env(&d, &["rebase", "-i", "--autosquash", "HEAD~3"], &[("GIT_SEQUENCE_EDITOR", "true")]);
    assert_eq!(subjects(&d), ["commit 2", "commit 1", "base"]);
    assert_eq!(std::fs::read_to_string(d.join("c1")).unwrap(), "1 fixed\n");
}

#[test]
fn edit_stop_commit_and_continue_then_abort_variants() {
    let (t, d) = setup(3);
    let ed = script(
        t.path(),
        "ed.sh",
        r#"sed 's/^pick \(.*commit 2\)/edit \1/' "$1" > "$1.new"; mv "$1.new" "$1""#,
    );
    let o = vcs_env(&d, &["rebase", "-i", "HEAD~3"], &[("GIT_SEQUENCE_EDITOR", &ed)]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stdout).contains("stopped at"));
    assert!(ok(&d, &["status"]).contains("rebase of"));
    assert!(!vcs_env(&d, &["rebase", "-i", "HEAD~1"], &[]).status.success());
    write(&d, "extra", "x\n");
    commit_all(&d, "inserted");
    ok(&d, &["rebase", "--continue"]);
    assert_eq!(subjects(&d), ["commit 3", "inserted", "commit 2", "commit 1", "base"]);

    // abort from an edit stop restores everything
    let o = vcs_env(
        &d,
        &["rebase", "-i", "HEAD~2"],
        &[(
            "GIT_SEQUENCE_EDITOR",
            &script(t.path(), "e2.sh", r#"sed 's/^pick/edit/' "$1" > "$1.new"; mv "$1.new" "$1""#),
        )],
    );
    assert!(o.status.success());
    ok(&d, &["rebase", "--abort"]);
    assert_eq!(subjects(&d), ["commit 3", "inserted", "commit 2", "commit 1", "base"]);
    assert!(!ok(&d, &["status"]).contains("rebase of"));
}

#[test]
fn exec_option_runs_without_an_editor_and_failure_stops() {
    let (_t, d) = setup(2);
    ok(&d, &["rebase", "-x", "test -f c1 || test -f c2", "HEAD~2"]);
    let o = vcs_env(&d, &["rebase", "-x", "test -f c2", "HEAD~2"], &[]);
    assert!(!o.status.success(), "exec failure on commit 1 must stop with a failure status");
    assert!(String::from_utf8_lossy(&o.stderr).contains("failed"));
    ok(&d, &["rebase", "--abort"]);
    assert_eq!(subjects(&d), ["commit 2", "commit 1", "base"]);
}

#[test]
fn skip_and_missing_editor_and_empty_todo() {
    let (t, d) = setup(2);
    // no editor at all: refused before anything changes
    let o = vcs_env(&d, &["rebase", "-i", "HEAD~2"], &[]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no editor"));
    // empty todo: nothing to do
    let empty = script(t.path(), "empty.sh", r#": > "$1""#);
    let o = vcs_env(&d, &["rebase", "-i", "HEAD~2"], &[("GIT_SEQUENCE_EDITOR", &empty)]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("nothing to do"));
    assert_eq!(subjects(&d), ["commit 2", "commit 1", "base"]);
    // --skip with no rebase
    assert!(!vcs_env(&d, &["rebase", "--skip"], &[]).status.success());
}
