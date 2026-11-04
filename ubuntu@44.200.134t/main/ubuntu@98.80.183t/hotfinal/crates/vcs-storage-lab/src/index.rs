//! Object-location indexes: id -> (pack, offset, length). All in-memory candidates share one trait; the
//! on-disk ones (B-tree, LSM, SQLite) live in `ondisk.rs`.
use crate::util::{u64_at, Id};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Loc {
    pub pack: u32,
    pub len: u32,
    pub off: u64,
}
pub trait LocIndex {
    fn name(&self) -> String;
    fn get(&self, id: &Id) -> Option<Loc>;
    /// Heap bytes held (what must be resident to answer lookups).
    fn bytes(&self) -> usize;
}

/// Input: objects grouped by pack, each pack's entries sorted by id (as `write_pack` does).
pub struct Packs {
    pub packs: Vec<Vec<(Id, Loc)>>,
}
impl Packs {
    pub fn total(&self) -> usize {
        self.packs.iter().map(|p| p.len()).sum()
    }
    /// Split `ids` into `p` packs. Skewed: pack sizes follow a geometric-ish law like real repacks
    /// (one big base pack, many small incremental ones) when `skew` is true.
    pub fn split(ids: &[Id], p: usize, skew: bool) -> Packs {
        let mut cuts = vec![0usize];
        if skew && p > 1 {
            // first pack takes 90%, the rest share 10% evenly
            let big = ids.len() * 9 / 10;
            cuts.push(big);
            for i in 1..p {
                cuts.push(big + (ids.len() - big) * i / (p - 1));
            }
        } else {
            for i in 1..=p {
                cuts.push(ids.len() * i / p);
            }
        }
        let mut packs = Vec::new();
        for (pi, w) in cuts.windows(2).enumerate() {
            let mut v: Vec<(Id, Loc)> = ids[w[0]..w[1]]
                .iter()
                .enumerate()
                .map(|(k, id)| (*id, Loc { pack: pi as u32, len: 100, off: 16 + 100 * k as u64 }))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            packs.push(v);
        }
        Packs { packs }
    }
}

/// Today's layout: P independent flat sorted arrays; lookup tries packs in order, binary search in each.
pub struct PerPackFlat {
    packs: Vec<Vec<(Id, u64, u32)>>,
}
impl PerPackFlat {
    pub fn build(p: &Packs) -> Self {
        PerPackFlat { packs: p.packs.iter().map(|v| v.iter().map(|(i, l)| (*i, l.off, l.len)).collect()).collect() }
    }
}
impl LocIndex for PerPackFlat {
    fn name(&self) -> String {
        format!("per_pack_flat_x{}", self.packs.len())
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        for (pi, p) in self.packs.iter().enumerate() {
            if let Ok(i) = p.binary_search_by(|e| e.0.cmp(id)) {
                return Some(Loc { pack: pi as u32, off: p[i].1, len: p[i].2 });
            }
        }
        None
    }
    fn bytes(&self) -> usize {
        self.packs.iter().map(|p| p.len() * std::mem::size_of::<(Id, u64, u32)>()).sum()
    }
}

/// Per-pack with a 256-entry fanout table (git .idx v2 style): first id byte narrows the binary search.
pub struct PerPackFanout {
    packs: Vec<(Vec<u32>, Vec<Id>, Vec<(u64, u32)>)>,
}
impl PerPackFanout {
    pub fn build(p: &Packs) -> Self {
        let packs = p
            .packs
            .iter()
            .map(|v| {
                let mut fan = vec![0u32; 256];
                for (id, _) in v {
                    fan[id[0] as usize] += 1;
                }
                for i in 1..256 {
                    fan[i] += fan[i - 1];
                }
                (fan, v.iter().map(|e| e.0).collect(), v.iter().map(|e| (e.1.off, e.1.len)).collect())
            })
            .collect();
        PerPackFanout { packs }
    }
}
impl LocIndex for PerPackFanout {
    fn name(&self) -> String {
        format!("per_pack_fanout256_x{}", self.packs.len())
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        for (pi, (fan, ids, locs)) in self.packs.iter().enumerate() {
            let lo = if id[0] == 0 { 0 } else { fan[id[0] as usize - 1] as usize };
            let hi = fan[id[0] as usize] as usize;
            if let Ok(i) = ids[lo..hi].binary_search(id) {
                let (off, len) = locs[lo + i];
                return Some(Loc { pack: pi as u32, off, len });
            }
        }
        None
    }
    fn bytes(&self) -> usize {
        self.packs.iter().map(|(f, i, l)| f.len() * 4 + i.len() * 32 + l.len() * 16).sum()
    }
}

