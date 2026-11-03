use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

/// Chain of `n` commits in `zone`, each with its own blob of `size` bytes. Returns head.
fn chain(s: &MemoryStore, zone: &str, n: usize, size: usize) -> ObjectId {
    let mut parent = None;
    for i in 0..n {
        let blob = s.put_blob(&vec![(i % 251) as u8 ^ zone.len() as u8; size + i]).unwrap();
        let t = s
            .put(&Object::Tree(
                Tree::new(vec![TreeEntry {
                    name: "f".into(),
                    mode: EntryMode::File,
                    id: blob,
                }])
                .unwrap(),
            ))
            .unwrap();
        parent = Some(
            s.put(&Object::Commit(Commit {
                zone: zone.into(),
                parents: parent.into_iter().collect(),
                root: t,
                change_id: ChangeId::from_bytes([i as u8; 16]),
                author: sig(),
                message: format!("{zone} {i}"),
            }))
            .unwrap(),
        );
    }
    parent.unwrap()
}

fn publish(b: &dyn Backend, s: &MemoryStore, ups: &[(&str, Option<ObjectId>, ObjectId)]) -> Result<Outcome, PublishError> {
    let mut u: Vec<RefUpdate> = ups
        .iter()
        .map(|(a, o, n)| RefUpdate {
            zone: (*a).into(),
            branch: "main".into(),
            old: *o,
            new: *n,
        })
        .collect();
    u.sort();
    Publisher::new(
        b,
        s,
        Transaction {
            updates: u,
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        },
    )
    .run()
}

#[test]
fn sparse_fetch_reads_only_requested_zone() {
    let remote = MemBackend::new();
    let src = MemoryStore::new();
    let eng = chain(&src, "engine", 5, 100);
    let assets = chain(&src, "assets", 5, 100_000);
    publish(&remote, &src, &[("engine", None, eng), ("assets", None, assets)]).unwrap();

    let before = remote.bytes_read();
    let dst = MemoryStore::new();
    let heads = fetch_heads(&remote, &dst, Some(&["engine".to_string()])).unwrap();
    assert_eq!(heads.len(), 1);
    let read = remote.bytes_read() - before;
    assert!(read < 10_000, "fetched {read} bytes: assets (500KB) must not be touched");
    assert!(dst.contains(&eng).unwrap());
    assert!(!dst.contains(&assets).unwrap(), "unsubscribed zone not downloaded");
    // every object read came from objects/ or the manifest, none of the assets' blobs
    for k in remote.read_log() {
        assert!(k == "manifest" || k.starts_with("objects/") || k.starts_with("packs/"));
    }
    // later subscribing to assets fetches only what's missing
    fetch_heads(&remote, &dst, Some(&["assets".to_string()])).unwrap();
    assert!(dst.contains(&assets).unwrap());
}

#[test]
fn incremental_fetch_downloads_only_new_objects() {
    let remote = MemBackend::new();
    let src = MemoryStore::new();
    let h1 = chain(&src, "a", 3, 50);
    publish(&remote, &src, &[("a", None, h1)]).unwrap();
    let dst = MemoryStore::new();
    fetch_heads(&remote, &dst, None).unwrap();
    // extend history by one commit
    let blob = src.put_blob(b"new").unwrap();
    let t = src
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    let h2 = src
        .put(&Object::Commit(Commit {
            zone: "a".into(),
            parents: vec![h1],
            root: t,
            change_id: ChangeId::from_bytes([9; 16]),
            author: sig(),
            message: "next".into(),
        }))
        .unwrap();
    let r0 = remote.requests();
    publish(&remote, &src, &[("a", Some(h1), h2)]).unwrap();
    // publish touched only: 3 new objects (existence checks + puts), tx, manifest — not the old history
    let used = remote.requests() - r0;
    assert!(used < 20, "publish used {used} requests; old history must be skipped");
    let before = remote.bytes_read();
    fetch_heads(&remote, &dst, None).unwrap();
    let used = remote.bytes_read() - before;
    // 4000: a v2 pack index carries a 1 KiB fanout table that the fetch downloads with each index
    assert!(used < 4000, "incremental fetch read {used} bytes");
    assert!(dst.contains(&h2).unwrap());
}

#[test]
fn tampered_remote_object_is_rejected() {
    let remote = MemBackend::new();
    let src = MemoryStore::new();
    let h = chain(&src, "a", 1, 10);
    publish(&remote, &src, &[("a", None, h)]).unwrap();
    // corrupt the one blob (chain(.., "a", 1, 10) stores vec![1; 10]) wherever it lives
    remote.tamper_object(&Object::Blob(vec![1u8; 10]).id(), Object::Blob(b"evil".to_vec()).encode());
    let dst = MemoryStore::new();
    let e = fetch_heads(&remote, &dst, None).unwrap_err();
    assert!(e.to_string().contains("hash verification"), "{e}");
    assert!(
        !dst.contains(&h).unwrap(),
        "commit must not be stored before its dependencies verify"
    );
}

