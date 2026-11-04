//! Pack v4: delta entries (codec 2) behind an opt-in format. Round trips, model/property tests,
//! hostile packs, legacy reads and the rollback to v2. See docs/research/storage/delta-gate.md.
use proptest::prelude::*;
use std::collections::BTreeMap;
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_store::pack::*;
use vcs_store::{LocalStore, ObjectStore};

fn lcg(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    move || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        s >> 24
    }
}

/// `files` files, each with `versions` successive edits, as encoded blobs.
fn history(files: usize, versions: usize, seed: u64) -> Vec<(ObjectId, Vec<u8>)> {
    let mut r = lcg(seed);
    let mut out = Vec::new();
    for f in 0..files {
        let mut text: Vec<u8> = (0..3000 + f * 100).map(|_| b'a' + (r() % 26) as u8).collect();
        for _ in 0..versions {
            let at = (r() as usize) % text.len();
            text.splice(at..at, format!("edit {}\n", r()).into_bytes());
            let enc = Object::Blob(text.clone()).encode();
            out.push((id_of_encoded(&enc), enc));
        }
    }
    out
}

fn depth_of(pack: &BuiltPack, idx: &PackIndex, id: &ObjectId) -> usize {
    let (mut off, len) = idx.find(id).unwrap();
    let mut payload = pack.pack[off as usize..(off + len as u64) as usize].to_vec();
    let mut d = 0;
    while payload[0] == CODEC_DELTA {
        d += 1;
        let (mut dist, mut sh, mut i) = (0u64, 0, 1);
        loop {
            let b = payload[i];
            i += 1;
            dist |= ((b & 0x7f) as u64) << sh;
            sh += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        assert!(dist > 0);
        off -= dist;
        let l = u32::from_le_bytes(pack.pack[off as usize - 4..off as usize].try_into().unwrap()) as usize;
        payload = pack.pack[off as usize..off as usize + l].to_vec();
    }
    d
}

#[test]
fn v4_is_much_smaller_and_reads_back_exactly() {
    let objs = history(6, 30, 1);
    let v2 = build_pack_fmt(objs.clone(), PackFormat::V2);
    let v4 = build_pack_fmt(objs.clone(), PackFormat::V4);
    assert!(v4.pack.len() * 3 < v2.pack.len(), "v4 {} vs v2 {}", v4.pack.len(), v2.pack.len());
    let d = tempfile::tempdir().unwrap();
    let p = write_pack_fmt(&d.path().join("pack"), d.path(), objs.clone(), PackFormat::V4).unwrap();
    assert_eq!(p.version(), 4);
    for (id, enc) in &objs {
        assert_eq!(&p.read(id).unwrap().unwrap(), enc);
    }
    assert!(p.verify().is_empty(), "{:?}", p.verify());
    let idx = PackIndex::parse(&v4.idx, std::path::Path::new("x")).unwrap();
    assert_eq!(idx.version(), 4);
    let max = objs.iter().map(|(id, _)| depth_of(&v4, &idx, id)).max().unwrap();
    assert!(max >= 1 && max <= MAX_WRITE_DEPTH as usize, "max depth {max}");
}

#[test]
fn small_objects_stay_whole_and_build_is_deterministic() {
    let mut objs = history(2, 5, 3);
    for i in 0..10u8 {
        let e = Object::Blob(vec![i; 10]).encode(); // below the deltify size floor
        objs.push((id_of_encoded(&e), e));
    }
    let a = build_pack_fmt(objs.clone(), PackFormat::V4);
    let b = build_pack_fmt(objs.clone(), PackFormat::V4);
    assert_eq!(a.pack, b.pack);
    assert_eq!(a.name, b.name);
}

fn dir_bytes(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir.join("pack"))
        .unwrap()
        .flatten()
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

#[test]
fn store_roundtrip_reopen_sweep_and_rollback() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("o");
    let s = LocalStore::open(&dir).unwrap();
    let objs = history(4, 25, 7);
    let mut ids = Vec::new();
    for (_, enc) in &objs {
        let Object::Blob(b) = Object::decode(enc).unwrap() else {
            unreachable!()
        };
        ids.push(s.put_blob(&b).unwrap());
        s.sync().unwrap(); // one object per barrier: stays loose (a big barrier would already be a pack)
    }
    s.repack_as(PackFormat::V4, false).unwrap();
    let v4_bytes = dir_bytes(&dir);
    for reopen in [false, true] {
        let s2 = LocalStore::open(&dir).unwrap();
        let s = if reopen { &s2 } else { &s };
        for (id, (_, enc)) in ids.iter().zip(&objs) {
            assert_eq!(&s.get(id).unwrap().encode(), enc);
        }
        assert!(s.verify_packs().is_empty());
    }
    // a sweep rewrites the pack without some objects: survivors (whose bases may be doomed) must still resolve
    let doomed: std::collections::BTreeSet<_> = ids.iter().step_by(3).copied().collect();
    let s = LocalStore::open(&dir).unwrap();
    s.sweep(&doomed).unwrap();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(s.contains(id).unwrap(), !doomed.contains(id), "object {i}");
        if !doomed.contains(id) {
            assert_eq!(&s.get(id).unwrap().encode(), &objs[i].1);
        }
    }
    // rollback: rewrite every pack as v2
    let (_, converted) = s.repack_as(PackFormat::V2, true).unwrap();
    assert!(converted >= 1);
    let s = LocalStore::open(&dir).unwrap();
    assert!(dir_bytes(&dir) > v4_bytes / 2, "sanity: v2 pack holds the surviving objects");
    for (i, id) in ids.iter().enumerate() {
        if !doomed.contains(id) {
            assert_eq!(&s.get(id).unwrap().encode(), &objs[i].1);
        }
    }
    assert!(s.verify_packs().is_empty());
}

