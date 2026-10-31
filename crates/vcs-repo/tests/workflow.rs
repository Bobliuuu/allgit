use std::fs;
use std::path::Path;
use vcs_objects::{Object, Signature};
use vcs_repo::{ChangeKind, Error, Repo};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
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
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn add_all(r: &Repo) {
    r.add(&["".to_string()]).unwrap();
}
fn commit(r: &Repo, m: &str) {
    r.commit(m, sig(1000)).unwrap();
}
fn st_clean(r: &Repo) -> bool {
    let s = r.status().unwrap();
    s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty())
}

#[test]
fn init_twice_fails_and_discover_works_from_subdir() {
    let (_d, r) = setup();
    assert!(Repo::init(r.worktree()).is_err());
    write(&r, "a/b/c.txt", "x");
    let r2 = Repo::discover(&r.worktree().join("a/b")).unwrap();
    assert_eq!(r2.worktree(), r.worktree());
}

#[test]
fn commit_status_diff_log_basic() {
    let (_d, r) = setup();
    assert!(matches!(r.commit("m", sig(1)), Err(Error::NothingToCommit)));
    write(&r, "hello.txt", "hello\n");
    assert_eq!(r.status().unwrap().untracked, vec!["hello.txt"]);
    add_all(&r);
    commit(&r, "one");
    assert!(st_clean(&r));
    write(&r, "hello.txt", "hello\nworld\n");
    let s = r.status().unwrap();
    assert_eq!(s.zones[0].unstaged[0].kind, ChangeKind::Modified);
    assert!(r.diff(false).unwrap().contains("+world"));
    assert_eq!(r.diff(true).unwrap(), "");
    add_all(&r);
    assert!(r.diff(true).unwrap().contains("+world"));
    assert_eq!(r.diff(false).unwrap(), "");
    r.commit("two", sig(2000)).unwrap();
    let log = r.log("root", None, 10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].1.message, "two");
    assert_eq!(log[0].1.parents, vec![log[1].0]);
    assert_ne!(log[0].1.change_id, log[1].1.change_id);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn no_git_repo_created() {
    let (_d, r) = setup();
    write(&r, "f", "x");
    add_all(&r);
    commit(&r, "m");
    assert!(!r.worktree().join(".git").exists());
}

#[test]
fn staged_deletion_and_untracked_directory_collapse() {
    let (_d, r) = setup();
    write(&r, "keep.txt", "k");
    write(&r, "gone.txt", "g");
    add_all(&r);
    commit(&r, "m");
    fs::remove_file(r.worktree().join("gone.txt")).unwrap();
    let s = r.status().unwrap();
    assert_eq!(
        s.zones[0].unstaged,
        vec![vcs_repo::Change {
            path: "gone.txt".into(),
            kind: ChangeKind::Deleted
        }]
    );
    r.add(&["gone.txt".into()]).unwrap();
    assert_eq!(r.status().unwrap().zones[0].staged[0].kind, ChangeKind::Deleted);
    commit(&r, "rm");
    write(&r, "newdir/a", "1");
    write(&r, "newdir/b", "2");
    assert_eq!(r.status().unwrap().untracked, vec!["newdir/"]);
}

#[test]
fn racy_same_size_same_mtime_edit_is_detected() {
    let (_d, r) = setup();
    write(&r, "f", "aaaa");
    r.add(&["f".into()]).unwrap();
    commit(&r, "m");
    let p = r.worktree().join("f");
    let mtime = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(&p, "bbbb").unwrap(); // same size
    fs::File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap(); // same mtime
    assert_eq!(r.status().unwrap().zones[0].unstaged.len(), 1, "stat cache must not hide this edit");
}

