//! Parallel pack fetch: `fetch_many_each` over several packs plus loose objects returns every
//! object exactly once with verified bytes, shares one pack catalog across calls (no second
//! LIST or index GET), reports a missing id, and stops on a sink error.

use std::collections::BTreeMap;
use vcs_objects::*;
use vcs_remote::backend::Versioned;
use vcs_remote::compact::compact;
use vcs_remote::packfetch::{self, fetch_many_each, fetch_many_with, probe_kinds_with, PackCatalog};
use vcs_remote::CompactOptions;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn payload(i: usize) -> Vec<u8> {
    // incompressible-ish and of varying size so ranges differ
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (i as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    (0..200 + (i * 37) % 3000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Publish `rounds` commits of `files` files each (every round a new pack), plus one round with
/// loose objects. Returns every blob id.
fn seed(b: &dyn Backend, rounds: usize, files: usize) -> Vec<ObjectId> {
    let s = MemoryStore::new();
    let mut all = Vec::new();
    let mut parent: Vec<ObjectId> = vec![];
    let mut old = None;
    for r in 0..=rounds {
        let mut es = Vec::new();
        for i in 0..files {
            let id = s.put_blob(&[payload(r * 1000 + i), vec![r as u8]].concat()).unwrap();
            all.push(id);
            es.push(TreeEntry {
                name: format!("f{i:04}"),
                mode: EntryMode::File,
                id,
            });
        }
        let root = s.put(&Object::Tree(Tree::new(es).unwrap())).unwrap();
        let c = s
            .put(&Object::Commit(Commit {
                zone: "z".into(),
                parents: parent.clone(),
                root,
                change_id: ChangeId::from_bytes([r as u8; 16]),
                author: sig(),
                message: format!("c{r}"),
            }))
            .unwrap();
        let tx = Transaction {
            updates: vec![RefUpdate {
                zone: "z".into(),
                branch: "main".into(),
                old,
                new: c,
            }],
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        };
        let p = Publisher::new(b, &s, tx);
        let p = if r == rounds { p.with_loose_objects() } else { p };
        p.run().unwrap();
        parent = vec![c];
        old = Some(c);
    }
    all
}

#[test]
fn many_packs_and_loose_arrive_once_and_verified() {
    let b = MemBackend::new();
    let ids = seed(&b, 4, 150);
    let cat = PackCatalog::new();
    let counted = CountingBackend::new(&b);
    let mut got: BTreeMap<ObjectId, Vec<u8>> = BTreeMap::new();
    fetch_many_each::<BackendError>(&counted, &cat, &ids, &mut |id, d| {
        assert!(got.insert(id, d).is_none(), "delivered twice");
        Ok(())
    })
    .unwrap();
    assert_eq!(got.len(), ids.len());
    for (id, d) in &got {
        assert_eq!(id_of_encoded(d), *id);
    }
    let first = counted.stats().requests;
    // second call: the catalog is warm, so no LIST and no index GET
    let again = fetch_many_with(&counted, &cat, &ids[..10]).unwrap();
    assert_eq!(again.len(), 10);
    let second = counted.stats().requests - first;
    assert!(second <= 6, "warm catalog call made {second} requests");
    // probing with the same catalog needs no pack requests at all
    let before = counted.stats().requests;
    let probed = probe_kinds_with(&counted, &cat, &ids[..50]).unwrap();
    assert!(probed.iter().all(|(_, k)| k.is_some()));
    assert!(counted.stats().requests - before <= 50);
}

#[test]
fn unknown_id_is_an_error_and_sink_error_stops_the_fetch() {
    let b = MemBackend::new();
    let ids = seed(&b, 3, 100);
    let cat = PackCatalog::new();
    let ghost = Object::Blob(b"not on the remote".to_vec()).id();
    let mut want = ids.clone();
    want.push(ghost);
    let e = fetch_many_each::<BackendError>(&b, &cat, &want, &mut |_, _| Ok(())).unwrap_err();
    assert!(
        e.to_string().contains("missing") || e.to_string().contains(&ghost.to_string()),
        "{e}"
    );

    let mut n = 0;
    let e = fetch_many_each::<BackendError>(&b, &cat, &ids, &mut |_, _| {
        n += 1;
        if n == 5 {
            Err(BackendError::Other("sink says stop".into()))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(e.to_string().contains("sink says stop"));
    assert_eq!(n, 5, "no object is delivered after the sink failed");
}

#[test]
fn empty_request_makes_no_request() {
    let b = MemBackend::new();
    seed(&b, 1, 3);
    let counted = CountingBackend::new(&b);
    fetch_many_each::<BackendError>(&counted, &PackCatalog::new(), &[], &mut |_, _| Ok(())).unwrap();
    assert_eq!(counted.stats().requests, 0);
}

/// Runs `compact --all` the first time a data range of a pack (not the 33-byte trailer check) is
/// requested, so the packs the fetch planned against vanish mid-read.
struct CompactsMidRead<'a> {
    inner: &'a MemBackend,
    done: std::sync::atomic::AtomicBool,
}
impl Backend for CompactsMidRead<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.inner.get(k)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        if k.ends_with(".pack") && l > 33 && !self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let opts = CompactOptions {
                all: true,
                dry_run: false,
                owner: "t".into(),
                ..Default::default()
            };
            compact(self.inner, &opts).unwrap();
        }
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.inner.delete(k)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, k: &str) -> Result<Option<u64>, BackendError> {
        self.inner.modified_ms(k)
    }
    fn touch(&self, k: &str) -> Result<bool, BackendError> {
        self.inner.touch(k)
    }
}

#[test]
fn compaction_during_a_parallel_fetch_is_retried_and_without_retry_is_an_error() {
    let ids = {
        let b = MemBackend::new();
        seed(&b, 4, 120)
    };
    for (rounds, must_succeed) in [(4usize, true), (1, false)] {
        let b = MemBackend::new();
        let ids2 = seed(&b, 4, 120);
        assert_eq!(ids, ids2, "seed is deterministic");
        let racy = CompactsMidRead {
            inner: &b,
            done: Default::default(),
        };
        let mut got = BTreeMap::new();
        let r = packfetch::fetch_many_each_retrying::<BackendError>(&racy, &PackCatalog::new(), &ids, rounds, &mut |id, d| {
            got.insert(id, d);
            Ok(())
        });
        if must_succeed {
            r.unwrap();
            assert_eq!(got.len(), ids.len());
            for (id, d) in &got {
                assert_eq!(id_of_encoded(d), *id);
            }
        } else {
            assert!(r.is_err(), "with no retry budget a vanished pack must not be silently skipped");
        }
    }
}
