//! Fault injection against the CURRENT implementation (issue #186, Phase 8 failure matrix):
//! corrupted packs, truncated packs, missing packs, index corruption, missing chunks,
//! undecodable-but-correctly-hashed objects, and a remote that lost an object below one the
//! publisher believes present. Each test states the invariant: corrupt remote data is never
//! accepted into a local store, and failures are clean errors, never panics or silent success.
//! Two tests document KNOWN GAPS (current behaviour that a hardened protocol should change).
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_store::pack::{write_pack, IDX_MAGIC};
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

/// One commit in `zone` whose tree holds the given files.
fn commit_with(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, files: &[(&str, Vec<u8>)]) -> ObjectId {
    let mut entries: Vec<TreeEntry> = files
        .iter()
        .map(|(n, d)| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: s.put_content(d).unwrap(),
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([files.len() as u8; 16]),
        author: sig(),
        message: "m".into(),
    }))
    .unwrap()
}

fn publish(b: &dyn Backend, s: &MemoryStore, zone: &str, old: Option<ObjectId>, new: ObjectId) -> Result<Outcome, PublishError> {
    Publisher::new(
        b,
        s,
        Transaction {
            updates: vec![RefUpdate {
                zone: zone.into(),
                branch: "main".into(),
                old,
                new,
            }],
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        },
    )
    .run()
}

fn copy_where(src: &MemBackend, keep: &dyn Fn(&str) -> bool) -> MemBackend {
    let dst = MemBackend::new();
    for k in src.keys().into_iter().filter(|k| keep(k)) {
        dst.put_if_absent(&k, &src.get(&k).unwrap().unwrap().data).unwrap();
    }
    dst
}

/// closure of `head` in `s`, as encoded objects
fn closure_of(s: &MemoryStore, head: ObjectId) -> Vec<(ObjectId, Vec<u8>)> {
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut st = vec![head];
    while let Some(id) = st.pop() {
        if !seen.insert(id) {
            continue;
        }
        let o = s.get(&id).unwrap();
        match &o {
            Object::Commit(c) => st.extend(c.parents.iter().copied().chain([c.root])),
            Object::Tree(t) => st.extend(t.entries().iter().map(|e| e.id)),
            Object::Chunked(m) => st.extend(m.chunks.iter().map(|c| c.id)),
            _ => {}
        }
        out.push((id, o.encode()));
    }
    out
}

/// A remote whose objects live ONLY in one pack (`packs/p1`), plus the manifest and tx records.
struct PackRemote {
    b: MemBackend,
    head: ObjectId,
    pack: Vec<u8>,
    idx: Vec<u8>,
    ids: Vec<ObjectId>,
}
fn pack_remote() -> PackRemote {
    let s = MemoryStore::new();
    let c1 = commit_with(&s, "a", None, &[("f", b"one".repeat(300)), ("g", b"two".repeat(500))]);
    let head = commit_with(&s, "a", Some(c1), &[("f", b"three".repeat(300))]);
    let loose = MemBackend::new();
    publish(&loose, &s, "a", None, head).unwrap();
    let b = copy_where(&loose, &|k| !k.starts_with("objects/"));
    let d = tempfile::tempdir().unwrap();
    let objs = closure_of(&s, head);
    let ids = objs.iter().map(|o| o.0).collect();
    let p = write_pack(&d.path().join("pack"), d.path(), objs).unwrap();
    let pack = std::fs::read(&p.pack_path).unwrap();
    let idx = std::fs::read(p.pack_path.with_extension("idx")).unwrap();
    b.put_if_absent("packs/p1.pack", &pack).unwrap();
    b.put_if_absent("packs/p1.idx", &idx).unwrap();
    PackRemote { b, head, pack, idx, ids }
}

fn reseal(idx: &mut Vec<u8>) {
    idx.truncate(idx.len() - 32);
    let sum = *blake3::hash(idx).as_bytes();
    idx.extend_from_slice(&sum);
}

