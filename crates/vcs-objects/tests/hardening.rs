//! Hardening of the object decoder (issue #88): round trips, never-panic on hostile bytes, and
//! the canonical-form invariant (anything that decodes re-encodes to exactly the same bytes).
use proptest::prelude::*;
use std::collections::BTreeMap;
use vcs_objects::{
    ChangeId, Commit, EntryMode, LockOp, Object, ObjectId, OpView, Operation, RefUpdate, Signature, Snapshot, Transaction, Tree, TreeEntry,
};

fn id() -> impl Strategy<Value = ObjectId> {
    any::<[u8; 32]>().prop_map(ObjectId::from_bytes)
}
fn zone() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,10}"
}
fn branch() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,8}(/[a-z0-9]{1,5}){0,2}"
}
fn sig() -> impl Strategy<Value = Signature> {
    ("\\PC{0,20}", "\\PC{0,20}", any::<u64>()).prop_map(|(name, email, timestamp)| Signature { name, email, timestamp })
}
fn msg() -> impl Strategy<Value = String> {
    "\\PC{0,60}"
}
fn lock_op() -> impl Strategy<Value = LockOp> {
    let path = "[a-z]{1,5}(/[a-z]{1,5}){0,2}";
    (zone(), path, any::<u64>(), any::<u64>(), 0u8..5).prop_map(|(zone, path, a, b, k)| match k {
        0 => LockOp::Acquire {
            zone,
            path,
            lease_ms: a,
            reason: "why".into(),
        },
        1 => LockOp::Renew {
            zone,
            path,
            token: a,
            lease_ms: b,
        },
        2 => LockOp::Release { zone, path, token: a },
        3 => LockOp::Break { zone, path },
        _ => LockOp::Present { zone, path, token: a },
    })
}

fn object() -> impl Strategy<Value = Object> {
    let blob = proptest::collection::vec(any::<u8>(), 0..200).prop_map(Object::Blob);
    let tree = proptest::collection::btree_map("[a-zA-Z0-9_][a-zA-Z0-9_.-]{0,10}", (1u8..=3, id()), 0..8).prop_map(|m| {
        let entries = m
            .into_iter()
            .map(|(name, (mode, id))| TreeEntry {
                name,
                mode: match mode {
                    1 => EntryMode::File,
                    2 => EntryMode::Executable,
                    _ => EntryMode::Directory,
                },
                id,
            })
            .collect();
        Object::Tree(Tree::new(entries).unwrap())
    });
    let commit = (zone(), proptest::collection::vec(id(), 0..4), id(), any::<[u8; 16]>(), sig(), msg()).prop_map(
        |(zone, parents, root, cid, author, message)| {
            Object::Commit(Commit {
                zone,
                parents,
                root,
                change_id: ChangeId::from_bytes(cid),
                author,
                message,
            })
        },
    );
    let snap = (proptest::collection::btree_map(zone(), id(), 0..5), sig(), msg())
        .prop_map(|(zones, author, message)| Object::Snapshot(Snapshot { zones, author, message }));
    let txn = (
        proptest::collection::btree_map((zone(), branch()), (proptest::option::of(id()), id()), 0..4),
        sig(),
        msg(),
        proptest::collection::vec(lock_op(), 0..3),
    )
        .prop_map(|(ups, author, message, lock_ops)| {
            let updates = ups
                .into_iter()
                .map(|((zone, branch), (old, new))| RefUpdate { zone, branch, old, new })
                .collect();
            Object::Transaction(Transaction {
                updates,
                author,
                message,
                lock_ops,
            })
        });
    let op = (
        proptest::collection::vec(id(), 0..3),
        proptest::collection::btree_map((zone(), branch()), id(), 0..4),
        proptest::collection::btree_map(zone(), branch(), 0..4),
        sig(),
        msg(),
    )
        .prop_map(|(parents, branches, workspace, author, description)| {
            Object::Operation(Operation {
                parents,
                view: OpView {
                    branches,
                    workspace,
                    tags: None,
                },
                author,
                description,
            })
        });
    prop_oneof![blob, tree, commit, snap, txn, op]
}

