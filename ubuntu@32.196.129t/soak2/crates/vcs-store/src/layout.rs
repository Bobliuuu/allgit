//! Object order inside a compacted pack (issue #249, `docs/research/storage/packing-experiments.md`).
//!
//! Commits first (parents before children), then trees (children before parents), then the
//! small non-file objects (snapshots, tags, operations), then file content grouped by path:
//! (zone, directory, file name), and within one path oldest version first. A reader that wants a
//! clone's history reads the metadata prefix in a few ranged GETs; a reader that wants the
//! versions of one file finds them adjacent; and a compressor that sees the pack as a stream (or
//! in independent blocks) sees related versions next to each other.
//!
//! Pure function of the object set: it never changes an object, only its position, and the result
//! is a permutation of the input.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};
use vcs_objects::{EntryMode, Object, ObjectId};

/// Where a file object sits in the tree of the commits that are in the set.
struct Place {
    zone: String,
    /// Directory components, then the file name last.
    path: Vec<String>,
    /// Index of the first commit (in commit order) that names it.
    seq: usize,
    /// 0 for the object named by the tree, 1+i for chunk i of a chunked file.
    sub: usize,
}

/// Reorder `objs` (id, encoded bytes) for locality. Objects that fail to decode keep their
/// relative order at the end of the metadata (they cannot be placed; the caller verifies them).
pub fn locality_order(objs: Vec<(ObjectId, Vec<u8>)>) -> Vec<(ObjectId, Vec<u8>)> {
    let n = objs.len();
    let mut decoded: Vec<Option<Object>> = objs.iter().map(|(_, b)| Object::decode(b).ok()).collect();
    let index: HashMap<ObjectId, usize> = objs.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();

    // 1. commits: Kahn's algorithm, ties by (zone, timestamp, id); parents before children.
    let mut commit_idx: Vec<usize> = Vec::new();
    for (i, d) in decoded.iter().enumerate() {
        if matches!(d, Some(Object::Commit(_))) {
            commit_idx.push(i);
        }
    }
    let key = |i: usize| -> (String, u64, ObjectId) {
        match &decoded[i] {
            Some(Object::Commit(c)) => (c.zone.clone(), c.author.timestamp, objs[i].0),
            _ => unreachable!(),
        }
    };
    let mut indeg: HashMap<usize, usize> = HashMap::new();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &commit_idx {
        let Some(Object::Commit(c)) = &decoded[i] else { continue };
        let mut d = 0;
        for p in c.parents.iter().collect::<BTreeSet<_>>() {
            if let Some(&pi) = index.get(p) {
                if matches!(decoded[pi], Some(Object::Commit(_))) {
                    d += 1;
                    children.entry(pi).or_default().push(i);
                }
            }
        }
        indeg.insert(i, d);
    }
    type Ready = Reverse<((String, u64, ObjectId), usize)>;
    let mut heap: BinaryHeap<Ready> = BinaryHeap::new();
    for &i in &commit_idx {
        if indeg[&i] == 0 {
            heap.push(Reverse((key(i), i)));
        }
    }
    let mut commits: Vec<usize> = Vec::with_capacity(commit_idx.len());
    while let Some(Reverse((_, i))) = heap.pop() {
        commits.push(i);
        for &c in children.get(&i).into_iter().flatten() {
            let d = indeg.get_mut(&c).expect("child is a commit");
            *d -= 1;
            if *d == 0 {
                heap.push(Reverse((key(c), c)));
            }
        }
    }
    if commits.len() != commit_idx.len() {
        // a parent cycle cannot happen with content addressing; keep every commit anyway
        let seen: HashSet<usize> = commits.iter().copied().collect();
        commits.extend(commit_idx.iter().copied().filter(|i| !seen.contains(i)));
    }

    // 2. trees: post-order from each commit's root (children first), then any leftover tree.
    let mut trees: Vec<usize> = Vec::new();
    let mut tree_seen: HashSet<usize> = HashSet::new();
    let mut roots: Vec<usize> = commits
        .iter()
        .filter_map(|&i| match &decoded[i] {
            Some(Object::Commit(c)) => index.get(&c.root).copied(),
            _ => None,
        })
        .collect();
    let mut leftover: Vec<usize> = (0..n).filter(|&i| matches!(decoded[i], Some(Object::Tree(_)))).collect();
    leftover.sort_by_key(|&i| objs[i].0);
    roots.extend(leftover);
    for r in roots {
        if !matches!(decoded[r], Some(Object::Tree(_))) || tree_seen.contains(&r) {
            continue;
        }
        let mut stack: Vec<(usize, bool)> = vec![(r, false)];
        while let Some((i, done)) = stack.pop() {
            if done {
                trees.push(i);
                continue;
            }
            if !tree_seen.insert(i) {
                continue;
            }
            stack.push((i, true));
            if let Some(Object::Tree(t)) = &decoded[i] {
                for e in t.entries().iter().rev() {
                    if e.mode == EntryMode::Directory {
                        if let Some(&ci) = index.get(&e.id) {
                            if matches!(decoded[ci], Some(Object::Tree(_))) && !tree_seen.contains(&ci) {
                                stack.push((ci, false));
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. paths of file objects, from the commits in the set (first appearance wins).
    let mut place: HashMap<usize, Place> = HashMap::new();
    let mut walked: HashSet<ObjectId> = HashSet::new();
    for (seq, &ci) in commits.iter().enumerate() {
        let Some(Object::Commit(c)) = &decoded[ci] else { continue };
        let mut stack: Vec<(ObjectId, Vec<String>)> = vec![(c.root, vec![])];
        while let Some((tid, dir)) = stack.pop() {
            if !walked.insert(tid) {
                continue;
            }
            let Some(&ti) = index.get(&tid) else { continue };
            let Some(Object::Tree(t)) = &decoded[ti] else { continue };
            for e in t.entries() {
                let mut path = dir.clone();
                path.push(e.name.clone());
                if e.mode == EntryMode::Directory {
                    stack.push((e.id, path));
                    continue;
                }
                let Some(&fi) = index.get(&e.id) else { continue };
                if place.contains_key(&fi) {
                    continue;
                }
                if let Some(Object::Chunked(m)) = &decoded[fi] {
                    for (k, ch) in m.chunks.iter().enumerate() {
                        if let Some(&chi) = index.get(&ch.id) {
                            place.entry(chi).or_insert(Place {
                                zone: c.zone.clone(),
                                path: path.clone(),
                                seq,
                                sub: k + 1,
                            });
                        }
                    }
                }
                place.insert(
                    fi,
                    Place {
                        zone: c.zone.clone(),
                        path,
                        seq,
                        sub: 0,
                    },
                );
            }
        }
    }

    // 4. assemble
    let mut taken = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for &i in commits.iter().chain(trees.iter()) {
        taken[i] = true;
        order.push(i);
    }
    // small non-file objects and undecodable ones, by id
    let mut misc: Vec<usize> = (0..n)
        .filter(|&i| !taken[i] && !place.contains_key(&i) && !matches!(decoded[i], Some(Object::Blob(_)) | Some(Object::Chunked(_))))
        .collect();
    misc.sort_by_key(|&i| objs[i].0);
    for i in misc {
        taken[i] = true;
        order.push(i);
    }
    let mut placed: Vec<usize> = place.keys().copied().filter(|&i| !taken[i]).collect();
    placed.sort_by(|&a, &b| {
        let (pa, pb) = (&place[&a], &place[&b]);
        (&pa.zone, &pa.path, pa.seq, pa.sub, objs[a].0).cmp(&(&pb.zone, &pb.path, pb.seq, pb.sub, objs[b].0))
    });
    for i in placed {
        taken[i] = true;
        order.push(i);
    }
    // files whose path is unknown (their commit is in another pack): by id
    let mut rest: Vec<usize> = (0..n).filter(|&i| !taken[i]).collect();
    rest.sort_by_key(|&i| objs[i].0);
    order.extend(rest);

    debug_assert_eq!(order.len(), n);
    decoded.clear();
    let mut slots: Vec<Option<(ObjectId, Vec<u8>)>> = objs.into_iter().map(Some).collect();
    order.into_iter().map(|i| slots[i].take().expect("each object once")).collect()
}

// ---------------------------------------------------------------- tiered merge policy

/// The tier of a pack of `len` bytes.
pub fn tier_of(len: u64, ratio: u64, base: u64) -> u32 {
    let mut t = 0;
    let mut cap = base;
    while len > cap {
        t += 1;
        cap = cap.saturating_mul(ratio);
        if cap == u64::MAX {
            break;
        }
    }
    t
}

/// The groups of packs to merge (each of at least two). Pure function of the sizes. Packs of one
/// tier are merged once `ratio` of them exist; a merge result may complete the next tier, so the
/// plan cascades in memory and each final group is written once. Groups over `max_bytes` shed
/// their largest members. Deterministic (ties by stem).
pub fn plan_tiers(packs: &[(String, u64)], ratio: u64, base: u64, max_bytes: u64) -> Vec<Vec<String>> {
    let ratio = ratio.max(2);
    let mut items: Vec<(u64, Vec<String>)> = packs.iter().map(|p| (p.1, vec![p.0.clone()])).collect();
    items.sort();
    loop {
        let mut by_tier: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        for (i, (len, _)) in items.iter().enumerate() {
            by_tier.entry(tier_of(*len, ratio, base)).or_default().push(i);
        }
        let Some(group) = by_tier.into_values().find(|g| g.len() as u64 >= ratio) else {
            break;
        };
        let gs: HashSet<usize> = group.iter().copied().collect();
        let mut merged = (0u64, Vec::new());
        let mut rest = Vec::new();
        for (i, it) in items.into_iter().enumerate() {
            if gs.contains(&i) {
                merged.0 += it.0;
                merged.1.extend(it.1);
            } else {
                rest.push(it);
            }
        }
        rest.push(merged);
        rest.sort();
        items = rest;
    }
    let mut out = Vec::new();
    for (_, mut members) in items {
        if members.len() < 2 {
            continue;
        }
        // shed the largest members until the group fits
        let size: BTreeMap<&str, u64> = packs.iter().map(|p| (p.0.as_str(), p.1)).collect();
        members.sort_by_key(|m| (size[m.as_str()], m.clone()));
        while members.len() >= 2 && members.iter().map(|m| size[m.as_str()]).sum::<u64>() > max_bytes {
            members.pop();
        }
        if members.len() >= 2 {
            members.sort();
            out.push(members);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcs_objects::*;

    fn sig(t: u64) -> Signature {
        Signature {
            name: "t".into(),
            email: "t@x".into(),
            timestamp: t,
        }
    }

    #[test]
    fn commits_then_trees_then_files_grouped_by_path() {
        // two commits touching a.txt and b.txt alternately: files must end up grouped by path
        let mut all: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        let mut put = |o: Object| {
            let e = o.encode();
            let id = id_of_encoded(&e);
            all.push((id, e));
            id
        };
        let mut parent = None;
        let mut commits = vec![];
        for v in 0..3u8 {
            let a = put(Object::Blob(format!("a version {v}").into_bytes()));
            let b = put(Object::Blob(format!("b version {v}").into_bytes()));
            let t = put(Object::Tree(
                Tree::new(vec![
                    TreeEntry {
                        name: "a.txt".into(),
                        mode: EntryMode::File,
                        id: a,
                    },
                    TreeEntry {
                        name: "b.txt".into(),
                        mode: EntryMode::File,
                        id: b,
                    },
                ])
                .unwrap(),
            ));
            let c = put(Object::Commit(Commit {
                zone: "z".into(),
                parents: parent.into_iter().collect(),
                root: t,
                change_id: ChangeId::from_bytes([v; 16]),
                author: sig(v as u64),
                message: "m".into(),
            }));
            commits.push((a, b, c));
            parent = Some(c);
        }
        // shuffle deterministically
        all.reverse();
        let ord = locality_order(all.clone());
        assert_eq!(ord.len(), all.len());
        let ids: Vec<ObjectId> = ord.iter().map(|o| o.0).collect();
        let pos = |id: &ObjectId| ids.iter().position(|x| x == id).unwrap();
        // commits in parent order, first
        assert!(pos(&commits[0].2) < pos(&commits[1].2) && pos(&commits[1].2) < pos(&commits[2].2));
        assert_eq!(pos(&commits[2].2), 2);
        // trees next (3), then the six blobs: a0 a1 a2 b0 b1 b2
        let blobs: Vec<ObjectId> = ids[6..].to_vec();
        let want: Vec<ObjectId> = (0..3).map(|v| commits[v].0).chain((0..3).map(|v| commits[v].1)).collect();
        assert_eq!(blobs, want);
        // permutation: same multiset of ids and bytes
        let mut a: Vec<_> = all.clone();
        let mut b = ord.clone();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }
}
