//! Backup, restore, verify and the GC safe point (issue #570, docs/backup-restore.md), against the
//! directory backend, the in-memory GC backend and the sharded manifest, including a backup that dies
//! at every possible write.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::dr::{read_safe_point, Hold};
use vcs_remote::gc::RemoteGcOptions;
use vcs_remote::sharded::{migrate_to_sharded, ShardedBackend};
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

const SEC: u64 = 1000;
const HOUR: u64 = 3600 * SEC;
const DAY: u64 = 24 * HOUR;

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, n: u32) -> ObjectId {
    let blob = s.put_blob(format!("{zone} content {n}").as_bytes()).unwrap();
    let root = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    let mut cid = [0u8; 16];
    cid[..4].copy_from_slice(&n.to_le_bytes());
    cid[4] = zone.bytes().fold(0, |a, b| a.wrapping_add(b));
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes(cid),
        author: sig(),
        message: format!("{zone}{n}"),
    }))
    .unwrap()
}

fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

/// `zones` zones with `n` commits each, one publish per commit (so several packs).
fn populate(b: &dyn Backend, zones: usize, n: u32) -> (MemoryStore, Vec<ObjectId>) {
    let s = MemoryStore::new();
    let mut heads = vec![];
    for z in 0..zones {
        let zn = format!("z{z}");
        let mut prev = None;
        for i in 1..=n {
            let c = commit(&s, &zn, prev, i);
            Publisher::new(b, &s, tx(&zn, prev, c)).run().unwrap();
            prev = Some(c);
        }
        heads.push(prev.unwrap());
    }
    (s, heads)
}

fn heads(b: &dyn Backend) -> Vec<ObjectId> {
    load_manifest(b).unwrap().0.branches.values().copied().collect()
}

fn fs() -> (tempfile::TempDir, FsBackend) {
    let d = tempfile::tempdir().unwrap();
    let b = FsBackend::open(d.path()).unwrap();
    (d, b)
}