#[test]
fn v1_and_v2_packs_still_read() {
    let objs = history(2, 6, 11);
    for fmt in [PackFormat::V1, PackFormat::V2] {
        let d = tempfile::tempdir().unwrap();
        let p = write_pack_fmt(&d.path().join("pack"), d.path(), objs.clone(), fmt).unwrap();
        assert_eq!(p.version(), fmt.version());
        for (id, enc) in &objs {
            assert_eq!(&p.read(id).unwrap().unwrap(), enc);
        }
    }
}

#[test]
fn decode_payload_refuses_a_bare_delta() {
    assert!(decode_payload(4, &[CODEC_DELTA, 1, 2, 3]).unwrap_err().contains("pack"));
    assert!(decode_payload(2, &[CODEC_DELTA, 1, 2, 3]).is_err());
}

fn first_delta(built: &BuiltPack) -> (usize, usize) {
    let idx = PackIndex::parse(&built.idx, std::path::Path::new("x")).unwrap();
    let found = idx
        .iter()
        .map(|(_, o, l)| (o as usize, l as usize))
        .find(|&(o, _)| built.pack[o] == CODEC_DELTA);
    found.expect("a delta entry")
}

fn read_tampered(built: &BuiltPack, pack: Vec<u8>) -> Vec<Result<Option<Vec<u8>>, vcs_store::StoreError>> {
    let d = tempfile::tempdir().unwrap();
    let stem = d.path().join("pack-x");
    std::fs::write(stem.with_extension("pack"), pack).unwrap();
    std::fs::write(stem.with_extension("idx"), &built.idx).unwrap();
    let p = Pack::open(&stem.with_extension("idx")).unwrap();
    let ids: Vec<_> = p.ids().collect();
    ids.iter().map(|id| p.read(id)).collect()
}

#[test]
fn hostile_delta_entries_are_errors() {
    let built = build_pack_fmt(history(1, 12, 5), PackFormat::V4);
    let (o, l) = first_delta(&built);
    // distance 0 (names itself), huge distance (before the pack), garbage zstd body
    for patch in [vec![0u8], vec![0xff, 0xff, 0xff, 0xff, 0x0f], vec![1u8, 0xde, 0xad, 0xbe, 0xef]] {
        let mut pack = built.pack.clone();
        let n = patch.len().min(l - 1);
        pack[o + 1..o + 1 + n].copy_from_slice(&patch[..n]);
        let res = read_tampered(&built, pack);
        assert!(res.iter().any(|r| r.is_err()), "tamper {patch:?} went unnoticed");
    }
}