fn assert_clean_failure(r: Result<Vec<(String, String, ObjectId)>, PublishError>, dst: &MemoryStore, head: ObjectId, what: &str) -> String {
    let e = r.err().unwrap_or_else(|| panic!("{what}: corrupt remote was accepted"));
    assert!(
        !dst.contains(&head).unwrap(),
        "{what}: head stored although its closure did not verify"
    );
    e.to_string()
}

#[test]
fn baseline_pack_only_remote_fetches() {
    let r = pack_remote();
    let dst = MemoryStore::new();
    fetch_heads(&r.b, &dst, None).unwrap();
    assert!(dst.contains(&r.head).unwrap());
    // batch path too
    let got = fetch_objects(&r.b, &r.ids).unwrap();
    assert_eq!(got.len(), r.ids.len());
}

#[test]
fn corrupt_pack_body_is_detected_on_every_read_path() {
    let r = pack_remote();
    // flip one byte inside every object's range, one object at a time (all positions covered by
    // the per-object hash): the body between the header and the trailer is all object bytes
    for pos in (16..r.pack.len() - 32).step_by(37) {
        let mut bad = r.pack.clone();
        bad[pos] ^= 0x40;
        let b = copy_where(&r.b, &|k| k != "packs/p1.pack");
        b.put_if_absent("packs/p1.pack", &bad).unwrap();
        let dst = MemoryStore::new();
        let single = fetch_heads(&b, &dst, None);
        let batch = fetch_objects(&b, &r.ids);
        // a flipped byte that falls in an object's payload must be rejected; bytes between
        // payloads (framing) are not read at all by indexed fetch, so success is only allowed
        // when the stored objects still verify
        match (single, batch) {
            (Err(_), Err(_)) => {}
            (Ok(_), Ok(objs)) => {
                for (id, o) in objs {
                    assert_eq!(o.id(), id, "accepted object does not match its id at pos {pos}");
                }
            }
            (a, b2) => panic!("paths disagree at pos {pos}: single ok={} batch ok={}", a.is_ok(), b2.is_ok()),
        }
    }
}

#[test]
fn truncated_pack_is_a_clean_error() {
    let r = pack_remote();
    // cuts that remove object bytes must fail on both read paths
    for cut in [0usize, 11, 12, r.pack.len() / 2, r.pack.len() - 33] {
        let b = copy_where(&r.b, &|k| k != "packs/p1.pack");
        b.put_if_absent("packs/p1.pack", &r.pack[..cut]).unwrap();
        let dst = MemoryStore::new();
        assert_clean_failure(fetch_heads(&b, &dst, None), &dst, r.head, &format!("cut {cut}"));
        assert!(fetch_objects(&b, &r.ids).is_err(), "batch path accepted truncated pack (cut {cut})");
    }
    // FINDING: the 32-byte pack trailer is never read by indexed remote fetch, so a pack missing
    // only (part of) its trailer is accepted. Safe (every object is still verified by hash) but
    // it means a half-finished upload of the trailer is not detected by readers.
    let b = copy_where(&r.b, &|k| k != "packs/p1.pack");
    b.put_if_absent("packs/p1.pack", &r.pack[..r.pack.len() - 1]).unwrap();
    let dst = MemoryStore::new();
    fetch_heads(&b, &dst, None).expect("trailer is not consulted by ranged reads");
    assert!(dst.contains(&r.head).unwrap());
}

#[test]
fn pack_missing_but_index_present_is_an_error_not_an_empty_result() {
    let r = pack_remote();
    let b = copy_where(&r.b, &|k| k != "packs/p1.pack");
    let dst = MemoryStore::new();
    let m = assert_clean_failure(fetch_heads(&b, &dst, None), &dst, r.head, "missing pack");
    assert!(m.contains("missing"), "{m}");
    let m = fetch_objects(&b, &r.ids).unwrap_err().to_string();
    assert!(m.contains("missing"), "{m}");
}

