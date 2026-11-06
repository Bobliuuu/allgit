//! The reachability cache (`.vcs/gc-reach`) must never change what gc finds: plans and sweeps with it
//! equal the full mark, over histories that grow, lose branches and gain orphans, and a damaged cache is ignored.
use std::collections::BTreeSet;
use std::fs;
use std::time::{Duration, SystemTime};
use vcs_objects::{Object, ObjectId, Signature};
use vcs_repo::{GcOptions, Repo, ROOT_ZONE};

const DAY: Duration = Duration::from_secs(86400);
fn later() -> SystemTime {
    SystemTime::now() + 30 * DAY
}
fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}
fn summary(r: &Repo) -> (usize, usize, BTreeSet<ObjectId>, usize) {
    let p = r.gc_plan_at(later(), 14 * DAY).unwrap();
    (
        p.total,
        p.reachable,
        p.would_delete.iter().map(|x| x.0).collect(),
        p.missing_referenced,
    )
}

#[test]
fn cached_mark_equals_full_mark_across_rounds() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let mut off = Repo::discover(r.worktree()).unwrap();
    off.set_reach_cache(false);
    let mut rng = Rng(3);
    let mut ts = 1000;
    let cache = r.worktree().join(".vcs/gc-reach");
    for round in 0..8u64 {
        for i in 0..6 {
            let p = format!("d{}/f{}.txt", rng.next(3), rng.next(4));
            fs::create_dir_all(r.worktree().join(&p).parent().unwrap()).unwrap();
            fs::write(r.worktree().join(&p), format!("{round} {i} {}", rng.next(1000))).unwrap();
            r.add(&[p]).unwrap();
            ts += 10;
            r.commit("c", sig(ts)).unwrap();
        }
        let b = format!("b{round}");
        r.create_branch(ROOT_ZONE, &b).unwrap();
        if round % 3 == 2 {
            // dropping a branch removes a root: the cache must step aside, not over-retain
            let _ = r.branch_delete(ROOT_ZONE, &format!("b{}", round - 1), true);
        }
        for k in 0..3 {
            let o = r.store().put(&Object::Blob(format!("orphan {round} {k}").into_bytes())).unwrap();
            let _ = o;
        }
        r.store().sync().unwrap();
        assert_eq!(summary(&r), summary(&off), "plan, round {round}");
        let run = r.gc_sweep_at(later(), GcOptions::new(14 * DAY)).unwrap();
        assert!(cache.exists());
        let after = summary(&off);
        assert!(after.2.is_empty(), "sweep left deletable objects: {after:?} {run:?}");
        assert_eq!(summary(&r), after);
        assert!(r.fsck().unwrap().problems.is_empty());
        // damaged caches are ignored
        let good = fs::read(&cache).unwrap();
        for bad in [b"junk".to_vec(), good[..good.len() / 2].to_vec(), {
            let mut b = good.clone();
            let n = b.len() / 2;
            b[n] ^= 1;
            b
        }] {
            fs::write(&cache, bad).unwrap();
            assert_eq!(summary(&r), after);
            assert!(
                r.fsck().unwrap().problems.iter().any(|p| p.starts_with("gc-reach")),
                "fsck must flag a damaged gc-reach"
            );
        }
        fs::write(&cache, good).unwrap();
    }
}
