//! The packed publish builds one pack at a time from a bounded buffer. Whatever the memory bound
//! and pack size, the bytes on the remote are identical; a crash at any step publishes nothing;
//! a local read failure while building a pack fails the publish without touching the manifest.

use std::collections::BTreeMap;
use vcs_objects::*;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore, StoreError};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

/// `n` commits in a line, each with a 40-file tree where file 0 changes.
fn history(s: &dyn ObjectStore, n: u8) -> Vec<ObjectId> {
    let mut parent: Option<ObjectId> = None;
    let mut ids = vec![];
    for k in 1..=n {
        let ents: Vec<TreeEntry> = (0..40)
            .map(|i| TreeEntry {
                name: format!("f{i:03}"),
                mode: EntryMode::File,
                id: s
                    .put(&Object::Blob(
                        format!("{} {}", if i == 0 { k as usize } else { i }, "x".repeat(300 + i)).into_bytes(),
                    ))
                    .unwrap(),
            })
            .collect();
        let root = s.put(&Object::Tree(Tree::new(ents).unwrap())).unwrap();
        let c = s
            .put(&Object::Commit(Commit {
                zone: "z".into(),
                parents: parent.into_iter().collect(),
                root,
                change_id: ChangeId::from_bytes([k; 16]),
                author: sig(),
                message: format!("c{k}"),
            }))
            .unwrap();
        ids.push(c);
        parent = Some(c);
    }
    ids
}

fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn packs_of(b: &MemBackend) -> BTreeMap<String, Vec<u8>> {
    b.list("packs/")
        .unwrap()
        .into_iter()
        .map(|k| {
            let d = b.get(&k).unwrap().unwrap().data;
            (k, d)
        })
        .collect()
}

fn publish(s: &dyn ObjectStore, ids: &[ObjectId], mem: Option<usize>, max: usize) -> MemBackend {
    let b = MemBackend::new().with_gc();
    let mut p = Publisher::new(&b, s, tx(None, *ids.last().unwrap())).with_max_pack_bytes(max);
    if let Some(m) = mem {
        p = p.with_pack_mem(m);
    }
    p.run().unwrap();
    b
}

#[test]
fn any_memory_bound_gives_byte_identical_packs() {
    let s = MemoryStore::new();
    let ids = history(&s, 12);
    for max in [4 << 10, 16 << 10, 1 << 20, 256 << 20] {
        let reference = packs_of(&publish(&s, &ids, None, max));
        assert!(!reference.is_empty());
        for mem in [0usize, 1, 3000, 20_000, 1 << 30] {
            let got = packs_of(&publish(&s, &ids, Some(mem), max));
            assert_eq!(got, reference, "pack bytes differ at mem={mem} max_pack={max}");
        }
        if max <= 16 << 10 {
            assert!(reference.len() >= 6, "expected several packs at max={max}, got {}", reference.len());
        }
    }
}

#[test]
fn tiny_bound_publish_is_readable_and_incremental() {
    let s = MemoryStore::new();
    let ids = history(&s, 12);
    let b = MemBackend::new().with_gc();
    Publisher::new(&b, &s, tx(None, ids[5]))
        .with_pack_mem(0)
        .with_max_pack_bytes(2 << 10)
        .run()
        .unwrap();
    Publisher::new(&b, &s, tx(Some(ids[5]), ids[11]))
        .with_pack_mem(100)
        .with_max_pack_bytes(2 << 10)
        .run()
        .unwrap();
    let dst = MemoryStore::new();
    let got = fetch_heads(&b, &dst, None).unwrap();
    assert_eq!(got[0].2, ids[11]);
    for id in &ids {
        assert!(dst.contains(id).unwrap());
    }
}

#[test]
fn crash_at_every_step_publishes_nothing() {
    let s = MemoryStore::new();
    let ids = history(&s, 12);
    let b = MemBackend::new().with_gc();
    Publisher::new(&b, &s, tx(None, ids[3])).run().unwrap();
    let before = b.get("manifest").unwrap().unwrap();
    let mut k = 0;
    loop {
        let mut p = Publisher::new(&b, &s, tx(Some(ids[3]), ids[11]))
            .with_pack_mem(0)
            .with_max_pack_bytes(3 << 10);
        for _ in 0..k {
            assert_eq!(p.step().unwrap(), Step::Continue);
        }
        let at_cas = p.next_is_cas();
        drop(p);
        let now = b.get("manifest").unwrap().unwrap();
        assert_eq!(
            (now.data, now.tag),
            (before.data.clone(), before.tag.clone()),
            "manifest moved after {k} steps"
        );
        let keys = b.list("packs/").unwrap();
        for key in &keys {
            if let Some(stem) = key.strip_suffix(".idx") {
                assert!(keys.contains(&format!("{stem}.pack")), "idx visible without pack after {k} steps");
            }
        }
        let dst = MemoryStore::new();
        assert_eq!(fetch_heads(&b, &dst, None).unwrap()[0].2, ids[3]);
        if at_cas {
            break;
        }
        k += 1;
        assert!(k < 100);
    }
    assert!(k > 6, "several packs mean several steps, k={k}");
    Publisher::new(&b, &s, tx(Some(ids[3]), ids[11]))
        .with_pack_mem(0)
        .with_max_pack_bytes(3 << 10)
        .run()
        .unwrap();
    let dst = MemoryStore::new();
    assert_eq!(fetch_heads(&b, &dst, None).unwrap()[0].2, ids[11]);
}

/// A store whose encoded reads fail: only a publish that re-reads (memory bound reached) notices.
struct Flaky(MemoryStore);

impl ObjectStore for Flaky {
    fn get(&self, id: &ObjectId) -> Result<Object, StoreError> {
        self.0.get(id)
    }
    fn get_encoded(&self, id: &ObjectId) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::NotFound(*id))
    }
    fn put(&self, o: &Object) -> Result<ObjectId, StoreError> {
        self.0.put(o)
    }
    fn contains(&self, id: &ObjectId) -> Result<bool, StoreError> {
        self.0.contains(id)
    }
    fn list(&self) -> Result<Vec<ObjectId>, StoreError> {
        self.0.list()
    }
}

#[test]
fn local_read_failure_during_build_fails_cleanly() {
    let f = Flaky(MemoryStore::new());
    let ids = history(&f, 4);
    let b = MemBackend::new().with_gc();
    let e = Publisher::new(&b, &f, tx(None, ids[3])).with_pack_mem(0).run().unwrap_err();
    assert!(matches!(e, PublishError::Local(_)), "{e}");
    assert!(b.get("manifest").unwrap().is_none(), "no manifest may appear");
    assert!(b.list("packs/").unwrap().is_empty(), "no pack may appear");
    // with everything buffered the same store publishes fine (it never re-reads)
    Publisher::new(&b, &f, tx(None, ids[3])).run().unwrap();
}
