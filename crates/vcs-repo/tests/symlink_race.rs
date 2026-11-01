//! Issue #282: working-tree writes by `stash pop`, `rebase` and `merge` never go through a symlink
//! (or a hard link) planted in the worktree where the operation will write.
//!
//! Each test plants the link before the operation starts, so it checks the preflight and the
//! temp-name-plus-rename write path. A swap that happens between the preflight and the write
//! cannot be tested deterministically and is not covered here (see docs/security-review.md).
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use vcs_objects::Signature;
use vcs_repo::Repo;

const CANARY: &str = "outside canary: must never change\n";

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn setup() -> (tempfile::TempDir, Repo, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let out = d.path().join("outside");
    fs::create_dir(&out).unwrap();
    (d, r, out)
}

fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}

fn commit_all(r: &Repo, msg: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(msg, sig()).unwrap();
}

/// Names directly inside `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

/// The canary file is byte-for-byte unchanged and nothing else appeared outside the worktree.
fn assert_outside_intact(d: &tempfile::TempDir, out: &Path, canary: &str, expect: &[&str]) {
    if !expect.is_empty() {
        assert_eq!(fs::read_to_string(out.join(canary)).unwrap(), CANARY, "write went through a link");
    }
    assert_eq!(names(out), expect, "something new appeared outside the worktree");
    assert_eq!(names(d.path()), ["outside", "r"], "something new appeared next to the worktree");
}

/// Remove `p` from the worktree (file, link, or empty/non-empty directory), if present.
fn clear(r: &Repo, p: &str) {
    let abs = r.worktree().join(p);
    match fs::symlink_metadata(&abs) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(&abs).unwrap(),
        Ok(_) => fs::remove_file(&abs).unwrap(),
        Err(_) => {}
    }
}

#[derive(Clone, Copy, Debug)]
enum Plant {
    /// `d` becomes a symlink to the outside directory, which already holds a canary `f`.
    DirLink,
    /// `f` becomes a symlink to the outside canary.
    FileLink,
    /// `f` becomes a hard link to the outside canary.
    HardLink,
    /// `d` becomes a dangling symlink into a missing outside directory.
    DanglingDir,
}

impl Plant {
    /// The path the operation will write.
    fn path(self) -> &'static str {
        match self {
            Plant::DirLink | Plant::DanglingDir => "d/f",
            _ => "f",
        }
    }
    /// Plant the link. Outside directory content is seeded here.
    fn plant(self, r: &Repo, out: &Path) {
        let wt = r.worktree();
        match self {
            Plant::DirLink => {
                fs::write(out.join("f"), CANARY).unwrap();
                symlink(out, wt.join("d")).unwrap();
            }
            Plant::FileLink => {
                fs::write(out.join("canary"), CANARY).unwrap();
                symlink(out.join("canary"), wt.join("f")).unwrap();
            }
            Plant::HardLink => {
                fs::write(out.join("canary"), CANARY).unwrap();
                fs::hard_link(out.join("canary"), wt.join("f")).unwrap();
            }
            Plant::DanglingDir => symlink(out.join("missing"), wt.join("d")).unwrap(),
        }
    }
    /// Names expected outside after the operation.
    fn expect(self) -> &'static [&'static str] {
        match self {
            Plant::DirLink => &["f"],
            Plant::FileLink | Plant::HardLink => &["canary"],
            Plant::DanglingDir => &[],
        }
    }
    fn canary(self) -> &'static str {
        match self {
            Plant::DirLink => "f",
            _ => "canary",
        }
    }
}

const NEW_FILE_PLANTS: [Plant; 4] = [Plant::DirLink, Plant::FileLink, Plant::HardLink, Plant::DanglingDir];

/// stash pop re-creates a path the stash added; the link sits where the file was.
#[test]
fn stash_pop_never_writes_through_a_planted_link() {
    for plant in NEW_FILE_PLANTS {
        let (d, r, out) = setup();
        write(&r, "base", "b\n");
        commit_all(&r, "base");
        write(&r, plant.path(), "stashed content\n");
        r.add(&["".into()]).unwrap();
        r.stash_push(None, sig()).unwrap();
        // the stash removed the new file; drop an emptied directory too, then plant the link
        clear(&r, plant.path().split('/').next().unwrap());
        plant.plant(&r, &out);
        let res = r.stash_pop(0);
        eprintln!("stash pop {plant:?}: {res:?}");
        assert_outside_intact(&d, &out, plant.canary(), plant.expect());
        // a refusal must leave the entry in place; success must have replaced the link
        match res {
            Err(_) => assert_eq!(r.stash_list().unwrap().len(), 1, "{plant:?}: refused but entry lost"),
            Ok(_) => {
                let m = fs::symlink_metadata(r.worktree().join(plant.path())).unwrap();
                assert!(m.is_file() && !m.file_type().is_symlink(), "{plant:?}: not a plain file");
                assert_eq!(fs::read_to_string(r.worktree().join(plant.path())).unwrap(), "stashed content\n");
            }
        }
    }
}

