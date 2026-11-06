//! On-disk commit-graph: for every commit its parents, commit time and generation number.
//!
//! It is a pure cache. Everything in it can be recomputed from the commit objects, so a
//! file that fails any check is simply discarded and rebuilt, and
//! [`CommitGraphFile::verify_against`] lets fsck compare it with the real commits.
//!
//! Format v2 (little endian, column oriented so that any commit is reachable in O(1) without
//! decoding the file): `"AGCG" || 2:u8 || 0:3 || count:u32 || npar:u32 || blake3(first 16 bytes)[..16]`
//! (32 byte header), then `count * id:32` (sorted, unique), `count * time:u64`, `count * generation:u32`,
//! `count * parents_end:u32` (running total of parent slots, so commit `i`'s parents are slots
//! `end[i-1]..end[i]`), `npar * parent:u32` (indexes into the id table), then `blake3(all preceding bytes)`.
//!
//! Format v1 (`"AGCG" || 1 || count:u32 || count * entry || checksum`, entries of variable length) is
//! still read: it is fully validated and converted to v2 in memory once ([`CommitGraphFile::upgraded`]
//! tells the caller to rewrite the file).
//!
//! Opening a v2 file ([`CommitGraphFile::open`]) maps it and checks only the header and the exact file
//! length, so it costs O(1) time and no anonymous memory at any size; queries touch just the pages they
//! need. Files up to [`EAGER_VERIFY_MAX`] bytes are verified completely at open (a millisecond), bigger
//! ones on demand with [`CommitGraphFile::verify_full`] (fsck). On a lazily opened file every node a
//! query visits is checked locally (parent index in range, generation strictly above its parents');
//! a violation makes the query answer `None` ("use the object walk") instead of a wrong result.

use crate::base::CommitGraph;
use crate::error::MergeError;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use vcs_objects::ObjectId;
use vcs_store::mapfile::MapFile;

const MAGIC: &[u8; 4] = b"AGCG";
const VERSION_V1: u8 = 1;
const VERSION: u8 = 2;
const HDR: usize = 32;
/// Commits with more parents than this are not representable; building fails (caller falls back).
const MAX_PARENTS: usize = 255;
/// Files at most this big are completely verified when opened (checksum and structure).
pub const EAGER_VERIFY_MAX: usize = 4 << 20;

#[derive(Clone)]
pub struct CommitGraphFile {
    /// The v2 image: header and columns (and, for a file, the trailing checksum after `body_len`).
    buf: Arc<MapFile>,
    n: usize,
    npar: usize,
    o_time: usize,
    o_gen: usize,
    o_pend: usize,
    o_par: usize,
    body_len: usize,
    /// Set when a query met data that contradicts the format; sticky, shared by clones.
    bad: Arc<AtomicBool>,
    upgraded: bool,
}

impl Default for CommitGraphFile {
    fn default() -> Self {
        Self::from_parts(Arc::new(MapFile::from_vec(header(0, 0).to_vec())), 0, 0)
    }
}

impl std::fmt::Debug for CommitGraphFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CommitGraphFile({} commits, {} parent slots, {:?})", self.n, self.npar, self.buf)
    }
}

impl PartialEq for CommitGraphFile {
    fn eq(&self, o: &Self) -> bool {
        self.n == o.n && self.npar == o.npar && self.buf[..self.body_len] == o.buf[..o.body_len]
    }
}
impl Eq for CommitGraphFile {}

fn corrupt<T>(what: &str) -> Result<T, MergeError> {
    Err(MergeError::Corrupt(format!("commit-graph: {what}")))
}

fn header(n: usize, npar: usize) -> [u8; HDR] {
    let mut h = [0u8; HDR];
    h[..4].copy_from_slice(MAGIC);
    h[4] = VERSION;
    h[8..12].copy_from_slice(&(n as u32).to_le_bytes());
    h[12..16].copy_from_slice(&(npar as u32).to_le_bytes());
    let c = blake3::hash(&h[..16]);
    h[16..32].copy_from_slice(&c.as_bytes()[..16]);
    h
}

fn rd32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Size of the v2 image without the checksum, `None` on overflow.
fn body_size(n: usize, npar: usize) -> Option<usize> {
    HDR.checked_add(n.checked_mul(32 + 8 + 4 + 4)?)?.checked_add(npar.checked_mul(4)?)
}

impl CommitGraphFile {
    /// The 32-byte v2 header for `n` commits and `npar` parent slots (for writers of synthetic files).
    pub fn header_v2(n: usize, npar: usize) -> [u8; HDR] {
        header(n, npar)
    }

    fn from_parts(buf: Arc<MapFile>, n: usize, npar: usize) -> CommitGraphFile {
        let o_time = HDR + 32 * n;
        let o_gen = o_time + 8 * n;
        let o_pend = o_gen + 4 * n;
        let o_par = o_pend + 4 * n;
        CommitGraphFile {
            buf,
            n,
            npar,
            o_time,
            o_gen,
            o_pend,
            o_par,
            body_len: o_par + 4 * npar,
            bad: Arc::new(AtomicBool::new(false)),
            upgraded: false,
        }
    }

    /// Open a graph file. A v2 file is mapped (O(1), see the module docs); a v1 file is decoded and fully
    /// validated. Any problem is an error: the caller discards the file and rebuilds.
    pub fn open(path: &Path) -> Result<CommitGraphFile, MergeError> {
        let m = MapFile::open(path, true).map_err(|e| MergeError::Corrupt(format!("commit-graph: {e}")))?;
        Self::from_map_with(m, EAGER_VERIFY_MAX)
    }

