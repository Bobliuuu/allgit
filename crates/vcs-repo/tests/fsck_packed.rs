//! fsck on packed objects (the pack-scan path, every core): a clean repository is clean at any batch size, and
//! damage to a pack is reported identically at every batch size. Its own test binary: it clears `VCS_PACK_MIN`,
//! which `fsck_batches.rs` sets for its loose-object repository.
use std::fs;
use vcs_objects::Signature;
use vcs_repo::Repo;

fn sig(t: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: t,
    }
}
fn rnd(n: usize, mut x: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// The same checks on packed objects (the pack scan path, every core) as on loose ones: a clean repository is clean
/// at any batch size, and a damaged pack is reported the same way at every batch size.
#[test]
fn fsck_of_packed_objects_reports_damage() {
    std::env::remove_var("VCS_PACK_MIN");
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("a")).unwrap();
    fs::write(r.worktree().join("big.bin"), rnd(2 << 20, 9)).unwrap();
    for i in 0..40 {
        fs::write(r.worktree().join(format!("f{i}.txt")), format!("file {i}")).unwrap();
        r.add(&["".into()]).unwrap();
        r.commit(&format!("c{i}"), sig(2000 + i)).unwrap();
    }
    r.repack().unwrap();
    let report = |batch: &str| {
        std::env::set_var("VCS_FSCK_BATCH", batch);
        let rep = r.fsck().unwrap();
        let mut p = rep.problems.clone();
        p.sort();
        (rep.objects, p)
    };
    let (n1, p1) = report("2");
    let (n2, p2) = report("100000");
    assert!(p1.is_empty() && p2.is_empty(), "{p1:?} {p2:?}");
    assert_eq!(n1, n2);
    // flip one byte inside a pack: some problem must be reported (hash, framing or checksum)
    let pack_dir = r.worktree().join(".vcs/objects/pack");
    let pack = fs::read_dir(&pack_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .expect("a pack");
    let orig = fs::read(&pack).unwrap();
    let mut bad = orig.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0xff;
    fs::write(&pack, &bad).unwrap();
    let (_, q1) = report("2");
    let (_, q2) = report("100000");
    assert!(!q1.is_empty(), "a flipped byte in a pack went unnoticed");
    assert_eq!(q1, q2);
    fs::write(&pack, &orig).unwrap();
    std::env::remove_var("VCS_FSCK_BATCH");
}
