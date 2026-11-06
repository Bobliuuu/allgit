//! Reachability bitmap for gc mark: `.vcs/gc-reach`, a pure cache written by a destructive sweep.
//!
//! Objects are immutable and content-addressed, so the closure of a root never changes. After a
//! sweep we store the sorted list `S` of every object that existed, a bitmap over `S` of the objects
//! that were reachable (`M`, closed under "children": every child of a marked object is marked), a
//! bitmap of the Transaction objects (which are roots) and the sorted roots `R` the mark started
//! from. The next gc whose roots are a superset of `R` knows `closure(R_now) = M + closure(new roots)`:
//! it reads and decompresses only objects that are new since the last sweep, instead of every
//! object (the mark phase and the transaction scan both read everything today). If any old root is
//! gone (a deleted branch, a changed index entry) the cache is not used and the full mark runs and
//! rewrites it; the cache can never make gc delete a reachable object, only fall back.
//!
//! File: `"AGRB" || version:u8 || nS:u32 || nR:u32 || nS*id:32 (sorted) || ceil(nS/8) marked bits ||
//! ceil(nS/8) tx bits || nR*id:32 (sorted) || blake3(all preceding)`. Anything wrong with it means
//! "not there". Written only when the mark found no missing referenced object.

use crate::repo::Repo;
use std::path::Path;
use vcs_objects::ObjectId;
use vcs_store::mapfile::MapFile;

const MAGIC: &[u8; 4] = b"AGRB";
const VERSION: u8 = 1;
const HEADER: usize = 4 + 1 + 4 + 4;

pub(crate) struct ReachCache {
    raw: MapFile,
    ns: usize,
    nr: usize,
}

/// Files up to this size are checksummed completely when opened lazily.
pub(crate) const EAGER_VERIFY_MAX: usize = 4 << 20;

impl ReachCache {
    /// Read and completely verify bytes already in memory.
    #[cfg(test)]
    pub(crate) fn from_bytes(raw: Vec<u8>) -> Option<ReachCache> {
        Self::from_map(MapFile::from_vec(raw), usize::MAX)
    }

    /// Map `path` without reading it: O(1) time and no anonymous memory at any size. Only the header and the
    /// exact length are checked (and, up to `EAGER_VERIFY_MAX`, the checksum); call [`verify_full`] before
    /// trusting the bits for anything destructive (gc does) and in fsck.
    pub(crate) fn open(path: &Path) -> Option<ReachCache> {
        Self::from_map(MapFile::open(path, true).ok()?, EAGER_VERIFY_MAX)
    }

    /// `open` followed by the whole-file checksum.
    pub(crate) fn open_verified(path: &Path) -> Option<ReachCache> {
        let c = Self::open(path)?;
        c.verify_full().then_some(c)
    }

    pub(crate) fn from_map(raw: MapFile, eager_max: usize) -> Option<ReachCache> {
        if raw.len() < HEADER + 32 || &raw[..4] != MAGIC || raw[4] != VERSION {
            return None;
        }
        let ns = u32::from_le_bytes(raw[5..9].try_into().ok()?) as usize;
        let nr = u32::from_le_bytes(raw[9..13].try_into().ok()?) as usize;
        let bits = ns.div_ceil(8);
        let want = HEADER
            .checked_add(ns.checked_mul(32)?)?
            .checked_add(bits.checked_mul(2)?)?
            .checked_add(nr.checked_mul(32)?)?;
        if want.checked_add(32)? != raw.len() {
            return None;
        }
        let c = ReachCache { raw, ns, nr };
        if c.raw.len() <= eager_max && !c.verify_full() {
            return None;
        }
        Some(c)
    }

    /// The whole-file checksum (what `open` skips for big files). O(size) time, no memory.
    pub(crate) fn verify_full(&self) -> bool {
        let (body, sum) = self.raw.split_at(self.raw.len() - 32);
        vcs_store::mapfile::blake3_big(body).as_bytes() == sum
    }

    fn id_at(&self, i: usize) -> &[u8] {
        &self.raw[HEADER + i * 32..HEADER + i * 32 + 32]
    }

