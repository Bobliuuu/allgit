//! Repositories written before the large-file layout keep working: v3 small-chunk manifests stay readable,
//! an unchanged file keeps its id, and the `features` file gates readers that do not know a feature.
//! One test only: it flips a process-wide environment variable.
use std::fs;
use vcs_objects::{content_id, Object, ObjectId, Signature};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
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
fn commit_all(r: &Repo) {
    r.add(&["".into()]).unwrap();
    r.commit("m", sig()).unwrap();
}
fn manifest_of(r: &Repo) -> (ObjectId, vcs_objects::ChunkedBlob) {
    let head = r.refs().unwrap().head("root", "main").unwrap();
    let Object::Commit(c) = r.store().get(&head).unwrap() else {
        panic!()
    };
    let Object::Tree(t) = r.store().get(&c.root).unwrap() else {
        panic!()
    };
    let e = t.entries().iter().find(|e| e.name == "big.bin").unwrap();
    let Object::Chunked(m) = r.store().get(&e.id).unwrap() else {
        panic!()
    };
    (e.id, m)
}

#[test]
fn v3_manifests_stay_readable_and_unchanged_files_keep_their_id() {
    let d = tempfile::tempdir().unwrap();
    let data = rnd(20 << 20, 11);
    let r = Repo::init(&d.path().join("a")).unwrap();
    let big = r.worktree().join("big.bin");
    // write as an older release would have: small-chunk manifest, no features file
    std::env::set_var("VCS_CHUNK_LAYOUT", "v3");
    fs::write(&big, &data).unwrap();
    commit_all(&r);
    std::env::remove_var("VCS_CHUNK_LAYOUT");
    let c_old = r.head_commit("root").unwrap().unwrap();
    let (old_id, old_m) = manifest_of(&r);
    assert!(!old_m.large && old_m.chunks.len() > 500);
    assert_ne!(old_id, content_id(&data), "the same bytes have a different canonical id now");
    assert!(!r.worktree().join(".vcs/features").exists());
    // status is clean, also after a touch; re-adding the unchanged file keeps the old id (no phantom change)
    let clean = |r: &Repo| {
        r.status()
            .unwrap()
            .zones
            .iter()
            .all(|z| z.staged.is_empty() && z.unstaged.is_empty())
    };
    assert!(clean(&r));
    let f = fs::OpenOptions::new().write(true).open(&big).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(9))
        .unwrap();
    drop(f);
    assert!(clean(&r));
    r.add(&["big.bin".into()]).unwrap();
    assert!(clean(&r), "re-adding an unchanged v3 file must not change its id");
    assert!(!r.worktree().join(".vcs/features").exists(), "nothing large was written");
    // a change is stored in the new layout and records the feature
    let mut v2 = data.clone();
    v2[5 << 20] ^= 1;
    fs::write(&big, &v2).unwrap();
    commit_all(&r);
    assert!(manifest_of(&r).1.large);
    assert!(r.worktree().join(".vcs/features").is_file());
    // the old commit still checks out byte-identically through the streaming path, and fsck is clean
    r.checkout_commit("root", "main", &c_old).unwrap();
    assert_eq!(fs::read(&big).unwrap(), data);
    assert!(r.fsck().unwrap().problems.is_empty());
    // a repository that names a feature this version does not know is refused with a clear message
    drop(r);
    fs::write(d.path().join("a/.vcs/features"), "large-chunked\nfuture-thing\n").unwrap();
    let e = Repo::discover(&d.path().join("a")).err().expect("must refuse").to_string();
    assert!(e.contains("future-thing"), "{e}");
    fs::write(d.path().join("a/.vcs/features"), "large-chunked\n").unwrap();
    Repo::discover(&d.path().join("a")).unwrap();
}