    /// Like [`open`](Self::open) on a file image already in memory.
    pub fn from_map(m: MapFile) -> Result<CommitGraphFile, MergeError> {
        Self::from_map_with(m, EAGER_VERIFY_MAX)
    }

    /// [`from_map`](Self::from_map) with the size up to which the image is completely verified.
    pub fn from_map_with(m: MapFile, eager_max: usize) -> Result<CommitGraphFile, MergeError> {
        if m.len() < 5 || &m[..4] != MAGIC {
            return corrupt("bad magic or truncated");
        }
        match m[4] {
            VERSION_V1 => Self::decode_v1(&m),
            VERSION => {
                if m.len() < HDR + 32 {
                    return corrupt("truncated");
                }
                let n = rd32(&m, 8) as usize;
                let npar = rd32(&m, 12) as usize;
                if m[..HDR] != header(n, npar) || m[5..8] != [0, 0, 0] {
                    return corrupt("header checksum mismatch");
                }
                match body_size(n, npar).and_then(|b| b.checked_add(32)) {
                    Some(want) if want == m.len() => {}
                    _ => return corrupt("length does not match the header"),
                }
                let g = Self::from_parts(Arc::new(m), n, npar);
                if g.buf.len() <= eager_max {
                    g.verify_full()?;
                }
                Ok(g)
            }
            _ => corrupt("unknown version"),
        }
    }

    /// Did this graph come from an old-format file (so the caller should write it back as v2)?
    pub fn upgraded(&self) -> bool {
        self.upgraded
    }

    /// Did a query meet data that contradicts the format? The graph must then not be trusted.
    pub fn is_bad(&self) -> bool {
        self.bad.load(Ordering::Relaxed)
    }

    fn poison(&self) {
        self.bad.store(true, Ordering::Relaxed);
    }

    fn ids_slice(&self) -> &[[u8; 32]] {
        self.buf[HDR..HDR + 32 * self.n].as_chunks::<32>().0
    }
    fn time_at(&self, i: usize) -> u64 {
        u64::from_le_bytes(self.buf[self.o_time + 8 * i..][..8].try_into().unwrap())
    }
    fn gen_at(&self, i: usize) -> u32 {
        rd32(&self.buf, self.o_gen + 4 * i)
    }
    fn pend_at(&self, i: usize) -> usize {
        rd32(&self.buf, self.o_pend + 4 * i) as usize
    }
    /// Parent slot range of commit `i`, `None` (and poisoned) if the offsets are inconsistent.
    fn par_range(&self, i: usize) -> Option<(usize, usize)> {
        let a = if i == 0 { 0 } else { self.pend_at(i - 1) };
        let b = self.pend_at(i);
        if a <= b && b <= self.npar {
            Some((a, b))
        } else {
            self.poison();
            None
        }
    }
    /// Parents of commit `i` as table indexes. Out-of-range ones are dropped and poison the graph.
    fn par(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        let (a, b) = self.par_range(i).unwrap_or((0, 0));
        let n = self.n;
        self.buf[self.o_par + 4 * a..self.o_par + 4 * b]
            .as_chunks::<4>()
            .0
            .iter()
            .filter_map(move |c| {
                let p = u32::from_le_bytes(*c) as usize;
                if p < n && p != i {
                    Some(p)
                } else {
                    self.poison();
                    None
                }
            })
    }
    /// Generation of parent `p` of a commit with generation `gen_child`, for pushing it on a walk's heap. The walks
    /// order by generation, so a parent not strictly below its child contradicts the file: poison and refuse.
    fn parent_gen(&self, p: usize, gen_child: u32) -> Option<u32> {
        let g = self.gen_at(p);
        if g < gen_child {
            Some(g)
        } else {
            self.poison();
            None
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    pub fn index_of(&self, id: &ObjectId) -> Option<usize> {
        self.ids_slice().binary_search(id.as_bytes()).ok()
    }
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.index_of(id).is_some()
    }
    /// The commit at table position `i` (ids are sorted).
    pub fn id_at(&self, i: usize) -> ObjectId {
        ObjectId::from_bytes(self.ids_slice()[i])
    }
    /// Every commit id in sorted order.
    pub fn iter_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.ids_slice().iter().map(|b| ObjectId::from_bytes(*b))
    }
    /// Commit time (the author timestamp) recorded for `id`.
    pub fn time(&self, id: &ObjectId) -> Option<u64> {
        self.index_of(id).map(|i| self.time_at(i))
    }
    /// Parents of table entry `i`, as ids.
    fn parent_ids(&self, i: usize) -> Vec<ObjectId> {
        self.par(i).map(|p| self.id_at(p)).collect()
    }

    /// Number of commits reachable from `from` (itself included) that are not reachable from
    /// `hide` (itself included); `None` if either is not in the graph. Costs the size of that set
    /// plus the commits between it and `hide`, not the whole history behind `hide`.
    pub fn count_reachable_not_from(&self, from: &ObjectId, hide: &ObjectId) -> Option<usize> {
        self.range_ids(std::slice::from_ref(from), std::slice::from_ref(hide))
            .map(|v| v.len())
    }