#[test]
fn zones_have_independent_branch_heads() {
    let (_d, r) = setup();
    write(&r, "README", "r");
    write(&r, "engine/core.rs", "v1");
    write(&r, "assets/a.bin", "A1");
    r.create_zone("engine", "engine").unwrap();
    r.create_zone("assets", "assets").unwrap();
    add_all(&r);
    commit(&r, "init");

    r.create_branch("engine", "feature").unwrap();
    r.checkout("engine", "feature").unwrap();
    write(&r, "engine/core.rs", "v2");
    r.add(&["engine".into()]).unwrap();
    commit(&r, "engine v2");

    let refs = r.refs().unwrap();
    assert_ne!(refs.head("engine", "feature"), refs.head("engine", "main"));
    // other zones unaffected
    assert_eq!(r.log("assets", None, 10).unwrap().len(), 1);
    assert_eq!(r.log("root", None, 10).unwrap().len(), 1);
    assert_eq!(r.branches("assets").unwrap().len(), 1);
    // commit objects are zone-tagged and parents stay in-zone
    for (_, c) in r.log("engine", None, 10).unwrap() {
        assert_eq!(c.zone, "engine");
    }
    // switching back restores contents only for that zone
    r.checkout("engine", "main").unwrap();
    assert_eq!(read(&r, "engine/core.rs"), "v1");
    assert_eq!(read(&r, "assets/a.bin"), "A1");
    r.checkout("engine", "feature").unwrap();
    assert_eq!(read(&r, "engine/core.rs"), "v2");
    assert!(st_clean(&r));
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn multi_zone_commit_is_one_transaction() {
    let (_d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    r.create_zone("shaders", "shaders").unwrap();
    write(&r, "engine/e", "1");
    write(&r, "shaders/s", "1");
    add_all(&r);
    let rep = r.commit("both", sig(5)).unwrap();
    assert_eq!(rep.commits.len(), 2);
    let Object::Transaction(t) = r.store().get(&rep.transaction).unwrap() else {
        panic!()
    };
    assert_eq!(t.updates.len(), 2);
    let refs = r.refs().unwrap();
    for u in &t.updates {
        assert_eq!(refs.head(&u.zone, &u.branch), Some(u.new));
    }
}

#[test]
fn stale_ref_update_is_all_or_nothing() {
    // CAS semantics at the refs layer: one stale precondition rejects the whole batch.
    let (_d, r) = setup();
    r.create_zone("a", "a").unwrap();
    r.create_zone("b", "b").unwrap();
    write(&r, "a/x", "1");
    write(&r, "b/x", "1");
    add_all(&r);
    commit(&r, "init");
    let before = r.refs().unwrap();
    let ha = before.head("a", "main").unwrap();
    let hb = before.head("b", "main").unwrap();
    let mut refs = before.clone();
    let bogus = vcs_objects::ObjectId::from_bytes([9; 32]);
    let ups = [
        vcs_objects::RefUpdate {
            zone: "a".into(),
            branch: "main".into(),
            old: Some(ha),
            new: hb,
        },
        vcs_objects::RefUpdate {
            zone: "b".into(),
            branch: "main".into(),
            old: Some(bogus),
            new: ha,
        },
    ];
    assert!(refs.apply(&ups).is_err());
    assert_eq!(refs.head("a", "main"), Some(ha), "first update must not have leaked");
}

#[test]
fn overlapping_zones_and_preexisting_content_rejected() {
    let (_d, r) = setup();
    write(&r, "src/engine/a.rs", "x");
    add_all(&r);
    commit(&r, "m");
    assert!(r.create_zone("engine", "src/engine").is_err(), "root already tracks files there");
    r.create_zone("src2", "lib").unwrap();
    assert!(r.create_zone("sub", "lib/sub").is_err());
    assert!(r.create_zone("par", "lib/..").is_err());
    assert!(r.create_zone("../evil", "x").is_err());
    assert!(r.create_zone("e", ".vcs").is_err());
    assert!(r.create_zone("e2", "/abs").is_err());
}

#[test]
fn unsubscribed_zone_is_not_deleted_and_not_touched() {
    let (_d, r) = setup();
    r.create_zone("assets", "assets").unwrap();
    write(&r, "assets/big.bin", "BIG");
    write(&r, "code.rs", "c");
    add_all(&r);
    commit(&r, "m");
    r.zone_remove("assets").unwrap();
    assert!(!r.worktree().join("assets").exists());
    let s = r.status().unwrap();
    assert_eq!(s.unsubscribed, vec!["assets"]);
    assert!(
        s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()),
        "missing != deleted"
    );
    // adding an unsubscribed path is refused
    write(&r, "assets/new.bin", "n");
    assert!(r.add(&["assets/new.bin".into()]).is_err());
    fs::remove_dir_all(r.worktree().join("assets")).unwrap();
    // a root commit must not drop the zone
    write(&r, "code.rs", "c2");
    add_all(&r);
    commit(&r, "root only");
    assert_eq!(r.log("assets", None, 5).unwrap().len(), 1);
    // re-subscribe restores
    r.zone_add("assets", None).unwrap();
    assert_eq!(read(&r, "assets/big.bin"), "BIG");
    assert!(st_clean(&r));
    assert!(r.zone_remove("root").is_err());
}

#[test]
fn dirty_zone_blocks_switch_and_remove() {
    let (_d, r) = setup();
    write(&r, "f", "1");
    add_all(&r);
    commit(&r, "m");
    r.create_branch("root", "b").unwrap();
    write(&r, "f", "dirty");
    assert!(r.checkout("root", "b").is_err());
    assert_eq!(read(&r, "f"), "dirty", "user data untouched");
}

#[test]
fn switch_refuses_to_clobber_untracked_file() {
    let (_d, r) = setup();
    write(&r, "f", "1");
    add_all(&r);
    commit(&r, "m");
    r.create_branch("root", "b").unwrap();
    r.checkout("root", "b").unwrap();
    write(&r, "extra", "from b");
    add_all(&r);
    commit(&r, "b adds extra");
    r.checkout("root", "main").unwrap();
    assert!(!r.worktree().join("extra").exists());
    write(&r, "extra", "precious local data");
    assert!(r.checkout("root", "b").is_err());
    assert_eq!(read(&r, "extra"), "precious local data");
}

#[cfg(unix)]
#[test]
fn checkout_refuses_to_write_through_symlink() {
    let (d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    write(&r, "engine/keep", "k");
    add_all(&r);
    commit(&r, "m");
    r.create_branch("engine", "b").unwrap();
    r.checkout("engine", "b").unwrap();
    write(&r, "engine/sub/payload", "x");
    add_all(&r);
    commit(&r, "adds sub/payload");
    r.checkout("engine", "main").unwrap();
    // attacker-controlled worktree: engine/sub is a symlink to outside
    let outside = d.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, r.worktree().join("engine/sub")).unwrap();
    assert!(r.checkout("engine", "b").is_err());
    assert!(!outside.join("payload").exists(), "nothing escaped the worktree");
}

#[test]
fn paths_outside_repo_and_meta_dir_rejected() {
    let (d, r) = setup();
    assert!(r.rel_path(r.worktree(), "../outside").is_err());
    assert!(r.rel_path(r.worktree(), ".vcs/objects").is_err());
    assert!(r.rel_path(r.worktree(), ".VCS/x").is_err());
    assert_eq!(r.rel_path(r.worktree(), "a/../b/./c").unwrap(), "b/c");
    assert_eq!(r.rel_path(r.worktree(), ".").unwrap(), "");
    let _ = d;
    // walking `add .` never stages metadata
    write(&r, "f", "1");
    add_all(&r);
    commit(&r, "m");
    let (_, root) = &r.log("root", None, 1).unwrap()[0];
    let Object::Tree(t) = r.store().get(&root.root).unwrap() else {
        panic!()
    };
    assert!(t.entries().iter().all(|e| !e.name.eq_ignore_ascii_case(".vcs")));
}

#[test]
fn fsck_detects_corruption() {
    let (_d, r) = setup();
    write(&r, "f", "important");
    add_all(&r);
    commit(&r, "m");
    assert!(r.fsck().unwrap().problems.is_empty());
    let id = r
        .store()
        .list()
        .unwrap()
        .into_iter()
        .find(|i| matches!(r.store().get(i), Ok(Object::Blob(_))))
        .unwrap();
    let h = id.to_hex();
    let p = r.worktree().join(".vcs/objects").join(&h[..2]).join(&h[2..]);
    let mut b = fs::read(&p).unwrap();
    *b.last_mut().unwrap() ^= 0xff;
    fs::write(&p, b).unwrap();
    let rep = r.fsck().unwrap();
    assert_eq!(rep.problems.len(), 1, "{:?}", rep.problems);
}

#[test]
fn stale_lock_blocks_mutation_with_clear_error() {
    let (_d, r) = setup();
    fs::write(r.worktree().join(".vcs/lock"), "pid 1").unwrap();
    write(&r, "f", "1");
    let e = r.add(&["f".into()]).unwrap_err();
    assert!(matches!(e, Error::Locked(_)), "{e}");
    assert!(e.to_string().contains("delete that file"));
}

#[test]
fn identical_content_dedups_across_zones_and_commits() {
    let (_d, r) = setup();
    r.create_zone("a", "a").unwrap();
    write(&r, "a/x", "same bytes");
    write(&r, "y", "same bytes");
    add_all(&r);
    commit(&r, "m");
    let blobs = r
        .store()
        .list()
        .unwrap()
        .into_iter()
        .filter(|i| matches!(r.store().get(i), Ok(Object::Blob(_))))
        .count();
    assert_eq!(blobs, 1);
}

#[test]
fn empty_message_and_bad_names_rejected() {
    let (_d, r) = setup();
    write(&r, "f", "1");
    add_all(&r);
    assert!(r.commit("  ", sig(1)).is_err());
    assert!(r.create_branch("root", "bad name").is_err());
    commit(&r, "ok");
    assert!(r.create_branch("root", "x/../y").is_err());
    assert!(r.create_branch("root", "main").is_err(), "exists");
    let _ = Path::new("");
}
