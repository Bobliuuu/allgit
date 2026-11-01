//! Patch files end to end: format-patch, am, apply, diff/show output, and interchange with real
//! git when a `git` binary is on the PATH (git is only a yardstick; vcs never calls it).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn vcs_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .env("VCS_TIMESTAMP", "1700000000");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> String {
    ok_env(dir, args, &[])
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
fn fail(dir: &Path, args: &[&str]) -> String {
    let o = vcs_env(dir, args, &[]);
    assert!(
        !o.status.success(),
        "vcs {args:?} unexpectedly succeeded: {}",
        String::from_utf8_lossy(&o.stdout)
    );
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}
fn write(dir: &Path, f: &str, c: &str) {
    let p = dir.join(f);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, c).unwrap();
}
fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}
fn commit_all(dir: &Path, msg: &str, ts: u64) {
    ok(dir, &["add", "."]);
    ok_env(dir, &["commit", "-m", msg], &[("VCS_TIMESTAMP", &ts.to_string())]);
}
fn tree_of(dir: &Path) -> String {
    ok(dir, &["cat-file", "-p", "HEAD"])
        .lines()
        .find_map(|l| l.strip_prefix("tree ").map(str::to_owned))
        .unwrap()
}
fn log_of(dir: &Path) -> Vec<String> {
    ok(dir, &["log", "-n", "1000", "--format=%an|%ae|%at|%s"])
        .lines()
        .map(str::to_owned)
        .collect()
}
#[cfg(unix)]
fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}
/// path -> content, exec bit, symlink target (skips .vcs and .git)
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
        for e in std::fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".vcs" || name == ".git" {
                continue;
            }
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.file_type().is_symlink() {
                out.insert(rel, format!("-> {}", std::fs::read_link(&p).unwrap().display()));
            } else if m.is_dir() {
                walk(root, &p, out);
            } else {
                #[cfg(unix)]
                let x = {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let x = false;
                out.insert(
                    rel,
                    format!(
                        "{}{}",
                        if x { "x:" } else { "-:" },
                        String::from_utf8_lossy(&std::fs::read(&p).unwrap())
                    ),
                );
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(dir, dir, &mut m);
    m
}
fn patch_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    v.sort();
    v
}
fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}
fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Gitter")
        .env("GIT_AUTHOR_EMAIL", "gitter@example.com")
        .env("GIT_COMMITTER_NAME", "Gitter")
        .env("GIT_COMMITTER_EMAIL", "gitter@example.com")
        .env("GIT_AUTHOR_DATE", "2024-03-05T10:20:30+0000")
        .env("GIT_COMMITTER_DATE", "2024-03-05T10:20:30+0000")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap()
}
fn git_ok(dir: &Path, args: &[&str]) -> String {
    let o = git(dir, args);
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A repo with a base commit, plus a clone of it taken at that point.
fn pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    ok(t.path(), &["init", "a"]);
    write(&a, "a.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n");
    write(&a, "dir/b.txt", "bee\n");
    write(&a, "run.sh", "#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    chmod(&a.join("run.sh"), 0o755);
    commit_all(&a, "base", 1_600_000_000);
    let r = t.path().join("remote");
    ok(&a, &["remote", "set", r.to_str().unwrap()]);
    ok(&a, &["publish"]);
    ok(t.path(), &["clone", r.to_str().unwrap(), "b"]);
    let b = t.path().join("b");
    (t, a, b)
}

#[test]
#[cfg(unix)]
fn format_patch_then_am_gives_identical_trees_and_authors() {
    let (t, a, b) = pair();
    // 1: edit + new nested file
    write(&a, "a.txt", "one\n2\nthree\nfour\nfive\nsix\nseven\neight\n");
    write(&a, "dir/sub/c.txt", "see\n");
    commit_all(&a, "edit a and add c\n\nThe body has two\nlines.", 1_700_000_001);
    // 2: delete, empty file, no trailing newline, new executable, symlink
    std::fs::remove_file(a.join("dir/b.txt")).unwrap();
    write(&a, "empty", "");
    write(&a, "nonl", "no newline at end");
    write(&a, "tool.sh", "#!/bin/sh\n");
    chmod(&a.join("tool.sh"), 0o755);
    std::os::unix::fs::symlink("a.txt", a.join("link")).unwrap();
    commit_all(&a, "assorted", 1_700_000_002);
    // 3: mode-only change and a unicode author
    chmod(&a.join("run.sh"), 0o644);
    ok(&a, &["add", "."]);
    ok_env(
        &a,
        &["commit", "-m", "drop exec bit"],
        &[
            ("VCS_AUTHOR_NAME", "Jürgen Müller"),
            ("VCS_AUTHOR_EMAIL", "j@example.org"),
            ("VCS_TIMESTAMP", "1700000003"),
        ],
    );
    // 4: rewrite a file to nothing and back changes
    write(&a, "a.txt", "");
    commit_all(&a, "empty a", 1_700000004);

    let out = t.path().join("patches");
    let names = ok(&a, &["format-patch", "-n", "4", "-o", out.to_str().unwrap()]);
    assert_eq!(names.lines().count(), 4);
    let files = patch_files(&out);
    assert_eq!(files.len(), 4);
    assert!(files[0].file_name().unwrap().to_string_lossy().starts_with("0001-edit-a-and-add-c"));
    let first = std::fs::read_to_string(&files[0]).unwrap();
    assert!(first.contains("Subject: [PATCH 1/4] edit a and add c"), "{first}");
    assert!(first.contains("Date: Tue, 14 Nov 2023 22:13:21 +0000"), "{first}");
    assert!(first.contains("The body has two\nlines."));

    let args: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut am = vec!["am"];
    am.extend(args.iter().map(String::as_str));
    ok(&b, &am);
    assert_eq!(tree_of(&b), tree_of(&a), "am must reproduce the exact tree");
    assert_eq!(log_of(&b), log_of(&a), "author, email, date and subject survive");
    assert_eq!(snapshot(&b), snapshot(&a));
    let full = |d: &Path| ok(d, &["log", "-n", "2", "--format=%B"]);
    assert_eq!(full(&b), full(&a));
    ok(&b, &["status"]);
    assert!(!ok(&b, &["status"]).contains("staged"), "{}", ok(&b, &["status"]));
}

#[test]
fn stdout_mailbox_and_range_forms() {
    let (_t, a, b) = pair();
    for i in 1..=3 {
        write(&a, &format!("f{i}"), &format!("{i}\n"));
        commit_all(&a, &format!("add f{i}"), 1_700_000_000 + i);
    }
    let mbox = ok(&a, &["format-patch", "--stdout", "HEAD~2"]);
    assert_eq!(
        mbox.matches("\nFrom: Dev <dev@example.com>\n").count() + usize::from(mbox.starts_with("From: ")),
        2
    );
    assert!(mbox.contains("[PATCH 1/2] add f2") && mbox.contains("[PATCH 2/2] add f3"));
    assert_eq!(ok(&a, &["format-patch", "--stdout", "HEAD~2..HEAD"]), mbox);
    // clone b is at base; pipe the mailbox through stdin
    let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
    c.arg("am")
        .current_dir(&b)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "d@e");
    c.stdin(std::process::Stdio::piped());
    let mut ch = c
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    let full = ok(&a, &["format-patch", "--stdout", "HEAD~3"]);
    ch.stdin.take().unwrap().write_all(full.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(tree_of(&b), tree_of(&a));
    // refuses without a selection and refuses to overwrite
    assert!(fail(&a, &["format-patch"]).contains("give what to format"));
}

#[test]
fn am_stops_on_conflict_and_continue_skip_abort() {
    let (t, a, b) = pair();
    write(&a, "a.txt", "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\n");
    commit_all(&a, "upper three", 1_700_000_001);
    write(&a, "x", "x\n");
    commit_all(&a, "add x", 1_700_000_002);
    let mbox = t.path().join("p.mbox");
    std::fs::write(&mbox, ok(&a, &["format-patch", "--stdout", "HEAD~2"])).unwrap();
    // b diverges on the same line
    write(&b, "a.txt", "one\ntwo\nthree-b\nfour\nfive\nsix\nseven\neight\n");
    commit_all(&b, "b's own edit", 1_700_000_100);
    let before = tree_of(&b);
    let o = fail(&b, &["am", mbox.to_str().unwrap()]);
    assert!(o.contains("did not apply") && o.contains("does not apply"), "{o}");
    assert_eq!(
        read(&b, "a.txt"),
        "one\ntwo\nthree-b\nfour\nfive\nsix\nseven\neight\n",
        "a failed patch changes nothing"
    );
    assert!(fail(&b, &["am", mbox.to_str().unwrap()]).contains("already in progress"));
    // abort restores
    ok(&b, &["am", "--abort"]);
    assert_eq!(tree_of(&b), before);
    // retry, fix by hand, continue: author/date/message come from the mail
    fail(&b, &["am", mbox.to_str().unwrap()]);
    write(&b, "a.txt", "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\n");
    ok(&b, &["add", "a.txt"]);
    ok(&b, &["am", "--continue"]);
    assert_eq!(
        log_of(&b)[..2],
        ["Dev|dev@example.com|1700000002|add x", "Dev|dev@example.com|1700000001|upper three"]
    );
    assert_eq!(read(&b, "x"), "x\n");
    // skip
    ok(&b, &["reset", "--hard", "HEAD~2"]);
    fail(&b, &["am", mbox.to_str().unwrap()]);
    ok(&b, &["am", "--skip"]);
    assert_eq!(log_of(&b)[0], "Dev|dev@example.com|1700000002|add x");
    assert!(!ok(&b, &["status"]).contains("am"));
}

#[test]
fn apply_check_index_reverse_and_atomic() {
    let (t, a, _b) = pair();
    write(&a, "a.txt", "one\ntwo\nthree\nFOUR\nfive\nsix\nseven\neight\n");
    write(&a, "new.txt", "new\n");
    let diff = ok(&a, &["diff"]);
    let p = t.path().join("d.patch");
    std::fs::write(&p, &diff).unwrap();
    // vcs diff output applies after restoring
    ok(&a, &["restore", "a.txt"]);
    std::fs::remove_file(a.join("new.txt")).ok();
    // new.txt is untracked so not in the diff; only a.txt is
    ok(&a, &["apply", "--check", p.to_str().unwrap()]);
    assert_eq!(read(&a, "a.txt"), "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n");
    ok(&a, &["apply", p.to_str().unwrap()]);
    assert!(read(&a, "a.txt").contains("FOUR"));
    // reverse
    ok(&a, &["apply", "-R", p.to_str().unwrap()]);
    assert!(read(&a, "a.txt").contains("four"));
    // --index stages
    ok(&a, &["apply", "--index", p.to_str().unwrap()]);
    assert!(ok(&a, &["diff", "--staged", "--name-only"]).contains("a.txt"));
    // a second --index on the now-staged file is refused (does not match the index)
    assert!(fail(&a, &["apply", "--index", "-R", p.to_str().unwrap()]).contains("does not match the index"));
    ok(&a, &["restore", "--staged", "a.txt"]);
    ok(&a, &["restore", "a.txt"]);

    // atomic: second file fails, first stays untouched
    let bad = "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-one\n+ONE\n--- a/dir/b.txt\n+++ b/dir/b.txt\n@@ -1,1 +1,1 @@\n-nope\n+yes\n";
    std::fs::write(&p, bad).unwrap();
    let e = fail(&a, &["apply", p.to_str().unwrap()]);
    assert!(e.contains("dir/b.txt") && e.contains("does not apply"), "{e}");
    assert!(read(&a, "a.txt").starts_with("one\n"));
    // 3-way and unsafe paths and binary are refused loudly
    assert!(fail(&a, &["apply", "-3", p.to_str().unwrap()]).contains("not supported"));
    std::fs::write(&p, "--- a/../evil\n+++ b/../evil\n@@ -0,0 +1 @@\n+x\n").unwrap();
    assert!(fail(&a, &["apply", p.to_str().unwrap()]).contains("unsafe path"));
    std::fs::write(
        &p,
        "diff --git a/z.bin b/z.bin\nnew file mode 100644\nindex 0000000..1111111\nGIT binary patch\nliteral 3\nKcmZQz0001a0RR91\n\n",
    )
    .unwrap();
    let e = fail(&a, &["apply", p.to_str().unwrap()]);
    assert!(e.contains("binary patches are not supported"), "{e}");
    assert!(!a.join("z.bin").exists());
    std::fs::write(&p, "nothing here\n").unwrap();
    assert!(fail(&a, &["apply", p.to_str().unwrap()]).contains("no patch"));
}

#[test]
fn apply_offset_and_symlink_parent_refused() {
    let (t, a, _b) = pair();
    // a patch made for a file with two extra leading lines still applies (offset search)
    let p = t.path().join("o.patch");
    std::fs::write(&p, "--- a/a.txt\n+++ b/a.txt\n@@ -1,4 +1,4 @@\n one\n-two\n+TWO\n three\n four\n").unwrap();
    write(&a, "a.txt", "x\ny\none\ntwo\nthree\nfour\nfive\n");
    ok(&a, &["apply", p.to_str().unwrap()]);
    assert_eq!(read(&a, "a.txt"), "x\ny\none\nTWO\nthree\nfour\nfive\n");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(t.path(), a.join("escape")).unwrap();
        std::fs::write(&p, "--- /dev/null\n+++ b/escape/pwn\n@@ -0,0 +1 @@\n+x\n").unwrap();
        let e = fail(&a, &["apply", p.to_str().unwrap()]);
        assert!(e.contains("symbolic link"), "{e}");
        assert!(!t.path().join("pwn").exists());
    }
}

