//! Scale runs (issue #436 item 5), in-memory backend: LOCAL numbers, not S3.
//!
//! * `lock_service_with_thousands_of_holders`: N principals each hold one lock; every acquire is a
//!   publish of the one manifest. Prints the time per acquire at the start and at the end and the manifest
//!   size. `SCALE_LOCK_N` (default 2000).
//! * `remote_gc_with_concurrent_pushes_at_scale`: a history of C published commits (one pack each),
//!   A aborted publishes (old orphans), then remote GC while W writers keep publishing. Invariant G-REACH
//!   after the sweep and every acknowledged push present. `SCALE_GC_COMMITS` (default 1500), `SCALE_GC_ORPHANS`
//!   (300), `SCALE_GC_WRITERS` (4).
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use vcs_objects::*;
use vcs_remote::gc::{remote_gc_with, RemoteGcOptions};
use vcs_remote::protocol::GcQuirks;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn env(name: &str, d: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, n: u64) -> ObjectId {
    let blob = s.put_blob(format!("content {zone} {n}").as_bytes()).unwrap();
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
    cid[..8].copy_from_slice(&n.to_le_bytes());
    cid[8] = zone.len() as u8;
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes(cid),
        author: sig(),
        message: format!("{zone} {n}"),
    }))
    .unwrap()
}

fn tx(zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId, lock_ops: Vec<LockOp>) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops,
    }
}

#[test]
fn lock_service_with_thousands_of_holders() {
    let n = env("SCALE_LOCK_N", 2000);
    let b = MemBackend::new();
    let s = MemoryStore::new();
    let c = commit(&s, "z", None, 0);
    Publisher::new(&b, &s, tx("z", "main", None, c, vec![])).run().unwrap();
    let mut first = std::time::Duration::ZERO;
    let mut last = std::time::Duration::ZERO;
    let t0 = Instant::now();
    for i in 0..n {
        let t = Transaction {
            updates: vec![],
            author: sig(),
            message: "lock".into(),
            lock_ops: vec![LockOp::Acquire {
                zone: "z".into(),
                path: format!("assets/f{i:06}.bin"),
                lease_ms: 3_600_000,
                reason: String::new(),
            }],
        };
        let t1 = Instant::now();
        Publisher::new(&b, &s, t)
            .with_principal(format!("user{i}"))
            .with_clock(|| 1000)
            .run()
            .unwrap();
        if i < 50 {
            first += t1.elapsed();
        }
        if i >= n.saturating_sub(50) {
            last += t1.elapsed();
        }
    }
    let total = t0.elapsed();
    let (m, _) = load_manifest(&b).unwrap();
    assert_eq!(m.locks.len(), n, "every holder holds its lock");
    for i in [0, n / 2, n - 1] {
        assert_eq!(m.locks[&("z".into(), format!("assets/f{i:06}.bin"))].holder, format!("user{i}"));
    }
    let bytes = b.get("manifest").unwrap().unwrap().data.len();
    let k = n.clamp(1, 50) as u32;
    eprintln!(
        "LOCKS n={n} total {:.2}s ({:.1} acquires/s); per acquire first {:?} last {:?}; manifest {bytes} bytes ({} per holder) [in-memory backend, local]",
        total.as_secs_f64(),
        n as f64 / total.as_secs_f64(),
        first / k,
        last / k,
        bytes / n
    );
}

fn deps_of(o: &Object) -> Vec<ObjectId> {
    match o {
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        _ => vec![],
    }
}

fn g_reach(b: &dyn Backend) -> Result<usize, String> {
    let (m, _) = load_manifest(b).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjectId> = m.branches.values().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = fetch_object(b, &id).map_err(|_| format!("G-REACH: a head needs {} which is gone", id.short()))?;
        stack.extend(deps_of(&o));
    }
    Ok(seen.len())
}

#[test]
fn remote_gc_with_concurrent_pushes_at_scale() {
    const DAY: u64 = 24 * 3600 * 1000;
    let commits = env("SCALE_GC_COMMITS", 1500);
    let orphans = env("SCALE_GC_ORPHANS", 300);
    let writers = env("SCALE_GC_WRITERS", 4);
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let s = MemoryStore::new();
    let t0 = Instant::now();
    let mut head = None;
    for i in 0..commits {
        let c = commit(&s, "z", head, i as u64);
        Publisher::new(&b, &s, tx("z", "main", head, c, vec![])).run().unwrap();
        head = Some(c);
    }
    let build = t0.elapsed();
    // aborted publishes: a pack and index are uploaded, the manifest never moves
    for i in 0..orphans {
        let c = commit(&s, "z", None, 1_000_000 + i as u64);
        let mut p = Publisher::new(&b, &s, tx("z", &format!("aborted{i}"), None, c, vec![]));
        for _ in 0..3 {
            p.step().unwrap();
        }
    }
    if std::env::var("SCALE_GC_PRECHECK").is_ok() {
        g_reach(&b).expect("G-REACH before the sweep");
    }
    let keys_before = b.keys().len();
    b.advance_ms(10 * DAY);
    let opts = RemoteGcOptions {
        grace_ms: DAY,
        hold_ms: 3_600_000,
        dry_run: false,
        lock_ttl_ms: 6 * 3_600_000,
        owner: "sweeper".into(),
        allow_short_grace: false,
        rewrite_packs: true,
    };
    let pushed = AtomicU64::new(0);
    let per_writer = env("SCALE_GC_PUSHES", 40);
    let t1 = Instant::now();
    let report = std::thread::scope(|sc| {
        let mut hs = vec![];
        for w in 0..writers {
            let b = &b;
            let pushed = &pushed;
            hs.push(sc.spawn(move || {
                let s = MemoryStore::new();
                let mut head = None;
                for i in 0..per_writer {
                    let c = commit(&s, &format!("w{w}"), head, i as u64);
                    match Publisher::new(b, &s, tx(&format!("w{w}"), "main", head, c, vec![])).run() {
                        Ok(_) => {
                            pushed.fetch_add(1, Ordering::SeqCst);
                            head = Some(c);
                        }
                        Err(e) => panic!("push during gc failed: {e:?}"),
                    }
                }
            }));
        }
        let r = remote_gc_with(&b, &opts, GcQuirks::default(), &mut |_| {}).unwrap();
        for h in hs {
            h.join().unwrap();
        }
        r
    });
    let gc_wall = t1.elapsed();
    let reach = g_reach(&b).expect("G-REACH");
    assert_eq!(pushed.load(Ordering::SeqCst) as usize, writers * per_writer);
    let (m, _) = load_manifest(&b).unwrap();
    assert_eq!(m.branches.len(), 1 + writers, "every writer's branch landed");
    assert!(
        report.deleted_packs.len() + report.deleted.len() > 0 || orphans == 0,
        "orphans were swept"
    );
    eprintln!(
        "GC commits={commits} orphans={orphans} writers={writers}x{per_writer}: build {build:.2?}; keys before {keys_before}; gc+pushes {gc_wall:.2?}; objects {} packs {} reachable {} deleted objects {} packs {}; closure after {reach} [in-memory backend, local]",
        report.objects,
        report.packs,
        report.reachable,
        report.deleted.len(),
        report.deleted_packs.len()
    );
}
