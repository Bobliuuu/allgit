//! `vcs merge --assist <command>` with scripted fake assistants (shell scripts). No model is involved.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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
/// A repo whose `main` and `side` both edited line 4 of f.rs; the checked-out branch is main.
fn conflicted_repo(root: &Path) -> PathBuf {
    ok(root, &["init", "r"]);
    let r = root.join("r");
    write(&r, "f.rs", "1\n2\n3\n4\n5\n6\n7\n8\n");
    commit_all(&r, "init");
    ok(&r, &["branch", "side"]);
    write(&r, "f.rs", "1\n2\n3\nMAIN\n5\n6\n7\n8\n");
    commit_all(&r, "main edit");
    ok(&r, &["switch", "side"]);
    write(&r, "f.rs", "1\n2\n3\nSIDE\n5\n6\n7\n8\n");
    commit_all(&r, "side edit");
    ok(&r, &["switch", "main"]);
    r
}
/// Write an executable-free script (run via `sh script`) and return the command string.
fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    format!("sh {}", p.display())
}
const GOOD: &str = r#"cat >/dev/null; printf '%s' '{"resolution":"1\n2\n3\nBOTH\n5\n6\n7\n8\n","explanation":"kept both","junk":[1,2]}'"#;

#[test]
fn proposal_is_saved_not_applied_then_accepted() {
    let t = tempfile::tempdir().unwrap();
    let r = conflicted_repo(t.path());
    let cmd = script(t.path(), "good.sh", GOOD);
    let (out, err) = fail(&r, &["merge", "side", "--assist", &cmd]);
    assert!(
        out.contains("assist: f.rs: proposal saved") && err.contains("conflicts"),
        "{out}{err}"
    );
    // the working file still has markers, the conflict is unresolved, commit is refused
    let marked = read(&r, "f.rs");
    assert!(marked.contains("<<<<<<<") && !marked.contains("BOTH"), "{marked}");
    assert!(ok(&r, &["conflict", "list"]).contains("unresolved"));
    fail(&r, &["commit", "-m", "x"]);
    // review shows a diff and the explanation
    let show = ok(&r, &["resolve"]);
    assert!(
        show.contains("proposal for f.rs") && show.contains("kept both") && show.contains("+ BOTH") && show.contains("- MAIN"),
        "{show}"
    );
    assert!(read(&r, "f.rs").contains("<<<<<<<"), "review must not apply");
    // accept
    let out = ok(&r, &["resolve", "--accept"]);
    assert!(out.contains("accepted proposal for f.rs"), "{out}");
    assert_eq!(read(&r, "f.rs"), "1\n2\n3\nBOTH\n5\n6\n7\n8\n");
    assert!(ok(&r, &["conflict", "list"]).contains("resolved"));
    ok(&r, &["commit", "-m", "merge side"]);
    ok(&r, &["fsck"]);
    // proposals are gone with the merge
    let (_, err) = fail(&r, &["resolve"]);
    assert!(err.contains("no merge in progress"), "{err}");
}

#[test]
fn request_carries_base_ours_theirs_path_and_language() {
    let t = tempfile::tempdir().unwrap();
    let r = conflicted_repo(t.path());
    let seen = t.path().join("seen.json");
    let cmd = script(t.path(), "spy.sh", &format!("cat > {}; printf '{{}}'", seen.display()));
    let (out, _) = fail(&r, &["merge", "side", "--assist", &cmd]);
    assert!(out.contains("assistant answer ignored"), "{out}"); // `{}` has no resolution
    let req = std::fs::read_to_string(seen).unwrap();
    for needle in [
        r#""version":1"#,
        r#""path":"f.rs""#,
        r#""language":"rust""#,
        r#""base":"1\n2\n3\n4\n5\n6\n7\n8\n""#,
        r#""ours":"1\n2\n3\nMAIN\n5\n6\n7\n8\n""#,
        r#""theirs":"1\n2\n3\nSIDE\n5\n6\n7\n8\n""#,
    ] {
        assert!(req.contains(needle), "missing {needle} in {req}");
    }
}

