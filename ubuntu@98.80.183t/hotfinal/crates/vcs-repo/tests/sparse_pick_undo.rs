//! #290 follow-up: cherry-pick, revert and `op undo` on a zone with a path sparse set. Clean changes
//! outside the set apply in the index/trees without touching the disk; conflicts outside the set
//! fail before any change; undo restores branch, index and the materialized part only.
use std::fs;
use vcs_objects::Signature;
use vcs_repo::{MergeOutcome, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn exists(r: &Repo, p: &str) -> bool {
    r.worktree().join(p).exists()
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn s(p: &[&str]) -> Vec<String> {
    p.iter().map(|x| x.to_string()).collect()
}
fn clean(r: &Repo) {
    let st = r.status().unwrap();
    for z in &st.zones {
        assert!(z.staged.is_empty() && z.unstaged.is_empty(), "{:?} {:?}", z.staged, z.unstaged);
    }
    assert!(st.untracked.is_empty(), "{:?}", st.untracked);
}
fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

/// main: a/x, b/x, c; branch f: one commit changing a/x and b/x. Returns (repo, f's commit message).
fn two_branches() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "a/x.txt", "ax\n");
    write(&r, "b/x.txt", "bx\n");
    write(&r, "c.txt", "c\n");
    commit_all(&r, "base");
    r.create_branch("root", "f").unwrap();
    r.checkout("root", "f").unwrap();
    write(&r, "a/x.txt", "af\n");
    write(&r, "b/x.txt", "bf\n");
    commit_all(&r, "f change");
    r.checkout("root", "main").unwrap();
    (d, r)
}

#[test]
fn cherry_pick_on_a_sparse_zone_applies_outside_paths_without_materializing_them() {
    let (_d, r) = two_branches();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    let out = r.cherry_pick("root", "f", sig()).unwrap();
    assert!(
        matches!(out, MergeOutcome::Merged { .. } | MergeOutcome::FastForward { .. }),
        "{out:?}"
    );
    assert_eq!(read(&r, "a/x.txt"), "af\n");
    assert!(!exists(&r, "b") && !exists(&r, "c.txt"), "unmaterialized paths stay off disk");
    clean(&r);
    // the commit really carries b/x.txt from f
    r.sparse_disable(None, "root").unwrap();
    assert_eq!(read(&r, "b/x.txt"), "bf\n");
    assert_eq!(read(&r, "c.txt"), "c\n");
    clean(&r);
    fsck(&r);
}

#[test]
fn revert_on_a_sparse_zone_undoes_outside_paths_without_materializing_them() {
    let (_d, r) = two_branches();
    r.cherry_pick("root", "f", sig()).unwrap();
    assert_eq!(read(&r, "b/x.txt"), "bf\n");
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    r.revert("root", "main", sig()).unwrap();
    assert_eq!(read(&r, "a/x.txt"), "ax\n");
    assert!(!exists(&r, "b"));
    clean(&r);
    r.sparse_disable(None, "root").unwrap();
    assert_eq!(read(&r, "b/x.txt"), "bx\n");
    clean(&r);
    fsck(&r);
}

#[test]
fn cherry_pick_conflicting_outside_the_set_fails_and_changes_nothing() {
    let (_d, r) = two_branches();
    // main diverges on b/x.txt, which f also changes
    write(&r, "b/x.txt", "bmain\n");
    commit_all(&r, "main b");
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    let head = r.refs().unwrap().head("root", "main").unwrap();
    let res = r.cherry_pick("root", "f", sig());
    match res {
        Err(e) => assert!(e.to_string().contains("sparse"), "unexpected error: {e}"),
        Ok(o) => panic!("expected a refusal, got {o:?}"),
    }
    assert_eq!(r.refs().unwrap().head("root", "main").unwrap(), head, "branch untouched");
    assert_eq!(read(&r, "a/x.txt"), "ax\n");
    assert!(!exists(&r, "b"));
    assert!(r.pick_marker().unwrap().is_none(), "no half-finished pick left behind");
    clean(&r);
    r.sparse_disable(None, "root").unwrap();
    assert_eq!(read(&r, "b/x.txt"), "bmain\n");
    fsck(&r);
}

#[test]
fn op_undo_on_a_sparse_zone_restores_the_branch_and_keeps_the_set() {
    let (_d, r) = two_branches();
    r.sparse_set(None, "root", &s(&["a"])).unwrap();
    let before = r.refs().unwrap().head("root", "main").unwrap();
    write(&r, "a/x.txt", "edited\n");
    commit_all(&r, "edit a");
    assert_ne!(r.refs().unwrap().head("root", "main").unwrap(), before);
    r.op_undo().unwrap();
    assert_eq!(r.refs().unwrap().head("root", "main").unwrap(), before);
    assert_eq!(read(&r, "a/x.txt"), "ax\n");
    assert!(!exists(&r, "b") && !exists(&r, "c.txt"), "undo must not materialize the rest");
    assert_eq!(r.sparse_list().unwrap()["root"], s(&["a"]));
    clean(&r);
    // undo of the undo goes forward again
    r.op_undo().unwrap();
    assert_eq!(read(&r, "a/x.txt"), "edited\n");
    assert!(!exists(&r, "b"));
    clean(&r);
    fsck(&r);
}
