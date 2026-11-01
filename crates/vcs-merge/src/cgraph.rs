//! On-disk commit-graph: for every commit its parents, commit time and generation number.
//!
//! It is a pure cache. Everything in it can be recomputed from the commit objects, so a
//! file that fails any check in [`CommitGraphFile::decode`] is simply discarded and rebuilt,
//! and [`CommitGraphFile::verify_against`] lets fsck compare it with the real commits.
//!
//! Format (little endian): `"AGCG" || version:u8 || count:u32 || count * entry || blake3(all
//! preceding bytes)`. Entry: `id:32 || time:u64 || generation:u32 || nparents:u8 || nparents * u32`
//! where parents are indexes into the (id-sorted) table.

use crate::base::CommitGraph;
use crate::error::MergeError;
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::ObjectId;

const MAGIC: &[u8; 4] = b"AGCG";
const VERSION: u8 = 1;
/// Commits with more parents than this are not representable; building fails (caller falls back).
const MAX_PARENTS: usize = 255;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitGraphFile {
    ids: Vec<ObjectId>,
    time: Vec<u64>,
    gen: Vec<u32>,
    parents: Vec<Vec<u32>>,
}

fn corrupt<T>(what: &str) -> Result<T, MergeError> {
    Err(MergeError::Corrupt(format!("commit-graph: {what}")))
}

