//! Tests of the sharded-manifest PROTOTYPE (`--features sharded`): crash at every protocol point,
//! all-or-nothing visibility, and a threaded stress with an atomicity invariant.
#![cfg(feature = "sharded")]

use std::collections::BTreeMap;
use vcs_objects::ObjectId;
use vcs_remote::shard::*;
use vcs_remote::*;

fn id(n: u64) -> ObjectId {
    ObjectId::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes())
}
fn up(zone: &str, old: Option<ObjectId>, new: ObjectId) -> ShardUpdate {
    ShardUpdate {
        zone: zone.into(),
        branch: "main".into(),
        old,
        new,
    }
}

#[test]
fn shard_codec_roundtrip_and_strictness() {
    let mut s = Shard {
        generation: 3,
        ..Default::default()
    };
    s.heads.insert("a".into(), id(1));
    s.heads.insert("b".into(), id(2));
    s.pending = Some((id(9), vec![("a".into(), Some(id(1)), id(5)), ("c".into(), None, id(6))]));
    let b = s.encode();
    assert_eq!(Shard::decode(&b).unwrap(), s);
    for n in 0..b.len() {
        assert!(Shard::decode(&b[..n]).is_err());
    }
    let mut t = b.clone();
    t.push(0);
    assert!(Shard::decode(&t).is_err());
}

#[test]
fn commit_diverge_and_snapshot() {
    let b = MemBackend::new();
    let mut p = ShardedPublisher::new(&b);
    assert_eq!(
        p.publish(id(100), &[up("a", None, id(1)), up("b", None, id(2))]).unwrap(),
        ShardOutcome::Committed
    );
    let snap = p.snapshot(&["a", "b", "c"]).unwrap();
    assert_eq!(snap.get(&("a".into(), "main".into())), Some(&id(1)));
    assert_eq!(snap.get(&("b".into(), "main".into())), Some(&id(2)));
    // stale precondition on b aborts the whole transaction: a is untouched
    match p.publish(id(101), &[up("a", Some(id(1)), id(3)), up("b", None, id(4))]) {
        Err(ShardError::Diverged { zone, .. }) => assert_eq!(zone, "b"),
        o => panic!("{o:?}"),
    }
    let snap = p.snapshot(&["a", "b"]).unwrap();
    assert_eq!(snap.get(&("a".into(), "main".into())), Some(&id(1)), "all-or-nothing");
    // re-running a committed txid is a no-op commit
    assert_eq!(
        p.publish(id(100), &[up("a", None, id(1)), up("b", None, id(2))]).unwrap(),
        ShardOutcome::Committed
    );
}

/// A writer that dies at any point is recovered by (a) another writer touching the same zones and
/// (b) re-running its txid; readers never see half of the transaction.
#[test]
fn crash_at_every_point_recovers_all_or_nothing() {
    let points = [
        Some(CrashAt::AfterPrepare(0)),
        Some(CrashAt::AfterPrepare(1)),
        Some(CrashAt::AfterDecide),
        Some(CrashAt::AfterFinalize(0)),
        Some(CrashAt::AfterFinalize(1)),
    ];
    for crash in points {
        for helper_first in [false, true] {
            let b = MemBackend::new();
            let ups = [up("a", None, id(1)), up("b", None, id(2))];
            let mut w = ShardedPublisher::new(&b);
            w.crash_at = crash;
            assert_eq!(w.publish(id(10), &ups).unwrap(), ShardOutcome::Crashed);
            let committed_by_crash = !matches!(crash, Some(CrashAt::AfterPrepare(_)));
            let r = ShardedPublisher::new(&b);
            let snap = r.snapshot(&["a", "b"]).unwrap();
            let seen = (
                snap.get(&("a".into(), "main".into())).copied(),
                snap.get(&("b".into(), "main".into())).copied(),
            );
            if committed_by_crash {
                assert_eq!(seen, (Some(id(1)), Some(id(2))), "{crash:?}: decided tx is visible in full");
            } else {
                assert_eq!(seen, (None, None), "{crash:?}: undecided tx is invisible");
            }
            let mut h = ShardedPublisher::new(&b);
            h.wait = std::time::Duration::from_millis(0); // the crashed writer's lease is considered expired
            if helper_first {
                // a different transaction on the same zones resolves the leftovers
                let cur = r.snapshot(&["a", "b"]).unwrap();
                let (oa, ob) = (
                    cur.get(&("a".into(), "main".into())).copied(),
                    cur.get(&("b".into(), "main".into())).copied(),
                );
                assert_eq!(
                    h.publish(id(11), &[up("a", oa, id(3)), up("b", ob, id(4))]).unwrap(),
                    ShardOutcome::Committed
                );
                let s = r.snapshot(&["a", "b"]).unwrap();
                assert_eq!(s.get(&("a".into(), "main".into())), Some(&id(3)));
                assert_eq!(s.get(&("b".into(), "main".into())), Some(&id(4)));
            } else {
                let out = h.publish(id(10), &ups).unwrap();
                assert_eq!(out, ShardOutcome::Committed, "re-running the txid finishes it");
                let s = r.snapshot(&["a", "b"]).unwrap();
                assert_eq!(s.get(&("a".into(), "main".into())), Some(&id(1)));
            }
            for z in ["a", "b"] {
                let (sh, _) = (Shard::decode(&b.get(&shard_key(z)).unwrap().unwrap().data).unwrap(), ());
                assert!(sh.pending.is_none(), "{crash:?} {helper_first}: no pending record left in {z}");
            }
        }
    }
}

