//! Lazy blob fetch: a clone/fetch brings commits and trees; file blobs come on first use,
//! in one batch for a checkout. Fault injection via MemBackend / FsBackend.

use std::fs;
use vcs_objects::{Object, ObjectId, Signature};
use vcs_remote::{Backend, CountingBackend, FsBackend, MemBackend};
use vcs_repo::Repo;
use vcs_store::MemoryStore;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn blob_id(s: &str) -> ObjectId {
    Object::Blob(s.as_bytes().to_vec()).id()
}

/// 2000 incompressible letters, deterministic per (file, version): packs compress with zstd, and the
/// byte-count tests below assume history blobs are as big as they look.
fn pad(i: usize, v: &str) -> String {
    let mut x =
        0x9E37_79B9_7F4A_7C15u64 ^ (i as u64 + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ v.bytes().fold(7u64, |a, b| a * 31 + b as u64);
    (0..2000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}

/// Origin with zone `engine`: 20 files at v1, then all rewritten at v2 (so v1 blobs are history).
fn seeded(remote: &dyn Backend, dir: &std::path::Path) -> Repo {
    let a = Repo::init(&dir.join("a")).unwrap();
    a.create_zone("engine", "engine").unwrap();
    for v in ["v1", "v2"] {
        for i in 0..20 {
            write(&a, &format!("engine/f{i}.rs"), &format!("file {i} {v} {}", pad(i, v)));
        }
        commit_all(&a, v);
    }
    a.publish(remote, None, sig()).unwrap();
    a
}
fn old_blob(i: usize) -> ObjectId {
    blob_id(&format!("file {i} v1 {}", pad(i, "v1")))
}
fn new_blob(i: usize) -> ObjectId {
    blob_id(&format!("file {i} v2 {}", pad(i, "v2")))
}

#[test]
fn clone_fetches_head_files_but_not_history_blobs() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    assert!(b.store().contains(&new_blob(3)).unwrap());
    assert!(!b.store().contains(&old_blob(3)).unwrap(), "history blob must stay remote");
    assert_eq!(
        fs::read_to_string(b.worktree().join("engine/f3.rs")).unwrap(),
        format!("file 3 v2 {}", pad(3, "v2"))
    );
    let rep = b.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    assert_eq!(rep.evictable.len(), 20, "20 history blobs are refetchable cache misses");
    // history (commits) is complete
    assert_eq!(b.log("engine", None, 10).unwrap().len(), 2);
}

#[test]
fn missing_history_blob_is_refetched_on_first_use() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let id = old_blob(5);
    assert!(b.store().get(&id).is_err());
    let got = b.with_lazy_fetch(&remote, || Ok(b.store().get(&id)?)).expect("refetched on demand");
    assert_eq!(got.id(), id);
    assert!(b.store().contains(&id).unwrap());
}

#[test]
fn tampered_history_blob_is_rejected_and_not_stored() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let id = old_blob(7);
    remote.tamper_object(&id, Object::Blob(b"evil".to_vec()).encode());
    let err = b.with_lazy_fetch(&remote, || Ok(b.store().get(&id)?)).unwrap_err().to_string();
    assert!(err.contains("hash verification"), "{err}");
    assert!(!b.store().contains(&id).unwrap());
}

#[test]
fn tampered_head_blob_fails_the_batch_prefetch_and_the_clone() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    remote.tamper_object(&new_blob(2), Object::Blob(b"evil".to_vec()).encode());
    let err = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()]))
        .err()
        .expect("clone must fail")
        .to_string();
    assert!(err.contains("hash verification"), "{err}");
}

#[test]
fn lazy_clone_downloads_fewer_bytes_than_eager_fetch() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    seeded(&remote, d.path());
    // eager baseline: whole closure including every history blob
    let eager = CountingBackend::new(&remote);
    vcs_remote::fetch_heads(&eager, &MemoryStore::new(), Some(&["engine".to_string()])).unwrap();
    let lazy = CountingBackend::new(&remote);
    Repo::clone_from(&lazy, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    let (e, l) = (eager.stats(), lazy.stats());
    assert!(
        l.bytes_down < e.bytes_down,
        "lazy {l} should download less than eager {e} (skips 20 history blobs of ~2KB)"
    );
    assert!(l.requests > 0 && l.bytes_down > 0);
}

/// Pack every given object into one remote pack, then compare per-object and batched fetches.
#[test]
fn batch_fetch_from_a_remote_pack_uses_far_fewer_requests() {
    let d = tempfile::tempdir().unwrap();
    let objs: Vec<(ObjectId, Vec<u8>)> = (0..40)
        .map(|i| {
            let enc = Object::Blob(format!("blob {i} {}", "y".repeat(500)).into_bytes()).encode();
            (vcs_objects::id_of_encoded(&enc), enc)
        })
        .collect();
    let pack = vcs_store::pack::write_pack(&d.path().join("pack"), d.path(), objs.clone()).unwrap();
    let mem = MemBackend::new();
    vcs_remote::packfetch::upload_pack(&mem, "one", &pack.pack_path).unwrap();
    let ids: Vec<ObjectId> = objs.iter().map(|o| o.0).collect();

    let single = CountingBackend::new(&mem);
    for id in &ids {
        vcs_remote::fetch_object(&single, id).unwrap();
    }
    let batch = CountingBackend::new(&mem);
    let got = vcs_remote::fetch_objects(&batch, &ids).unwrap();
    assert_eq!(got.len(), 40);
    let (s, b) = (single.stats(), batch.stats());
    // per object: loose probe + list + idx + range = 4; batch: list + idx + coalesced ranges
    assert!(b.requests * 10 <= s.requests, "batch {b} vs single {s}");
    assert!(b.requests <= 4, "{b}");
}

