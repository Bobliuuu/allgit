//! End-to-end tests of `git-remote-allgit` driven by the real `git` binary (skipped when git is absent).
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn vcs(dir: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_vcs"))
        .args(args)
        .current_dir(dir)
        .env("VCS_AUTHOR_NAME", "Dev")
        .env("VCS_AUTHOR_EMAIL", "dev@example.com")
        .output()
        .unwrap();
    assert!(o.status.success(), "vcs {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn git_raw(dir: &Path, args: &[&str]) -> Output {
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_git-remote-allgit"));
    let path = format!(
        "{}:{}",
        helper.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .env("GIT_AUTHOR_NAME", "Gitter")
        .env("GIT_AUTHOR_EMAIL", "git@example.com")
        .env("GIT_COMMITTER_NAME", "Gitter")
        .env("GIT_COMMITTER_EMAIL", "git@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = git_raw(dir, args);
    assert!(
        o.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn commit_all(vcs_dir: &Path, msg: &str) {
    vcs(vcs_dir, &["add", "."]);
    vcs(vcs_dir, &["commit", "-m", msg]);
}

fn seed(t: &Path) -> PathBuf {
    let d = t.join("repo");
    std::fs::create_dir(&d).unwrap();
    vcs(&d, &["init"]);
    std::fs::write(d.join("a.txt"), "one\n").unwrap();
    std::os::unix::fs::symlink("a.txt", d.join("ln")).unwrap();
    commit_all(&d, "first");
    std::fs::create_dir(d.join("src")).unwrap();
    std::fs::write(d.join("src/b.txt"), "two\n").unwrap();
    commit_all(&d, "second");
    vcs(&d, &["tag", "v1"]);
    vcs(&d, &["tag", "v2", "-m", "annotated release"]);
    d
}

#[test]
fn clone_push_roundtrip() {
    if !have_git() {
        eprintln!("git not installed; skipping");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let repo = seed(t.path());
    let url = format!("allgit::{}", repo.display());
    let c = t.path().join("clone");
    git(t.path(), &["clone", &url, "clone"]);

    // contents, symlink, history and tags arrived
    assert_eq!(std::fs::read_to_string(c.join("a.txt")).unwrap(), "one\n");
    assert_eq!(std::fs::read_link(c.join("ln")).unwrap(), Path::new("a.txt"));
    assert_eq!(std::fs::read_to_string(c.join("src/b.txt")).unwrap(), "two\n");
    let log = git(&c, &["log", "--format=%an|%s"]);
    assert!(log.starts_with("Dev|second") && log.contains("Dev|first"), "{log}");
    let tags = git(&c, &["tag", "-l"]);
    assert!(tags.contains("v1") && tags.contains("v2"), "{tags}");
    // annotated tags arrive as lightweight tags (see docs/interop.md)
    assert_eq!(git(&c, &["cat-file", "-t", "v2"]).trim(), "commit");
    assert!(git(&c, &["ls-tree", "HEAD", "ln"]).starts_with("120000"));
    git(&c, &["fsck", "--strict"]);

    // commit in git (new file, executable, new symlink, annotated tag) and push to the checked-out branch
    std::fs::write(c.join("new.sh"), "#!/bin/sh\n").unwrap();
    std::os::unix::fs::symlink("src/b.txt", c.join("ln2")).unwrap();
    git(&c, &["update-index", "--add", "--chmod=+x", "new.sh"]);
    git(&c, &["add", "ln2"]);
    git(&c, &["commit", "-m", "from git"]);
    git(&c, &["tag", "-a", "v3", "-m", "git tag"]);
    git(&c, &["push", "origin", "HEAD:refs/heads/main", "v3"]);

    let alog = vcs(&repo, &["log", "--oneline"]);
    assert!(alog.contains("from git"), "{alog}");
    assert_eq!(std::fs::read_to_string(repo.join("new.sh")).unwrap(), "#!/bin/sh\n");
    assert_eq!(std::fs::read_link(repo.join("ln2")).unwrap(), Path::new("src/b.txt"));
    let tl = vcs(&repo, &["tag"]);
    assert!(tl.contains("v3"), "{tl}");
    assert!(vcs(&repo, &["status"]).contains("clean") || !vcs(&repo, &["status"]).contains("modified"));
    assert!(vcs(&repo, &["fsck"]).contains("ok"));

    // a second push is incremental: only the new commit is added
    std::fs::write(c.join("a.txt"), "one\nmore\n").unwrap();
    git(&c, &["commit", "-am", "second from git"]);
    git(&c, &["push", "origin", "HEAD:refs/heads/main"]);
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "one\nmore\n");
    let alog = vcs(&repo, &["log", "--oneline", "-n", "10"]);
    assert_eq!(alog.matches("from git").count(), 2, "{alog}");

    // push to a new branch
    git(&c, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(c.join("t.txt"), "t\n").unwrap();
    git(&c, &["add", "t.txt"]);
    git(&c, &["commit", "-m", "topic work"]);
    git(&c, &["push", "origin", "topic"]);
    assert!(vcs(&repo, &["branch"]).contains("topic"));

    // allgit moves on; git fetches the new commits and the old ones keep their ids
    let before = git(&c, &["rev-parse", "main"]);
    std::fs::write(repo.join("c.txt"), "c\n").unwrap();
    commit_all(&repo, "allgit again");
    git(&c, &["checkout", "-q", "main"]);
    git(&c, &["pull", "-q", "--ff-only", "origin", "main"]);
    assert_eq!(std::fs::read_to_string(c.join("c.txt")).unwrap(), "c\n");
    assert!(git(&c, &["log", "--format=%s"]).contains("allgit again"));
    assert!(git(&c, &["merge-base", "--is-ancestor", before.trim(), "HEAD"]).is_empty());

    // a fresh clone sees everything, including what git pushed
    let c2 = t.path().join("clone2");
    git(t.path(), &["clone", &url, "clone2"]);
    assert!(git(&c2, &["log", "--format=%s"]).contains("second from git"));
    assert_eq!(std::fs::read_link(c2.join("ln2")).unwrap(), Path::new("src/b.txt"));
    git(&c2, &["fsck", "--strict"]);
}

#[test]
fn push_errors_are_reported_per_ref() {
    if !have_git() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let repo = seed(t.path());
    let url = format!("allgit::{}", repo.display());
    let c = t.path().join("clone");
    git(t.path(), &["clone", &url, "clone"]);

    // allgit gets a commit the clone does not have, then the clone pushes a diverging one
    std::fs::write(repo.join("x.txt"), "x\n").unwrap();
    commit_all(&repo, "only in allgit");
    std::fs::write(c.join("y.txt"), "y\n").unwrap();
    git(&c, &["add", "y.txt"]);
    git(&c, &["commit", "-m", "diverging"]);
    let o = git_raw(&c, &["push", "origin", "HEAD:refs/heads/main"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("-> main") && err.contains("not part of the import"), "{err}");
    assert!(!vcs(&repo, &["log", "--oneline"]).contains("diverging"));

    // deleting a remote branch is refused
    let o = git_raw(&c, &["push", "origin", ":refs/heads/main"]);
    assert!(!o.status.success());

    // not a repository
    let o = git_raw(t.path(), &["ls-remote", &format!("allgit::{}", t.path().join("nope").display())]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("not an allgit repository"));
}

#[test]
fn ls_remote_lists_branches_and_tags() {
    if !have_git() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let repo = seed(t.path());
    let out = git(t.path(), &["ls-remote", &format!("allgit::{}", repo.display())]);
    assert!(out.contains("refs/heads/main") && out.contains("refs/tags/v1"), "{out}");
}
