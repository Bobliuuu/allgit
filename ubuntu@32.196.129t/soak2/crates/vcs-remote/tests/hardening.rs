//! Hardening of the remote manifest decoder (issue #88). The manifest is fetched from a
//! bucket anyone with write access can alter, so it is fully untrusted input.
use proptest::prelude::*;
use vcs_objects::ObjectId;
use vcs_remote::manifest::{Lock, Manifest};

fn id(b: u8) -> ObjectId {
    ObjectId::from_bytes([b; 32])
}

fn manifest() -> impl Strategy<Value = Manifest> {
    (
        any::<u64>(),
        proptest::collection::btree_map(("[a-z]{1,6}", "[a-z]{1,6}(/[a-z]{1,4})?"), any::<u8>(), 0..5),
        proptest::collection::btree_map("[a-z]{1,6}", any::<u8>(), 0..4),
        proptest::collection::vec(any::<u8>(), 0..6),
        proptest::collection::btree_map("[a-z]{1,6}", "[a-z]{1,5}(/[a-z]{1,5})?", 0..4),
        proptest::option::of((1u64..1000, any::<u64>(), "[a-z]{1,6}", "[a-z]{1,6}(/[a-z]{1,4})?")),
    )
        .prop_map(|(generation, br, sn, ap, zones, lock)| {
            let mut m = Manifest {
                generation,
                branches: br.into_iter().map(|(k, v)| (k, id(v))).collect(),
                snapshots: sn.into_iter().map(|(k, v)| (k, id(v))).collect(),
                applied: ap.into_iter().map(id).collect(),
                zones,
                ..Default::default()
            };
            if let Some((next, hw, z, p)) = lock {
                m.next_token = next + 1;
                m.time_hw = hw;
                m.locks.insert(
                    (z, p),
                    Lock {
                        holder: "h".into(),
                        token: next,
                        acquired_at: 1,
                        expires_at: 2,
                        reason: "r".into(),
                    },
                );
            }
            m
        })
}

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
        match rng.below(5) {
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
            _ => {
                let n = rng.below(b.len() + 1);
                b.truncate(n);
            }
        }
    }
}

fn check(bytes: &[u8]) {
    if let Ok(m) = Manifest::decode(bytes) {
        assert_eq!(m.encode(), bytes, "manifest decoder accepted a non-canonical encoding");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn roundtrip(m in manifest()) {
        let enc = m.encode();
        prop_assert_eq!(Manifest::decode(&enc).unwrap(), m);
    }

    #[test]
    fn random_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..256)) {
        check(&data);
        for v in 1..=3u8 {
            let mut b = b"AGMF".to_vec();
            b.push(v);
            b.extend_from_slice(&data);
            check(&b);
        }
    }

    #[test]
    fn mutated_manifests_never_panic(m in manifest(), seed in any::<u64>()) {
        let mut rng = Rng(seed | 1);
        let mut b = m.encode();
        for _ in 0..32 {
            mutate(&mut rng, &mut b);
            check(&b);
        }
    }
}

#[test]
fn hostile_counts_do_not_allocate_or_loop() {
    // header + generation 0, then each list count claims 2^63 entries with no bytes behind it
    for v in 1..=3u8 {
        let mut b = b"AGMF".to_vec();
        b.extend_from_slice(&[v, 0]);
        b.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
        assert!(Manifest::decode(&b).is_err());
    }
    // applied list longer than MAX_APPLIED must be refused even if the bytes are present
    let m = Manifest {
        applied: (0..=vcs_remote::manifest::MAX_APPLIED)
            .map(|i| ObjectId::from_bytes([(i % 251) as u8; 32]))
            .collect(),
        ..Default::default()
    };
    assert!(Manifest::decode(&m.encode()).is_err());
}

#[test]
fn mutation_loop_over_a_seed_corpus() {
    let mut m = Manifest {
        generation: 3,
        next_token: 5,
        time_hw: 77,
        ..Default::default()
    };
    m.branches.insert(("root".into(), "main".into()), id(1));
    m.snapshots.insert("rel".into(), id(2));
    m.applied.push(id(3));
    m.zones.insert("docs".into(), "docs".into());
    m.locks.insert(
        ("docs".into(), "a.bin".into()),
        Lock {
            holder: "alice".into(),
            token: 4,
            acquired_at: 1,
            expires_at: 9,
            reason: "x".into(),
        },
    );
    let mut v2 = m.clone();
    v2.locks.clear();
    v2.next_token = 0;
    v2.time_hw = 0;
    v2.labels.insert("docs".into(), "manual".into());
    let seeds = [m.encode(), v2.encode(), Manifest::default().encode()];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut accepted = 0usize;
    for round in 0..300_000 {
        let mut b = seeds[round % seeds.len()].clone();
        mutate(&mut rng, &mut b);
        if Manifest::decode(&b).is_ok() {
            accepted += 1;
        }
        check(&b);
    }
    assert!(accepted > 0);
}

#[test]
fn unsorted_or_duplicate_keys_are_rejected() {
    // Regression: the decoder inserted into maps, so reordered or repeated keys were accepted
    // and re-encoded differently (and a duplicate silently replaced the earlier head).
    let mut m = Manifest::default();
    m.branches.insert(("a".into(), "main".into()), id(1));
    m.branches.insert(("b".into(), "main".into()), id(2));
    let good = m.encode();
    assert!(Manifest::decode(&good).is_ok());
    // swap the two branch records: bytes after "AGMF",ver,gen,count are 2 records of equal size
    let rec = 1 + 1 + 1 + 4 + 32;
    let head = 4 + 1 + 1 + 1;
    let mut swapped = good[..head].to_vec();
    swapped.extend_from_slice(&good[head + rec..head + 2 * rec]);
    swapped.extend_from_slice(&good[head..head + rec]);
    swapped.extend_from_slice(&good[head + 2 * rec..]);
    assert!(Manifest::decode(&swapped).is_err(), "reordered branches");
    let mut dup = good[..head].to_vec();
    dup.extend_from_slice(&good[head..head + rec]);
    dup.extend_from_slice(&good[head..head + rec]);
    dup.extend_from_slice(&good[head + 2 * rec..]);
    assert!(Manifest::decode(&dup).is_err(), "duplicate branch");
}

#[test]
fn v2_manifest_without_labels_is_rejected() {
    // Regression, found by the manifest_decode fuzz target: `AGMF 02 01 00 00 00 00 00` decoded
    // fine but re-encoded as v1, breaking the one-value-one-encoding rule.
    let crash = [b'A', b'G', b'M', b'F', 2, 1, 0, 0, 0, 0, 0];
    assert!(Manifest::decode(&crash).is_err());
}