#[test]
fn garbage_output_is_ignored() {
    let t = tempfile::tempdir().unwrap();
    for (i, body) in [
        "cat >/dev/null; echo not json at all",
        "cat >/dev/null; printf '[1,2,3]'",
        r#"cat >/dev/null; printf '{"resolution":42}'"#,
        r#"cat >/dev/null; printf '{"resolution":"<<<<<<< x\na\n=======\nb\n>>>>>>> y\n"}'"#,
        "cat >/dev/null; printf '\\377\\376'",
        "cat >/dev/null; exit 3",
        "cat >/dev/null",
        "exit 0",
    ]
    .iter()
    .enumerate()
    {
        let root = t.path().join(format!("c{i}"));
        std::fs::create_dir(&root).unwrap();
        let r = conflicted_repo(&root);
        let cmd = script(&root, "bad.sh", body);
        let (out, _) = fail(&r, &["merge", "side", "--assist", &cmd]);
        assert!(
            !out.contains("proposal saved") && !out.contains("proposal(s) saved"),
            "case {i}: {out}"
        );
        assert!(out.contains("assist: f.rs:"), "case {i}: {out}");
        assert!(ok(&r, &["resolve"]).contains("no proposals"), "case {i}");
        let (_, err) = fail(&r, &["resolve", "--accept"]);
        assert!(err.contains("no proposals"), "case {i}: {err}");
        assert!(read(&r, "f.rs").contains("<<<<<<<"), "case {i}");
    }
}

#[test]
fn oversized_output_is_rejected_and_killed() {
    let t = tempfile::tempdir().unwrap();
    let r = conflicted_repo(t.path());
    let cmd = script(t.path(), "big.sh", "cat >/dev/null; yes '{\"resolution\":\"x\"}' ");
    let start = Instant::now();
    let (out, _) = fail(&r, &["merge", "side", "--assist", &cmd]);
    assert!(out.contains("too large"), "{out}");
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn slow_assistant_times_out() {
    let t = tempfile::tempdir().unwrap();
    let r = conflicted_repo(t.path());
    let cmd = script(t.path(), "slow.sh", "cat >/dev/null; exec sleep 30");
    let start = Instant::now();
    let (out, _) = fail(&r, &["merge", "side", "--assist", &cmd, "--assist-timeout", "1"]);
    assert!(out.contains("timed out after 1s"), "{out}");
    assert!(start.elapsed() < Duration::from_secs(15), "{:?}", start.elapsed());
    assert!(ok(&r, &["resolve"]).contains("no proposals"));
    // the merge is still intact and abortable
    ok(&r, &["merge", "--abort"]);
}

#[test]
fn accept_refuses_edited_file_unless_forced_and_abort_drops_proposals() {
    let t = tempfile::tempdir().unwrap();
    let r = conflicted_repo(t.path());
    let cmd = script(t.path(), "good.sh", GOOD);
    fail(&r, &["merge", "side", "--assist", &cmd]);
    write(&r, "f.rs", "my own edit\n");
    let (_, err) = fail(&r, &["resolve", "--accept"]);
    assert!(err.contains("edited since"), "{err}");
    assert_eq!(read(&r, "f.rs"), "my own edit\n");
    ok(&r, &["resolve", "--accept", "--force", "f.rs"]);
    assert_eq!(read(&r, "f.rs"), "1\n2\n3\nBOTH\n5\n6\n7\n8\n");
    ok(&r, &["merge", "--abort"]);
    // a new merge starts without the old proposal
    fail(&r, &["merge", "side"]);
    assert!(ok(&r, &["resolve"]).contains("no proposals"));
}

#[test]
fn binary_conflicts_are_skipped() {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let r = t.path().join("r");
    std::fs::write(r.join("b.bin"), [0u8, 1, 2]).unwrap();
    commit_all(&r, "init");
    ok(&r, &["branch", "side"]);
    std::fs::write(r.join("b.bin"), [0u8, 1, 3]).unwrap();
    commit_all(&r, "m");
    ok(&r, &["switch", "side"]);
    std::fs::write(r.join("b.bin"), [0u8, 1, 4]).unwrap();
    commit_all(&r, "s");
    ok(&r, &["switch", "main"]);
    let cmd = script(t.path(), "good.sh", GOOD);
    let (out, _) = fail(&r, &["merge", "side", "--assist", &cmd]);
    assert!(out.contains("assist: b.bin: skipped (binary"), "{out}");
}