    /// Best common ancestors of `a` and `b`, the same answer as [`crate::merge_base`] over this graph
    /// but on table indexes (no id hashing or maps). `None` if either is not in the graph (or the file
    /// contradicts itself).
    pub fn merge_bases_indexed(&self, a: &ObjectId, b: &ObjectId) -> Option<Vec<ObjectId>> {
        use std::collections::BinaryHeap;
        const P1: u8 = 1;
        const P2: u8 = 2;
        const STALE: u8 = 4;
        let (ia, ib) = (self.index_of(a)?, self.index_of(b)?);
        let mut flags = vec![0u8; self.n];
        let mut done = vec![false; self.n];
        // the table is sorted by id, so (generation, index) pops in the same order as (generation, id)
        let mut heap: BinaryHeap<(u32, usize)> = BinaryHeap::new();
        flags[ia] |= P1;
        flags[ib] |= P2;
        heap.push((self.gen_at(ia), ia));
        if ia != ib {
            heap.push((self.gen_at(ib), ib));
        }
        let mut results: Vec<usize> = Vec::new();
        while heap.iter().any(|(_, n)| flags[*n] & STALE == 0) {
            let (gn, n) = heap.pop()?;
            if done[n] {
                continue;
            }
            done[n] = true;
            let mut f = flags[n];
            if f & (P1 | P2 | STALE) == (P1 | P2) {
                results.push(n);
                f |= STALE;
                flags[n] = f;
            }
            for p in self.par(n) {
                let new = flags[p] | (f & (P1 | P2 | STALE));
                if new != flags[p] {
                    flags[p] = new;
                    if !done[p] {
                        if let Some(gp) = self.parent_gen(p, gn) {
                            heap.push((gp, p));
                        }
                    }
                }
            }
        }
        // drop candidates that are ancestors of other candidates
        let reaches = |anc: usize, desc: usize| -> bool {
            let ganc = self.gen_at(anc);
            if ganc > self.gen_at(desc) {
                return false;
            }
            let mut seen_set = HashSet::new();
            let mut stack = vec![desc];
            while let Some(c) = stack.pop() {
                if c == anc {
                    return true;
                }
                if !seen_set.insert(c) || self.gen_at(c) <= ganc {
                    continue;
                }
                stack.extend(self.par(c));
            }
            false
        };
        let mut out: Vec<ObjectId> = results
            .iter()
            .filter(|&&r| !results.iter().any(|&s| s != r && reaches(r, s)))
            .map(|&r| self.id_at(r))
            .collect();
        if self.is_bad() {
            return None;
        }
        out.sort();
        Some(out)
    }

    /// The commits reachable from any of `include` but from none of `exclude` (`include ^exclude`
    /// in git terms), unordered. Walks commits by decreasing generation number and stops as soon
    /// as no commit that could still be a result is waiting, so the cost follows the size of the
    /// answer plus the commits between it and the excluded history, not the whole history.
    /// `None` if some id is not in the graph.
    pub fn range_ids(&self, include: &[ObjectId], exclude: &[ObjectId]) -> Option<Vec<ObjectId>> {
        use std::collections::BinaryHeap;
        const INT: u8 = 1;
        const UN: u8 = 2;
        const QUEUED: u8 = 4;
        const LIVE: u8 = 8; // queued and not (yet) known uninteresting: counted in `live`
        let n = self.n;
        let mut flags = vec![0u8; n];
        let mut heap: BinaryHeap<(u32, usize)> = BinaryHeap::new();
        let mut live = 0usize;
        let mut seed = |flags: &mut Vec<u8>, i: usize, f: u8| {
            flags[i] |= f;
            if flags[i] & QUEUED == 0 {
                flags[i] |= QUEUED;
                heap.push((self.gen_at(i), i));
                if flags[i] & UN == 0 {
                    flags[i] |= LIVE;
                    live += 1;
                }
            } else if flags[i] & UN != 0 && flags[i] & LIVE != 0 {
                flags[i] &= !LIVE;
                live -= 1;
            }
        };
        for id in exclude {
            seed(&mut flags, self.index_of(id)?, UN);
        }
        for id in include {
            seed(&mut flags, self.index_of(id)?, INT);
        }
        let mut out = Vec::new();
        while live > 0 {
            let (gi, i) = heap.pop()?;
            let f = flags[i];
            if f & LIVE != 0 {
                live -= 1;
                // popped: a (corrupt, mis-ordered) file must not be able to count this node out a second time
                flags[i] &= !LIVE;
            }
            if f & UN == 0 {
                out.push(self.id_at(i));
            }
            let pass = f & (INT | UN);
            for p in self.par(i) {
                flags[p] |= pass;
                if flags[p] & QUEUED == 0 {
                    let Some(gp) = self.parent_gen(p, gi) else { continue };
                    flags[p] |= QUEUED;
                    heap.push((gp, p));
                    if flags[p] & UN == 0 {
                        flags[p] |= LIVE;
                        live += 1;
                    }
                } else if flags[p] & UN != 0 && flags[p] & LIVE != 0 {
                    flags[p] &= !LIVE;
                    live -= 1;
                }
            }
        }
        if self.is_bad() {
            return None;
        }
        Some(out)
    }