/// Deterministic xorshift; the mutation loop must be reproducible without extra dependencies.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, b: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        match rng.below(6) {
            0 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] = rng.next() as u8;
            }
            2 if !b.is_empty() => {
                let i = rng.below(b.len());
                b.remove(i);
            }
            3 => {
                let i = rng.below(b.len() + 1);
                b.insert(i, rng.next() as u8);
            }
            4 => {
                let n = rng.below(b.len() + 1);
                b.truncate(n);
            }
            _ if !b.is_empty() => {
                // splice in a hostile varint (huge length / count) at a random spot
                let i = rng.below(b.len());
                let huge: &[&[u8]] = &[
                    &[0xff, 0xff, 0xff, 0xff, 0x0f],
                    &[0xff; 9],
                    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
                    &[0x80, 0x00],
                ];
                let h = huge[rng.below(huge.len())];
                b.splice(i..(i + h.len()).min(b.len()), h.iter().copied());
            }
            _ => {}
        }
    }
}

/// Whatever the decoder accepts must be canonical: one object, one byte string.
fn check(bytes: &[u8]) {
    if let Ok(o) = Object::decode(bytes) {
        assert_eq!(o.encode(), bytes, "decoder accepted a non-canonical encoding");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn roundtrip_every_kind(o in object()) {
        let enc = o.encode();
        let back = Object::decode(&enc).unwrap();
        prop_assert_eq!(&back, &o);
        prop_assert_eq!(back.encode(), enc);
        prop_assert_eq!(back.id(), o.id());
    }

    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..256)) {
        check(&data);
        // force the kind/version header so the payload parsers are reached
        for kind in 1..=6u8 {
            let mut v = vec![kind, 1];
            v.extend_from_slice(&data);
            check(&v);
        }
    }

    #[test]
    fn mutated_valid_objects_never_panic(o in object(), seed in any::<u64>()) {
        let mut rng = Rng(seed | 1);
        let mut b = o.encode();
        for _ in 0..32 {
            mutate(&mut rng, &mut b);
            check(&b);
        }
    }
}

#[test]
fn hostile_counts_are_rejected_without_allocating() {
    // kind, version, then a count of 2^63-ish with no payload behind it
    for kind in [2u8, 3, 4, 5, 6] {
        for count in [
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f][..],
            &[0xff, 0xff, 0xff, 0x7f][..],
            &[0x80, 0x80, 0x40][..],
        ] {
            let mut v = vec![kind, 1];
            // commit/snapshot/operation start with a string; give them a valid short zone first
            if matches!(kind, 3) {
                v.extend_from_slice(&[1, b'a']);
            }
            v.extend_from_slice(count);
            assert!(Object::decode(&v).is_err(), "kind {kind} accepted a hostile count");
        }
    }
    // a string length far beyond the input
    assert!(Object::decode(&[3, 1, 0xff, 0xff, 0xff, 0xff, 0x0f]).is_err());
}

#[test]
fn mutation_loop_over_a_seed_corpus() {
    let sig = Signature {
        name: "n".into(),
        email: "e@x".into(),
        timestamp: 5,
    };
    let i = |b| ObjectId::from_bytes([b; 32]);
    let mut seeds = vec![
        Object::Blob(b"hello".to_vec()),
        Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "a".into(),
                mode: EntryMode::File,
                id: i(1),
            }])
            .unwrap(),
        ),
        Object::Commit(Commit {
            zone: "root".into(),
            parents: vec![i(2), i(3)],
            root: i(4),
            change_id: ChangeId::from_bytes([9; 16]),
            author: sig.clone(),
            message: "m".into(),
        }),
        Object::Snapshot(Snapshot {
            zones: BTreeMap::from([("root".to_owned(), i(5))]),
            author: sig.clone(),
            message: "s".into(),
        }),
    ];
    seeds.push(Object::Transaction(Transaction {
        updates: vec![RefUpdate {
            zone: "root".into(),
            branch: "main".into(),
            old: None,
            new: i(6),
        }],
        author: sig.clone(),
        message: "t".into(),
        lock_ops: vec![LockOp::Break {
            zone: "root".into(),
            path: "a/b".into(),
        }],
    }));
    seeds.push(Object::Operation(Operation {
        parents: vec![i(7)],
        view: OpView {
            branches: BTreeMap::from([(("root".to_owned(), "main".to_owned()), i(8))]),
            workspace: BTreeMap::from([("root".to_owned(), "main".to_owned())]),
            tags: Some(BTreeMap::from([(("root".to_owned(), "v1".to_owned()), i(9))])),
        },
        author: sig,
        description: "d".into(),
    }));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut accepted = 0usize;
    for round in 0..300_000 {
        let mut b = seeds[round % seeds.len()].encode();
        mutate(&mut rng, &mut b);
        if Object::decode(&b).is_ok() {
            accepted += 1;
        }
        check(&b);
    }
    assert!(accepted > 0, "the loop should still reach the accepting paths");
}