/// One global sorted array with a 2^16-entry fanout: a single probe regardless of pack count.
pub struct GlobalSorted {
    fan: Vec<u32>,
    ids: Vec<Id>,
    locs: Vec<Loc>,
}
impl GlobalSorted {
    pub fn build(p: &Packs) -> Self {
        let mut all: Vec<(Id, Loc)> = p.packs.iter().flatten().copied().collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let mut fan = vec![0u32; 65537];
        for (id, _) in &all {
            fan[(u16::from_be_bytes([id[0], id[1]]) as usize) + 1] += 1;
        }
        for i in 1..fan.len() {
            fan[i] += fan[i - 1];
        }
        GlobalSorted { fan, ids: all.iter().map(|e| e.0).collect(), locs: all.iter().map(|e| e.1).collect() }
    }
}
impl LocIndex for GlobalSorted {
    fn name(&self) -> String {
        "global_sorted_fanout64k".into()
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        let b = u16::from_be_bytes([id[0], id[1]]) as usize;
        let (lo, hi) = (self.fan[b] as usize, self.fan[b + 1] as usize);
        self.ids[lo..hi].binary_search(id).ok().map(|i| self.locs[lo + i])
    }
    fn bytes(&self) -> usize {
        self.fan.len() * 4 + self.ids.len() * 32 + self.locs.len() * std::mem::size_of::<Loc>()
    }
}

/// Global open-addressing hash table keyed by the first 8 id bytes (linear probing, load ~0.75).
/// Slot = 16 B: 64-bit key + packed location (pack:16 | off:36 in 8-byte units... kept simple: pack:12, off:40, len:12 bits
/// is NOT enough for len, so length is elided and read from the pack's own 4-byte length prefix).
/// A hit is a candidate: the reader hashes the bytes it reads, so a 64-bit prefix collision is caught for free.
pub struct GlobalHash {
    slots: Vec<(u64, u64)>,
    mask: usize,
}
impl GlobalHash {
    pub fn build(p: &Packs) -> Self {
        let n = p.total();
        let cap = ((n as f64 / 0.75) as usize).next_power_of_two();
        let mut slots = vec![(0u64, 0u64); cap];
        let mask = cap - 1;
        for pk in &p.packs {
            for (id, l) in pk {
                let k = u64_at(id, 0) | 1; // 0 = empty
                let mut i = (u64_at(id, 1) as usize) & mask;
                while slots[i].0 != 0 {
                    i = (i + 1) & mask;
                }
                slots[i] = (k, (l.pack as u64) << 48 | l.off);
            }
        }
        GlobalHash { slots, mask }
    }
}
impl LocIndex for GlobalHash {
    fn name(&self) -> String {
        "global_hash_prefix64".into()
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        let k = u64_at(id, 0) | 1;
        let mut i = (u64_at(id, 1) as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s.0 == 0 {
                return None;
            }
            if s.0 == k {
                return Some(Loc { pack: (s.1 >> 48) as u32, off: s.1 & ((1 << 48) - 1), len: 0 });
            }
            i = (i + 1) & self.mask;
        }
    }
    fn bytes(&self) -> usize {
        self.slots.len() * 16
    }
}

/// Minimal perfect hash over the 64-bit id prefixes (boomphf BBHash) + a 16-byte slot per key holding a
/// 32-bit fingerprint, pack and offset. Absent ids are rejected by the fingerprint with probability 1 - 2^-32.
pub struct Mph {
    f: boomphf::Mphf<u64>,
    slots: Vec<(u32, u32, u64)>, // (fingerprint, pack, off)
    pub gamma: f64,
    mphf_bytes: usize,
}
impl Mph {
    pub fn build(p: &Packs, gamma: f64) -> Self {
        let keys: Vec<u64> = p.packs.iter().flatten().map(|(i, _)| u64_at(i, 0)).collect();
        let before = crate::util::live_bytes();
        let f = boomphf::Mphf::new_parallel(gamma, &keys, None);
        let mphf_bytes = crate::util::live_bytes().saturating_sub(before);
        let mut slots = vec![(0u32, 0u32, 0u64); keys.len()];
        for pk in &p.packs {
            for (id, l) in pk {
                let s = f.hash(&u64_at(id, 0)) as usize;
                slots[s] = ((u64_at(id, 2) & 0xffff_ffff) as u32, l.pack, l.off);
            }
        }
        Mph { f, slots, gamma, mphf_bytes }
    }
}
impl LocIndex for Mph {
    fn name(&self) -> String {
        format!("mph_bbhash_g{}_fp32", self.gamma)
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        let s = self.f.try_hash(&u64_at(id, 0))? as usize;
        let e = self.slots.get(s)?;
        if e.0 == (u64_at(id, 2) & 0xffff_ffff) as u32 {
            Some(Loc { pack: e.1, off: e.2, len: 0 })
        } else {
            None
        }
    }
    fn bytes(&self) -> usize {
        // mphf_bytes is the retained heap of the Mphf, measured with the counting allocator
        self.slots.len() * 16 + self.mphf_bytes
    }
}

/// std BTreeMap (in-memory B-tree) as a control for the on-disk B-tree.
pub struct BTreeMem(BTreeMap<Id, Loc>);
impl BTreeMem {
    pub fn build(p: &Packs) -> Self {
        BTreeMem(p.packs.iter().flatten().copied().collect())
    }
}
impl LocIndex for BTreeMem {
    fn name(&self) -> String {
        "btreemap_mem".into()
    }
    fn get(&self, id: &Id) -> Option<Loc> {
        self.0.get(id).copied()
    }
    fn bytes(&self) -> usize {
        // B=6 nodes hold 5..=11 entries; ~ 2/3 full average => key+value per slot / 0.67 (estimate, not measured)
        (self.0.len() as f64 * (32.0 + std::mem::size_of::<Loc>() as f64) / 0.67) as usize
    }
}
