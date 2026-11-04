//! Switching to a branch at the same commit needs no status walk: the index's cached tree id
//! proves nothing is staged. Staged changes still refuse it, in the fast and the slow path alike;
//! unstaged edits survive it (nothing is written to the working tree).

use std::fs;
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn s(p: &str) -> Vec<String> {
    vec![p.to_string()]
}

fn repo() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    fs::create_dir_all(r.worktree().join("a/b")).unwrap();
    for p in ["a/one.txt", "a/b/two.txt", "top.txt"] {
        fs::write(r.worktree().join(p), "x\n").unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    (d, r)
}

fn branch(r: &Repo) -> String {
    r.current_branch("root").unwrap().unwrap()
}

#[test]
fn same_commit_switch_works_repeatedly_and_keeps_state() {
    let (_d, r) = repo();
    r.create_branch("root", "topic").unwrap();
    for _ in 0..3 {
        // the first pass may take the slow path (and then caches the tree); later ones the fast one
        r.checkout("root", "topic").unwrap();
        assert_eq!(branch(&r), "topic");
        r.checkout("root", "main").unwrap();
        assert_eq!(branch(&r), "main");
    }
    let st = r.status().unwrap();
    assert!(st.zones[0].staged.is_empty() && st.zones[0].unstaged.is_empty());
}

#[test]
fn staged_changes_refuse_a_same_commit_switch_even_with_a_warm_cache() {
    let (_d, r) = repo();
    r.create_branch("root", "topic").unwrap();
    r.checkout("root", "topic").unwrap(); // warms the cached tree
    r.checkout("root", "main").unwrap();
    fs::write(r.worktree().join("a/one.txt"), "changed\n").unwrap();
    r.add(&s("a/one.txt")).unwrap();
    let e = r.checkout("root", "topic").unwrap_err().to_string();
    assert!(e.contains("uncommitted changes") && e.contains("1 staged"), "{e}");
    assert_eq!(branch(&r), "main");
    // staging a new file is refused too
    r.restore(&s("a/one.txt"), true).unwrap();
    fs::write(r.worktree().join("new.txt"), "n\n").unwrap();
    r.add(&s("new.txt")).unwrap();
    assert!(r.checkout("root", "topic").is_err());
}

#[test]
fn unstaged_edits_do_not_block_and_are_kept() {
    let (_d, r) = repo();
    r.create_branch("root", "topic").unwrap();
    fs::write(r.worktree().join("top.txt"), "edited\n").unwrap();
    r.checkout("root", "topic").unwrap();
    r.checkout("root", "main").unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("top.txt")).unwrap(), "edited\n");
    let st = r.status().unwrap();
    assert_eq!(st.zones[0].unstaged.len(), 1);
    assert!(st.zones[0].staged.is_empty());
}

#[test]
fn a_different_commit_still_refuses_unstaged_edits() {
    let (_d, r) = repo();
    r.create_branch("root", "topic").unwrap();
    r.checkout("root", "topic").unwrap();
    fs::write(r.worktree().join("top.txt"), "second\n").unwrap();
    r.add(&s("top.txt")).unwrap();
    r.commit("second", sig()).unwrap();
    r.checkout("root", "main").unwrap();
    fs::write(r.worktree().join("top.txt"), "edited\n").unwrap();
    assert!(r.checkout("root", "topic").is_err());
}