/// stash pop three-way merge of a modified tracked file whose worktree copy is a hard link.
#[test]
fn stash_pop_modify_on_hardlinked_file_keeps_canary() {
    let (d, r, out) = setup();
    write(&r, "f", "1\n2\n3\n");
    commit_all(&r, "base");
    write(&r, "f", "1\n2\nstashed\n");
    r.stash_push(None, sig()).unwrap();
    // after the stash `f` is the committed content: hard link it to the canary location
    fs::hard_link(r.worktree().join("f"), out.join("canary")).unwrap();
    let committed = fs::read_to_string(out.join("canary")).unwrap();
    assert_eq!(committed, "1\n2\n3\n");
    let _ = r.stash_pop(0);
    assert_eq!(
        fs::read_to_string(out.join("canary")).unwrap(),
        committed,
        "write went through the hard link"
    );
    assert_eq!(names(&out), ["canary"]);
    assert_eq!(names(d.path()), ["outside", "r"]);
}

/// A clean merge creates a path the other branch added; the link sits there.
#[test]
fn merge_clean_never_writes_through_a_planted_link() {
    for plant in NEW_FILE_PLANTS {
        let (d, r, out) = setup();
        write(&r, "base", "b\n");
        commit_all(&r, "base");
        r.create_branch("root", "other").unwrap();
        r.checkout("root", "other").unwrap();
        write(&r, plant.path(), "from other\n");
        commit_all(&r, "other adds");
        r.checkout("root", "main").unwrap();
        write(&r, "base", "b2\n");
        commit_all(&r, "main moves"); // forces a real merge, not a fast-forward
        clear(&r, plant.path().split('/').next().unwrap());
        plant.plant(&r, &out);
        let res = r.merge("root", "other", sig());
        eprintln!("merge {plant:?}: {res:?}");
        assert_outside_intact(&d, &out, plant.canary(), plant.expect());
        if res.is_ok() {
            let m = fs::symlink_metadata(r.worktree().join(plant.path())).unwrap();
            assert!(m.is_file() && !m.file_type().is_symlink(), "{plant:?}: not a plain file");
        }
    }
}

/// Fast-forward merges write too.
#[test]
fn merge_fast_forward_never_writes_through_a_planted_link() {
    for plant in NEW_FILE_PLANTS {
        let (d, r, out) = setup();
        write(&r, "base", "b\n");
        commit_all(&r, "base");
        r.create_branch("root", "other").unwrap();
        r.checkout("root", "other").unwrap();
        write(&r, plant.path(), "from other\n");
        commit_all(&r, "other adds");
        r.checkout("root", "main").unwrap();
        clear(&r, plant.path().split('/').next().unwrap());
        plant.plant(&r, &out);
        let res = r.merge("root", "other", sig());
        eprintln!("merge {plant:?}: {res:?}");
        assert_outside_intact(&d, &out, plant.canary(), plant.expect());
    }
}

/// Conflict markers are written into a tracked file whose worktree copy is a hard link to
/// an outside file with identical content (so the worktree still looks clean).
#[test]
fn merge_conflict_markers_do_not_write_through_a_hard_link() {
    let (d, r, out) = setup();
    write(&r, "f", "1\n2\n3\n");
    commit_all(&r, "base");
    r.create_branch("root", "other").unwrap();
    r.checkout("root", "other").unwrap();
    write(&r, "f", "other\n2\n3\n");
    commit_all(&r, "other");
    r.checkout("root", "main").unwrap();
    write(&r, "f", "main\n2\n3\n");
    commit_all(&r, "main");
    fs::hard_link(r.worktree().join("f"), out.join("canary")).unwrap();
    let res = r.merge("root", "other", sig());
    eprintln!("merge hardlink conflict: {res:?}");
    assert_eq!(
        fs::read_to_string(out.join("canary")).unwrap(),
        "main\n2\n3\n",
        "markers written through the hard link"
    );
    assert_eq!(names(&out), ["canary"]);
    assert_eq!(names(d.path()), ["outside", "r"]);
    if res.is_ok() {
        let m = fs::symlink_metadata(r.worktree().join("f")).unwrap();
        assert!(m.is_file() && !m.file_type().is_symlink());
    }
}

