//! Automatic repack: threshold, grace period, lock, and interplay with reads, fsck and GC.
use std::fs;
use std::time::{Duration, SystemTime};
use vcs_objects::Signature;
use vcs_repo::{GcOptions, Repo};

fn sig() -> Signature {
    Signature { name: "T".into(), email: "t@x".into(), timestamp: 1 }
}

fn repo_with(n: usize) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    for i in 0..n {
        fs::write(r.worktree().join(format!("f{i}")), format!("content {i}")).unwrap();
    }
    let all: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
    r.add(&all).unwrap();
    r.commit("c", sig()).unwrap();
    (d, r)
}

fn loose(r: &Repo) -> usize {
    let mut n = 0;
    for e in fs::read_dir(r.worktree().join(".vcs/objects")).unwrap().flatten() {
        if e.file_name().len() == 2 {
            n += fs::read_dir(e.path()).unwrap().count();
        }
    }
    n
}

#[test]
fn below_threshold_or_disabled_does_nothing() {
    let (_d, r) = repo_with(50);
    let before = loose(&r);
    assert_eq!(r.auto_repack(6700, Duration::ZERO).unwrap(), None);
    assert_eq!(r.auto_repack(0, Duration::ZERO).unwrap(), None);
    assert_eq!(loose(&r), before);
}

#[test]
fn packs_only_old_loose_objects_and_keeps_everything_readable() {
    let (_d, r) = repo_with(600);
    let all = r.store().list().unwrap();
    // too young for the grace period: nothing moves
    let n = r.auto_repack_at(SystemTime::now(), 1, Duration::from_secs(3600), false).unwrap();
    assert_eq!(n, Some(0));
    assert_eq!(loose(&r), all.len());
    // old enough: all packed, loose files gone, all objects still there
    let later = SystemTime::now() + Duration::from_secs(7200);
    let n = r.auto_repack_at(later, 1, Duration::from_secs(3600), false).unwrap().unwrap();
    assert_eq!(n, all.len());
    assert_eq!(loose(&r), 0);
    assert_eq!(r.store().list().unwrap(), all);
    for id in &all {
        r.store().get(id).unwrap();
    }
    assert!(r.fsck().unwrap().problems.is_empty());
    // a fresh handle (reads packs from disk) sees the same, and real GC keeps live objects
    let r2 = Repo::discover(r.worktree()).unwrap();
    assert_eq!(r2.store().list().unwrap(), all);
    r2.gc_sweep_at(later + Duration::from_secs(60 * 86400), GcOptions::new(Duration::from_secs(14 * 86400))).unwrap();
    assert!(r2.fsck().unwrap().problems.is_empty());
    assert_eq!(r2.log("root", None, 10).unwrap().len(), 1);
}

#[test]
fn a_held_lock_skips_the_repack() {
    let (_d, r) = repo_with(10);
    let lock = r.worktree().join(".vcs/lock");
    fs::write(&lock, "pid 1\n").unwrap();
    let later = SystemTime::now() + Duration::from_secs(7200);
    assert_eq!(r.auto_repack_at(later, 0, Duration::ZERO, true).unwrap(), None);
    assert!(loose(&r) > 0);
    fs::remove_file(&lock).unwrap();
    assert!(r.auto_repack_at(later, 0, Duration::ZERO, true).unwrap().unwrap() > 0);
}
