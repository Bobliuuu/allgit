//! Changed-path filters: for a commit and each of its parents, a small Bloom filter holding every
//! file path that differs from that parent, plus all the directories above those files. "Does this
//! commit change `a/b`?" then costs one hash lookup instead of reading trees; a miss is certain
//! (the entries are equal), a hit is only "maybe" and is settled by comparing the real entries.
//!
//! One file per zone, `.vcs/path-filters/<hash of zone id>`, a pure cache like the commit-graph:
//! `blake3`-checked, discarded when anything is wrong, never needed for a correct answer, built
//! lazily from the commits a deep path-limited walk passes. Commits with very large changes (more
//! than [`MAX_PATHS`] paths, e.g. an import commit) and root commits get no filter ("unknown").
//!
//! File: `"AGPF" || version:u8 || count:u32 || count * id:32 (sorted) || count * offset:u32 ||
//! data || blake3(all preceding)`. Data per commit: `nparents:u8`, then per parent
//! `len:u32` (`u32::MAX` = unknown) and `len` filter bytes. Offsets are into `data`.

use crate::cgraph::cg_name;
use crate::error::Result;
use crate::meta::write_cache_atomic;
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use vcs_objects::{Commit, ObjectId};
use vcs_store::mapfile::MapFile;

pub(crate) const PF_DIR: &str = "path-filters";
const MAGIC: &[u8; 4] = b"AGPF";
const VERSION: u8 = 1;
/// More changed paths than this: no filter (it would be big and always say "maybe" anyway).
pub(crate) const MAX_PATHS: usize = 512;
const HASHES: u64 = 7;
const BITS_PER_PATH: usize = 12;
const UNKNOWN: u32 = u32::MAX;

/// A filter for one (commit, parent) pair: `None` = unknown (anything may have changed).
pub(crate) type Filter = Option<Vec<u8>>;

fn hash_pair(path: &str) -> (u64, u64) {
    let h = blake3::hash(path.as_bytes());
    let b = h.as_bytes();
    (
        u64::from_le_bytes(b[..8].try_into().unwrap()),
        u64::from_le_bytes(b[8..16].try_into().unwrap()) | 1,
    )
}

fn bit_positions(path: &str, nbits: u64) -> impl Iterator<Item = u64> {
    let (h1, h2) = hash_pair(path);
    (0..HASHES).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % nbits)
}

/// Is `path` possibly in the filter? Always yes for an unknown filter.
pub(crate) fn may_contain(f: &Filter, path: &str) -> bool {
    match f {
        None => true,
        Some(b) if b.is_empty() => false,
        Some(b) => bit_positions(path, b.len() as u64 * 8).all(|p| b[(p / 8) as usize] & (1 << (p % 8)) != 0),
    }
}

/// Filter of a change touching `files` (full zone-relative paths). `None` if too many.
pub(crate) fn build_filter<'a>(files: impl Iterator<Item = &'a String>) -> Filter {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for f in files {
        let mut end = f.len();
        loop {
            paths.insert(&f[..end]);
            match f[..end].rfind('/') {
                Some(i) => end = i,
                None => break,
            }
        }
        if paths.len() > MAX_PATHS {
            return None;
        }
    }
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let bytes = (paths.len() * BITS_PER_PATH).div_ceil(8).max(8);
    let mut b = vec![0u8; bytes];
    for p in paths {
        for pos in bit_positions(p, bytes as u64 * 8) {
            b[(pos / 8) as usize] |= 1 << (pos % 8);
        }
    }
    Some(b)
}

/// Files up to this size are verified completely when opened lazily.
pub(crate) const EAGER_VERIFY_MAX: usize = 4 << 20;

/// The filters of a zone: what is in the (memory mapped) file plus what was computed this run.
pub(crate) struct PathBlooms {
    raw: MapFile,
    count: usize,
    added: BTreeMap<ObjectId, Vec<Filter>>,
}

impl Default for PathBlooms {
    fn default() -> Self {
        PathBlooms {
            raw: MapFile::from_vec(Vec::new()),
            count: 0,
            added: BTreeMap::new(),
        }
    }
}

impl PathBlooms {
    /// Parse and completely verify bytes in memory; anything wrong gives an empty set.
    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> PathBlooms {
        Self::from_map(MapFile::from_vec(bytes), usize::MAX).unwrap_or_default()
    }

