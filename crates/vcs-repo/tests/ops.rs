//! Operation log: recording, undo, restore, safety rules.
use std::fs;
use vcs_objects::{Object, ObjectId, Operation, Signature};
use vcs_remote::{load_manifest, MemBackend};
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
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn commit(r: &Repo, p: &str, content: &str, msg: &str) -> ObjectId {
    write(r, p, content);
    r.add(&["".into()]).unwrap();
    r.commit(msg, sig()).unwrap().commits[0].2
}
fn head(r: &Repo, zone: &str) -> Option<ObjectId> {
    let ws = r.workspace().unwrap();
    r.refs().unwrap().head(zone, &ws.subs[zone])
}
fn descs(r: &Repo) -> Vec<String> {
    r.op_log(100).unwrap().into_iter().map(|(_, o)| o.description).collect()
}
fn fsck_ok(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn every_mutation_is_recorded() {
    let (_d, r) = setup();
    assert_eq!(descs(&r), ["init repository"]);
    r.create_zone("engine", "engine").unwrap();
    write(&r, "engine/a", "1");
    write(&r, "README", "r");
    r.add(&["".into()]).unwrap();
    r.commit("first", sig()).unwrap();
    r.create_branch("engine", "feat").unwrap();
    r.checkout("engine", "feat").unwrap();
    r.snapshot_create("s1", "m", sig()).unwrap();
    r.zone_remove("engine").unwrap();
    r.zone_add("engine", Some("feat")).unwrap();
    let d = descs(&r);
    assert_eq!(d.len(), 8, "{d:?}");
    assert!(d[0].starts_with("zone add") && d[1].starts_with("zone remove") && d[2].starts_with("snapshot"));
    assert!(d[3].starts_with("checkout") && d[4].starts_with("create branch") && d[5].starts_with("commit: first"));
    assert!(d[6].starts_with("create zone"));
    // every op's parent chain is linear and fsck passes
    let log = r.op_log(100).unwrap();
    for w in log.windows(2) {
        assert_eq!(w[0].1.parents, vec![w[1].0]);
    }
    assert_eq!(r.op_head().unwrap(), Some(log[0].0));
    fsck_ok(&r);
}

#[test]
fn undo_commit_restores_branch_and_worktree() {
    let (_d, r) = setup();
    let c1 = commit(&r, "f.txt", "one", "c1");
    let c2 = commit(&r, "f.txt", "two", "c2");
    assert_eq!(head(&r, "root"), Some(c2));
    r.op_undo().unwrap();
    assert_eq!(head(&r, "root"), Some(c1));
    assert_eq!(read(&r, "f.txt"), "one");
    let st = r.status().unwrap();
    assert!(st.zones[0].staged.is_empty() && st.zones[0].unstaged.is_empty());
    // the undo is itself an operation, and objects were never deleted
    assert!(descs(&r)[0].starts_with("undo operation"));
    assert!(r.store().contains(&c2).unwrap());
    fsck_ok(&r);
}

#[test]
fn undo_first_commit_leaves_unborn_branch() {
    let (_d, r) = setup();
    commit(&r, "f.txt", "one", "c1");
    r.op_undo().unwrap();
    assert_eq!(head(&r, "root"), None);
    assert!(!r.worktree().join("f.txt").exists());
    fsck_ok(&r);
}

#[test]
fn undo_checkout_and_branch_creation() {
    let (_d, r) = setup();
    commit(&r, "f.txt", "main", "c1");
    r.create_branch("root", "feat").unwrap();
    r.checkout("root", "feat").unwrap();
    commit(&r, "f.txt", "feat", "c2");
    r.checkout("root", "main").unwrap();
    assert_eq!(read(&r, "f.txt"), "main");
    r.op_undo().unwrap(); // undo checkout main -> back on feat
    assert_eq!(r.workspace().unwrap().subs["root"], "feat");
    assert_eq!(read(&r, "f.txt"), "feat");
    r.op_undo().unwrap(); // toggles: undo the undo -> main again
    assert_eq!(r.workspace().unwrap().subs["root"], "main");
    assert_eq!(read(&r, "f.txt"), "main");
    // undo create_branch removes the local branch ref
    let (_d2, r2) = setup();
    commit(&r2, "f.txt", "x", "c1");
    r2.create_branch("root", "tmp").unwrap();
    r2.op_undo().unwrap();
    assert!(r2.refs().unwrap().head("root", "tmp").is_none());
    fsck_ok(&r);
    fsck_ok(&r2);
}

#[test]
fn undo_zone_add_and_remove() {
    let (_d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    write(&r, "engine/a.rs", "a");
    r.add(&["".into()]).unwrap();
    r.commit("init", sig()).unwrap();
    r.zone_remove("engine").unwrap();
    assert!(!r.worktree().join("engine").exists());
    r.op_undo().unwrap(); // undo remove => subscribed + materialized
    assert_eq!(read(&r, "engine/a.rs"), "a");
    assert!(r.workspace().unwrap().subs.contains_key("engine"));
    r.op_undo().unwrap(); // toggle back => unsubscribed again
    assert!(!r.workspace().unwrap().subs.contains_key("engine"));
    assert!(!r.worktree().join("engine").exists());
    // now undo a zone_add: add it, then undo
    r.op_undo().unwrap(); // re-subscribe via toggle
    r.zone_remove("engine").unwrap();
    r.zone_add("engine", None).unwrap();
    assert!(r.worktree().join("engine/a.rs").exists());
    r.op_undo().unwrap();
    assert!(!r.worktree().join("engine").exists());
    assert!(!r.workspace().unwrap().subs.contains_key("engine"));
    fsck_ok(&r);
}

#[test]
fn restore_to_old_op_and_redo() {
    let (_d, r) = setup();
    let c1 = commit(&r, "f", "1", "c1");
    let op1 = r.op_head().unwrap().unwrap();
    let c2 = commit(&r, "f", "2", "c2");
    let op2 = r.op_head().unwrap().unwrap();
    let c3 = commit(&r, "f", "3", "c3");
    r.op_restore(&op1).unwrap();
    assert_eq!(head(&r, "root"), Some(c1));
    assert_eq!(read(&r, "f"), "1");
    // "undo twice" then redo via restore of the newest commit op
    let before = r.op_log(100).unwrap().len();
    r.op_undo().unwrap(); // undoes the restore => c3
    assert_eq!(head(&r, "root"), Some(c3));
    r.op_undo().unwrap(); // toggles => c1
    assert_eq!(head(&r, "root"), Some(c1));
    r.op_restore(&op2).unwrap();
    assert_eq!(head(&r, "root"), Some(c2));
    assert_eq!(read(&r, "f"), "2");
    assert_eq!(r.op_log(100).unwrap().len(), before + 3);
    // prefix resolution
    assert_eq!(r.op_resolve(&op1.to_hex()[..10]).unwrap(), op1);
    assert!(r.op_resolve("zz").is_err());
    fsck_ok(&r);
}

#[test]
fn refuses_when_affected_zone_is_dirty_and_changes_nothing() {
    let (_d, r) = setup();
    commit(&r, "f", "1", "c1");
    let c2 = commit(&r, "f", "2", "c2");
    write(&r, "f", "dirty edit");
    let head_op = r.op_head().unwrap();
    let e = r.op_undo().unwrap_err().to_string();
    assert!(e.contains("uncommitted"), "{e}");
    assert_eq!(head(&r, "root"), Some(c2));
    assert_eq!(read(&r, "f"), "dirty edit");
    assert_eq!(r.op_head().unwrap(), head_op);
    // staged (not just unstaged) changes also block
    r.add(&["f".into()]).unwrap();
    assert!(r.op_undo().is_err());
}

#[test]
fn dirty_unaffected_zone_does_not_block() {
    let (_d, r) = setup();
    r.create_zone("engine", "engine").unwrap();
    commit(&r, "engine/a", "1", "c1");
    let op1 = r.op_head().unwrap().unwrap();
    write(&r, "README", "scratch"); // untracked root file; root zone unaffected by restore of op1
    r.op_restore(&op1).unwrap();
    assert_eq!(read(&r, "README"), "scratch");
}

#[test]
fn restoring_view_with_missing_commit_is_an_error() {
    let (_d, r) = setup();
    let c1 = commit(&r, "f", "1", "c1");
    let c2 = commit(&r, "f", "2", "c2");
    // simulate a view whose commit is not available locally
    let hex = c1.to_hex();
    fs::remove_file(r.worktree().join(".vcs/objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    let e = r.op_undo().unwrap_err().to_string();
    assert!(e.contains("missing locally"), "{e}");
    assert_eq!(head(&r, "root"), Some(c2));
    assert_eq!(read(&r, "f"), "2");
}

#[test]
fn crash_between_object_store_and_op_head_keeps_previous_op_head_valid() {
    let (_d, r) = setup();
    commit(&r, "f", "1", "c1");
    let good = r.op_head().unwrap().unwrap();
    // Simulate the crash window: Operation stored, op_head not yet moved.
    let orphan = Operation {
        parents: vec![good],
        view: r.current_view().unwrap(),
        description: "interrupted".into(),
        author: sig(),
    };
    r.store().put(&Object::Operation(orphan)).unwrap();
    assert_eq!(r.op_head().unwrap(), Some(good));
    assert_eq!(descs(&r)[0], "commit: c1");
    fsck_ok(&r);
    // the repository keeps working and chains from the still-valid head
    commit(&r, "f", "2", "c2");
    let log = r.op_log(10).unwrap();
    assert_eq!(log[0].1.parents, vec![good]);
    // a torn temp file for op_head is ignored too
    fs::write(r.worktree().join(".vcs/op_head.tmp999"), b"garbage").unwrap();
    assert!(r.op_head().unwrap().is_some());
    fsck_ok(&r);
}

#[test]
fn fsck_detects_broken_op_chain() {
    let (_d, r) = setup();
    commit(&r, "f", "1", "c1");
    let log = r.op_log(10).unwrap();
    let old = log[1].0.to_hex();
    fs::remove_file(r.worktree().join(".vcs/objects").join(&old[..2]).join(&old[2..])).unwrap();
    let rep = r.fsck().unwrap();
    assert!(rep.problems.iter().any(|p| p.contains("operation")), "{:?}", rep.problems);
    // garbage op_head
    fs::write(r.worktree().join(".vcs/op_head"), b"nope").unwrap();
    assert!(r.fsck().unwrap().problems.iter().any(|p| p.contains("op_head")));
}

#[test]
fn undo_never_touches_remote_tracking_refs_or_remote() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = Repo::init(&d.path().join("a")).unwrap();
    commit(&a, "f", "1", "c1");
    a.publish(&remote, None, sig()).unwrap();
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), None).unwrap();
    let b_c1 = head(&b, "root").unwrap();
    commit(&a, "f", "2", "c2");
    a.publish(&remote, None, sig()).unwrap();
    let (m_before, _) = load_manifest(&remote).unwrap();
    // sync fast-forward is recorded as an operation
    b.sync(&remote).unwrap();
    assert_eq!(read(&b, "f"), "2");
    assert!(descs(&b)[0].starts_with("sync: fast-forward"));
    let tracking = fs::read(b.worktree().join(".vcs/remote-refs")).unwrap();
    b.op_undo().unwrap();
    assert_eq!(head(&b, "root"), Some(b_c1));
    assert_eq!(read(&b, "f"), "1");
    assert_eq!(fs::read(b.worktree().join(".vcs/remote-refs")).unwrap(), tracking);
    let (m_after, _) = load_manifest(&remote).unwrap();
    assert_eq!(m_before.generation, m_after.generation);
    assert_eq!(m_before.branches, m_after.branches);
    fsck_ok(&b);
}