/// 8 threads: pair transactions always write the SAME token to zones p0 and p1 (atomicity
/// invariant), single-zone transactions write other zones. Readers must never see p0 != p1.
#[test]
fn threaded_stress_keeps_pairs_atomic_and_loses_no_update() {
    let b = MemBackend::new();
    let token_of: std::sync::Mutex<BTreeMap<ObjectId, u64>> = Default::default();
    let committed = std::sync::atomic::AtomicU64::new(0);
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        let reader = sc.spawn(|| {
            let r = ShardedPublisher::new(&b);
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let s = r.snapshot(&["p0", "p1"]).unwrap();
                let (a, c) = (s.get(&("p0".into(), "main".into())), s.get(&("p1".into(), "main".into())));
                assert_eq!(a, c, "torn read of an atomic pair");
                n += 1;
            }
            n
        });
        let ws: Vec<_> = (0..6u64)
            .map(|w| {
                let (b, token_of, committed) = (&b, &token_of, &committed);
                sc.spawn(move || {
                    let mut p = ShardedPublisher::new(b);
                    p.wait = std::time::Duration::from_millis(5);
                    for i in 0..25u64 {
                        let token = w * 1000 + i + 1;
                        let new = id(token);
                        token_of.lock().unwrap().insert(new, token);
                        loop {
                            let zones: &[&str] = if w % 3 == 0 {
                                &["p0", "p1"]
                            } else if w % 3 == 1 {
                                &["p1", "p0"]
                            } else {
                                &["s0"]
                            };
                            let snap = p.snapshot(zones).unwrap();
                            let ups: Vec<ShardUpdate> = zones
                                .iter()
                                .map(|z| up(z, snap.get(&((*z).to_owned(), "main".into())).copied(), new))
                                .collect();
                            match p.publish_retrying(&id(token * 7919), &ups, 50) {
                                Ok(_) => {
                                    committed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    break;
                                }
                                Err(ShardError::Diverged { .. }) => continue, // someone moved a head: re-read, retry
                                Err(e) => panic!("{e}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for w in ws {
            w.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
    });
    assert_eq!(
        committed.load(std::sync::atomic::Ordering::Relaxed),
        150,
        "every transaction eventually committed exactly once"
    );
    let r = ShardedPublisher::new(&b);
    let s = r.snapshot(&["p0", "p1", "s0"]).unwrap();
    assert_eq!(s.get(&("p0".into(), "main".into())), s.get(&("p1".into(), "main".into())));
    for z in ["p0", "p1", "s0"] {
        assert!(Shard::decode(&b.get(&shard_key(z)).unwrap().unwrap().data)
            .unwrap()
            .pending
            .is_none());
    }
}
