//! Symlink entries and case-fold/normalization collisions.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
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

fn clean(r: &Repo) -> bool {
    let s = r.status().unwrap();
    s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty())
}

/// Publish a crafted root-zone commit as a branch without going through `add`.
fn plant_branch(r: &Repo, branch: &str, entries: Vec<(&str, EntryMode, &[u8])>) {
    let mut es = Vec::new();
    for (name, mode, data) in entries {
        let id = r.store().put(&Object::Blob(data.to_vec())).unwrap();
        es.push(TreeEntry {
            name: name.into(),
            mode,
            id,
        });
    }
    let root = r.store().put(&Object::Tree(Tree::new(es).unwrap())).unwrap();
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

#[test]
fn symlink_round_trips_through_commit_and_checkout() {
    let (_d, r) = setup();
    write(&r, "dir/target.txt", "hello");
    symlink("target.txt", r.worktree().join("dir/link")).unwrap();
    symlink("dir/target.txt", r.worktree().join("top")).unwrap();
    symlink("missing", r.worktree().join("dangling")).unwrap(); // dangling but inside: allowed
    r.add(&["".into()]).unwrap();
    r.commit("with links", sig()).unwrap();
    assert!(clean(&r));

    let (_, c) = &r.log("root", None, 1).unwrap()[0];
    let Object::Tree(t) = r.store().get(&c.root).unwrap() else {
        panic!()
    };
    let top = t.entries().iter().find(|e| e.name == "top").unwrap();
    assert_eq!(top.mode, EntryMode::Symlink);
    assert_eq!(r.store().get_blob(&top.id).unwrap(), b"dir/target.txt");

    // status notices a retargeted link and a link replaced by a file
    fs::remove_file(r.worktree().join("top")).unwrap();
    symlink("dir", r.worktree().join("top")).unwrap();
    assert!(!clean(&r));
    r.add(&["top".into()]).unwrap();
    r.commit("retarget", sig()).unwrap();
    assert!(clean(&r));

    // branch with the old link target, then back: links are recreated, not written through
    r.create_branch("root", "other").unwrap();
    r.checkout("root", "other").unwrap();
    fs::remove_file(r.worktree().join("top")).unwrap();
    symlink("dir/target.txt", r.worktree().join("top")).unwrap();
    r.add(&["top".into()]).unwrap();
    r.commit("back", sig()).unwrap();
    r.checkout("root", "main").unwrap();
    assert_eq!(fs::read_link(r.worktree().join("top")).unwrap().to_str().unwrap(), "dir");
    assert_eq!(fs::read_to_string(r.worktree().join("dir/target.txt")).unwrap(), "hello");
    assert!(fs::symlink_metadata(r.worktree().join("dangling"))
        .unwrap()
        .file_type()
        .is_symlink());
    r.checkout("root", "other").unwrap();
    assert_eq!(fs::read_link(r.worktree().join("top")).unwrap().to_str().unwrap(), "dir/target.txt");
    assert!(clean(&r));
}

#[test]
fn escaping_links_are_refused_at_add() {
    let (d, r) = setup();
    fs::write(d.path().join("secret"), "s").unwrap();
    write(&r, "a/f", "x");
    symlink("../../secret", r.worktree().join("a/up")).unwrap();
    symlink("/etc/passwd", r.worktree().join("abs")).unwrap();
    symlink("a/../../secret", r.worktree().join("dots")).unwrap();
    // explicit path: hard error
    for p in ["a/up", "abs", "dots"] {
        assert!(r.add(&[p.into()]).is_err(), "{p} must be refused");
    }
    // tree walk: skipped and reported, the rest still staged
    let rep = r.add(&["".into()]).unwrap();
    assert_eq!(rep.skipped.len(), 3, "{:?}", rep.skipped);
    assert_eq!(rep.staged, 1);
}

#[test]
fn escaping_links_are_refused_at_checkout() {
    let (d, r) = setup();
    write(&r, "keep", "k");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    for (i, bad) in ["../outside", "/etc/passwd", "a/../../outside", "..\\x"].iter().enumerate() {
        let br = format!("evil{i}");
        plant_branch(
            &r,
            &br,
            vec![("keep", EntryMode::File, b"k"), ("l", EntryMode::Symlink, bad.as_bytes())],
        );
        let err = r.checkout("root", &br).unwrap_err().to_string();
        assert!(err.contains("symlink"), "{bad}: {err}");
        assert!(fs::symlink_metadata(r.worktree().join("l")).is_err(), "{bad}: nothing created");
    }
    assert!(!d.path().join("outside").exists());
}

#[test]
fn checkout_never_writes_through_an_existing_link() {
    let (d, r) = setup();
    write(&r, "f", "orig");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    let outside = d.path().join("victim");
    fs::write(&outside, "untouched").unwrap();
    // branch changes f; the worktree's f was swapped for a link to a file outside
    r.create_branch("root", "b").unwrap();
    r.checkout("root", "b").unwrap();
    write(&r, "f", "changed");
    r.add(&["f".into()]).unwrap();
    r.commit("change", sig()).unwrap();
    r.checkout("root", "main").unwrap();
    fs::remove_file(r.worktree().join("f")).unwrap();
    symlink(&outside, r.worktree().join("f")).unwrap();
    let _ = r.checkout("root", "b"); // may refuse (dirty) or replace the link; must not follow it
    assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
}

#[test]
fn case_and_normalization_collisions_are_refused_at_checkout() {
    for (a, b) in [("README", "readme"), ("caf\u{e9}", "cafe\u{301}")] {
        let (_d, r) = setup();
        write(&r, "keep", "k");
        r.add(&["".into()]).unwrap();
        r.commit("base", sig()).unwrap();
        plant_branch(
            &r,
            "evil",
            vec![
                (a, EntryMode::File, b"1"),
                (b, EntryMode::File, b"2"),
                ("keep", EntryMode::File, b"k"),
            ],
        );
        let err = r.checkout("root", "evil").unwrap_err().to_string();
        assert!(err.contains("collide"), "{a}/{b}: {err}");
        assert_eq!(fs::read_to_string(r.worktree().join("keep")).unwrap(), "k");
    }
}

#[test]
fn collisions_are_refused_at_add() {
    let (_d, r) = setup();
    write(&r, "Doc/a", "1");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    let alt = r.worktree().join("doc");
    fs::create_dir_all(&alt).unwrap();
    if alt.join("a").exists() {
        return; // folding filesystem: "doc" is the same directory as "Doc", nothing to collide
    }
    fs::write(alt.join("b"), "2").unwrap();
    let err = r.add(&["".into()]).unwrap_err().to_string();
    assert!(err.contains("collide"), "{err}");
}

#[test]
fn restore_recreates_a_symlink_without_following_it() {
    let (d, r) = setup();
    write(&r, "f", "x");
    symlink("f", r.worktree().join("l")).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    let victim = d.path().join("victim");
    fs::write(&victim, "untouched").unwrap();
    fs::remove_file(r.worktree().join("l")).unwrap();
    symlink(&victim, r.worktree().join("l")).unwrap();
    r.restore(&["l".into()], false).unwrap();
    assert_eq!(fs::read_link(r.worktree().join("l")).unwrap().to_str().unwrap(), "f");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
    assert!(clean(&r));
}

#[test]
fn stash_saves_and_restores_symlinks_as_links() {
    let (_d, r) = setup();
    write(&r, "target.txt", "hello");
    write(&r, "plain.txt", "one");
    symlink("target.txt", r.worktree().join("link")).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("with link", sig()).unwrap();
    // an unmodified tracked link must not make `stash` refuse or treat it as modified
    write(&r, "plain.txt", "two");
    r.stash_push(None, sig()).unwrap();
    assert!(clean(&r));
    assert_eq!(fs::read_link(r.worktree().join("link")).unwrap().to_str(), Some("target.txt"));
    r.stash_pop(0).unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("plain.txt")).unwrap(), "two");
    assert!(fs::symlink_metadata(r.worktree().join("link")).unwrap().is_symlink());
    // a retargeted link is a real change: stashed, reverted, and brought back as a link
    r.add(&["".into()]).unwrap();
    r.commit("plain two", sig()).unwrap();
    fs::remove_file(r.worktree().join("link")).unwrap();
    symlink("plain.txt", r.worktree().join("link")).unwrap();
    r.stash_push(None, sig()).unwrap();
    assert_eq!(fs::read_link(r.worktree().join("link")).unwrap().to_str(), Some("target.txt"));
    r.stash_pop(0).unwrap();
    assert_eq!(fs::read_link(r.worktree().join("link")).unwrap().to_str(), Some("plain.txt"));
    assert!(fs::symlink_metadata(r.worktree().join("link")).unwrap().is_symlink());
}