    /// Add every commit reachable from `tips` that is not yet present. `load(id)` returns
    /// `(parents, commit time)` of the real commit. Fails if a commit cannot be loaded or has
    /// too many parents; the caller then just does not use a graph. The result is an in-memory graph.
    pub fn extend<F>(&self, tips: &[ObjectId], mut load: F) -> Result<CommitGraphFile, MergeError>
    where
        F: FnMut(&ObjectId) -> Result<(Vec<ObjectId>, u64), MergeError>,
    {
        // new: id -> (parents, time, generation once known)
        let mut new: BTreeMap<ObjectId, (Vec<ObjectId>, u64, Option<u32>)> = BTreeMap::new();
        let mut stack: Vec<ObjectId> = tips.iter().filter(|t| !self.contains(t)).copied().collect();
        while let Some(id) = stack.pop() {
            if self.contains(&id) || new.contains_key(&id) {
                continue;
            }
            let (ps, t) = load(&id)?;
            if ps.len() > MAX_PARENTS {
                return corrupt("too many parents");
            }
            stack.extend(ps.iter().filter(|p| !self.contains(p) && !new.contains_key(p)));
            new.insert(id, (ps, t, None));
        }
        if new.is_empty() {
            return Ok(self.clone());
        }
        // Generations, children after parents (iterative DFS; histories can be very deep).
        let keys: Vec<ObjectId> = new.keys().copied().collect();
        for k in keys {
            let mut path = vec![k];
            // membership of `path`, so the cycle check is O(1) (a linear scan made a deep history quadratic)
            let mut on_path: HashSet<ObjectId> = HashSet::from([k]);
            while let Some(&n) = path.last() {
                if new[&n].2.is_some() {
                    on_path.remove(&n);
                    path.pop();
                    continue;
                }
                let mut max = 0u32;
                let mut pending = None;
                for p in &new[&n].0 {
                    let g = match self.index_of(p) {
                        Some(i) => Some(self.gen_at(i)),
                        None => new.get(p).and_then(|e| e.2),
                    };
                    match g {
                        Some(g) => max = max.max(g),
                        None => {
                            pending = Some(*p);
                            break;
                        }
                    }
                }
                match pending {
                    Some(p) => {
                        if !on_path.insert(p) {
                            return corrupt("cycle");
                        }
                        path.push(p);
                    }
                    None => {
                        new.get_mut(&n).unwrap().2 = Some(max.checked_add(1).ok_or(MergeError::Corrupt("generation overflow".into()))?);
                        on_path.remove(&n);
                        path.pop();
                    }
                }
            }
        }
        // Merge the (already id-sorted) new commits into the id-sorted table: one linear pass over the old columns,
        // no per-commit map. Old parent indexes are shifted by the number of new ids that sort before them.
        type NewEntry = (ObjectId, (Vec<ObjectId>, u64, Option<u32>));
        let new: Vec<NewEntry> = new.into_iter().collect();
        let total = self.n + new.len();
        let npar_total = self.npar + new.iter().map(|(_, (ps, _, _))| ps.len()).sum::<usize>();
        if total > u32::MAX as usize || npar_total > u32::MAX as usize {
            return corrupt("too large for the format");
        }
        let mut remap: Vec<u32> = Vec::with_capacity(self.n);
        // slot -> Ok(old index) | Err(new index)
        let mut slots: Vec<Result<u32, u32>> = Vec::with_capacity(total);
        let (mut i, mut j) = (0usize, 0usize);
        let old_ids = self.ids_slice();
        while i < self.n || j < new.len() {
            let take_old = j >= new.len() || (i < self.n && old_ids[i] < *new[j].0.as_bytes());
            if take_old {
                remap.push(slots.len() as u32);
                slots.push(Ok(i as u32));
                i += 1;
            } else {
                slots.push(Err(j as u32));
                j += 1;
            }
        }
        let mut buf = vec![0u8; body_size(total, npar_total).ok_or(MergeError::Corrupt("commit-graph: too large".into()))?];
        buf[..HDR].copy_from_slice(&header(total, npar_total));
        let mut out = CommitGraphFile::from_parts(Arc::new(MapFile::from_vec(Vec::new())), total, npar_total);
        // new ids first (the parent lookups below need the full id column)
        for (k, s) in slots.iter().enumerate() {
            let id = match s {
                Ok(o) => old_ids[*o as usize],
                Err(nn) => *new[*nn as usize].0.as_bytes(),
            };
            buf[HDR + 32 * k..HDR + 32 * k + 32].copy_from_slice(&id);
        }
        let mut cursor = 0usize;
        for (k, s) in slots.iter().enumerate() {
            let (time, gen) = match s {
                Ok(o) => {
                    let o = *o as usize;
                    for p in self.par(o) {
                        buf[out.o_par + 4 * cursor..][..4].copy_from_slice(&remap[p].to_le_bytes());
                        cursor += 1;
                    }
                    (self.time_at(o), self.gen_at(o))
                }
                Err(nn) => {
                    let (_, (ps, t, g)) = &new[*nn as usize];
                    let idxs: Vec<u32> = {
                        let ids = buf[HDR..HDR + 32 * total].as_chunks::<32>().0;
                        ps.iter().map(|p| ids.binary_search(p.as_bytes()).expect("closed") as u32).collect()
                    };
                    for idx in idxs {
                        buf[out.o_par + 4 * cursor..][..4].copy_from_slice(&idx.to_le_bytes());
                        cursor += 1;
                    }
                    (*t, g.expect("computed"))
                }
            };
            buf[out.o_time + 8 * k..][..8].copy_from_slice(&time.to_le_bytes());
            buf[out.o_gen + 4 * k..][..4].copy_from_slice(&gen.to_le_bytes());
            buf[out.o_pend + 4 * k..][..4].copy_from_slice(&(cursor as u32).to_le_bytes());
        }
        debug_assert_eq!(cursor, npar_total);
        out.buf = Arc::new(MapFile::from_vec(buf));
        Ok(out)
    }