#[test]
fn chain_deeper_than_reader_cap_is_refused() {
    // 61 hand-made entries, each a delta (distance = previous entry) of the one before.
    let base = Object::Blob(vec![b'x'; 4000]).encode();
    let mut pack = Vec::new();
    pack.extend_from_slice(PACK_MAGIC_V4);
    pack.extend_from_slice(&61u32.to_le_bytes());
    let mut ents = Vec::new();
    let mut prev_start = 0u64;
    let mut cur = base.clone();
    let mut ids = Vec::new();
    for k in 0..61 {
        let payload = if k == 0 {
            encode_payload(PackFormat::V2, &cur)
        } else {
            let mut next = cur.clone();
            next.push(k as u8);
            let d = vcs_store::delta::create(&vcs_store::delta::Index::new(&cur), &next, usize::MAX).unwrap();
            cur = next;
            let here = pack.len() as u64 + 4;
            let mut v = vec![CODEC_DELTA];
            let dist = here - prev_start;
            assert!(dist < 128);
            v.push(dist as u8);
            v.extend_from_slice(&zstd::bulk::compress(&d, 3).unwrap());
            v
        };
        pack.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        prev_start = pack.len() as u64;
        let id = id_of_encoded(&cur);
        ents.push((id, prev_start, payload.len() as u32));
        ids.push(id);
        pack.extend_from_slice(&payload);
    }
    let sum = *blake3::hash(&pack).as_bytes();
    pack.extend_from_slice(&sum);
    ents.sort_by_key(|e| e.0);
    let mut idx = Vec::new();
    idx.extend_from_slice(IDX_MAGIC_V4);
    idx.extend_from_slice(&(ents.len() as u32).to_le_bytes());
    idx.extend_from_slice(&(pack.len() as u64).to_le_bytes());
    idx.extend_from_slice(&sum);
    let mut counts = [0u32; 256];
    for e in &ents {
        counts[e.0.as_bytes()[0] as usize] += 1;
    }
    let mut run = 0;
    for c in counts {
        run += c;
        idx.extend_from_slice(&run.to_le_bytes());
    }
    for (id, off, len) in &ents {
        idx.extend_from_slice(id.as_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
        idx.extend_from_slice(&len.to_le_bytes());
    }
    idx.extend_from_slice(blake3::hash(&idx).as_bytes());
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("pack-h.pack"), &pack).unwrap();
    std::fs::write(d.path().join("pack-h.idx"), &idx).unwrap();
    let p = Pack::open(&d.path().join("pack-h.idx")).unwrap();
    // depth 50 resolves, depth 60 is refused
    assert!(p.read(&ids[50]).unwrap().is_some());
    let err = p.read(&ids[60]).unwrap_err();
    assert!(format!("{err}").contains("too deep"), "{err}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Model: whatever goes in comes out byte-identical, the pack verifies, chains are bounded,
    /// and the same input always gives the same pack.
    #[test]
    fn prop_roundtrip_bounded_deterministic(
        seed in any::<u64>(),
        files in 1usize..4,
        versions in 1usize..14,
        extra in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..400), 0..8),
    ) {
        let mut objs = history(files, versions, seed);
        for e in extra {
            let enc = Object::Blob(e).encode();
            objs.push((id_of_encoded(&enc), enc));
        }
        let a = build_pack_fmt(objs.clone(), PackFormat::V4);
        prop_assert_eq!(&a.pack, &build_pack_fmt(objs.clone(), PackFormat::V4).pack);
        let d = tempfile::tempdir().unwrap();
        let p = write_pack_fmt(&d.path().join("pack"), d.path(), objs.clone(), PackFormat::V4).unwrap();
        let model: BTreeMap<ObjectId, Vec<u8>> = objs.iter().cloned().collect();
        prop_assert_eq!(p.ids().count(), model.len());
        let idx = PackIndex::parse(&a.idx, std::path::Path::new("x")).unwrap();
        for (id, enc) in &model {
            prop_assert_eq!(&p.read(id).unwrap().unwrap(), enc);
            prop_assert!(depth_of(&a, &idx, id) <= MAX_WRITE_DEPTH as usize);
        }
        prop_assert!(p.verify().is_empty());
    }

    /// Bit flips in a v4 pack may fail reads but must never panic or hang, and any read that
    /// succeeds and hashes right is the original object.
    #[test]
    fn prop_tampered_pack_never_panics(seed in any::<u64>(), flips in proptest::collection::vec((any::<usize>(), 0u8..8), 1..6)) {
        let objs = history(2, 8, seed);
        let built = build_pack_fmt(objs.clone(), PackFormat::V4);
        let mut pack = built.pack.clone();
        let body = pack.len() - 32;
        for (at, bit) in flips {
            pack[12 + at % (body - 12)] ^= 1 << bit;
        }
        let model: BTreeMap<ObjectId, Vec<u8>> = objs.into_iter().collect();
        let mut ids: Vec<_> = model.keys().copied().collect();
        ids.sort();
        for (id, r) in ids.iter().zip(read_tampered(&built, pack)) {
            if let Ok(Some(b)) = r {
                if id_of_encoded(&b) == *id {
                    prop_assert_eq!(&b, &model[id]);
                }
            }
        }
    }
}