#[test]
fn batch_fetch_rejects_a_tampered_pack() {
    let d = tempfile::tempdir().unwrap();
    let objs: Vec<(ObjectId, Vec<u8>)> = (0..5)
        .map(|i| {
            let enc = Object::Blob(vec![i as u8; 300]).encode();
            (vcs_objects::id_of_encoded(&enc), enc)
        })
        .collect();
    let pack = vcs_store::pack::write_pack(&d.path().join("pack"), d.path(), objs.clone()).unwrap();
    let mem = MemBackend::new();
    vcs_remote::packfetch::upload_pack(&mem, "one", &pack.pack_path).unwrap();
    let mut bad = fs::read(&pack.pack_path).unwrap();
    for x in bad.iter_mut().skip(12) {
        *x ^= 0xFF;
    }
    mem.tamper("packs/one.pack", bad);
    let ids: Vec<ObjectId> = objs.iter().map(|o| o.0).collect();
    assert!(vcs_remote::fetch_objects(&mem, &ids).is_err());
}

#[test]
fn batch_fetch_missing_object_is_an_error() {
    let mem = MemBackend::new();
    assert!(vcs_remote::fetch_objects(&mem, &[ObjectId::from_bytes([7; 32])]).is_err());
}

#[test]
fn fs_backend_lazy_clone_publish_and_sync_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let remote = FsBackend::open(d.path().join("remote")).unwrap();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "fs", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    assert!(!b.store().contains(&old_blob(1)).unwrap());
    // publish from the lazy clone must not need history blobs
    write(&b, "engine/f1.rs", "changed in b");
    commit_all(&b, "b change");
    b.publish(&remote, Some(&["engine".to_string()]), sig()).unwrap();
    // origin fast-forwards; sync prefetches the new head's missing files in one batch
    a.sync(&remote).unwrap();
    assert_eq!(fs::read_to_string(a.worktree().join("engine/f1.rs")).unwrap(), "changed in b");
    // and a lazy fetch in b after a's later commit
    write(&a, "engine/f2.rs", "changed in a");
    commit_all(&a, "a change");
    a.publish(&remote, Some(&["engine".to_string()]), sig()).unwrap();
    let counting = CountingBackend::new(&remote);
    b.sync(&counting).unwrap();
    assert_eq!(fs::read_to_string(b.worktree().join("engine/f2.rs")).unwrap(), "changed in a");
    let rep = b.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn prefetch_subscribed_fetches_new_head_blobs_once_and_a_failing_backend_stores_nothing_bad() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    // a new head on the remote: fetch stores commits and trees, blobs stay remote
    for i in 0..20 {
        write(&a, &format!("engine/f{i}.rs"), &format!("file {i} v3 {}", pad(i, "v3")));
    }
    commit_all(&a, "v3");
    a.publish(&remote, None, sig()).unwrap();
    b.fetch(&remote, None).unwrap();
    let v3 = |i: usize| blob_id(&format!("file {i} v3 {}", pad(i, "v3")));
    assert!(!b.store().contains(&v3(5)).unwrap());
    // a different zone name fetches nothing
    assert_eq!(b.prefetch_subscribed(&remote, Some(&["nope".to_string()])).unwrap(), 0);
    assert_eq!(b.prefetch_subscribed(&remote, None).unwrap(), 20);
    assert!(b.store().contains(&v3(5)).unwrap());
    // idempotent: nothing left to fetch
    assert_eq!(b.prefetch_subscribed(&remote, None).unwrap(), 0);
    let rep = b.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

#[test]
fn prefetch_subscribed_rejects_a_tampered_blob_and_does_not_store_it() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = seeded(&remote, d.path());
    let b = Repo::clone_from(&remote, "mem", &d.path().join("b"), Some(&["engine".to_string()])).unwrap();
    write(&a, "engine/f0.rs", "brand new");
    commit_all(&a, "v3");
    a.publish(&remote, None, sig()).unwrap();
    b.fetch(&remote, None).unwrap();
    let id = blob_id("brand new");
    remote.tamper_object(&id, Object::Blob(b"evil".to_vec()).encode());
    let err = b.prefetch_subscribed(&remote, None).err().expect("must fail").to_string();
    assert!(err.contains("hash verification"), "{err}");
    assert!(!b.store().contains(&id).unwrap(), "tampered blob must not be stored");
    // the worktree was never touched
    assert_eq!(
        fs::read_to_string(b.worktree().join("engine/f0.rs")).unwrap(),
        format!("file 0 v2 {}", pad(0, "v2"))
    );
}