    /// The v2 file image: header, columns and the checksum.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.body_len + 32);
        v.extend_from_slice(&self.buf[..self.body_len]);
        let sum = blake3::hash(&v);
        v.extend_from_slice(sum.as_bytes());
        v
    }

    /// Parse and fully validate any version from bytes: checksum, sorted unique ids, parent indexes in
    /// range, and every generation equal to 1 + max(parent generations). Anything else is an error.
    pub fn decode(bytes: &[u8]) -> Result<CommitGraphFile, MergeError> {
        if bytes.len() >= 5 && &bytes[..4] == MAGIC && bytes[4] == VERSION_V1 {
            return Self::decode_v1(bytes);
        }
        Self::from_map_with(MapFile::from_vec(bytes.to_vec()), usize::MAX)
    }

    /// Full validation of an opened graph (what `open` skips for big files): the checksum if the image has one,
    /// then sorted unique ids, offsets, parent indexes and generations. O(n) time, O(1) memory.
    pub fn verify_full(&self) -> Result<(), MergeError> {
        self.buf.advise_sequential();
        if self.buf.len() == self.body_len + 32 {
            if vcs_store::mapfile::blake3_big(&self.buf[..self.body_len]).as_bytes() != &self.buf[self.body_len..] {
                return corrupt("checksum mismatch");
            }
        } else if self.buf.len() != self.body_len {
            return corrupt("length mismatch");
        }
        if self.buf[..HDR] != header(self.n, self.npar) {
            return corrupt("header mismatch");
        }
        let ids = self.ids_slice();
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            return corrupt("ids not sorted and unique");
        }
        let mut prev_end = 0usize;
        for i in 0..self.n {
            let end = self.pend_at(i);
            if end < prev_end || end > self.npar || end - prev_end > MAX_PARENTS {
                return corrupt("parent offsets inconsistent");
            }
            let mut max = 0u32;
            for c in self.buf[self.o_par + 4 * prev_end..self.o_par + 4 * end].as_chunks::<4>().0 {
                let p = u32::from_le_bytes(*c) as usize;
                if p >= self.n || p == i {
                    return corrupt("parent index out of range");
                }
                max = max.max(self.gen_at(p));
            }
            if self.gen_at(i) != max.wrapping_add(1) || self.gen_at(i) == 0 {
                return corrupt("generation number inconsistent");
            }
            prev_end = end;
        }
        if prev_end != self.npar {
            return corrupt("parent offsets inconsistent");
        }
        Ok(())
    }

    /// v1 files: variable length entries; decode, validate completely and convert.
    fn decode_v1(bytes: &[u8]) -> Result<CommitGraphFile, MergeError> {
        if bytes.len() < 9 + 32 {
            return corrupt("truncated");
        }
        let (body, sum) = bytes.split_at(bytes.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return corrupt("checksum mismatch");
        }
        if &body[..4] != MAGIC || body[4] != VERSION_V1 {
            return corrupt("bad magic or version");
        }
        let n = u32::from_le_bytes(body[5..9].try_into().unwrap()) as usize;
        let mut pos: usize = 9;
        let mut take = |len: usize| -> Result<&[u8], MergeError> {
            let s = body
                .get(pos..pos.checked_add(len).ok_or(MergeError::Corrupt("commit-graph: truncated".into()))?)
                .ok_or(MergeError::Corrupt("commit-graph: truncated".into()))?;
            pos += len;
            Ok(s)
        };
        let cap = n.min(body.len() / 45);
        let mut ids: Vec<[u8; 32]> = Vec::with_capacity(cap);
        let mut time: Vec<u64> = Vec::with_capacity(cap);
        let mut gen: Vec<u32> = Vec::with_capacity(cap);
        let mut pend: Vec<u32> = Vec::with_capacity(cap);
        let mut pflat: Vec<u32> = Vec::new();
        for _ in 0..n.min(body.len()) {
            let head = take(45)?;
            ids.push(head[..32].try_into().unwrap());
            time.push(u64::from_le_bytes(head[32..40].try_into().unwrap()));
            gen.push(u32::from_le_bytes(head[40..44].try_into().unwrap()));
            let np = head[44] as usize;
            for c in take(np * 4)?.as_chunks::<4>().0 {
                pflat.push(u32::from_le_bytes(*c));
            }
            if pflat.len() > u32::MAX as usize {
                return corrupt("too large for the format");
            }
            pend.push(pflat.len() as u32);
        }
        if ids.len() != n || pos != body.len() {
            return corrupt("entry count or length mismatch");
        }
        let mut buf = vec![0u8; body_size(n, pflat.len()).ok_or(MergeError::Corrupt("commit-graph: too large".into()))?];
        buf[..HDR].copy_from_slice(&header(n, pflat.len()));
        let mut g = Self::from_parts(Arc::new(MapFile::from_vec(Vec::new())), n, pflat.len());
        for i in 0..n {
            buf[HDR + 32 * i..][..32].copy_from_slice(&ids[i]);
            buf[g.o_time + 8 * i..][..8].copy_from_slice(&time[i].to_le_bytes());
            buf[g.o_gen + 4 * i..][..4].copy_from_slice(&gen[i].to_le_bytes());
            buf[g.o_pend + 4 * i..][..4].copy_from_slice(&pend[i].to_le_bytes());
        }
        for (k, p) in pflat.iter().enumerate() {
            buf[g.o_par + 4 * k..][..4].copy_from_slice(&p.to_le_bytes());
        }
        g.buf = Arc::new(MapFile::from_vec(buf));
        g.upgraded = true;
        g.verify_full()?;
        Ok(g)
    }

    /// Compare against the real commits (`load` as in [`extend`](Self::extend)). Returns one
    /// message per discrepancy; empty means the file agrees with the objects.
    pub fn verify_against<F>(&self, mut load: F) -> Vec<String>
    where
        F: FnMut(&ObjectId) -> Result<(Vec<ObjectId>, u64), MergeError>,
    {
        let mut problems = Vec::new();
        for i in 0..self.n {
            let id = self.id_at(i);
            match load(&id) {
                Err(e) => problems.push(format!("commit-graph: {id}: cannot load real commit: {e}")),
                Ok((ps, t)) => {
                    if self.parent_ids(i) != ps {
                        problems.push(format!("commit-graph: {id}: parents differ from the commit"));
                    }
                    if t != self.time_at(i) {
                        problems.push(format!("commit-graph: {id}: commit time differs from the commit"));
                    }
                    for p in &ps {
                        if !self.contains(p) {
                            problems.push(format!("commit-graph: {id}: parent {p} missing from the graph"));
                        }
                    }
                }
            }
        }
        problems
    }
}