    fn pos(&self, id: &ObjectId) -> Option<usize> {
        let (mut lo, mut hi) = (0, self.ns);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.id_at(mid).cmp(id.as_bytes().as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    fn bit(&self, which: usize, i: usize) -> bool {
        let base = HEADER + self.ns * 32 + which * self.ns.div_ceil(8);
        self.raw[base + i / 8] & (1 << (i % 8)) != 0
    }

    /// Was this object scanned (so its transaction-ness is known)?
    pub(crate) fn scanned(&self, id: &ObjectId) -> bool {
        self.pos(id).is_some()
    }

    /// Was it reachable at the last sweep (so is its whole closure marked)?
    pub(crate) fn marked(&self, id: &ObjectId) -> bool {
        self.pos(id).is_some_and(|i| self.bit(0, i))
    }

    pub(crate) fn is_tx(&self, id: &ObjectId) -> bool {
        self.pos(id).is_some_and(|i| self.bit(1, i))
    }

    /// Every transaction recorded.
    pub(crate) fn txs(&self) -> Vec<ObjectId> {
        (0..self.ns)
            .filter(|&i| self.bit(1, i))
            .map(|i| ObjectId::from_bytes(self.id_at(i).try_into().unwrap()))
            .collect()
    }

    /// Is every root of the last mark among `roots` (sorted, deduplicated)?
    pub(crate) fn roots_within(&self, roots: &[ObjectId]) -> bool {
        let base = HEADER + self.ns * 32 + 2 * self.ns.div_ceil(8);
        (0..self.nr).all(|i| {
            let r = &self.raw[base + i * 32..base + i * 32 + 32];
            roots.binary_search_by(|x| x.as_bytes().as_slice().cmp(r)).is_ok()
        })
    }
}

/// `ids` sorted; `marked`/`tx` parallel to it; `roots` sorted and deduplicated.
pub(crate) fn encode(ids: &[ObjectId], marked: &[bool], tx: &[bool], roots: &[ObjectId]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + ids.len() * 33 + roots.len() * 32 + 32);
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    out.extend_from_slice(&(roots.len() as u32).to_le_bytes());
    for id in ids {
        out.extend_from_slice(id.as_bytes());
    }
    for flags in [marked, tx] {
        let mut bits = vec![0u8; ids.len().div_ceil(8)];
        for (i, f) in flags.iter().enumerate() {
            if *f {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        out.extend_from_slice(&bits);
    }
    for r in roots {
        out.extend_from_slice(r.as_bytes());
    }
    let sum = blake3::hash(&out);
    out.extend_from_slice(sum.as_bytes());
    out
}

impl Repo {
    fn reach_path(&self) -> std::path::PathBuf {
        self.common.join("gc-reach")
    }

    pub(crate) fn load_reach_cache(&self) -> Option<ReachCache> {
        // gc deletes what the cache says is reachable, so it checks the whole file; fsck and probes may open lazily
        ReachCache::open_verified(&self.reach_path())
    }

    /// Check `gc-reach` completely (checksum), one message per problem.
    pub(crate) fn fsck_reach_cache(&self, problems: &mut Vec<String>) {
        let path = self.reach_path();
        if !path.exists() {
            return;
        }
        if ReachCache::open(&path).is_none_or(|c| !c.verify_full()) {
            problems.push("gc-reach: unreadable, truncated or checksum mismatch (it is only a cache and is rebuilt by the next gc)".into());
        }
    }

    pub(crate) fn save_reach_cache(&self, bytes: &[u8]) {
        let _ = crate::meta::write_cache_nosync(&self.reach_path(), bytes); // best effort: only a cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn oid(n: u32) -> ObjectId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_be_bytes());
        b[9] = 3;
        ObjectId::from_bytes(b)
    }

    /// (ids sorted, marked, tx, roots sorted)
    type Model = (Vec<u32>, Vec<bool>, Vec<bool>, Vec<u32>);

    fn model() -> impl Strategy<Value = Model> {
        proptest::collection::btree_set(0u32..500, 0..80).prop_flat_map(|set| {
            let ids: Vec<u32> = set.into_iter().collect();
            let n = ids.len();
            (
                Just(ids),
                proptest::collection::vec(any::<bool>(), n),
                proptest::collection::vec(any::<bool>(), n),
                proptest::collection::btree_set(0u32..500, 0..8).prop_map(|s| s.into_iter().collect::<Vec<u32>>()),
            )
        })
    }

    fn bytes_of(m: &Model) -> Vec<u8> {
        let ids: Vec<ObjectId> = m.0.iter().map(|k| oid(*k)).collect();
        let roots: Vec<ObjectId> = m.3.iter().map(|k| oid(*k)).collect();
        encode(&ids, &m.1, &m.2, &roots)
    }

    proptest! {
        /// Lazily mapped reader == fully verified reader == the model on every query.
        #[test]
        fn lazy_matches_model(m in model()) {
            let bytes = bytes_of(&m);
            let eager = ReachCache::from_bytes(bytes.clone()).unwrap();
            let lazy = ReachCache::from_map(MapFile::from_vec(bytes), 0).unwrap();
            prop_assert!(lazy.verify_full());
            for k in 0..520u32 {
                for c in [&eager, &lazy] {
                    let at = m.0.iter().position(|x| *x == k);
                    prop_assert_eq!(c.scanned(&oid(k)), at.is_some());
                    prop_assert_eq!(c.marked(&oid(k)), at.is_some_and(|i| m.1[i]));
                    prop_assert_eq!(c.is_tx(&oid(k)), at.is_some_and(|i| m.2[i]));
                }
            }
            let want_tx: Vec<ObjectId> = m.0.iter().zip(&m.2).filter(|(_, t)| **t).map(|(k, _)| oid(*k)).collect();
            prop_assert_eq!(lazy.txs(), want_tx);
            let all_roots: Vec<ObjectId> = (0..520u32).map(oid).collect();
            prop_assert!(lazy.roots_within(&all_roots));
            if let Some(r) = m.3.first() {
                let without: Vec<ObjectId> = all_roots.iter().filter(|x| **x != oid(*r)).copied().collect();
                prop_assert!(!lazy.roots_within(&without));
            }
        }

        /// A flipped byte: the eager reader rejects it; a lazy one either rejects at open or fails
        /// `verify_full`, and its lookups never panic.
        #[test]
        fn corruption_degrades_safely(m in model(), pos in any::<usize>(), bit in 0u8..8) {
            let mut bytes = bytes_of(&m);
            let i = pos % bytes.len();
            bytes[i] ^= 1 << bit;
            prop_assert!(ReachCache::from_bytes(bytes.clone()).is_none());
            if let Some(l) = ReachCache::from_map(MapFile::from_vec(bytes), 0) {
                prop_assert!(!l.verify_full());
                for k in 0..520u32 {
                    let _ = (l.scanned(&oid(k)), l.marked(&oid(k)), l.is_tx(&oid(k)));
                }
                let _ = l.txs();
                let _ = l.roots_within(&[oid(1)]);
            }
        }
    }

    #[test]
    fn tiny_sizes_and_truncation() {
        let empty = bytes_of(&(vec![], vec![], vec![], vec![]));
        let e = ReachCache::from_bytes(empty).unwrap();
        assert!(!e.scanned(&oid(0)) && e.txs().is_empty() && e.roots_within(&[]));
        let m: Model = (vec![3], vec![true], vec![false], vec![3]);
        let bytes = bytes_of(&m);
        assert!(ReachCache::from_bytes(bytes.clone()).unwrap().marked(&oid(3)));
        for len in 0..bytes.len() {
            // the exact-length check rejects every truncation even without reading the checksum
            assert!(
                ReachCache::from_map(MapFile::from_vec(bytes[..len].to_vec()), 0).is_none(),
                "len {len}"
            );
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(ReachCache::from_map(MapFile::from_vec(longer), 0).is_none());
    }

    #[test]
    fn open_from_file_lazy_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let m: Model = ((0..50).collect(), vec![true; 50], vec![false; 50], vec![0, 49]);
        let p = dir.path().join("gc-reach");
        let mut bytes = bytes_of(&m);
        std::fs::write(&p, &bytes).unwrap();
        assert!(ReachCache::open(&p).unwrap().marked(&oid(7)));
        assert!(ReachCache::open_verified(&p).unwrap().marked(&oid(7)));
        // flip one marked bit: small files are verified at open, so even the lazy open refuses
        let at = HEADER + 50 * 32;
        bytes[at] ^= 0x04;
        std::fs::write(&p, &bytes).unwrap();
        assert!(ReachCache::open(&p).is_none() && ReachCache::open_verified(&p).is_none());
        assert!(ReachCache::open(&dir.path().join("none")).is_none());
    }
}
