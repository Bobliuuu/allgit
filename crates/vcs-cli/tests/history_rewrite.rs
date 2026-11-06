//! `vcs rewrite` (git filter-branch / filter-repo core) through the real binary: path filters,
//! message / env / tree / tag-name filters, tag moves, one-op undo, and the refusal to change
//! published history without --force. Contract: docs/history-rewrite.md.
use std::path::Path;
use std::process::{Command, Output};

fn vcs(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000")
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
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
fn commit_all(dir: &Path, msg: &str) {
    ok(dir, &["add", "."]);
    ok(dir, &["commit", "-m", msg]);
}
/// `%H|%an|%ae|%at|%s` per commit, newest first.
fn rows(dir: &Path) -> Vec<Vec<String>> {
    ok(dir, &["log", "--format", "%H|%an|%ae|%at|%s"])
        .lines()
        .map(|l| l.splitn(5, '|').map(str::to_owned).collect())
        .collect()
}
fn subjects(dir: &Path) -> Vec<String> {
    rows(dir).into_iter().map(|r| r[4].clone()).collect()
}
fn repo(t: &Path) -> std::path::PathBuf {
    ok(t, &["init", "r"]);
    t.join("r")
}

/// lib/ + README history: c1 adds both, c2 touches README only, c3 edits lib.
fn lib_history(d: &Path) {
    write(d, "README", "hello\n");
    write(d, "lib/a.txt", "a1\n");
    commit_all(d, "add lib and readme");
    ok(d, &["tag", "v0", "-m", "first release"]);
    write(d, "README", "hello world\n");
    commit_all(d, "readme only");
    write(d, "lib/a.txt", "a2\n");
    commit_all(d, "edit lib");
    ok(d, &["tag", "v1"]);
}

#[test]
fn subdirectory_filter_moves_branch_and_tags_and_one_undo_restores_all() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    lib_history(&d);
    let before = rows(&d);
    assert_eq!(before.len(), 3);

    let out = ok(&d, &["rewrite", "--subdirectory-filter", "lib"]);
    assert!(out.contains("1 dropped as empty"), "{out}");
    assert_eq!(
        subjects(&d),
        ["edit lib", "add lib and readme"],
        "the README-only commit is dropped"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("a.txt")).unwrap(),
        "a2\n",
        "lib/ is the new root, worktree follows"
    );
    assert!(!d.join("README").exists() && !d.join("lib").exists());
    // both tags follow their commits (annotated one stays annotated)
    let head = rows(&d)[0][0].clone();
    let list = ok(&d, &["tag"]);
    assert!(list.contains("v1") && list.contains("annotated: first release"), "{list}");
    assert_eq!(ok(&d, &["rev-parse", "v1"]).trim(), head);
    assert_eq!(ok(&d, &["rev-parse", "v0"]).trim(), rows(&d)[1][0]);
    assert!(ok(&d, &["op", "log"]).contains("rewrite history of root/main"));

    ok(&d, &["op", "undo"]);
    assert_eq!(rows(&d), before, "every commit id is back");
    assert!(d.join("README").exists() && d.join("lib/a.txt").exists());
    assert_eq!(ok(&d, &["rev-parse", "v1"]).trim(), before[0][0]);
    assert_eq!(ok(&d, &["rev-parse", "v0"]).trim(), before[2][0]);
}

#[test]
fn keep_and_remove_paths() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    write(&d, "src/main.rs", "m1\n");
    write(&d, "src/gen/big.rs", "g\n");
    write(&d, "secret.env", "KEY=1\n");
    commit_all(&d, "one");
    write(&d, "secret.env", "KEY=2\n");
    commit_all(&d, "rotate secret");
    write(&d, "src/main.rs", "m2\n");
    commit_all(&d, "edit main");

    ok(&d, &["rewrite", "--remove", "secret.env", "--remove", "src/gen"]);
    assert_eq!(
        subjects(&d),
        ["edit main", "one"],
        "the commit that only touched the secret vanishes"
    );
    assert!(!d.join("secret.env").exists() && !d.join("src/gen").exists());
    assert_eq!(std::fs::read_to_string(d.join("src/main.rs")).unwrap(), "m2\n");
    ok(&d, &["op", "undo"]);
    assert_eq!(subjects(&d).len(), 3);
    assert!(d.join("secret.env").exists());

    let out = ok(&d, &["rewrite", "--keep", "src/main.rs", "--keep-empty"]);
    assert!(out.contains("0 dropped"), "{out}");
    assert_eq!(subjects(&d).len(), 3, "--keep-empty keeps the emptied commit");
    assert!(!d.join("secret.env").exists() && d.join("src/main.rs").exists() && !d.join("src/gen").exists());
}

