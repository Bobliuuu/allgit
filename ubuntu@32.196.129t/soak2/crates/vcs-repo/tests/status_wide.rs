//! Regression test for the untracked walk on a wide layout (many small sibling directories, as
//! in the 100-files-per-directory benchmark): the walk used to spawn one thread per directory;
//! workers now pull directories from a shared counter. The result must be complete, sorted and
//! identical with and without the directory cache.

use std::fs;
use vcs_objects::Signature;
use vcs_repo::{Repo, StatusOpts};

#[test]
fn wide_layout_untracked_is_complete_sorted_and_cache_independent() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let w = r.worktree().to_path_buf();
    for i in 0..300 {
        let dir = w.join(format!("d{i:04}"));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), format!("a{i}")).unwrap();
        fs::write(dir.join("sub/b.txt"), format!("b{i}")).unwrap();
    }
    r.add(&[String::new()]).unwrap();
    r.commit(
        "base",
        Signature {
            name: "T".into(),
            email: "t@x".into(),
            timestamp: 1,
        },
    )
    .unwrap();
    let mut expect = Vec::new();
    for i in (0..300).step_by(7) {
        fs::write(w.join(format!("d{i:04}/new.txt")), "n").unwrap();
        fs::write(w.join(format!("d{i:04}/sub/new2.txt")), "n").unwrap();
        expect.push(format!("d{i:04}/new.txt"));
        expect.push(format!("d{i:04}/sub/new2.txt"));
    }
    expect.sort();
    for _ in 0..3 {
        let cached = r.status().unwrap();
        let full = r.status_with(StatusOpts {
            dir_cache: false,
            ..StatusOpts::default()
        });
        let full = full.unwrap();
        assert_eq!(cached.untracked, expect);
        assert_eq!(full.untracked, expect);
        assert!(cached.zones[0].unstaged.is_empty());
    }
}