    /// Map the file without reading it: O(1) time and no anonymous memory at any size. Only the header and the
    /// layout arithmetic are checked at open (and, up to `EAGER_VERIFY_MAX`, everything); every lookup checks the
    /// record it touches and treats a malformed one as absent, and [`verify_full`](Self::verify_full) (fsck) checks
    /// the whole file. A missing or invalid file gives an empty set.
    pub(crate) fn open(path: &std::path::Path) -> PathBlooms {
        MapFile::open(path, true)
            .ok()
            .and_then(|m| Self::from_map(m, EAGER_VERIFY_MAX))
            .unwrap_or_default()
    }

    pub(crate) fn from_map(raw: MapFile, eager_max: usize) -> Option<PathBlooms> {
        if raw.len() < 4 + 1 + 4 + 32 || &raw[..4] != MAGIC || raw[4] != VERSION {
            return None;
        }
        let count = u32::from_le_bytes(raw[5..9].try_into().ok()?) as usize;
        let table = 9usize.checked_add(count.checked_mul(36)?)?;
        if table > raw.len() - 32 {
            return None;
        }
        let b = PathBlooms {
            raw,
            count,
            added: BTreeMap::new(),
        };
        if b.raw.len() <= eager_max && !b.verify_full() {
            return None;
        }
        Some(b)
    }

    fn body_len(&self) -> usize {
        self.raw.len() - 32
    }

    fn table(&self) -> usize {
        9 + self.count * 36
    }

    fn id_at(&self, i: usize) -> &[u8] {
        &self.raw[9 + i * 32..9 + i * 32 + 32]
    }

    /// Bytes of record `i`, `None` if its offset or contents run outside the data.
    fn record(&self, i: usize) -> Option<&[u8]> {
        let table = self.table();
        let off = u32::from_le_bytes(self.raw[9 + self.count * 32 + i * 4..][..4].try_into().unwrap()) as usize;
        let body_len = self.body_len();
        if off >= body_len - table {
            return None;
        }
        let start = table + off;
        let mut p = start;
        let np = *self.raw.get(p)? as usize;
        p += 1;
        for _ in 0..np {
            let l = u32::from_le_bytes(self.raw.get(p..p + 4)?.try_into().ok()?);
            p += 4;
            if l != UNKNOWN {
                p = p.checked_add(l as usize)?;
            }
        }
        (p <= body_len).then(|| &self.raw[start..p])
    }

    /// Everything: checksum, sorted ids and every record well formed. O(file size) time, O(1) memory.
    pub(crate) fn verify_full(&self) -> bool {
        self.raw.advise_sequential();
        let body = &self.raw[..self.body_len()];
        if vcs_store::mapfile::blake3_big(body).as_bytes() != &self.raw[self.body_len()..] {
            return false;
        }
        (1..self.count).all(|i| self.id_at(i - 1) < self.id_at(i)) && (0..self.count).all(|i| self.record(i).is_some())
    }

