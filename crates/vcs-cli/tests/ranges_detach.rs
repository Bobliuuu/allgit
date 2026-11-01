//! Native `vcs` ranges (`A..B`, `A...B`, `^A`, `--not`, `--all`, `--all-zones`), `rev-list`,
//! detached HEAD (`switch --detach`, commit, warning, rescue, `op undo`) and `branch -m`.
use std::path::Path;
use std::process::{Command, Output};

fn run(dir: &Path, ts: u64, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", ts.to_string())
        .output()
        .unwrap()
}
fn both(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn ok(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, 1_700_000_000, args);
    assert!(o.status.success(), "vcs {args:?} failed: {}", both(&o));
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn fails(dir: &Path, args: &[&str]) -> String {
    let o = run(dir, 1_700_000_000, args);
    assert!(!o.status.success(), "vcs {args:?} should fail: {}", both(&o));
    both(&o)
}
fn commit(dir: &Path, file: &str, msg: &str, ts: u64) {
    std::fs::write(dir.join(file), msg).unwrap();
    let o = run(dir, ts, &["add", file]);
    assert!(o.status.success(), "{}", both(&o));
    let o = run(dir, ts, &["commit", "-m", msg]);
    assert!(o.status.success(), "{}", both(&o));
}
fn subjects(dir: &Path, args: &[&str]) -> Vec<String> {
    let mut a = vec!["log", "-n", "100000", "--format=%s"];
    a.extend_from_slice(args);
    ok(dir, &a).lines().map(str::to_owned).filter(|l| !l.is_empty()).collect()
}

/// m0..m(n-1) on main, `side` branches off at m(n/2) with s1 s2.
fn repo(n: u64) -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    ok(t.path(), &["init", "r"]);
    let d = t.path().join("r");
    for i in 0..n {
        commit(&d, &format!("f{}", i % 5), &format!("m{i}"), 1_700_000_000 + i * 10);
        if i == n / 2 {
            ok(&d, &["branch", "side"]);
        }
    }
    ok(&d, &["switch", "side"]);
    commit(&d, "s1f", "s1", 1_700_100_000);
    commit(&d, "s2f", "s2", 1_700_100_010);
    ok(&d, &["switch", "main"]);
    (t, d)
}

#[test]
fn ranges_rev_list_and_caches_agree() {
    let (_t, d) = repo(60);
    let want_side_only = ["s2", "s1"];
    assert_eq!(subjects(&d, &["main..side"]), want_side_only);
    assert_eq!(subjects(&d, &["^main", "side"]), want_side_only);
    assert_eq!(subjects(&d, &["side", "--not", "main"]), want_side_only);
    assert_eq!(subjects(&d, &["side", "--not", "main"]), subjects(&d, &["side", "^main"]));
    assert_eq!(subjects(&d, &["main..side", "--reverse"]), ["s1", "s2"]);
    let sym = subjects(&d, &["main...side"]);
    assert_eq!(sym.len(), 2 + 29, "{sym:?}"); // s1 s2 + main commits after the fork (m31..m59)
    assert_eq!(sym[0], "s2");
    assert_eq!(sym[2], "m59");
    assert_eq!(subjects(&d, &["main...side"]), subjects(&d, &["side...main"]));
    assert_eq!(subjects(&d, &["main~3..main"]), ["m59", "m58", "m57"]);
    assert_eq!(ok(&d, &["rev-list", "--count", "main..side"]).trim(), "2");
    assert_eq!(ok(&d, &["rev-list", "--count", "main...side"]).trim(), "31");
    assert_eq!(ok(&d, &["rev-list", "--count", "--all"]).trim(), "62");
    assert_eq!(ok(&d, &["rev-list", "-n", "5", "--all"]).lines().count(), 5);
    assert_eq!(subjects(&d, &["--all"]).len(), 62);
    // a range limited to one path
    assert_eq!(subjects(&d, &["main..side", "--", "s1f"]), ["s1"]);
    // an empty range prints nothing, and is not an error
    assert!(subjects(&d, &["side..side"]).is_empty());
    assert!(fails(&d, &["log", "nope..main"]).contains("nope"));
    // shortlog over a range
    assert!(ok(&d, &["shortlog", "-s", "main..side"]).contains('2'));
    // the commit-graph and changed-path caches are only caches
    let before = (ok(&d, &["rev-list", "main...side"]), ok(&d, &["rev-list", "--all"]));
    let _ = std::fs::remove_dir_all(d.join(".vcs/commit-graphs"));
    let _ = std::fs::remove_dir_all(d.join(".vcs/path-filters"));
    let after = (ok(&d, &["rev-list", "main...side"]), ok(&d, &["rev-list", "--all"]));
    assert_eq!(before, after);
    // json
    let j = ok(&d, &["--json", "rev-list", "main..side"]);
    assert!(j.contains("\"count\":2") || j.contains("\"count\": 2"), "{j}");
}

