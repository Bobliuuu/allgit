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

#[test]
fn readopted_old_orphan_is_kept_after_dedup_put() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    let o = orphan(&r, b"old orphan");
    // Backdate it 30 days, as if it had been orphaned long ago.
    let p = d.path().join("r/.vcs/objects").join(&o.to_hex()[..2]).join(&o.to_hex()[2..]);
    let old = SystemTime::now() - 30 * DAY;
    fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
    let rep = r.gc_plan(Duration::from_secs(86400)).unwrap();
    assert_eq!(
        rep.would_delete.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![o],
        "precondition: it is sweepable"
    );

    // A writer re-adopts it: put dedups, a pointer to it is about to be installed.
    assert_eq!(r.store().put(&Object::Blob(b"old orphan".to_vec())).unwrap(), o);
    r.store().sync().unwrap();
    // Not yet referenced, but no longer old: even a plan that treats anything older than 1 day
    // (and one at grace zero evaluated at the moment of the put) must not select it.
    let rep = r.gc_plan(Duration::from_secs(86400)).unwrap();
    assert!(
        rep.would_delete.is_empty(),
        "re-adopted orphan must survive: {:?}",
        rep.would_delete
    );
    assert!(rep.kept_young.contains(&o));
    let rep = r.gc_plan_at(SystemTime::now() - Duration::from_secs(5), Duration::ZERO).unwrap();
    assert!(
        !rep.would_delete.iter().any(|x| x.0 == o),
        "grace-zero plan as of just before the put keeps it"
    );
}

// ---------------------------------------------------------------------------------------------
// Destructive sweep (`gc_sweep_at`). Everything below deletes for real.

use vcs_repo::{GcOptions, RebaseOutcome};

fn opts() -> GcOptions {
    GcOptions::new(14 * DAY)
}
fn fsck_ok(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
fn pack_files(d: &std::path::Path) -> Vec<String> {
    let pd = d.join("r/.vcs/objects/pack");
    let mut v: Vec<String> = fs::read_dir(pd)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn sweep_deletes_old_orphans_and_keeps_everything_reachable() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    commit(&r, "a", "two");
    let o1 = orphan(&r, b"garbage 1");
    let o2 = orphan(&r, b"garbage 2");
    let live = r.store().list().unwrap().len() - 2;
    let run = r.gc_sweep_at(later(), opts()).unwrap();
    let mut gone: Vec<_> = run.deleted.iter().map(|x| x.0).collect();
    gone.sort();
    let mut want = vec![o1, o2];
    want.sort();
    assert_eq!(gone, want);
    assert_eq!(run.sweep.loose_removed, 2);
    assert!(!r.store().contains(&o1).unwrap() && !r.store().contains(&o2).unwrap());
    assert_eq!(r.store().list().unwrap().len(), live);
    fsck_ok(&r);
    // history, status and the operation log still work
    assert_eq!(r.log("root", None, 10).unwrap().len(), 2);
    r.op_undo().unwrap();
    // idempotent
    assert!(r.gc_sweep_at(later(), opts()).unwrap().deleted.is_empty());
}

#[test]
fn sweep_keeps_young_objects_and_refuses_unsafe_grace() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    let young = orphan(&r, b"just written");
    let run = r.gc_sweep(opts()).unwrap();
    assert!(run.deleted.is_empty());
    assert!(r.store().contains(&young).unwrap());
    // a grace period at or below the touch-on-dedup threshold is refused
    let e = r.gc_sweep_at(later(), GcOptions::new(Duration::from_secs(3600))).unwrap_err();
    assert!(e.to_string().contains("touch-on-dedup"), "{e}");
    assert!(r.store().contains(&young).unwrap());
    // ... unless explicitly overridden
    let mut o = GcOptions::new(Duration::ZERO);
    o.allow_short_grace = true;
    assert_eq!(r.gc_sweep_at(later(), o).unwrap().deleted.len(), 1);
}

#[test]
fn sweep_rewrites_packs_without_garbage_and_drops_fully_dead_packs() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    let g1 = orphan(&r, b"dead in a mixed pack");
    assert!(r.repack().unwrap() > 0); // pack 1: live + g1
    let g2 = orphan(&r, b"dead alone 1");
    let g3 = orphan(&r, b"dead alone 2");
    assert_eq!(r.repack().unwrap(), 2); // pack 2: only garbage
    assert_eq!(pack_files(d.path()).len(), 4);
    let run = r.gc_sweep_at(later(), opts()).unwrap();
    assert_eq!(run.deleted.len(), 3);
    assert_eq!(
        (run.sweep.packs_rewritten, run.sweep.packs_removed, run.sweep.packed_dropped),
        (1, 1, 3)
    );
    for g in [g1, g2, g3] {
        assert!(!r.store().contains(&g).unwrap());
    }
    assert_eq!(pack_files(d.path()).len(), 2, "one pack + index remain");
    fsck_ok(&r);
    // survives a reopen
    let r2 = Repo::discover(&d.path().join("r")).unwrap();
    assert_eq!(r2.log("root", None, 10).unwrap().len(), 1);
    fsck_ok(&r2);
}