#[test]
fn msg_and_env_filters() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    write(&d, "f", "1\n");
    commit_all(&d, "first");
    write(&d, "f", "2\n");
    commit_all(&d, "second");

    ok(&d, &["rewrite", "--msg-filter", "sed 's/^/[proj] /'"]);
    assert_eq!(subjects(&d), ["[proj] second", "[proj] first"]);

    let script = r#"if [ "$GIT_AUTHOR_EMAIL" = "dev@example.com" ]; then GIT_AUTHOR_NAME="Real Name"; GIT_AUTHOR_EMAIL="real@corp.example"; GIT_AUTHOR_DATE="@86400 +0000"; fi"#;
    ok(&d, &["rewrite", "--env-filter", script]);
    for r in rows(&d) {
        assert_eq!(
            (r[1].as_str(), r[2].as_str(), r[3].as_str()),
            ("Real Name", "real@corp.example", "86400"),
            "{r:?}"
        );
    }
    // committer edits cannot be represented: warned about, not applied
    let o = vcs(&d, &["rewrite", "--env-filter", "GIT_COMMITTER_NAME=Someone"]);
    assert!(o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("GIT_COMMITTER_*"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(rows(&d)[0][1], "Real Name");
    assert!(String::from_utf8_lossy(&o.stdout).contains("nothing to rewrite"));
}

#[test]
fn tree_filter_edits_every_tree_and_keeps_exec_bits() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    write(&d, "keep.txt", "k\n");
    write(&d, "pw.txt", "hunter2\n");
    write(&d, "run.sh", "#!/bin/sh\n");
    commit_all(&d, "one");
    write(&d, "keep.txt", "k2\n");
    commit_all(&d, "two");

    ok(
        &d,
        &[
            "rewrite",
            "--tree-filter",
            "rm -f pw.txt && chmod +x run.sh && echo extra > gen.txt",
        ],
    );
    assert!(!d.join("pw.txt").exists());
    assert_eq!(std::fs::read_to_string(d.join("gen.txt")).unwrap(), "extra\n");
    assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "k2\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(std::fs::metadata(d.join("run.sh")).unwrap().permissions().mode() & 0o111 != 0);
    }
    // history reads back the same way: the first commit has no pw.txt either
    let first = rows(&d)[1][0].clone();
    assert_eq!(ok(&d, &["cat-file", "-p", &format!("{first}:gen.txt")]), "extra\n");
    assert!(!vcs(&d, &["cat-file", "-p", &format!("{first}:pw.txt")]).status.success());
    // scratch checkouts are cleaned up
    let leftovers: Vec<_> = std::fs::read_dir(d.join(".vcs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("rewrite-tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn tag_name_filter_renames_tags() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    lib_history(&d);
    let out = ok(&d, &["rewrite", "--msg-filter", "cat", "--tag-name-filter", "sed 's/^v/rel-/'"]);
    assert!(out.contains("tag v0 -> rel-0") || out.contains("tag v1 -> rel-1"), "{out}");
    let list = ok(&d, &["tag"]);
    assert!(list.contains("rel-0") && list.contains("rel-1"), "{list}");
    ok(&d, &["op", "undo"]);
    let list = ok(&d, &["tag"]);
    assert!(list.contains("v0") && list.contains("v1") && !list.contains("rel-"), "{list}");
}

#[test]
fn all_branches_rewrites_every_branch_but_default_only_the_current_one() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    write(&d, "f", "1\n");
    commit_all(&d, "base");
    ok(&d, &["branch", "topic"]);
    ok(&d, &["switch", "topic"]);
    write(&d, "g", "2\n");
    commit_all(&d, "topic work");
    ok(&d, &["switch", "main"]);
    write(&d, "f", "3\n");
    commit_all(&d, "main work");

    ok(&d, &["rewrite", "--msg-filter", "tr a-z A-Z"]);
    assert_eq!(subjects(&d), ["MAIN WORK", "BASE"]);
    ok(&d, &["switch", "topic"]);
    assert_eq!(subjects(&d), ["topic work", "base"], "topic was not selected: untouched");
    ok(&d, &["switch", "main"]);
    ok(&d, &["op", "undo"]);
    ok(&d, &["rewrite", "--msg-filter", "tr a-z A-Z", "--all-branches"]);
    ok(&d, &["switch", "topic"]);
    assert_eq!(subjects(&d), ["TOPIC WORK", "BASE"]);
}

