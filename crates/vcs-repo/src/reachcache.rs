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
use std::fs;
use vcs_objects::ObjectId;

const MAGIC: &[u8; 4] = b"AGRB";
const VERSION: u8 = 1;
const HEADER: usize = 4 + 1 + 4 + 4;

pub(crate) struct ReachCache {
    raw: Vec<u8>,
    ns: usize,
    nr: usize,
}

impl ReachCache {
    pub(crate) fn from_bytes(raw: Vec<u8>) -> Option<ReachCache> {
        if raw.len() < HEADER + 32 || &raw[..4] != MAGIC || raw[4] != VERSION {
            return None;
        }
        let (body, sum) = raw.split_at(raw.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return None;
        }
        let ns = u32::from_le_bytes(raw[5..9].try_into().ok()?) as usize;
        let nr = u32::from_le_bytes(raw[9..13].try_into().ok()?) as usize;
        let bits = ns.div_ceil(8);
        let want = HEADER
            .checked_add(ns.checked_mul(32)?)?
            .checked_add(bits.checked_mul(2)?)?
            .checked_add(nr.checked_mul(32)?)?;
        (want == body.len()).then_some(ReachCache { raw, ns, nr })
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
        ReachCache::from_bytes(fs::read(self.reach_path()).ok()?)
    }

    pub(crate) fn save_reach_cache(&self, bytes: &[u8]) {
        let _ = crate::meta::write_cache_nosync(&self.reach_path(), bytes); // best effort: only a cache
    }
}
