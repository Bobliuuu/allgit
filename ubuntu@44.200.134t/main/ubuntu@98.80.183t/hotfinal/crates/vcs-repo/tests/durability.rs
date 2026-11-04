//! Crash-simulation tests for the batched durability model (docs/architecture.md, "Durability").
//!
//! A "crash" is modelled by dropping the `Repo` (and its `LocalStore`) without reaching the
//! durability barrier `ObjectStore::sync()`. Unsynced objects must then be invisible under their
//! final names, and no ref/index may point at a missing object.

use std::fs;
use vcs_objects::{EntryMode, Object, Signature, Tree, TreeEntry};
use vcs_repo::{Repo, ROOT_ZONE};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}

fn add_all(r: &Repo) {
    r.add(&[String::new()]).unwrap();
}

fn assert_valid(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "fsck problems: {:?}", rep.problems);
}

#[test]
fn crash_before_barrier_leaves_repo_valid_and_objects_invisible() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("r");
    let (base_objects, orphan_blob, orphan_tree);
    {
        let r = Repo::init(&root).unwrap();
        write(&r, "a/one.txt", "one\n");
        write(&r, "two.txt", "two\n");
        add_all(&r);
        r.commit("base", sig(1)).unwrap();
        assert_valid(&r);
        base_objects = r.store().list().unwrap();

        // An `add` / `commit` that dies after writing objects but before the barrier:
        // objects exist in-process, but index and refs were never updated.
        orphan_blob = r.store().put_blob(b"staged but never durable\n").unwrap();
        orphan_tree = r
            .store()
            .put(&Object::Tree(
                Tree::new(vec![TreeEntry {
                    name: "x".into(),
                    mode: EntryMode::File,
                    id: orphan_blob,
                }])
                .unwrap(),
            ))
            .unwrap();
        assert!(r.store().contains(&orphan_blob).unwrap(), "readable in-process before the barrier");
        // crash: drop without sync()
    }

    let r = Repo::discover(&root).unwrap();
    assert_valid(&r);
    assert_eq!(r.store().list().unwrap(), base_objects, "unsynced objects must not be published");
    assert!(!r.store().contains(&orphan_blob).unwrap());
    assert!(!r.store().contains(&orphan_tree).unwrap());
    // every index entry and every ref target is present
    let log = r.log(ROOT_ZONE, None, 10).unwrap();
    assert_eq!(log.len(), 1);
    let st = r.status().unwrap();
    assert!(st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()));

    // the repo remains fully usable and the lost work can simply be redone
    write(&r, "new.txt", "after the crash\n");
    add_all(&r);
    r.commit("recovered", sig(2)).unwrap();
    assert_valid(&r);
    assert_eq!(r.log(ROOT_ZONE, None, 10).unwrap().len(), 2);
}

#[test]
fn completed_add_and_commit_are_durable_after_reopen() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("r");
    {
        let r = Repo::init(&root).unwrap();
        for i in 0..50 {
            write(&r, &format!("d{}/f{i}.txt", i % 5), &format!("content {i}\n"));
        }
        add_all(&r); // barrier inside add: blobs durable before the index names them
                     // crash between add and commit: index is on disk, so its blobs must be too
    }
    {
        let r = Repo::discover(&root).unwrap();
        assert_valid(&r); // fsck checks every index entry's blob exists
        r.commit("c1", sig(1)).unwrap();
    }
    let r = Repo::discover(&root).unwrap();
    assert_valid(&r);
    assert_eq!(r.log(ROOT_ZONE, None, 10).unwrap().len(), 1);
    assert!(r.status().unwrap().untracked.is_empty());
}

#[test]
fn re_adding_unchanged_files_writes_no_new_objects() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for i in 0..20 {
        write(&r, &format!("f{i}.txt"), &format!("v{i}\n"));
    }
    add_all(&r);
    let before = r.store().list().unwrap();
    add_all(&r);
    assert_eq!(r.store().list().unwrap(), before);
    let tmp = d.path().join("r/.vcs/objects/tmp");
    if tmp.exists() {
        assert_eq!(fs::read_dir(tmp).unwrap().count(), 0, "no leftover tmp files after the barrier");
    }
}