#[test]
fn sweep_cleans_orphan_tmp_files_and_indexless_packs_but_not_fresh_ones() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "one");
    r.repack().unwrap();
    let objs = d.path().join("r/.vcs/objects");
    let old_tmp = objs.join("tmp/crashed.1.0.tmp");
    let fresh_tmp = objs.join("tmp/inflight.2.0.tmp");
    let old_pack = objs.join("pack/pack-dead.pack");
    let fresh_pack = objs.join("pack/pack-fresh.pack");
    for p in [&old_tmp, &fresh_tmp, &old_pack, &fresh_pack] {
        fs::write(p, b"junk").unwrap();
    }
    let ago = SystemTime::now() - 30 * DAY;
    for p in [&old_tmp, &old_pack] {
        fs::File::options().write(true).open(p).unwrap().set_modified(ago).unwrap();
    }
    let run = r.gc_sweep(opts()).unwrap();
    assert_eq!((run.orphans.tmp_files, run.orphans.packs_without_index), (1, 1));
    assert!(!old_tmp.exists() && !old_pack.exists());
    assert!(fresh_tmp.exists() && fresh_pack.exists(), "fresh files may belong to a live writer");
    // the real pack (with its index) is untouched
    fsck_ok(&r);
}

#[test]
fn sweep_survives_a_crash_at_every_point_without_losing_live_data() {
    use vcs_store::SweepCrash;
    for point in [
        SweepCrash::AfterNewPack,
        SweepCrash::AfterOldIndexRemoved,
        SweepCrash::BeforeLooseRemoval,
    ] {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        commit(&r, "a", "one");
        commit(&r, "b", "two");
        let g = orphan(&r, b"dead");
        r.repack().unwrap();
        let g_loose = orphan(&r, b"dead loose");
        r.gc_set_sweep_crash(Some(point));
        assert!(r.gc_sweep_at(later(), opts()).is_err(), "{point:?}: crash must surface");
        // reopen as after a real crash: nothing live is missing, repository is usable
        let r2 = Repo::discover(&d.path().join("r")).unwrap();
        fsck_ok(&r2);
        assert_eq!(r2.log("root", None, 10).unwrap().len(), 2, "{point:?}");
        // a second, uninterrupted run finishes the job and cleans index-less packs
        let run = r2.gc_sweep_at(later() + 60 * DAY, opts()).unwrap();
        let _ = run;
        assert!(!r2.store().contains(&g).unwrap(), "{point:?}");
        assert!(!r2.store().contains(&g_loose).unwrap(), "{point:?}");
        fsck_ok(&r2);
        let leftovers = pack_files(d.path());
        assert_eq!(leftovers.len(), 2, "{point:?}: exactly one pack + index, got {leftovers:?}");
        assert_eq!(r2.log("root", None, 10).unwrap().len(), 2);
    }
}

#[test]
fn sweep_keeps_branch_snapshot_tag_stash_index_and_op_log_roots() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, "a", "v1");
    r.snapshot_create("s1", "pin", sig()).unwrap();
    commit(&r, "a", "v2");
    r.tag_create("root", "t1", None, Some(("annotated", sig(), b"body".to_vec())))
        .unwrap();
    commit(&r, "a", "v3");
    r.op_undo().unwrap(); // v3 is now only in the op log
    fs::write(r.worktree().join("s"), "staged").unwrap();
    r.add(&["s".into()]).unwrap();
    fs::write(r.worktree().join("a"), "dirty").unwrap();
    r.add(&["a".into()]).unwrap();
    r.stash_push(Some("wip"), sig()).unwrap();
    fs::write(r.worktree().join("b"), "staged too").unwrap();
    r.add(&["b".into()]).unwrap();
    let before = r.gc_plan_at(later(), opts().grace).unwrap();
    assert!(before.would_delete.is_empty(), "{:?}", before.would_delete);
    let all = r.store().list().unwrap();
    let run = r.gc_sweep_at(later(), opts()).unwrap();
    assert!(run.deleted.is_empty());
    assert_eq!(r.store().list().unwrap(), all);
    fsck_ok(&r);
    r.restore(&["b".into()], true).unwrap();
    fs::remove_file(r.worktree().join("b")).unwrap();
    r.stash_pop(0).unwrap();
    assert_eq!(fs::read_to_string(r.worktree().join("s")).unwrap(), "staged");
}

#[test]
fn sweep_keeps_objects_of_a_stopped_rebase_and_it_can_still_finish() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let base = "a\nb\nc\nd\ne\nf\ng\nh\n";
    fs::write(r.worktree().join("f"), base).unwrap();
    commit_all(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    fs::write(r.worktree().join("f"), "A\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    commit_all(&r, "feature one");
    fs::write(r.worktree().join("g"), "new\n").unwrap();
    commit_all(&r, "feature two");
    r.checkout("root", "main").unwrap();
    fs::write(r.worktree().join("f"), "Z\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    commit_all(&r, "main work");
    r.checkout("root", "feature").unwrap();
    assert!(matches!(r.rebase("root", "main", sig()).unwrap(), RebaseOutcome::Conflicted { .. }));
    let run = r.gc_sweep_at(later(), opts()).unwrap();
    assert!(
        run.deleted.is_empty(),
        "stopped rebase/merge state must pin everything: {:?}",
        run.deleted
    );
    fsck_ok(&r);
    r.rebase_abort().unwrap();
    fsck_ok(&r);
}

fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