// ---- macOS (APFS: case-insensitive, normalization-insensitive but preserving) ----
// The tests above return early when the filesystem folds names. These run ONLY there and assert
// the folding really happens, so a checkout that would collide on APFS is exercised for real.

#[cfg(target_os = "macos")]
fn fs_folds_case(dir: &std::path::Path) -> bool {
    let p = dir.join("CaseProbe");
    fs::write(&p, "x").unwrap();
    let folds = dir.join("caseprobe").exists();
    fs::remove_file(&p).unwrap();
    folds
}

#[cfg(target_os = "macos")]
fn names(r: &Repo) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(r.worktree())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    v
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_checkout_of_a_case_only_rename_leaves_one_file_with_the_new_spelling() {
    let (d, r) = setup();
    assert!(fs_folds_case(d.path()), "the macOS temp dir is expected to be case-insensitive");
    write(&r, "readme", "old");
    write(&r, "keep", "k");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    plant_branch(
        &r,
        "evil",
        vec![("README", EntryMode::File, b"new"), ("keep", EntryMode::File, b"k")],
    );
    r.checkout("root", "evil").unwrap();
    assert_eq!(names(&r), ["README", "keep"], "stale lower-case entry or wrong spelling");
    assert_eq!(fs::read_to_string(r.worktree().join("readme")).unwrap(), "new");
    assert!(clean(&r));
    r.checkout("root", "main").unwrap();
    assert_eq!(names(&r), ["keep", "readme"]);
    assert_eq!(fs::read_to_string(r.worktree().join("README")).unwrap(), "old");
    assert!(clean(&r));
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_checkout_of_a_normalization_only_rename_leaves_one_file() {
    let (d, r) = setup();
    assert!(fs_folds_case(d.path()));
    let (nfc, nfd) = ("caf\u{e9}", "cafe\u{301}");
    write(&r, nfc, "old");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    plant_branch(&r, "nfd", vec![(nfd, EntryMode::File, b"new")]);
    r.checkout("root", "nfd").unwrap();
    assert_eq!(names(&r).len(), 1, "{:?}", names(&r));
    assert_eq!(fs::read_to_string(r.worktree().join(nfc)).unwrap(), "new");
    assert!(clean(&r));
}
