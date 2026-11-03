//! `untracked_only` (used by `vcs clean`) must agree with the untracked part of `status`.

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

#[test]
fn untracked_only_equals_the_untracked_part_of_status() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    fs::create_dir_all(r.worktree().join("a/b")).unwrap();
    for p in ["a/one.txt", "a/b/two.txt", "top.txt"] {
        fs::write(r.worktree().join(p), "x\n").unwrap();
    }
    r.add(&s("")).unwrap();
    r.commit("base", sig()).unwrap();
    fs::create_dir_all(r.worktree().join("new/deep")).unwrap();
    for p in ["new/deep/u1.txt", "new/u2.txt", "a/b/u3.txt", "loose.txt"] {
        fs::write(r.worktree().join(p), "u\n").unwrap();
    }
    fs::write(r.worktree().join("a/one.txt"), "modified\n").unwrap(); // tracked changes must not matter
    for _ in 0..2 {
        // twice: the second pass runs on the directory cache the first one wrote
        let st = r.status().unwrap();
        let (u, un) = r.untracked_only(Default::default()).unwrap();
        assert_eq!(u, st.untracked);
        assert_eq!(un, st.unsupported);
    }
    assert!(!r.status().unwrap().untracked.is_empty());
}