    fn lookup_raw(&self, id: &ObjectId) -> Option<Vec<Filter>> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.id_at(mid).cmp(id.as_bytes().as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return self.record(mid).map(parse_record),
            }
        }
        None
    }

    pub(crate) fn get(&self, id: &ObjectId) -> Option<Vec<Filter>> {
        self.added.get(id).cloned().or_else(|| self.lookup_raw(id))
    }

    pub(crate) fn insert(&mut self, id: ObjectId, f: Vec<Filter>) {
        self.added.insert(id, f);
    }

    /// Were any filters read from disk?
    pub(crate) fn has_file(&self) -> bool {
        self.count > 0
    }

    pub(crate) fn pending(&self) -> usize {
        self.added.len()
    }

    /// Every commit id and its filters (file records that are well formed, overridden by new ones), in id order.
    pub(crate) fn for_each(&self, mut f: impl FnMut(ObjectId, Vec<Filter>)) {
        let mut added = self.added.iter().peekable();
        for i in 0..self.count {
            let id = ObjectId::from_bytes(self.id_at(i).try_into().unwrap());
            while let Some((k, v)) = added.next_if(|(k, _)| **k < id) {
                f(*k, v.clone());
            }
            if let Some((k, v)) = added.next_if(|(k, _)| **k == id) {
                f(*k, v.clone());
            } else if let Some(r) = self.record(i) {
                f(id, parse_record(r));
            }
        }
        for (k, v) in added {
            f(*k, v.clone());
        }
    }

    /// The file image of file plus new filters (merged in one pass over the sorted sources, copying file records as
    /// bytes). `None` if it would not fit the format's 32-bit counts and offsets: the cache is then simply not written.
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        let mut ids = Vec::with_capacity((self.count + self.added.len()) * 32);
        let mut offs = Vec::with_capacity((self.count + self.added.len()) * 4);
        let mut data: Vec<u8> = Vec::new();
        let mut count = 0usize;
        let mut push = |id: &[u8], rec: &[u8], data: &mut Vec<u8>| {
            ids.extend_from_slice(id);
            offs.extend_from_slice(&(data.len() as u32).to_le_bytes());
            data.extend_from_slice(rec);
            count += 1;
        };
        let mut added = self.added.iter().peekable();
        for i in 0..self.count {
            let id = ObjectId::from_bytes(self.id_at(i).try_into().unwrap());
            while let Some((k, v)) = added.next_if(|(k, _)| **k < id) {
                push(k.as_bytes(), &encode_record(v), &mut data);
            }
            if let Some((k, v)) = added.next_if(|(k, _)| **k == id) {
                push(k.as_bytes(), &encode_record(v), &mut data);
            } else if let Some(r) = self.record(i) {
                push(id.as_bytes(), r, &mut data);
            }
            if data.len() > u32::MAX as usize {
                return None;
            }
        }
        for (k, v) in added {
            push(k.as_bytes(), &encode_record(v), &mut data);
        }
        if data.len() > u32::MAX as usize || count > u32::MAX as usize {
            return None;
        }
        let mut out = Vec::with_capacity(9 + ids.len() + offs.len() + data.len() + 32);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(count as u32).to_le_bytes());
        out.extend_from_slice(&ids);
        out.extend_from_slice(&offs);
        out.extend_from_slice(&data);
        let sum = blake3::hash(&out);
        out.extend_from_slice(sum.as_bytes());
        Some(out)
    }
}

fn parse_record(rec: &[u8]) -> Vec<Filter> {
    let np = rec[0] as usize;
    let mut p = 1;
    let mut out = Vec::with_capacity(np);
    for _ in 0..np {
        let l = u32::from_le_bytes(rec[p..p + 4].try_into().unwrap());
        p += 4;
        if l == UNKNOWN {
            out.push(None);
        } else {
            out.push(Some(rec[p..p + l as usize].to_vec()));
            p += l as usize;
        }
    }
    out
}

fn encode_record(fs: &[Filter]) -> Vec<u8> {
    let mut d = vec![fs.len() as u8];
    for f in fs {
        match f {
            None => d.extend_from_slice(&UNKNOWN.to_le_bytes()),
            Some(b) => {
                d.extend_from_slice(&(b.len() as u32).to_le_bytes());
                d.extend_from_slice(b);
            }
        }
    }
    d
}

impl Repo {
    pub(crate) fn pf_path(&self, zone: &str) -> std::path::PathBuf {
        self.common.join(PF_DIR).join(cg_name(zone))
    }

    pub(crate) fn load_path_blooms(&self, zone: &str) -> PathBlooms {
        PathBlooms::open(&self.pf_path(zone))
    }

    pub(crate) fn save_path_blooms(&self, zone: &str, b: &PathBlooms) {
        let _ = fs::create_dir_all(self.common.join(PF_DIR));
        if let Some(bytes) = b.encode() {
            let _ = write_cache_atomic(&self.pf_path(zone), &bytes); // best effort: it is only a cache
        }
    }