#[test]
fn index_corruption_is_detected() {
    let r = pack_remote();
    // 1. every single-bit flip anywhere in the index is rejected (checksum), never a wrong read
    for pos in (0..r.idx.len()).step_by(7) {
        let mut bad = r.idx.clone();
        bad[pos] ^= 1;
        let b = copy_where(&r.b, &|k| k != "packs/p1.idx");
        b.put_if_absent("packs/p1.idx", &bad).unwrap();
        let dst = MemoryStore::new();
        assert_clean_failure(fetch_heads(&b, &dst, None), &dst, r.head, &format!("idx flip {pos}"));
    }
    // 2. truncated index
    for cut in [0usize, 7, 8, 40, r.idx.len() - 1] {
        let b = copy_where(&r.b, &|k| k != "packs/p1.idx");
        b.put_if_absent("packs/p1.idx", &r.idx[..cut]).unwrap();
        let dst = MemoryStore::new();
        assert_clean_failure(fetch_heads(&b, &dst, None), &dst, r.head, &format!("idx cut {cut}"));
    }
    // 3. index with a valid checksum that points object A at object B's bytes: the id check on
    //    the fetched range must catch it (entries start at 52, 44 bytes each; offset+len at +32)
    let mut swapped = r.idx.clone();
    let (e0, e1) = (52usize, 52 + 44);
    for i in 32..44 {
        swapped.swap(e0 + i, e1 + i);
    }
    reseal(&mut swapped);
    assert_eq!(&swapped[..8], IDX_MAGIC);
    let b = copy_where(&r.b, &|k| k != "packs/p1.idx");
    b.put_if_absent("packs/p1.idx", &swapped).unwrap();
    let dst = MemoryStore::new();
    let m = assert_clean_failure(fetch_heads(&b, &dst, None), &dst, r.head, "swapped index entries");
    assert!(m.contains("verification") || m.contains("decode") || m.contains("missing"), "{m}");
    // 4. index claims a longer pack than exists (ranged reads come back short)
    let mut longer = r.idx.clone();
    let claimed = u64::from_le_bytes(longer[12..20].try_into().unwrap()) + 1_000_000;
    longer[12..20].copy_from_slice(&claimed.to_le_bytes());
    // shift the last entry past the real end but inside the claimed length
    let last = 52 + 44 * (r.ids.len() - 1);
    let off = claimed - 40;
    longer[last + 32..last + 40].copy_from_slice(&off.to_le_bytes());
    reseal(&mut longer);
    let b = copy_where(&r.b, &|k| k != "packs/p1.idx");
    b.put_if_absent("packs/p1.idx", &longer).unwrap();
    assert!(
        fetch_objects(&b, &r.ids).is_err(),
        "index pointing past the real pack end was accepted"
    );
}

#[test]
fn missing_chunk_fails_the_fetch_and_stores_no_dependents() {
    let s = MemoryStore::new();
    let big: Vec<u8> = (0..(2u32 << 20)).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let head = commit_with(&s, "a", None, &[("big", big)]);
    let full = MemBackend::new();
    publish(&full, &s, "a", None, head).unwrap();
    let chunk_ids: Vec<ObjectId> = closure_of(&s, head)
        .into_iter()
        .filter(|(_, e)| matches!(Object::decode(e), Ok(Object::Chunked(_))))
        .flat_map(|(_, e)| match Object::decode(&e).unwrap() {
            Object::Chunked(m) => m.chunks.into_iter().map(|c| c.id).collect::<Vec<_>>(),
            _ => vec![],
        })
        .collect();
    assert!(chunk_ids.len() > 8, "test file must be chunked");
    for victim in [chunk_ids[0], chunk_ids[chunk_ids.len() / 2], *chunk_ids.last().unwrap()] {
        let vk = object_key(&victim);
        let b = copy_where(&full, &|k| k != vk);
        let dst = MemoryStore::new();
        let m = assert_clean_failure(fetch_heads(&b, &dst, None), &dst, head, "missing chunk");
        assert!(m.contains("missing"), "{m}");
        // nothing that depends on the missing chunk may have been stored
        for (id, e) in closure_of(&s, head) {
            if matches!(
                Object::decode(&e),
                Ok(Object::Chunked(_)) | Ok(Object::Tree(_)) | Ok(Object::Commit(_))
            ) {
                assert!(!dst.contains(&id).unwrap(), "dependent {id} stored before its chunk");
            }
        }
    }
}