impl CommitGraph for CommitGraphFile {
    fn parents(&self, id: &ObjectId) -> Result<Vec<ObjectId>, MergeError> {
        let i = self.index_of(id).ok_or(MergeError::Missing(*id))?;
        let v = self.parent_ids(i);
        if self.is_bad() {
            return corrupt("inconsistent data");
        }
        Ok(v)
    }
    fn generation(&self, id: &ObjectId) -> Option<u32> {
        self.index_of(id).map(|i| self.gen_at(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::{is_ancestor, merge_base, MemGraph};
    use proptest::prelude::*;

    fn id(n: u32) -> ObjectId {
        let mut b = [0u8; 32];
        b[..4].copy_from_slice(&n.to_be_bytes());
        ObjectId::from_bytes(b)
    }

    /// Random DAG: node i has up to 3 distinct parents among 0..i.
    fn dag() -> impl Strategy<Value = Vec<Vec<u32>>> {
        (1usize..40).prop_flat_map(|n| {
            (0..n)
                .map(|i| proptest::collection::vec(0..(i.max(1) as u32), if i == 0 { 0..1 } else { 0..4 }))
                .collect::<Vec<_>>()
        })
    }

    fn build(d: &[Vec<u32>]) -> (MemGraph, CommitGraphFile) {
        let mut m = MemGraph::default();
        for (i, ps) in d.iter().enumerate() {
            let mut ps = ps.clone();
            ps.sort();
            ps.dedup();
            if i == 0 {
                ps.clear();
            }
            m.add(id(i as u32), ps.iter().map(|p| id(*p)).collect());
        }
        let tips: Vec<ObjectId> = (0..d.len() as u32).map(id).collect();
        let f = CommitGraphFile::default()
            .extend(&tips, |c| {
                Ok((m.parents(c)?, u64::from_be_bytes(c.as_bytes()[..8].try_into().unwrap())))
            })
            .unwrap();
        (m, f)
    }

    /// Regression (scaling suite): the cycle check scanned the DFS path linearly, so building the graph of a
    /// linear history was quadratic (300,000 commits took 69 s), and extending by one commit rebuilt a BTreeMap of
    /// the whole graph. A 200,000-commit chain must now build in well under the time the old code needed for 60,000.
    #[test]
    fn deep_chain_builds_in_linear_time_and_extends_like_a_rebuild() {
        let n = 200_000u32;
        let t = std::time::Instant::now();
        let parents = |c: &ObjectId| -> Result<(Vec<ObjectId>, u64), MergeError> {
            let k = u32::from_be_bytes(c.as_bytes()[..4].try_into().unwrap());
            Ok((if k == 0 { vec![] } else { vec![id(k - 1)] }, k as u64))
        };
        let half = CommitGraphFile::default().extend(&[id(n / 2)], parents).unwrap();
        let both = half.extend(&[id(n - 1)], parents).unwrap();
        let all = CommitGraphFile::default().extend(&[id(n - 1)], parents).unwrap();
        assert!(t.elapsed().as_secs() < 20, "chain build took {:?}", t.elapsed());
        assert_eq!(both, all);
        assert_eq!(all.len(), n as usize);
        assert_eq!(all.generation(&id(n - 1)), Some(n));
        assert_eq!(CommitGraphFile::decode(&all.encode()).unwrap(), all);
    }

    proptest! {
        #[test]
        fn merge_base_and_ancestor_match_plain(d in dag()) {
            let (m, f) = build(&d);
            for a in 0..d.len() as u32 {
                for b in 0..d.len() as u32 {
                    prop_assert_eq!(
                        merge_base(&f, &id(a), &id(b)).unwrap(),
                        crate::base::merge_base_plain(&m, &id(a), &id(b)).unwrap()
                    );
                    prop_assert_eq!(
                        is_ancestor(&f, &id(a), &id(b)).unwrap(),
                        is_ancestor(&m, &id(a), &id(b)).unwrap()
                    );
                    prop_assert_eq!(
                        f.merge_bases_indexed(&id(a), &id(b)).unwrap(),
                        crate::base::merge_base_plain(&m, &id(a), &id(b)).unwrap()
                    );
                    let n = f.count_reachable_not_from(&id(a), &id(b)).unwrap();
                    let behind = crate::base::ancestors(&m, &id(b)).unwrap();
                    let from = crate::base::ancestors(&m, &id(a)).unwrap();
                    prop_assert_eq!(n, from.difference(&behind).count());
                }
            }
        }

        #[test]
        fn range_ids_match_set_difference(
            d in dag(),
            inc in proptest::collection::vec(0u32..40, 1..4),
            exc in proptest::collection::vec(0u32..40, 0..4),
        ) {
            let (m, f) = build(&d);
            let n = d.len() as u32;
            let inc: Vec<ObjectId> = inc.into_iter().map(|x| id(x % n)).collect();
            let exc: Vec<ObjectId> = exc.into_iter().map(|x| id(x % n)).collect();
            let mut want = std::collections::BTreeSet::new();
            for i in &inc {
                want.extend(crate::base::ancestors(&m, i).unwrap());
            }
            for e in &exc {
                for a in crate::base::ancestors(&m, e).unwrap() {
                    want.remove(&a);
                }
            }
            let mut got = f.range_ids(&inc, &exc).unwrap();
            got.sort();
            prop_assert_eq!(got, want.into_iter().collect::<Vec<_>>());
        }

        #[test]
        fn encode_decode_roundtrip_and_incremental(d in dag(), cut in 0usize..40) {
            let (m, f) = build(&d);
            prop_assert_eq!(CommitGraphFile::decode(&f.encode()).unwrap(), f.clone());
            // Incremental: graph of a prefix, extended to all tips, equals the full build.
            let k = cut.min(d.len() - 1) as u32;
            let part = CommitGraphFile::default().extend(&[id(k)], |c| Ok((m.parents(c)?, 0))).unwrap();
            let all: Vec<ObjectId> = (0..d.len() as u32).map(id).collect();
            let full = part.extend(&all, |c| Ok((m.parents(c)?, u64::from_be_bytes(c.as_bytes()[..8].try_into().unwrap())))).unwrap();
            // times of the prefix differ (we passed 0), so compare structure only
            prop_assert_eq!(full.iter_ids().collect::<Vec<_>>(), f.iter_ids().collect::<Vec<_>>());
            for c in f.iter_ids() {
                prop_assert_eq!(full.generation(&c), f.generation(&c));
                prop_assert_eq!(full.parents(&c).unwrap(), f.parents(&c).unwrap());
            }
        }

        #[test]
        fn any_single_byte_flip_is_rejected(d in dag(), pos in any::<usize>(), bit in 0u8..8) {
            let (_, f) = build(&d);
            let mut b = f.encode();
            let i = pos % b.len();
            b[i] ^= 1 << bit;
            prop_assert!(CommitGraphFile::decode(&b).is_err());
        }
    }

    #[test]
    fn truncation_and_garbage_rejected() {
        let (_, f) = build(&[vec![], vec![0], vec![0, 1]]);
        let b = f.encode();
        for len in 0..b.len() {
            assert!(CommitGraphFile::decode(&b[..len]).is_err(), "len {len}");
        }
        assert!(CommitGraphFile::decode(b"garbage garbage garbage garbage garbage garbage").is_err());
    }

    #[test]
    fn verify_detects_wrong_parents() {
        let (m, f) = build(&[vec![], vec![0], vec![1]]);
        let real = |c: &ObjectId| Ok((m.parents(c)?, u64::from_be_bytes(c.as_bytes()[..8].try_into().unwrap())));
        assert!(f.verify_against(real).is_empty());
        let mut lie = m.clone();
        lie.add(id(2), vec![id(0)]);
        let wrong = |c: &ObjectId| Ok((lie.parents(c)?, u64::from_be_bytes(c.as_bytes()[..8].try_into().unwrap())));
        assert!(!f.verify_against(wrong).is_empty());
    }

    /// The old (v1) file encoding, kept here to prove that old files still load.
    fn encode_v1(f: &CommitGraphFile) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC);
        v.push(1);
        v.extend_from_slice(&(f.len() as u32).to_le_bytes());
        for i in 0..f.len() {
            v.extend_from_slice(f.id_at(i).as_bytes());
            v.extend_from_slice(&f.time_at(i).to_le_bytes());
            v.extend_from_slice(&f.gen_at(i).to_le_bytes());
            let ps: Vec<usize> = f.par(i).collect();
            v.push(ps.len() as u8);
            for p in ps {
                v.extend_from_slice(&(p as u32).to_le_bytes());
            }
        }
        let sum = blake3::hash(&v);
        v.extend_from_slice(sum.as_bytes());
        v
    }

    fn lazy(f: &CommitGraphFile) -> CommitGraphFile {
        // through a real file and mapping, never verified at open
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g");
        std::fs::write(&p, f.encode()).unwrap();
        let m = MapFile::open(&p, true).unwrap();
        CommitGraphFile::from_map_with(m, 0).unwrap()
    }

    fn all_queries(f: &CommitGraphFile, n: u32) {
        for a in 0..n {
            for b in 0..n {
                let _ = f.merge_bases_indexed(&id(a), &id(b));
                let _ = f.count_reachable_not_from(&id(a), &id(b));
                let _ = f.range_ids(&[id(a)], &[id(b)]);
            }
            let _ = f.parents(&id(a));
            let _ = f.time(&id(a));
        }
    }

    fn sorted(v: Option<Vec<ObjectId>>) -> Option<Vec<ObjectId>> {
        v.map(|mut v| {
            v.sort();
            v
        })
    }

    proptest! {
        /// Lazily mapped file == in-memory build == converted v1 file, on every query.
        #[test]
        fn lazy_reader_matches_old_reader(d in dag()) {
            let (m, f) = build(&d);
            let l = lazy(&f);
            let old = CommitGraphFile::decode(&encode_v1(&f)).unwrap();
            prop_assert!(old.upgraded());
            prop_assert_eq!(&old, &f);
            prop_assert_eq!(&l, &f);
            l.verify_full().unwrap();
            for a in 0..d.len() as u32 {
                prop_assert_eq!(l.time(&id(a)), f.time(&id(a)));
                prop_assert_eq!(l.parents(&id(a)).unwrap(), m.parents(&id(a)).unwrap());
                for b in 0..d.len() as u32 {
                    prop_assert_eq!(l.merge_bases_indexed(&id(a), &id(b)), old.merge_bases_indexed(&id(a), &id(b)));
                    prop_assert_eq!(
                        l.count_reachable_not_from(&id(a), &id(b)),
                        old.count_reachable_not_from(&id(a), &id(b))
                    );
                    prop_assert_eq!(
                        sorted(l.range_ids(&[id(a)], &[id(b)])),
                        sorted(old.range_ids(&[id(a)], &[id(b)]))
                    );
                }
            }
            prop_assert!(!l.is_bad());
        }

        /// A flipped byte in a lazily opened file never panics a query; `verify_full` catches it.
        #[test]
        fn lazy_corruption_degrades_safely(d in dag(), pos in any::<usize>(), bit in 0u8..8) {
            let (_, f) = build(&d);
            let mut b = f.encode();
            let i = pos % b.len();
            b[i] ^= 1 << bit;
            if let Ok(g) = CommitGraphFile::from_map_with(MapFile::from_vec(b), 0) {
                prop_assert!(g.verify_full().is_err());
                all_queries(&g, d.len() as u32);
            }
        }
    }

    #[test]
    fn tiny_and_empty_sizes() {
        let e = CommitGraphFile::default();
        assert!(e.is_empty());
        assert_eq!(CommitGraphFile::decode(&e.encode()).unwrap(), e);
        assert!(e.index_of(&id(1)).is_none());
        assert_eq!(e.range_ids(&[], &[]), Some(vec![]));
        assert!(CommitGraphFile::from_map(MapFile::from_vec(vec![])).is_err());
        assert!(CommitGraphFile::from_map(MapFile::from_vec(b"AGCG".to_vec())).is_err());
        let (_, one) = build(&[vec![]]);
        assert_eq!(one.len(), 1);
        assert_eq!(lazy(&one), one);
        assert_eq!(one.merge_bases_indexed(&id(0), &id(0)), Some(vec![id(0)]));
        // a v1 file with zero commits
        let mut v1 = b"AGCG\x01\0\0\0\0".to_vec();
        let s = blake3::hash(&v1);
        v1.extend_from_slice(s.as_bytes());
        let g = CommitGraphFile::decode(&v1).unwrap();
        assert!(g.is_empty() && g.upgraded());
    }

    #[test]
    fn lazy_open_rejects_truncation_and_header_damage_without_reading_the_body() {
        let (_, f) = build(&[vec![], vec![0], vec![0, 1], vec![2], vec![1, 3]]);
        let b = f.encode();
        for len in 0..b.len() {
            assert!(
                CommitGraphFile::from_map_with(MapFile::from_vec(b[..len].to_vec()), 0).is_err(),
                "len {len}"
            );
        }
        let mut longer = b.clone();
        longer.push(0);
        assert!(CommitGraphFile::from_map_with(MapFile::from_vec(longer), 0).is_err());
        for i in 0..HDR {
            let mut x = b.clone();
            x[i] ^= 1;
            assert!(CommitGraphFile::from_map_with(MapFile::from_vec(x), 0).is_err(), "header byte {i}");
        }
    }

    #[test]
    fn lazy_query_declines_on_inconsistent_body() {
        // chain 0 <- 1 <- 2 <- 3; corrupt commit 3's generation so it is not above its parent
        let (_, f) = build(&[vec![], vec![0], vec![1], vec![2]]);
        let i3 = f.index_of(&id(3)).unwrap();
        let mut b = f.encode();
        let at = f.o_gen + 4 * i3;
        b[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        let g = CommitGraphFile::from_map_with(MapFile::from_vec(b), 0).unwrap(); // lazy: not noticed at open
        assert!(g.verify_full().is_err());
        assert_eq!(g.range_ids(&[id(3)], &[id(0)]), None);
        assert!(g.is_bad());
        // an out-of-range parent index
        let mut b = f.encode();
        let at = f.o_par; // first parent slot
        b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let g = CommitGraphFile::from_map_with(MapFile::from_vec(b), 0).unwrap();
        assert!(g.verify_full().is_err());
        all_queries(&g, 4);
        assert!(g.is_bad());
    }

    #[test]
    fn open_from_file_maps_and_upgrades_v1() {
        let dir = tempfile::tempdir().unwrap();
        let (_, f) = build(&[vec![], vec![0], vec![0, 1]]);
        let p2 = dir.path().join("v2");
        std::fs::write(&p2, f.encode()).unwrap();
        assert_eq!(CommitGraphFile::open(&p2).unwrap(), f);
        let p1 = dir.path().join("v1");
        std::fs::write(&p1, encode_v1(&f)).unwrap();
        let g = CommitGraphFile::open(&p1).unwrap();
        assert!(g.upgraded());
        assert_eq!(g, f);
        assert!(CommitGraphFile::open(&dir.path().join("missing")).is_err());
    }
}