impl CommitGraphFile {
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub fn index_of(&self, id: &ObjectId) -> Option<usize> {
        self.ids.binary_search(id).ok()
    }
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.index_of(id).is_some()
    }
    /// Commit time (the author timestamp) recorded for `id`.
    pub fn time(&self, id: &ObjectId) -> Option<u64> {
        self.index_of(id).map(|i| self.time[i])
    }
    pub fn ids(&self) -> &[ObjectId] {
        &self.ids
    }

    /// Number of commits reachable from `from` (itself included) that are not reachable from
    /// `hide` (itself included); `None` if either is not in the graph. Index based, no hashing.
    pub fn count_reachable_not_from(&self, from: &ObjectId, hide: &ObjectId) -> Option<usize> {
        let (a, b) = (self.index_of(from)?, self.index_of(hide)?);
        let mark = |start: usize, seen: &mut Vec<bool>| {
            let mut todo = vec![start];
            seen[start] = true;
            while let Some(i) = todo.pop() {
                for &p in &self.parents[i] {
                    if !seen[p as usize] {
                        seen[p as usize] = true;
                        todo.push(p as usize);
                    }
                }
            }
        };
        let mut hidden = vec![false; self.ids.len()];
        mark(b, &mut hidden);
        let mut seen = hidden.clone();
        let mut n = 0;
        // walk from `from`, not entering anything hidden or already counted
        if !seen[a] {
            seen[a] = true;
            n += 1;
            let mut todo = vec![a];
            while let Some(i) = todo.pop() {
                for &p in &self.parents[i] {
                    if !seen[p as usize] {
                        seen[p as usize] = true;
                        n += 1;
                        todo.push(p as usize);
                    }
                }
            }
        }
        Some(n)
    }

    /// Best common ancestors of `a` and `b`, the same answer as [`crate::merge_base`] over this graph
    /// but on table indexes (no id hashing or maps). `None` if either is not in the graph.
    pub fn merge_bases_indexed(&self, a: &ObjectId, b: &ObjectId) -> Option<Vec<ObjectId>> {
        use std::collections::BinaryHeap;
        const P1: u8 = 1;
        const P2: u8 = 2;
        const STALE: u8 = 4;
        let (ia, ib) = (self.index_of(a)?, self.index_of(b)?);
        let mut flags = vec![0u8; self.ids.len()];
        let mut done = vec![false; self.ids.len()];
        // the table is sorted by id, so (generation, index) pops in the same order as (generation, id)
        let mut heap: BinaryHeap<(u32, usize)> = BinaryHeap::new();
        flags[ia] |= P1;
        flags[ib] |= P2;
        heap.push((self.gen[ia], ia));
        if ia != ib {
            heap.push((self.gen[ib], ib));
        }
        let mut results: Vec<usize> = Vec::new();
        while heap.iter().any(|(_, n)| flags[*n] & STALE == 0) {
            let (_, n) = heap.pop()?;
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
            for &p in &self.parents[n] {
                let p = p as usize;
                let new = flags[p] | (f & (P1 | P2 | STALE));
                if new != flags[p] {
                    flags[p] = new;
                    if !done[p] {
                        heap.push((self.gen[p], p));
                    }
                }
            }
        }
        // drop candidates that are ancestors of other candidates
        let reaches = |anc: usize, desc: usize| -> bool {
            if self.gen[anc] > self.gen[desc] {
                return false;
            }
            let mut seen_set = std::collections::HashSet::new();
            let mut stack = vec![desc];
            while let Some(c) = stack.pop() {
                if c == anc {
                    return true;
                }
                if !seen_set.insert(c) || self.gen[c] <= self.gen[anc] {
                    continue;
                }
                stack.extend(self.parents[c].iter().map(|&p| p as usize));
            }
            false
        };
        let mut out: Vec<ObjectId> = results
            .iter()
            .filter(|&&r| !results.iter().any(|&s| s != r && reaches(r, s)))
            .map(|&r| self.ids[r])
            .collect();
        out.sort();
        Some(out)
    }

    /// Add every commit reachable from `tips` that is not yet present. `load(id)` returns
    /// `(parents, commit time)` of the real commit. Fails if a commit cannot be loaded or has
    /// too many parents; the caller then just does not use a graph.
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
            while let Some(&n) = path.last() {
                if new[&n].2.is_some() {
                    path.pop();
                    continue;
                }
                let mut max = 0u32;
                let mut pending = None;
                for p in &new[&n].0 {
                    let g = match self.index_of(p) {
                        Some(i) => Some(self.gen[i]),
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
                        if path.contains(&p) {
                            return corrupt("cycle");
                        }
                        path.push(p);
                    }
                    None => {
                        new.get_mut(&n).unwrap().2 = Some(max.checked_add(1).ok_or(MergeError::Corrupt("generation overflow".into()))?);
                        path.pop();
                    }
                }
            }
        }
        // Merge into one id-sorted table and re-index parents.
        let mut all: BTreeMap<ObjectId, (Vec<ObjectId>, u64, u32)> = BTreeMap::new();
        for i in 0..self.ids.len() {
            let ps = self.parents[i].iter().map(|&p| self.ids[p as usize]).collect();
            all.insert(self.ids[i], (ps, self.time[i], self.gen[i]));
        }
        for (id, (ps, t, g)) in new {
            all.insert(id, (ps, t, g.expect("computed")));
        }
        let ids: Vec<ObjectId> = all.keys().copied().collect();
        let mut out = CommitGraphFile::default();
        for id in &ids {
            let (ps, t, g) = &all[id];
            out.ids.push(*id);
            out.time.push(*t);
            out.gen.push(*g);
            out.parents
                .push(ps.iter().map(|p| ids.binary_search(p).expect("closed") as u32).collect());
        }
        Ok(out)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(16 + self.ids.len() * 80);
        v.extend_from_slice(MAGIC);
        v.push(VERSION);
        v.extend_from_slice(&(self.ids.len() as u32).to_le_bytes());
        for i in 0..self.ids.len() {
            v.extend_from_slice(self.ids[i].as_bytes());
            v.extend_from_slice(&self.time[i].to_le_bytes());
            v.extend_from_slice(&self.gen[i].to_le_bytes());
            v.push(self.parents[i].len() as u8);
            for p in &self.parents[i] {
                v.extend_from_slice(&p.to_le_bytes());
            }
        }
        let sum = blake3::hash(&v);
        v.extend_from_slice(sum.as_bytes());
        v
    }

    /// Parse and fully validate: checksum, sorted unique ids, parent indexes in range, and
    /// every generation equal to 1 + max(parent generations). Anything else is an error.
    pub fn decode(bytes: &[u8]) -> Result<CommitGraphFile, MergeError> {
        if bytes.len() < 9 + 32 {
            return corrupt("truncated");
        }
        let (body, sum) = bytes.split_at(bytes.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return corrupt("checksum mismatch");
        }
        if &body[..4] != MAGIC || body[4] != VERSION {
            return corrupt("bad magic or version");
        }
        let n = u32::from_le_bytes(body[5..9].try_into().unwrap()) as usize;
        let mut pos = 9;
        let mut take = |len: usize| -> Result<&[u8], MergeError> {
            let s = body
                .get(pos..pos + len)
                .ok_or(MergeError::Corrupt("commit-graph: truncated".into()))?;
            pos += len;
            Ok(s)
        };
        let mut g = CommitGraphFile::default();
        for _ in 0..n.min(body.len()) {
            let id = ObjectId::from_bytes(take(32)?.try_into().unwrap());
            g.time.push(u64::from_le_bytes(take(8)?.try_into().unwrap()));
            g.gen.push(u32::from_le_bytes(take(4)?.try_into().unwrap()));
            let np = take(1)?[0] as usize;
            let mut ps = Vec::with_capacity(np);
            for _ in 0..np {
                ps.push(u32::from_le_bytes(take(4)?.try_into().unwrap()));
            }
            g.ids.push(id);
            g.parents.push(ps);
        }
        if g.ids.len() != n || pos != body.len() {
            return corrupt("entry count or length mismatch");
        }
        if g.ids.windows(2).any(|w| w[0] >= w[1]) {
            return corrupt("ids not sorted and unique");
        }
        for i in 0..n {
            let mut max = 0u32;
            for &p in &g.parents[i] {
                if p as usize >= n || p as usize == i {
                    return corrupt("parent index out of range");
                }
                max = max.max(g.gen[p as usize]);
            }
            if g.gen[i] != max.wrapping_add(1) || g.gen[i] == 0 {
                return corrupt("generation number inconsistent");
            }
        }
        Ok(g)
    }

    /// Compare against the real commits (`load` as in [`extend`](Self::extend)). Returns one
    /// message per discrepancy; empty means the file agrees with the objects.
    pub fn verify_against<F>(&self, mut load: F) -> Vec<String>
    where
        F: FnMut(&ObjectId) -> Result<(Vec<ObjectId>, u64), MergeError>,
    {
        let mut problems = Vec::new();
        let known: BTreeSet<&ObjectId> = self.ids.iter().collect();
        for i in 0..self.ids.len() {
            let id = &self.ids[i];
            match load(id) {
                Err(e) => problems.push(format!("commit-graph: {id}: cannot load real commit: {e}")),
                Ok((ps, t)) => {
                    let mine: Vec<ObjectId> = self.parents[i].iter().map(|&p| self.ids[p as usize]).collect();
                    if mine != ps {
                        problems.push(format!("commit-graph: {id}: parents differ from the commit"));
                    }
                    if t != self.time[i] {
                        problems.push(format!("commit-graph: {id}: commit time differs from the commit"));
                    }
                    for p in &ps {
                        if !known.contains(p) {
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
        Ok(self.parents[i].iter().map(|&p| self.ids[p as usize]).collect())
    }
    fn generation(&self, id: &ObjectId) -> Option<u32> {
        self.index_of(id).map(|i| self.gen[i])
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
        fn encode_decode_roundtrip_and_incremental(d in dag(), cut in 0usize..40) {
            let (m, f) = build(&d);
            prop_assert_eq!(CommitGraphFile::decode(&f.encode()).unwrap(), f.clone());
            // Incremental: graph of a prefix, extended to all tips, equals the full build.
            let k = cut.min(d.len() - 1) as u32;
            let part = CommitGraphFile::default().extend(&[id(k)], |c| Ok((m.parents(c)?, 0))).unwrap();
            let all: Vec<ObjectId> = (0..d.len() as u32).map(id).collect();
            let full = part.extend(&all, |c| Ok((m.parents(c)?, u64::from_be_bytes(c.as_bytes()[..8].try_into().unwrap())))).unwrap();
            // times of the prefix differ (we passed 0), so compare structure only
            prop_assert_eq!(full.ids(), f.ids());
            for c in f.ids() {
                prop_assert_eq!(full.generation(c), f.generation(c));
                prop_assert_eq!(full.parents(c).unwrap(), f.parents(c).unwrap());
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
}