/// KNOWN GAP (documented current behaviour): sparse/lazy fetch skips file content, so a remote
/// that lost a chunk is only noticed when the file is actually read.
#[test]
fn known_gap_lazy_fetch_does_not_notice_a_missing_chunk() {
    let s = MemoryStore::new();
    let big: Vec<u8> = (0..(2u32 << 20)).map(|i| (i.wrapping_mul(40503) >> 7) as u8).collect();
    let head = commit_with(&s, "a", None, &[("big", big)]);
    let full = MemBackend::new();
    publish(&full, &s, "a", None, head).unwrap();
    let victim = closure_of(&s, head)
        .into_iter()
        .find_map(|(_, e)| match Object::decode(&e) {
            Ok(Object::Chunked(m)) => Some(m.chunks[1].id),
            _ => None,
        })
        .unwrap();
    let vk = object_key(&victim);
    let b = copy_where(&full, &|k| k != vk);
    let dst = MemoryStore::new();
    fetch_heads_lazy(&b, &dst, None).expect("lazy fetch only reads commits and trees");
    assert!(fetch_object(&b, &victim).is_err(), "reading the missing chunk must fail");
}

/// KNOWN GAP: the publisher prunes its upload walk at any object the remote already has, trusting
/// "present => whole closure present". If the remote lost a descendant (corruption, a buggy GC),
/// a new publish on top of the damaged object succeeds and the new head's closure is broken.
/// Fix (not done here, production protocol): verify with a pack/object listing, or periodic fsck.
#[test]
fn known_gap_publish_trusts_a_present_object_whose_descendant_is_gone() {
    let s = MemoryStore::new();
    let c1 = commit_with(&s, "a", None, &[("f", b"leaf".to_vec())]);
    let r = MemBackend::new();
    publish(&r, &s, "a", None, c1).unwrap();
    let leaf = closure_of(&s, c1)
        .into_iter()
        .find(|(_, e)| matches!(Object::decode(e), Ok(Object::Blob(_))))
        .unwrap()
        .0;
    let lk = object_key(&leaf);
    let damaged = copy_where(&r, &|k| k != lk);
    let c2 = commit_with(&s, "a", Some(c1), &[("g", b"new".to_vec())]);
    // publish succeeds: nothing notices that c1's blob is gone
    assert!(publish(&damaged, &s, "a", Some(c1), c2).is_ok());
    let dst = MemoryStore::new();
    assert!(fetch_heads(&damaged, &dst, None).is_err(), "a reader hits the hole");
    assert!(!dst.contains(&c2).unwrap());
}

#[test]
fn correctly_hashed_but_undecodable_object_is_rejected() {
    let b = MemBackend::new();
    let garbage = vec![0xFFu8, 0, 1, 2, 3, 4, 5];
    let id = id_of_encoded(&garbage);
    b.put_if_absent(&object_key(&id), &garbage).unwrap();
    let e = fetch_object(&b, &id).unwrap_err().to_string();
    assert!(e.contains("object"), "{e}");
    assert!(fetch_objects(&b, &[id]).is_err());
}

#[test]
fn hash_mismatch_loose_object_is_rejected_on_batch_path() {
    let b = MemBackend::new();
    let real = Object::Blob(b"real".to_vec());
    let id = real.id();
    b.put_if_absent(&object_key(&id), &Object::Blob(b"fake".to_vec()).encode()).unwrap();
    assert!(fetch_objects(&b, &[id]).is_err());
    assert!(fetch_object(&b, &id).is_err());
}