#[test]
fn refusals_leave_everything_untouched() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    write(&d, "f", "1\n");
    commit_all(&d, "one");
    let before = rows(&d);
    assert!(fail(&d, &["rewrite"]).contains("nothing to do"));
    write(&d, "f", "dirty\n");
    assert!(fail(&d, &["rewrite", "--msg-filter", "cat"]).contains("uncommitted"));
    ok(&d, &["restore", "f"]);
    assert!(fail(&d, &["rewrite", "--msg-filter", "exit 3"]).contains("--msg-filter failed"));
    assert!(fail(&d, &["rewrite", "--remove", "f"]).contains("would be removed"));
    assert!(fail(&d, &["rewrite", "--env-filter", "GIT_AUTHOR_NAME="]).contains("empty author"));
    assert_eq!(rows(&d), before);
    // dry run reports and moves nothing
    let out = ok(&d, &["rewrite", "--msg-filter", "tr a-z A-Z", "-n"]);
    assert!(out.contains("would rewrite 1 of 1") && out.contains("dry run"), "{out}");
    assert_eq!(rows(&d), before);
}

#[test]
fn published_history_needs_force_and_force_is_undoable() {
    let t = tempfile::tempdir().unwrap();
    let remote = t.path().join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    let d = repo(t.path());
    write(&d, "f", "1\n");
    commit_all(&d, "one");
    write(&d, "f", "2\n");
    commit_all(&d, "two");
    ok(&d, &["remote", "set", remote.to_str().unwrap()]);
    ok(&d, &["publish"]);
    write(&d, "f", "3\n");
    commit_all(&d, "local only");
    let before = rows(&d);

    let e = fail(&d, &["rewrite", "--msg-filter", "tr a-z A-Z"]);
    assert!(e.contains("already published") && e.contains("--force"), "{e}");
    assert_eq!(rows(&d), before);
    // rewriting only what is not published works without --force: the message filter leaves
    // published commits alone only if it does not change them
    ok(&d, &["rewrite", "--msg-filter", "sed 's/^local only$/unpublished tweak/'"]);
    assert_eq!(subjects(&d), ["unpublished tweak", "two", "one"]);
    ok(&d, &["op", "undo"]);

    let o = vcs(&d, &["rewrite", "--force", "--msg-filter", "tr a-z A-Z"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("publish --force"));
    assert_eq!(subjects(&d), ["LOCAL ONLY", "TWO", "ONE"]);
    ok(&d, &["op", "undo"]);
    assert_eq!(rows(&d), before);
}

#[test]
fn git_command_lines_translate() {
    let t = tempfile::tempdir().unwrap();
    let d = repo(t.path());
    lib_history(&d);
    let out = ok(&d, &["git", "filter-branch", "--subdirectory-filter", "lib", "--", "--all"]);
    assert!(out.contains("rewrote"), "{out}");
    assert_eq!(subjects(&d), ["edit lib", "add lib and readme"]);
    ok(&d, &["op", "undo"]);
    let out = ok(&d, &["git", "filter-repo", "--path", "README", "--invert-paths"]);
    assert!(out.contains("rewrote"), "{out}");
    assert!(!d.join("README").exists() && d.join("lib/a.txt").exists());
}
