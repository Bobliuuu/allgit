//! `gc_plan` (dry run): reachability roots, grace period, and that nothing is deleted.
use std::fs;
use std::time::{Duration, SystemTime};
use vcs_objects::{Object, ObjectId, Signature};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn commit(r: &Repo, p: &str, c: &str) {
    fs::write(r.worktree().join(p), c).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("m", sig()).unwrap();
}
fn orphan(r: &Repo, data: &[u8]) -> ObjectId {
    let id = r.store().put(&Object::Blob(data.to_vec())).unwrap();
    r.store().sync().unwrap();
    id
}
const DAY: Duration = Duration::from_secs(86400);
fn later() -> SystemTime {
    SystemTime::now() + 30 * DAY
}

#[test]
fn dry_run_reports_unreachable_and_deletes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    let o = orphan(&r, b"nobody references me");
    let before = r.store().list().unwrap();
    let rep = r.gc_plan_at(later(), 14 * DAY).unwrap();
    assert_eq!(rep.would_delete.iter().map(|x| x.0).collect::<Vec<_>>(), vec![o]);
    assert_eq!(rep.reachable + 1, rep.total);
    assert_eq!(r.store().list().unwrap(), before);
    assert!(r.store().contains(&o).unwrap());
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn recently_written_unreferenced_object_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    let o = orphan(&r, b"in-flight publish, manifest not written yet");
    let rep = r.gc_plan(14 * DAY).unwrap();
    assert!(rep.would_delete.is_empty());
    assert_eq!(rep.kept_young, vec![o]);
    // Unflushed (pending) objects are also never candidates.
    let p = r.store().put(&Object::Blob(b"pending".to_vec())).unwrap();
    let rep = r.gc_plan_at(later(), 14 * DAY).unwrap();
    assert!(!rep.would_delete.iter().any(|x| x.0 == p));
}

#[test]
fn history_snapshots_and_operation_log_keep_objects_alive() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "v1");
    r.snapshot_create("s1", "pin", sig()).unwrap();
    commit(&r, "a", "v2");
    // Branch moves back via undo: v2's commit is no longer a branch head, only in the op log.
    r.op_undo().unwrap();
    let rep = r.gc_plan_at(later(), Duration::ZERO).unwrap();
    assert!(rep.would_delete.is_empty(), "{:?}", rep.would_delete);
    assert_eq!(rep.missing_referenced, 0);
}

#[test]
fn staged_but_uncommitted_blob_is_a_root() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "v1");
    fs::write(r.worktree().join("b"), "staged only").unwrap();
    r.add(&["b".into()]).unwrap();
    let rep = r.gc_plan_at(later(), Duration::ZERO).unwrap();
    assert!(rep.would_delete.is_empty(), "{:?}", rep.would_delete);
}

#[test]
fn old_orphan_is_reported_with_grace_zero() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "v1");
    let b = orphan(&r, b"x");
    let rep = r.gc_plan_at(later(), Duration::ZERO).unwrap();
    assert!(rep.would_delete.iter().any(|x| x.0 == b));
}
