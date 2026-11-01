//! Parallel pack fetch: `fetch_many_each` over several packs plus loose objects returns every
//! object exactly once with verified bytes, shares one pack catalog across calls (no second
//! LIST or index GET), reports a missing id, and stops on a sink error.

use std::collections::BTreeMap;
use vcs_objects::*;
use vcs_remote::packfetch::{fetch_many_each, fetch_many_with, probe_kinds_with, PackCatalog};
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