#[test]
fn verify_passes_on_a_healthy_remote_and_counts_what_it_walked() {
    let (_d, b) = fs();
    let (_s, _) = populate(&b, 2, 3);
    let r = verify_remote(&b, &VerifyOptions::default()).unwrap();
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(r.branches, 2);
    // 6 commits, 6 trees, 6 blobs
    assert_eq!(r.objects_walked, 18);
    let r = verify_remote(
        &b,
        &VerifyOptions {
            deep: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(r.is_ok());
    assert!(r.deep_objects >= 18, "{r:?}");
}

#[test]
fn verify_reports_missing_and_corrupt_objects_without_stopping_at_the_first() {
    let (d, b) = fs();
    populate(&b, 2, 2);
    // lose one pack (missing) and flip a byte in another (corrupt)
    let packs: Vec<String> = b.list("packs/").unwrap().into_iter().filter(|k| k.ends_with(".pack")).collect();
    assert!(packs.len() >= 4, "{packs:?}");
    std::fs::remove_file(d.path().join(&packs[0])).unwrap();
    let victim = d.path().join(&packs[1]);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes[40] ^= 0xff;
    std::fs::write(&victim, bytes).unwrap();
    let r = verify_remote(
        &b,
        &VerifyOptions {
            deep: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!r.is_ok());
    assert!(r.missing_total + r.corrupt_total >= 2, "{r:?}");
    assert!(r.corrupt_total >= 1, "the flipped pack must be reported: {r:?}");
}

#[test]
fn verify_of_an_empty_store_is_not_ok() {
    let (_d, b) = fs();
    let r = verify_remote(&b, &VerifyOptions::default()).unwrap();
    assert!(!r.manifest_present && !r.is_ok());
}

#[test]
fn backup_then_restore_into_a_fresh_store_gives_the_same_heads() {
    let (_d1, src) = fs();
    let (store, h) = populate(&src, 3, 4);
    let (_d2, bak) = fs();
    let rep = backup_copy(&src, &bak, &CopyOptions::default()).unwrap();
    assert!(rep.copied > 0 && rep.verify.is_ok() && rep.held);
    assert_eq!(heads(&bak), heads(&src));
    // the hold is gone and a second backup moves nothing
    let again = backup_copy(&src, &bak, &CopyOptions::default()).unwrap();
    assert_eq!(again.copied, 0, "{again:?}");
    // restore into a fresh store and read it with the ordinary fetch path
    let (_d3, fresh) = fs();
    backup_copy(
        &bak,
        &fresh,
        &CopyOptions {
            require_empty_dest: true,
            ..Default::default()
        },
    )
    .unwrap();
    let local = MemoryStore::new();
    fetch_heads(&fresh, &local, None).unwrap();
    for id in &h {
        assert_eq!(local.get(id).unwrap().encode(), store.get(id).unwrap().encode());
    }
    assert!(verify_remote(
        &fresh,
        &VerifyOptions {
            deep: true,
            ..Default::default()
        }
    )
    .unwrap()
    .is_ok());
    // a second restore into the now non-empty store is refused
    let e = backup_copy(
        &bak,
        &fresh,
        &CopyOptions {
            require_empty_dest: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(e, DrError::NotEmpty(_)), "{e}");
}

#[test]
fn a_sharded_source_is_flattened_to_one_manifest_and_restores() {
    let d = tempfile::tempdir().unwrap();
    let raw = FsBackend::open(d.path()).unwrap();
    populate(&raw, 3, 2);
    migrate_to_sharded(&raw).unwrap();
    let sharded = ShardedBackend::new(Box::new(FsBackend::open(d.path()).unwrap()));
    let before = load_manifest(&sharded).unwrap().0;
    assert!(verify_remote(
        &sharded,
        &VerifyOptions {
            deep: true,
            ..Default::default()
        }
    )
    .unwrap()
    .is_ok());
    let (_d2, bak) = fs();
    backup_copy(&sharded, &bak, &CopyOptions::default()).unwrap();
    let after = load_manifest(&bak).unwrap().0;
    assert_eq!(after.branches, before.branches);
    assert!(!sharded::is_sharded(&bak).unwrap(), "the backup is a plain single manifest");
    // and a sharded destination works too (restore to a store that will be sharded)
    let d3 = tempfile::tempdir().unwrap();
    let dst = ShardedBackend::new(Box::new(FsBackend::open(d3.path()).unwrap()));
    backup_copy(
        &bak,
        &dst,
        &CopyOptions {
            require_empty_dest: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(load_manifest(&dst).unwrap().0.branches, before.branches);
}

// ---- a backend that dies after N writes ----

struct Dying {
    inner: Arc<dyn Backend>,
    budget: AtomicI64,
}
impl Dying {
    fn tick(&self) -> Result<(), BackendError> {
        if self.budget.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return Err(BackendError::Other("killed".into()));
        }
        Ok(())
    }
}
impl Backend for Dying {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.tick()?;
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.tick()?;
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(p)
    }
}

struct Plain(Arc<FsBackend>);
impl Backend for Plain {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.0.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.0.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.0.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.0.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.0.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.0.list(p)
    }
}

#[test]
fn a_backup_killed_at_every_write_never_leaves_a_manifest_without_its_data() {
    let (_ds, src) = fs();
    let (s, h) = populate(&src, 2, 2);
    // total writes of a complete backup
    let (_dc, count) = fs();
    let counter = Dying {
        inner: Arc::new(Plain(Arc::new(count))),
        budget: AtomicI64::new(i64::MAX),
    };
    backup_copy(&src, &counter, &CopyOptions::default()).unwrap();
    let total = i64::MAX - counter.budget.load(Ordering::SeqCst);
    assert!(total > 10, "{total}");

    // an earlier, complete backup (state 1); the source then moves on (state 2)
    let first = src.get("manifest").unwrap().unwrap().data;
    let _ = first;
    for kill_after in 0..total {
        // fresh destination: killed backup => no manifest => verify says so; re-run completes
        let (dd, dst) = fs();
        let dst = Arc::new(dst);
        let dying = Dying {
            inner: Arc::new(Plain(dst.clone())),
            budget: AtomicI64::new(kill_after),
        };
        let r = backup_copy(&src, &dying, &CopyOptions::default());
        if kill_after < total {
            assert!(r.is_err(), "kill after {kill_after} of {total} must fail");
        }
        let v = verify_remote(
            &*dst,
            &VerifyOptions {
                deep: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            !v.manifest_present,
            "no manifest may exist before the last write (kill {kill_after})"
        );
        let done = backup_copy(&src, &*dst, &CopyOptions::default()).unwrap();
        assert!(done.verify.is_ok());
        assert!(verify_remote(
            &*dst,
            &VerifyOptions {
                deep: true,
                ..Default::default()
            }
        )
        .unwrap()
        .is_ok());
        let local = MemoryStore::new();
        fetch_heads(&*dst, &local, None).unwrap();
        for id in &h {
            assert!(local.get(id).is_ok());
        }
        drop(dd);
    }

    // updating an existing backup: a kill at any write keeps the OLD backup fully verifiable
    let (_db, bak) = fs();
    let bak = Arc::new(bak);
    backup_copy(&src, &*bak, &CopyOptions::default()).unwrap();
    let old_heads = heads(&*bak);
    let c3 = commit(&s, "z0", Some(h[0]), 3);
    Publisher::new(&src, &s, tx("z0", Some(h[0]), c3)).run().unwrap();
    for kill_after in 0..8 {
        let dying = Dying {
            inner: Arc::new(Plain(bak.clone())),
            budget: AtomicI64::new(kill_after),
        };
        let _ = backup_copy(&src, &dying, &CopyOptions::default());
        let v = verify_remote(
            &*bak,
            &VerifyOptions {
                deep: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(v.is_ok(), "kill after {kill_after}: {v:?}");
        let now = heads(&*bak);
        assert!(now == old_heads || now.contains(&c3));
    }
    backup_copy(&src, &*bak, &CopyOptions::default()).unwrap();
    assert!(heads(&*bak).contains(&c3));
}

// ---- GC safe point ----

fn gc_opts() -> RemoteGcOptions {
    RemoteGcOptions {
        grace_ms: DAY,
        hold_ms: HOUR,
        dry_run: false,
        lock_ttl_ms: 6 * HOUR,
        owner: "sweeper".into(),
        allow_short_grace: false,
        rewrite_packs: true,
    }
}

#[test]
fn a_hold_blocks_gc_and_compaction_until_released_or_expired() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    populate(&b, 1, 2);
    b.advance_ms(10 * DAY);
    let h = Hold::acquire(&b, "backup test", HOUR).unwrap().expect("gc-capable backend");
    let e = remote_gc(&b, &gc_opts()).unwrap_err();
    assert!(matches!(e, GcError::BackupHold(_)), "{e}");
    let c = compact(
        &b,
        &CompactOptions {
            dry_run: false,
            ..Default::default()
        },
    );
    assert!(matches!(c, Err(GcError::BackupHold(_))), "{c:?}");
    // a dry run reports and is allowed
    assert!(remote_gc(
        &b,
        &RemoteGcOptions {
            dry_run: true,
            ..gc_opts()
        }
    )
    .is_ok());
    // the gc lock was released after the refusal
    assert!(b.get("gc/lock").unwrap().is_none());
    drop(h);
    assert!(remote_gc(&b, &gc_opts()).is_ok());

    // a hold that is never released (kill -9) stops mattering when it expires
    let h = Hold::acquire(&b, "dead backup", HOUR).unwrap().unwrap();
    std::mem::forget(h);
    assert!(remote_gc(&b, &gc_opts()).is_err());
    b.advance_ms(2 * HOUR);
    assert!(remote_gc(&b, &gc_opts()).is_ok());
    assert!(b.list("gc/hold/").unwrap().is_empty(), "the expired hold was cleared");
}

#[test]
fn a_backup_refuses_to_start_while_gc_runs_and_leaves_no_hold_behind() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    populate(&b, 1, 1);
    let dst = MemBackend::new();
    let r = gc::hold_lock_for_test(&b, "sweeper", HOUR, || backup_copy(&b, &dst, &CopyOptions::default())).unwrap();
    assert!(matches!(r, Err(DrError::GcRunning(_))), "{r:?}");
    assert!(b.list("gc/hold/").unwrap().is_empty());
    assert!(backup_copy(&b, &dst, &CopyOptions::default()).is_ok());
}

#[test]
fn a_destructive_gc_records_the_safe_point() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let s = MemoryStore::new();
    let a = commit(&s, "z", None, 1);
    Publisher::new(&b, &s, tx("z", None, a)).run().unwrap();
    assert!(read_safe_point(&b).unwrap().is_none());
    let x = commit(&s, "z", None, 2);
    Publisher::new(&b, &s, tx("z", Some(a), x)).with_applied_window(1).run().unwrap();
    let y = commit(&s, "z", None, 3);
    Publisher::new(&b, &s, tx("z", Some(x), y)).with_applied_window(1).run().unwrap(); // a left the retention window: garbage
    b.advance_ms(10 * DAY);
    let rep = remote_gc(&b, &gc_opts()).unwrap();
    assert!(!rep.deleted.is_empty());
    let sp = read_safe_point(&b).unwrap().expect("recorded");
    assert_eq!(sp.objects_deleted as usize, rep.deleted.len());
    assert!(sp.at_ms >= 1_000_000 + 10 * DAY);
}

#[test]
fn an_old_manifest_is_flagged_after_gc_but_the_live_remote_verifies() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let s = MemoryStore::new();
    let a = commit(&s, "z", None, 1);
    Publisher::new(&b, &s, tx("z", None, a)).run().unwrap();
    let old = Manifest::decode(&b.get("manifest").unwrap().unwrap().data).unwrap();
    let x = commit(&s, "z", None, 2);
    Publisher::new(&b, &s, tx("z", Some(a), x)).with_applied_window(1).run().unwrap();
    let y = commit(&s, "z", None, 3);
    Publisher::new(&b, &s, tx("z", Some(x), y)).with_applied_window(1).run().unwrap(); // a left the retention window
    b.advance_ms(10 * DAY);
    // before gc the old manifest is restorable
    assert!(verify_remote(
        &b,
        &VerifyOptions {
            manifest: Some(old.clone()),
            ..Default::default()
        }
    )
    .unwrap()
    .is_ok());
    remote_gc(&b, &gc_opts()).unwrap();
    assert!(verify_remote(&b, &VerifyOptions::default()).unwrap().is_ok());
    let r = verify_remote(
        &b,
        &VerifyOptions {
            manifest: Some(old),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!r.is_ok() && r.missing_total > 0, "{r:?}");
    assert!(r.safe_point.is_some());
}