    /// The filters of `c` (one per parent) computed from the trees; `None` for a root commit.
    pub(crate) fn compute_filters(&self, c: &Commit) -> Result<Option<Vec<Filter>>> {
        if c.parents.is_empty() {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(c.parents.len());
        for p in &c.parents {
            let pr = self.commit_obj(p)?.root;
            let d = treeops::diff_trees_files(&self.store, Some(pr), Some(c.root), "")?;
            let f = if d.old.len() + d.new.len() > 2 * MAX_PATHS {
                None
            } else {
                build_filter(d.old.keys().chain(d.new.keys()))
            };
            out.push(f);
        }
        Ok(Some(out))
    }

    /// Check every file under `.vcs/path-filters` against the real commits; one message per problem.
    pub(crate) fn fsck_path_filters(&self, problems: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(self.common.join(PF_DIR)) else {
            return;
        };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.contains('.') {
                continue;
            }
            if fs::metadata(&path).is_err() {
                continue;
            }
            // lazily opened, then checked completely here (the only place that pays for the whole file)
            let ok = MapFile::open(&path, false)
                .ok()
                .and_then(|m| PathBlooms::from_map(m, 0))
                .filter(|b| b.verify_full());
            let Some(b) = ok.filter(|b| b.count > 0) else {
                problems.push(format!(
                    "path-filters {name}: unreadable, truncated, corrupt or empty (it is only a cache and is rebuilt on next use)"
                ));
                continue;
            };
            b.for_each(|id, f| match self.commit_obj(&id) {
                Ok(c) if cg_name(&c.zone) == name => match self.compute_filters(&c) {
                    Ok(Some(want)) if want == f => {}
                    Ok(None) => problems.push(format!("path-filters {name}: {id} is a root commit")),
                    Ok(Some(_)) => problems.push(format!("path-filters {name}: wrong filter for {id}")),
                    Err(e) => problems.push(format!("path-filters {name}: cannot check {id}: {e}")),
                },
                Ok(_) => problems.push(format!("path-filters {name}: {id} belongs to another zone")),
                Err(_) => problems.push(format!("path-filters {name}: {id} is not a readable commit")),
            });
        }
    }
}