#[test]
fn show_output_applies_elsewhere() {
    let (_t, a, b) = pair();
    write(&a, "a.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\nEIGHT\n");
    write(&a, "added.txt", "hello\n");
    commit_all(&a, "change", 1_700_000_001);
    let shown = ok(&a, &["show", "HEAD"]);
    let p = _t.path().join("show.patch");
    std::fs::write(&p, shown).unwrap();
    ok(&b, &["apply", "--index", p.to_str().unwrap()]);
    ok(&b, &["commit", "-m", "applied"]);
    assert_eq!(tree_of(&b), tree_of(&a));
}

#[test]
fn real_git_patches_apply_with_vcs_and_back() {
    if !have_git() {
        eprintln!("git not installed; skipping interchange test");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let (g, v) = (t.path().join("g"), t.path().join("v"));
    std::fs::create_dir(&g).unwrap();
    git_ok(&g, &["init", "-q", "-b", "main"]);
    ok(t.path(), &["init", "v"]);
    for d in [&g, &v] {
        write(d, "a.txt", "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n");
        write(d, "old/name.txt", "alpha\nbeta\ngamma\ndelta\nepsilon\n");
        write(d, "gone.txt", "bye\n");
        write(d, "keep.sh", "#!/bin/sh\n");
        #[cfg(unix)]
        chmod(&d.join("keep.sh"), 0o755);
    }
    git_ok(&g, &["add", "."]);
    git_ok(&g, &["commit", "-q", "-m", "base"]);
    commit_all(&v, "base", 1_600_000_000);

    // git side: edit, rename with a change, delete, add exec file, no-newline file
    write(&g, "a.txt", "one\ntwo\nthree\nFOUR\nfive\nsix\nseven\neight\nnine\nten\neleven\n");
    std::fs::create_dir_all(g.join("new")).unwrap();
    git_ok(&g, &["mv", "old/name.txt", "new/place.txt"]);
    write(&g, "new/place.txt", "alpha\nbeta\nGAMMA\ndelta\nepsilon\n");
    git_ok(&g, &["rm", "-q", "gone.txt"]);
    write(&g, "tool.sh", "#!/bin/sh\necho t\n");
    #[cfg(unix)]
    chmod(&g.join("tool.sh"), 0o755);
    write(&g, "nonl.txt", "tail");
    git_ok(&g, &["add", "-A"]);
    git_ok(&g, &["commit", "-q", "-m", "git change\n\nwith a body"]);
    let gp = git_ok(&g, &["format-patch", "-1", "--stdout"]);
    let pf = t.path().join("git.patch");
    std::fs::write(&pf, &gp).unwrap();

    // plain apply onto the vcs twin
    ok(&v, &["apply", pf.to_str().unwrap()]);
    let want = snapshot(&g);
    assert_eq!(snapshot(&v), want, "git's patch must give git's tree");
    // and am: author, date, message from git's mail
    ok(&v, &["restore", "."]);
    // am wants a clean tree and the base state; reset by checking out base content again
    let _ = std::fs::remove_dir_all(v.join("new"));
    ok(&v, &["reset", "--hard"]);
    ok(&v, &["clean", "-f", "-d"]);
    ok(&v, &["am", pf.to_str().unwrap()]);
    assert_eq!(snapshot(&v), want);
    let l = ok(&v, &["log", "-n", "1", "--format=%an|%ae|%at|%s"]);
    assert_eq!(l.trim(), "Gitter|gitter@example.com|1709634030|git change");
    assert!(ok(&v, &["log", "-n", "1", "--format=%B"]).contains("with a body"));

    // other direction: vcs patch applied by real git
    write(&v, "a.txt", "one\ntwo\nthree\nFOUR\nfive\nsix\nSEVEN\neight\nnine\nten\neleven\n");
    write(&v, "dir2/x.txt", "x\n");
    std::fs::remove_file(v.join("nonl.txt")).unwrap();
    commit_all(&v, "vcs change\n\nvcs body", 1_700_000_050);
    let vp = ok(&v, &["format-patch", "--stdout", "-n", "1"]);
    let vpf = t.path().join("vcs.patch");
    std::fs::write(&vpf, &vp).unwrap();
    git_ok(&g, &["apply", "--check", vpf.to_str().unwrap()]);
    git_ok(&g, &["am", vpf.to_str().unwrap()]);
    assert_eq!(snapshot(&g), snapshot(&v), "vcs's patch must give vcs's tree in git");
    assert_eq!(
        git_ok(&g, &["log", "-1", "--format=%an|%ae|%at|%s"]).trim(),
        "Dev|dev@example.com|1700000050|vcs change"
    );
    assert!(git_ok(&g, &["log", "-1", "--format=%B"]).contains("vcs body"));
    // vcs diff (working tree) is accepted by git apply as well
    write(&v, "a.txt", "one\ntwo\nthree\nFOUR\nfive\nsix\nSEVEN\neight\nnine\nTEN\neleven\n");
    let d = ok(&v, &["diff"]);
    let df = t.path().join("wt.patch");
    std::fs::write(&df, d).unwrap();
    git_ok(&g, &["apply", "--check", df.to_str().unwrap()]);
}

#[test]
fn binary_changes_are_refused_by_format_patch() {
    let (t, a, _b) = pair();
    std::fs::write(a.join("blob.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    commit_all(&a, "add binary", 1_700_000_001);
    let e = fail(&a, &["format-patch", "-n", "1", "-o", t.path().join("o").to_str().unwrap()]);
    assert!(e.contains("blob.bin") && e.contains("binary"), "{e}");
    assert!(!t.path().join("o").exists() || std::fs::read_dir(t.path().join("o")).unwrap().count() == 0);
}