#[test]
fn all_covers_branches_tags_and_all_zones_interleave() {
    let (_t, d) = repo(10);
    ok(&d, &["tag", "v-old", "main~5"]);
    // --all: every branch and tag of the zone; a tag alone adds nothing new here
    assert_eq!(subjects(&d, &["--all"]).len(), 12);
    // a second zone has its own history
    std::fs::create_dir_all(d.join("lib")).unwrap();
    ok(&d, &["zone", "create", "lib", "--path", "lib"]);
    commit(&d, "lib/a.txt", "lib-a", 1_700_000_005);
    commit(&d, "lib/b.txt", "lib-b", 1_700_200_000);
    // zone semantics: `--all` stays inside the zone of the current directory
    let root_all = subjects(&d, &["--all"]);
    assert!(!root_all.iter().any(|s| s.starts_with("lib-")), "{root_all:?}");
    let lib_all = ok(&d, &["log", "--zone", "lib", "--all", "--format=%s"]);
    assert_eq!(lib_all.lines().collect::<Vec<_>>(), ["lib-b", "lib-a"]);
    // --all-zones interleaves both by time
    let z = subjects(&d, &["--all-zones"]);
    assert_eq!(z.len(), 12 + 2, "{z:?}");
    assert_eq!(z[0], "lib-b");
    let pos = |n: &str| z.iter().position(|s| s == n).unwrap();
    // lib-a has time +5: after m1 (+10) and before m0 (+0) in newest-first order
    assert!(pos("lib-a") > pos("m1") && pos("lib-a") < pos("m0"), "{z:?}");
}

#[test]
fn detached_head_end_to_end() {
    let (_t, d) = repo(6);
    ok(&d, &["tag", "v1", "main~3"]);
    // a tag without --detach is not a branch
    let e = fails(&d, &["switch", "v1"]);
    assert!(e.contains("--detach"), "{e}");
    let out = run(&d, 1_700_000_000, &["switch", "--detach", "v1"]);
    assert!(out.status.success(), "{}", both(&out));
    let text = both(&out);
    assert!(text.contains("detached"), "{text}");
    let st = ok(&d, &["status"]);
    assert!(st.contains("HEAD detached at"), "{st}");
    // branches list does not invent a branch
    let br = ok(&d, &["branch"]);
    assert!(br.contains("(HEAD detached at"), "{br}");
    assert!(!br.contains(".detached"), "{br}");
    // commit on the detached HEAD
    commit(&d, "dx", "det1", 1_700_300_000);
    commit(&d, "dy", "det2", 1_700_300_010);
    assert_eq!(subjects(&d, &[])[..2], ["det2", "det1"]);
    assert!(subjects(&d, &["--all"]).contains(&"det2".to_owned()));
    assert_eq!(subjects(&d, &["main..HEAD"]), ["det2", "det1"]);
    // publish/sync never see it: nothing to publish without a remote, but the bundle excludes it
    let bundle = d.join("../b.bundle");
    ok(&d, &["bundle", "create", bundle.to_str().unwrap()]);
    let v = ok(&d, &["bundle", "verify", bundle.to_str().unwrap()]);
    assert!(!v.contains(".detached"), "{v}");
    // leaving warns about the commits no branch reaches, and says how to keep them
    let left = run(&d, 1_700_000_000, &["switch", "main"]);
    assert!(left.status.success(), "{}", both(&left));
    let lt = both(&left);
    assert!(lt.contains("leaving 2 commit(s)"), "{lt}");
    assert!(lt.contains("det2") && lt.contains("vcs branch <name>"), "{lt}");
    assert!(ok(&d, &["status"]).contains("branch main"));
    // `op undo` puts the detached HEAD back, with its files
    ok(&d, &["op", "undo"]);
    assert!(ok(&d, &["status"]).contains("HEAD detached at"));
    assert!(d.join("dy").exists());
    // rescue: a branch at the detached commit, then switch without a warning
    ok(&d, &["branch", "rescued"]);
    let left = run(&d, 1_700_000_000, &["switch", "main"]);
    assert!(!both(&left).contains("leaving"), "{}", both(&left));
    assert_eq!(subjects(&d, &["rescued"])[..2], ["det2", "det1"]);
    // dirty tree refuses to detach
    std::fs::write(d.join("f0"), "dirty").unwrap();
    fails(&d, &["switch", "--detach", "v1"]);
    ok(&d, &["restore", "f0"]);
    // gc keeps the commits of a detached HEAD
    ok(&d, &["switch", "--detach", "rescued"]);
    commit(&d, "dz", "det3", 1_700_300_020);
    ok(&d, &["gc"]);
    assert_eq!(subjects(&d, &[])[0], "det3");
    ok(&d, &["fsck"]);
}

#[test]
fn branch_rename_and_start_point() {
    let (_t, d) = repo(6);
    ok(&d, &["branch", "old", "main~2"]);
    assert_eq!(subjects(&d, &["old"])[0], "m3");
    ok(&d, &["branch", "-m", "old", "new"]);
    assert!(fails(&d, &["log", "old"]).contains("old"));
    assert_eq!(subjects(&d, &["new"])[0], "m3");
    // the checked-out branch: `-m <new>` and the workspace follows
    ok(&d, &["branch", "-m", "trunk"]);
    assert!(ok(&d, &["status"]).contains("branch trunk"));
    assert!(fails(&d, &["branch", "-m", "trunk", "new"]).contains("exists"));
    ok(&d, &["branch", "-M", "trunk", "new"]); // replaces `new`, which is not checked out
    assert!(ok(&d, &["status"]).contains("branch new"));
    // undo restores names and the workspace
    ok(&d, &["op", "undo"]);
    assert!(ok(&d, &["status"]).contains("branch trunk"));
    assert!(fails(&d, &["branch", "-m", "nope", "x"]).contains("nope"));
    ok(&d, &["fsck"]);
}