impl Repo {
    /// Build the changed-path filters of every commit in each zone's commit-graph (all cores), so the
    /// first path walk after a gc starts with them instead of computing them on the fly. Commits that
    /// already have filters are kept. Best effort: it is only a cache. Returns how many commits got new
    /// filters. Run by `gc` (not by import, which would pay for it before anyone asks).
    pub fn build_path_filters(&self) -> usize {
        if !self.use_commit_graph {
            return 0;
        }
        let Ok(refs) = self.refs() else { return 0 };
        let mut done = BTreeSet::new();
        let mut total = 0;
        for ((zone, _), head) in &refs.branches {
            if !done.insert(zone.clone()) {
                continue;
            }
            let Some(g) = self.commit_graph(&[*head]) else { continue };
            let mut blooms = self.load_path_blooms(zone);
            let todo: Vec<ObjectId> = g.iter_ids().filter(|id| blooms.get(id).is_none()).collect();
            if todo.is_empty() {
                continue;
            }
            let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(16);
            let per = todo.len().div_ceil(threads).max(1);
            let results: Vec<Vec<(ObjectId, Vec<Filter>)>> = std::thread::scope(|sc| {
                let hs: Vec<_> = todo
                    .chunks(per)
                    .map(|ch| {
                        sc.spawn(move || {
                            let mut out = Vec::new();
                            for id in ch {
                                if let Ok(c) = self.commit_obj(id) {
                                    if let Ok(Some(f)) = self.compute_filters(&c) {
                                        out.push((*id, f));
                                    }
                                }
                            }
                            out
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            });
            for (id, f) in results.into_iter().flatten() {
                blooms.insert(id, f);
                total += 1;
            }
            if blooms.pending() > 0 {
                self.save_path_blooms(zone, &blooms);
            }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn oid(n: u32) -> ObjectId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_be_bytes());
        b[4] = 7;
        ObjectId::from_bytes(b)
    }

    fn filt() -> impl Strategy<Value = Filter> {
        prop_oneof![
            Just(None),
            Just(Some(Vec::new())),
            proptest::collection::vec(any::<u8>(), 1..40).prop_map(Some)
        ]
    }

    fn model() -> impl Strategy<Value = BTreeMap<u32, Vec<Filter>>> {
        proptest::collection::btree_map(0u32..200, proptest::collection::vec(filt(), 1..4), 0..60)
    }

    fn lazy(bytes: Vec<u8>) -> Option<PathBlooms> {
        PathBlooms::from_map(MapFile::from_vec(bytes), 0)
    }

    proptest! {
        /// Lazy reader (nothing verified at open) == fully verified reader == the model, for hits, misses and iteration.
        #[test]
        fn lazy_matches_model(m in model(), extra in model()) {
            let bytes = {
                let mut b = PathBlooms::default();
                for (k, v) in &m { b.insert(oid(*k), v.clone()); }
                b.encode().unwrap()
            };
            let eager = PathBlooms::from_bytes(bytes.clone());
            let mut l = lazy(bytes).unwrap();
            prop_assert_eq!(eager.has_file(), !m.is_empty());
            for k in 0..210u32 {
                prop_assert_eq!(l.get(&oid(k)), m.get(&k).cloned());
                prop_assert_eq!(eager.get(&oid(k)), m.get(&k).cloned());
            }
            prop_assert!(l.verify_full());
            // new filters override / extend, and the merged encoding equals the encoding of the merged model
            let mut want = m.clone();
            for (k, v) in &extra {
                l.insert(oid(*k), v.clone());
                want.insert(*k, v.clone());
            }
            let mut seen = Vec::new();
            l.for_each(|id, f| seen.push((id, f)));
            let want_seen: Vec<_> = want.iter().map(|(k, v)| (oid(*k), v.clone())).collect();
            prop_assert_eq!(seen, want_seen);
            let merged = l.encode().unwrap();
            let mut fresh = PathBlooms::default();
            for (k, v) in &want { fresh.insert(oid(*k), v.clone()); }
            prop_assert_eq!(merged, fresh.encode().unwrap());
        }

        /// A flipped byte never panics a lazily opened reader; `verify_full` and the eager path reject it.
        #[test]
        fn corruption_degrades_safely(m in model(), pos in any::<usize>(), bit in 0u8..8) {
            let mut b = PathBlooms::default();
            for (k, v) in &m { b.insert(oid(*k), v.clone()); }
            let mut bytes = b.encode().unwrap();
            let i = pos % bytes.len();
            bytes[i] ^= 1 << bit;
            prop_assert!(PathBlooms::from_bytes(bytes.clone()).count == 0);
            if let Some(l) = lazy(bytes) {
                prop_assert!(!l.verify_full());
                for k in 0..210u32 { let _ = l.get(&oid(k)); }
                l.for_each(|_, _| {});
                let _ = l.encode();
            }
        }
    }

    #[test]
    fn tiny_and_truncated_files() {
        // empty set and a single record
        let empty = PathBlooms::default().encode().unwrap();
        let e = PathBlooms::from_bytes(empty.clone());
        assert!(!e.has_file() && e.get(&oid(1)).is_none());
        assert!(lazy(empty).unwrap().verify_full());
        let mut one = PathBlooms::default();
        one.insert(oid(5), vec![Some(vec![1, 2, 3]), None]);
        let bytes = one.encode().unwrap();
        assert_eq!(
            PathBlooms::from_bytes(bytes.clone()).get(&oid(5)),
            Some(vec![Some(vec![1, 2, 3]), None])
        );
        // every truncation: never panics, never verifies, never yields a wrong record
        for len in 0..bytes.len() {
            let t = bytes[..len].to_vec();
            assert_eq!(PathBlooms::from_bytes(t.clone()).count, 0, "eager len {len}");
            if let Some(l) = lazy(t) {
                assert!(!l.verify_full(), "len {len}");
                if let Some(f) = l.get(&oid(5)) {
                    assert_eq!(f, vec![Some(vec![1, 2, 3]), None]);
                }
            }
        }
        assert!(lazy(b"AGPF".to_vec()).is_none());
        assert!(lazy(Vec::new()).is_none());
    }

    #[test]
    fn open_from_file_and_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = PathBlooms::default();
        b.insert(oid(1), vec![Some(vec![9])]);
        let p = dir.path().join("pf");
        std::fs::write(&p, b.encode().unwrap()).unwrap();
        assert_eq!(PathBlooms::open(&p).get(&oid(1)), Some(vec![Some(vec![9])]));
        assert!(!PathBlooms::open(&dir.path().join("nope")).has_file());
        std::fs::write(&p, b"garbage").unwrap();
        assert!(!PathBlooms::open(&p).has_file());
    }
}