/// Conflict markers for a path that both sides added; `d` was swapped for a link first.
#[test]
fn merge_conflict_markers_do_not_write_through_a_dir_link() {
    for plant in [Plant::DirLink, Plant::DanglingDir] {
        let (d, r, out) = setup();
        write(&r, "base", "b\n");
        commit_all(&r, "base");
        r.create_branch("root", "other").unwrap();
        r.checkout("root", "other").unwrap();
        write(&r, "d/f", "other\n");
        commit_all(&r, "other adds");
        r.checkout("root", "main").unwrap();
        write(&r, "d/f", "main\n");
        commit_all(&r, "main adds");
        clear(&r, "d");
        plant.plant(&r, &out);
        let res = r.merge("root", "other", sig());
        eprintln!("merge {plant:?}: {res:?}");
        assert_outside_intact(&d, &out, plant.canary(), plant.expect());
    }
}

fn rebase_fixture(r: &Repo, feature_f: &str, main_f: &str) {
    write(r, "f", "1\n2\n3\n");
    commit_all(r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(r, "f", feature_f);
    commit_all(r, "feature edits f");
    r.checkout("root", "main").unwrap();
    write(r, "f", main_f);
    commit_all(r, "main edits f");
    r.checkout("root", "feature").unwrap();
}

/// Clean replay onto a moved base rewrites `f`, which is a hard link to an outside file.
#[test]
fn rebase_replay_does_not_write_through_a_hard_link() {
    let (d, r, out) = setup();
    rebase_fixture(&r, "1\n2\nfeature\n", "main\n2\n3\n");
    fs::hard_link(r.worktree().join("f"), out.join("canary")).unwrap();
    let res = r.rebase("root", "main", sig());
    eprintln!("rebase hardlink: {res:?}");
    assert_eq!(
        fs::read_to_string(out.join("canary")).unwrap(),
        "1\n2\nfeature\n",
        "rebase wrote through the hard link"
    );
    assert_eq!(names(&out), ["canary"]);
    assert_eq!(names(d.path()), ["outside", "r"]);
    if res.is_ok() {
        assert_eq!(fs::read_to_string(r.worktree().join("f")).unwrap(), "main\n2\nfeature\n");
    }
}

/// A conflicting replay writes markers, then `--continue` writes the resolved commit.
#[test]
fn rebase_conflict_and_continue_do_not_write_through_a_hard_link() {
    let (d, r, out) = setup();
    rebase_fixture(&r, "feature\n2\n3\n", "main\n2\n3\n");
    fs::hard_link(r.worktree().join("f"), out.join("canary")).unwrap();
    let res = r.rebase("root", "main", sig());
    eprintln!("rebase hardlink: {res:?}");
    assert_eq!(
        fs::read_to_string(out.join("canary")).unwrap(),
        "feature\n2\n3\n",
        "markers written through the hard link"
    );
    if res.is_ok() {
        // resolve (fresh file) and continue
        let abs = r.worktree().join("f");
        let _ = fs::remove_file(&abs);
        fs::write(&abs, "resolved\n2\n3\n").unwrap();
        let _ = r.resolve("f", &vcs_repo::Resolution::Mark);
        let _ = r.rebase_continue(sig());
    }
    assert_eq!(fs::read_to_string(out.join("canary")).unwrap(), "feature\n2\n3\n");
    assert_eq!(names(&out), ["canary"]);
    assert_eq!(names(d.path()), ["outside", "r"]);
}

/// Replay of a commit that adds `d/f` or `f` while the worktree has a link at that spot.
#[test]
fn rebase_replay_of_added_path_never_writes_through_a_planted_link() {
    for plant in NEW_FILE_PLANTS {
        let (d, r, out) = setup();
        write(&r, "base", "b\n");
        commit_all(&r, "base");
        r.create_branch("root", "feature").unwrap();
        r.checkout("root", "feature").unwrap();
        write(&r, plant.path(), "feature adds\n");
        commit_all(&r, "feature adds");
        r.checkout("root", "main").unwrap();
        write(&r, "base", "b2\n");
        commit_all(&r, "main moves");
        r.checkout("root", "feature").unwrap();
        // the link replaces the feature's own file in the worktree
        clear(&r, plant.path().split('/').next().unwrap());
        plant.plant(&r, &out);
        let res = r.rebase("root", "main", sig());
        eprintln!("rebase {plant:?}: {res:?}");
        assert_outside_intact(&d, &out, plant.canary(), plant.expect());
    }
}
