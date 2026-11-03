//! Hardening of the pack index and pack data readers (issue #88). Pack files may come from a
//! remote bucket or a damaged disk, so index and data are untrusted input.
use std::fs;
use std::path::Path;
use vcs_objects::{Object, ObjectId};
use vcs_store::pack::{write_pack, Pack, PackIndex, IDX_MAGIC};

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
    for _ in 0..1 + rng.below(3) {
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

fn objects(n: usize) -> Vec<(ObjectId, Vec<u8>)> {
    (0..n)
        .map(|i| {
            let o = Object::Blob(format!("object number {i}").into_bytes().repeat(1 + i % 5));
            (o.id(), o.encode())
        })
        .collect()
}

/// Recompute the index trailer so a mutation survives the checksum and reaches the parser.
fn reseal(idx: &mut Vec<u8>) {
    if idx.len() >= 32 {
        idx.truncate(idx.len() - 32);
    }
    let sum = *blake3::hash(idx).as_bytes();
    idx.extend_from_slice(&sum);
}

fn index_bytes(pack_len: u64, entries: &[(ObjectId, u64, u32)]) -> Vec<u8> {
    let mut idx = Vec::new();
    idx.extend_from_slice(IDX_MAGIC);
    idx.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    idx.extend_from_slice(&pack_len.to_le_bytes());
    idx.extend_from_slice(&[7u8; 32]);
    for (id, off, len) in entries {
        idx.extend_from_slice(id.as_bytes());
        idx.extend_from_slice(&off.to_le_bytes());
        idx.extend_from_slice(&len.to_le_bytes());
    }
    reseal(&mut idx);
    idx
}

fn fresh_pack(dir: &Path, n: usize) -> (std::path::PathBuf, Vec<u8>, Vec<u8>) {
    let tmp = dir.join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    let p = write_pack(&dir.join("packs"), &tmp, objects(n)).unwrap();
    let pack = fs::read(&p.pack_path).unwrap();
    let idx_path = p.pack_path.with_extension("idx");
    let idx = fs::read(&idx_path).unwrap();
    (idx_path, idx, pack)
}

#[test]
fn index_offset_overflow_is_rejected() {
    // Regression: `off + len` wrapped for an offset near u64::MAX, so an entry far outside the
    // pack passed the bounds check (and panicked outright with overflow checks on).
    let id = ObjectId::from_bytes([1; 32]);
    let idx = index_bytes(u64::MAX, &[(id, u64::MAX - 1, 10)]);
    assert!(PackIndex::parse(&idx, Path::new("x.idx")).is_err());
    let idx = index_bytes(u64::MAX, &[(id, u64::MAX, u32::MAX)]);
    assert!(PackIndex::parse(&idx, Path::new("x.idx")).is_err());
}

#[test]
fn index_entry_inside_the_header_is_rejected() {
    let id = ObjectId::from_bytes([1; 32]);
    for off in [0u64, 4, 12, 15] {
        let idx = index_bytes(1000, &[(id, off, 4)]);
        assert!(
            PackIndex::parse(&idx, Path::new("x.idx")).is_err(),
            "offset {off} overlaps the pack header"
        );
    }
}

#[test]
fn index_count_far_larger_than_the_input_is_rejected() {
    let mut idx = index_bytes(100, &[]);
    idx[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal(&mut idx);
    assert!(PackIndex::parse(&idx, Path::new("x.idx")).is_err());
}

#[test]
fn index_mutation_loop_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let (_, idx, _) = fresh_pack(dir.path(), 20);
    assert!(PackIndex::parse(&idx, Path::new("x.idx")).is_ok());
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    let mut accepted = 0;
    for _ in 0..200_000 {
        let mut b = idx.clone();
        mutate(&mut rng, &mut b);
        // half the time keep the checksum valid so the structural checks are exercised
        if rng.below(2) == 0 {
            reseal(&mut b);
        }
        if PackIndex::parse(&b, Path::new("x.idx")).is_ok() {
            accepted += 1;
        }
    }
    assert!(accepted > 0);
}

#[test]
fn pack_file_mutation_loop_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let (idx_path, idx, pack) = fresh_pack(dir.path(), 12);
    let pack_path = idx_path.with_extension("pack");
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    for round in 0..3000 {
        let mut p = pack.clone();
        let mut i = idx.clone();
        match round % 3 {
            0 => mutate(&mut rng, &mut p),
            1 => {
                mutate(&mut rng, &mut i);
                reseal(&mut i);
            }
            _ => {
                mutate(&mut rng, &mut p);
                mutate(&mut rng, &mut i);
                reseal(&mut i);
            }
        }
        fs::write(&pack_path, &p).unwrap();
        fs::write(&idx_path, &i).unwrap();
        if let Ok(opened) = Pack::open(&idx_path) {
            let ids: Vec<_> = opened.ids().collect();
            for id in ids {
                let _ = opened.read(&id);
            }
            let _ = opened.verify();
        }
    }
}

#[test]
fn intact_pack_still_reads_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let (idx_path, _, _) = fresh_pack(dir.path(), 8);
    let p = Pack::open(&idx_path).unwrap();
    assert!(p.verify().is_empty());
    for (id, enc) in objects(8) {
        assert_eq!(p.read(&id).unwrap().unwrap(), enc);
    }
}
