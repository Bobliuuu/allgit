//! Checkout keeps the cache-tree and the case-fold check exact: the tree ids a checkout leaves
//! in the index give the same next commit as a from-scratch build, and a directory whose names
//! change is still checked for collisions. Also the attribute lookup that skips decoding the index.
use std::fs;
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Tree, TreeEntry};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn head_root(r: &Repo, branch: &str) -> ObjectId {
    let h = r.refs().unwrap().head("root", branch).unwrap();
    r.log("root", Some(branch), 1)
        .unwrap()
        .into_iter()
        .find(|(i, _)| *i == h)
        .unwrap()
        .1
        .root
}
fn clean(r: &Repo) {
    let st = r.status().unwrap();
    let full = r.status_full().unwrap();
    for z in st.zones.iter().chain(full.zones.iter()) {
        assert!(z.staged.is_empty() && z.unstaged.is_empty(), "{:?} {:?}", z.staged, z.unstaged);
    }
}

const FILES: &[(&str, &str)] = &[
    ("a/x", "ax"),
    ("a/y", "ay"),
    ("a/deep/p", "p"),
    ("a/deep/q", "q"),
    ("b/z", "bz"),
    ("b/sub/s", "s"),
    ("top", "top"),
    ("c/d/e/f", "f"),
];

/// Repos reaching the same final contents by different routes have the same root tree id.
#[test]
fn commit_after_checkout_matches_a_from_scratch_tree() {
    let (_d, r) = setup();
    for (p, c) in FILES {
        write(&r, p, c);
    }
    commit_all(&r, "base");
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    write(&r, "a/deep/p", "p-side");
    write(&r, "a/new", "n");
    write(&r, "b/sub/s2", "s2");
    fs::remove_file(r.worktree().join("b/z")).unwrap();
    fs::remove_file(r.worktree().join("c/d/e/f")).unwrap();
    commit_all(&r, "side");
    for _ in 0..3 {
        r.checkout("root", "main").unwrap();
        clean(&r);
        r.checkout("root", "side").unwrap();
        clean(&r);
    }
    // the next commit builds on whatever tree ids the checkout left in the index
    write(&r, "top", "top2");
    commit_all(&r, "after");
    let got = head_root(&r, "side");

    let (_d2, c) = setup();
    for (p, content) in [
        ("a/x", "ax"),
        ("a/y", "ay"),
        ("a/deep/p", "p-side"),
        ("a/deep/q", "q"),
        ("a/new", "n"),
        ("b/sub/s", "s"),
        ("b/sub/s2", "s2"),
        ("top", "top2"),
    ] {
        write(&c, p, content);
    }
    commit_all(&c, "scratch");
    assert_eq!(got, head_root(&c, "main"));
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn exec_bits_and_symlinks_survive_switches() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (_d, r) = setup();
        write(&r, "bin/run", "#!/bin/sh\n");
        fs::set_permissions(r.worktree().join("bin/run"), fs::Permissions::from_mode(0o755)).unwrap();
        write(&r, "bin/data", "d");
        symlink("data", r.worktree().join("bin/link")).unwrap();
        commit_all(&r, "base");
        r.create_branch("root", "side").unwrap();
        r.checkout("root", "side").unwrap();
        write(&r, "bin/data", "d2");
        fs::set_permissions(r.worktree().join("bin/run"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_file(r.worktree().join("bin/link")).unwrap();
        symlink("run", r.worktree().join("bin/link")).unwrap();
        commit_all(&r, "side");
        r.checkout("root", "main").unwrap();
        clean(&r);
        let m = fs::metadata(r.worktree().join("bin/run")).unwrap();
        assert_ne!(m.permissions().mode() & 0o111, 0);
        assert_eq!(fs::read_link(r.worktree().join("bin/link")).unwrap().to_str(), Some("data"));
        r.checkout("root", "side").unwrap();
        clean(&r);
        assert_eq!(fs::metadata(r.worktree().join("bin/run")).unwrap().permissions().mode() & 0o111, 0);
        assert_eq!(fs::read_link(r.worktree().join("bin/link")).unwrap().to_str(), Some("run"));
        assert_eq!(fs::read_to_string(r.worktree().join("bin/data")).unwrap(), "d2");
    }
}

fn plant(r: &Repo, branch: &str, dir: &str, names: &[&str]) {
    let mut kids = Vec::new();
    for n in names {
        let id = r.store().put(&Object::Blob(n.as_bytes().to_vec())).unwrap();
        kids.push(TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id,
        });
    }
    let sub = r.store().put(&Object::Tree(Tree::new(kids).unwrap())).unwrap();
    let keep = r.store().put(&Object::Blob(b"k".to_vec())).unwrap();
    let root = r
        .store()
        .put(&Object::Tree(
            Tree::new(vec![
                TreeEntry {
                    name: dir.into(),
                    mode: EntryMode::Directory,
                    id: sub,
                },
                TreeEntry {
                    name: "keep".into(),
                    mode: EntryMode::File,
                    id: keep,
                },
            ])
            .unwrap(),
        ))
        .unwrap();
    let c = Commit {
        zone: "root".into(),
        parents: vec![],
        root,
        change_id: ChangeId::from_bytes([7; 16]),
        author: sig(),
        message: "crafted".into(),
    };
    let cid: ObjectId = r.store().put(&Object::Commit(c)).unwrap();
    r.store().sync().unwrap();
    let mut refs = r.refs().unwrap();
    refs.apply(&[RefUpdate {
        zone: "root".into(),
        branch: branch.into(),
        old: None,
        new: cid,
    }])
    .unwrap();
    fs::write(r.worktree().join(".vcs/refs"), refs.encode()).unwrap();
}