#[test]
fn garbage_manifest_is_a_clean_error() {
    let remote = MemBackend::new();
    remote.tamper("manifest", b"not a manifest".to_vec());
    let e = fetch_heads(&remote, &MemoryStore::new(), None).unwrap_err();
    assert!(matches!(e, PublishError::Remote(_)));
}

#[test]
fn diverged_publish_reports_both_heads() {
    let remote = MemBackend::new();
    let s1 = MemoryStore::new();
    let s2 = MemoryStore::new();
    let x = chain(&s1, "a", 1, 1);
    let y = chain(&s2, "a", 2, 1);
    publish(&remote, &s1, &[("a", None, x)]).unwrap();
    match publish(&remote, &s2, &[("a", None, y)]).unwrap_err() {
        PublishError::Diverged { found, expected, .. } => {
            assert_eq!(found, Some(x));
            assert_eq!(expected, None);
        }
        e => panic!("{e}"),
    }
}

#[test]
fn lost_ack_then_retry_is_idempotent() {
    let remote = MemBackend::new();
    let s = MemoryStore::new();
    let x = chain(&s, "a", 1, 1);
    publish(&remote, &s, &[("a", None, x)]).unwrap();
    let y = chain(&s, "a", 2, 1);
    remote.inject_cas_fault(CasFault::LostAck);
    // the library itself retries after the lost ack and must report success exactly once
    let r = publish(&remote, &s, &[("a", Some(x), y)]).unwrap();
    assert_eq!(r, Outcome::AlreadyApplied);
    let (m, _) = load_manifest(&remote).unwrap();
    assert_eq!(m.generation, 2);
    assert_eq!(m.head("a", "main"), Some(y));
}

/// Flake note: this binary failed once under load. It was run 200 times on a 30-core box with 8
/// concurrent copies (0 failures), 208 times beside 60 CPU spinners (0 failures) and 60 times
/// beside 24 `dsync` writers (0 failures): not reproduced. The only timing-dependent path is
/// `FsBackend`'s CAS lock (10 s give-up while another writer fsyncs), whose critical section
/// now excludes the fsync; that is a hardening, not a confirmed fix. The concurrent-CAS test
/// below also asserts that no temp file is left behind by a lost race.
#[test]
fn works_on_fs_backend_with_concurrent_threads() {
    let d = tempfile::tempdir().unwrap();
    let remote = std::sync::Arc::new(FsBackend::open(d.path().join("remote")).unwrap());
    let hs: Vec<_> = (0..6)
        .map(|i| {
            let remote = remote.clone();
            std::thread::spawn(move || {
                let s = MemoryStore::new();
                let zone = format!("zone{i}");
                let h = chain(&s, &zone, 2, 10);
                publish(remote.as_ref(), &s, &[(zone.as_str(), None, h)]).unwrap();
                h
            })
        })
        .collect();
    let heads: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    let (m, _) = load_manifest(remote.as_ref()).unwrap();
    assert_eq!(m.generation, 6, "all six disjoint writers committed");
    assert_eq!(m.applied.len(), 6);
    for (i, h) in heads.iter().enumerate() {
        assert_eq!(m.head(&format!("zone{i}"), "main"), Some(*h));
    }
    let dst = MemoryStore::new();
    assert_eq!(fetch_heads(remote.as_ref(), &dst, None).unwrap().len(), 6);
    let leftovers = std::fs::read_dir(d.path().join("remote").join(".tmp")).unwrap().count();
    assert_eq!(leftovers, 0, "lost CAS races must not leave temp files behind");
}

#[test]
fn unavailable_backend_yields_unknown_not_success() {
    struct Down;
    impl Backend for Down {
        fn get(&self, _: &str) -> Result<Option<Versioned>, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
        fn get_range(&self, _: &str, _: u64, _: u64) -> Result<Option<Vec<u8>>, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
        fn exists(&self, _: &str) -> Result<bool, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
        fn put_if_absent(&self, _: &str, _: &[u8]) -> Result<bool, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
        fn put_if_match(&self, _: &str, _: &[u8], _: &str) -> Result<Option<String>, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
        fn list(&self, _: &str) -> Result<Vec<String>, BackendError> {
            Err(BackendError::Unavailable("down".into()))
        }
    }
    let s = MemoryStore::new();
    let x = chain(&s, "a", 1, 1);
    let e = publish(&Down, &s, &[("a", None, x)]).unwrap_err();
    assert!(matches!(e, PublishError::Unknown(..)), "{e}");
}