/// A directory that exists on both sides keeps its collision check when a colliding name appears.
#[test]
fn collision_in_an_existing_directory_is_still_refused() {
    let (_d, r) = setup();
    write(&r, "keep", "k");
    write(&r, "dir/readme", "1");
    commit_all(&r, "base");
    plant(&r, "evil", "dir", &["readme", "README"]);
    let err = r.checkout("root", "evil").unwrap_err().to_string();
    assert!(err.contains("collide"), "{err}");
    assert_eq!(fs::read_to_string(r.worktree().join("dir/readme")).unwrap(), "1");
}

/// A change that keeps the set of names in a directory (only contents differ) is a plain switch.
#[test]
fn same_names_different_content_switches() {
    let (_d, r) = setup();
    write(&r, "keep", "k");
    write(&r, "dir/readme", "1");
    write(&r, "dir/other", "1");
    commit_all(&r, "base");
    plant(&r, "same", "dir", &["readme", "other"]);
    r.checkout("root", "same").unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("dir/readme")).unwrap(), "readme");
    clean(&r);
}

#[test]
fn attributes_found_with_and_without_a_mention_in_the_index() {
    let (_d, r) = setup();
    write(&r, "f", "1");
    commit_all(&r, "no attrs");
    assert!(r.attrs().unwrap().is_empty());
    write(&r, "sub/dir/.vcsattributes", "*.bin lockable\n");
    write(&r, "sub/dir/m.bin", "x");
    commit_all(&r, "attrs");
    assert!(!r.attrs().unwrap().is_empty());
    assert!(r.attrs().unwrap().lookup("sub/dir/m.bin").lockable);
    assert!(!r.attrs().unwrap().lookup("f").lockable);
}

/// Sparse sets: paths outside the set move in the index only; the cache-tree must still match.
#[test]
fn sparse_checkout_switch_then_commit_matches_full() {
    let (_d, r) = setup();
    for (p, c) in FILES {
        write(&r, p, c);
    }
    commit_all(&r, "base");
    r.create_branch("root", "side").unwrap();
    r.checkout("root", "side").unwrap();
    write(&r, "a/x", "ax-side");
    write(&r, "b/z", "bz-side");
    commit_all(&r, "side");
    r.checkout("root", "main").unwrap();
    r.sparse_set(None, "root", &["a".to_string()]).unwrap();
    for _ in 0..2 {
        r.checkout("root", "side").unwrap();
        assert_eq!(fs::read_to_string(r.worktree().join("a/x")).unwrap(), "ax-side");
        assert!(!r.worktree().join("b").exists());
        r.checkout("root", "main").unwrap();
    }
    r.checkout("root", "side").unwrap();
    write(&r, "a/y", "ay2");
    commit_all(&r, "more");
    let got = head_root(&r, "side");
    r.sparse_disable(None, "root").unwrap();
    let (_d2, c) = setup();
    for (p, content) in [
        ("a/x", "ax-side"),
        ("a/y", "ay2"),
        ("a/deep/p", "p"),
        ("a/deep/q", "q"),
        ("b/z", "bz-side"),
        ("b/sub/s", "s"),
        ("top", "top"),
        ("c/d/e/f", "f"),
    ] {
        write(&c, p, content);
    }
    commit_all(&c, "scratch");
    assert_eq!(got, head_root(&c, "main"));
}
