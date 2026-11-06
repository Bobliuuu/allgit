//! `show-branch`: which commits each of several revisions has, as a matrix; plus its
//! `--merge-base` and `--independent` forms.
//!
//! Every head gets one bit. Bits flow from the tips to their parents in date order until a commit
//! carries every bit (a common ancestor of all heads); the output stops there, as git does. The
//! rows are the commits that miss at least one bit, plus the common ancestors on the boundary.
//! Ties in commit time are broken by the order the walk reached the commits, so rows with equal
//! times can come out in a different order than git's.

use crate::error::{msg, Result};
use crate::repo::Repo;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use vcs_objects::ObjectId;

/// Most heads one matrix can hold (one bit each).
pub const MAX_HEADS: usize = 26;

/// One head of the matrix.
#[derive(Clone, Debug)]
pub struct SbHead {
    /// The revision as the user wrote it (a branch name, `HEAD~2`, ...).
    pub name: String,
    pub id: ObjectId,
    pub subject: String,
}

/// One row: a commit and the heads that reach it.
#[derive(Clone, Debug)]
pub struct SbRow {
    pub id: ObjectId,
    /// Bit `i` set: head `i` reaches this commit.
    pub mask: u32,
    pub is_merge: bool,
    /// `main`, `main~2`, `main^2~1`: how the commit is named from the first head that reached it.
    pub name: String,
    pub subject: String,
}

/// Result of [`Repo::show_branch`].
#[derive(Clone, Debug, Default)]
pub struct ShowBranch {
    pub heads: Vec<SbHead>,
    pub rows: Vec<SbRow>,
    /// The common ancestors that are not an ancestor of another common ancestor.
    pub merge_bases: Vec<ObjectId>,
}

struct Node {
    parents: Vec<ObjectId>,
    time: u64,
    subject: String,
}

impl Repo {
    fn sb_node(&self, cache: &mut HashMap<ObjectId, Node>, id: &ObjectId) -> Result<()> {
        if !cache.contains_key(id) {
            let c = self.commit_obj(id)?;
            cache.insert(
                *id,
                Node {
                    parents: c.parents.clone(),
                    time: c.author.timestamp,
                    subject: c.message.lines().next().unwrap_or("").to_owned(),
                },
            );
        }
        Ok(())
    }

    /// Resolve the heads: each rev to a commit of `zone`; the same name twice counts once.
    pub fn show_branch_heads(&self, zone: &str, revs: &[String]) -> Result<Vec<SbHead>> {
        let zone = self.resolve_zone(zone)?;
        let mut heads: Vec<SbHead> = Vec::new();
        for r in revs {
            if heads.iter().any(|h| &h.name == r) {
                continue;
            }
            let id = self.resolve_rev(&zone, r)?;
            let subject = self.commit_obj(&id)?.message.lines().next().unwrap_or("").to_owned();
            heads.push(SbHead {
                name: r.clone(),
                id,
                subject,
            });
        }
        if heads.len() > MAX_HEADS {
            return msg(format!("show-branch: at most {MAX_HEADS} revisions at a time"));
        }
        Ok(heads)
    }

    /// Branch names of a zone, sorted (what `show-branch` shows without arguments).
    pub fn show_branch_default_revs(&self, zone: &str) -> Result<Vec<String>> {
        let zone = self.resolve_zone(zone)?;
        Ok(self.refs()?.branches_of(&zone).into_iter().map(|(b, _)| b).collect())
    }

    /// The matrix for `heads`. `more` is the number of commits to show after the first common
    /// ancestors (git `--more=N`).
    pub fn show_branch(&self, heads: Vec<SbHead>, more: usize) -> Result<ShowBranch> {
        let n = heads.len();
        if n == 0 {
            return msg("show-branch: no revisions");
        }
        let all: u32 = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        let mut cache: HashMap<ObjectId, Node> = HashMap::new();
        let mut mask: HashMap<ObjectId, u32> = HashMap::new();
        let mut names: HashMap<ObjectId, (String, u32)> = HashMap::new();
        let mut heap: BinaryHeap<(u64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
        let mut seq = 0u64;
        let mut queued: BTreeSet<ObjectId> = BTreeSet::new();
        for (i, h) in heads.iter().enumerate() {
            self.sb_node(&mut cache, &h.id)?;
            *mask.entry(h.id).or_default() |= 1 << i;
            names.entry(h.id).or_insert((h.name.clone(), 0));
        }
        for h in &heads {
            if queued.insert(h.id) {
                seq += 1;
                heap.push((cache[&h.id].time, Reverse(seq), h.id));
            }
        }
        let tips: BTreeSet<ObjectId> = heads.iter().map(|h| h.id).collect();
        let mut expanded: HashMap<ObjectId, u32> = HashMap::new();
        let mut popped_at: HashMap<ObjectId, u64> = HashMap::new();
        let mut pops = 0u64;
        while let Some((_, _, c)) = heap.pop() {
            queued.remove(&c);
            pops += 1;
            popped_at.insert(c, pops);
            let m = mask[&c];
            if m == all && !expanded.contains_key(&c) {
                continue;
            }
            if expanded.get(&c) == Some(&m) {
                continue;
            }
            expanded.insert(c, m);
            let parents = cache[&c].parents.clone();
            let (base, gen) = names.get(&c).cloned().unwrap_or_default();
            for (k, p) in parents.iter().enumerate() {
                self.sb_node(&mut cache, p)?;
                names.entry(*p).or_insert_with(|| {
                    if k == 0 {
                        (base.clone(), gen + 1)
                    } else {
                        (format!("{}^{}", sb_name(&base, gen), k + 1), 0)
                    }
                });
                let e = mask.entry(*p).or_default();
                if *e | m != *e {
                    *e |= m;
                    if queued.insert(*p) {
                        seq += 1;
                        heap.push((cache[p].time, Reverse(seq), *p));
                    }
                }
            }
        }

        // Rows: commits that miss a bit, then the common ancestors on the boundary.
        let mut shown: BTreeSet<ObjectId> = BTreeSet::new();
        let mut boundary: BTreeSet<ObjectId> = BTreeSet::new();
        for (c, m) in &mask {
            if *m != all {
                shown.insert(*c);
                for p in &cache[c].parents {
                    if mask.get(p) == Some(&all) {
                        boundary.insert(*p);
                    }
                }
            }
        }
        for t in &tips {
            if mask.get(t) == Some(&all) {
                boundary.insert(*t);
            }
        }
        let mut merge_bases: Vec<ObjectId> = Vec::new();
        for b in &boundary {
            let mut dominated = false;
            for o in &boundary {
                if o != b && self.is_ancestor(*b, *o)? {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                merge_bases.push(*b);
            }
        }
        shown.extend(boundary.iter().copied());
        let mut order: Vec<ObjectId> = shown.into_iter().collect();
        order.sort_by_key(|c| popped_at.get(c).copied().unwrap_or(u64::MAX));
        // --more: keep walking past the boundary, newest first.
        if more > 0 {
            let mut extra: BinaryHeap<(u64, Reverse<u64>, ObjectId)> = BinaryHeap::new();
            let mut seen: BTreeSet<ObjectId> = order.iter().copied().collect();
            for b in &boundary {
                let (base, gen) = names.get(b).cloned().unwrap_or_default();
                for (k, p) in cache[b].parents.clone().into_iter().enumerate() {
                    self.sb_node(&mut cache, &p)?;
                    names.entry(p).or_insert_with(|| {
                        if k == 0 {
                            (base.clone(), gen + 1)
                        } else {
                            (format!("{}^{}", sb_name(&base, gen), k + 1), 0)
                        }
                    });
                    if seen.insert(p) {
                        seq += 1;
                        extra.push((cache[&p].time, Reverse(seq), p));
                    }
                }
            }
            let mut left = more;
            while left > 0 {
                let Some((_, _, c)) = extra.pop() else { break };
                mask.insert(c, all);
                order.push(c);
                left -= 1;
                let (base, gen) = names.get(&c).cloned().unwrap_or_default();
                for (k, p) in cache[&c].parents.clone().iter().enumerate() {
                    self.sb_node(&mut cache, p)?;
                    names.entry(*p).or_insert_with(|| {
                        if k == 0 {
                            (base.clone(), gen + 1)
                        } else {
                            (format!("{}^{}", sb_name(&base, gen), k + 1), 0)
                        }
                    });
                    if seen.insert(*p) {
                        seq += 1;
                        extra.push((cache[p].time, Reverse(seq), *p));
                    }
                }
            }
        }
        let rows = order
            .into_iter()
            .map(|c| {
                let nd = &cache[&c];
                let (b, g) = names.get(&c).cloned().unwrap_or_default();
                SbRow {
                    id: c,
                    mask: mask[&c],
                    is_merge: nd.parents.len() > 1,
                    name: sb_name(&b, g),
                    subject: nd.subject.clone(),
                }
            })
            .collect();
        Ok(ShowBranch { heads, rows, merge_bases })
    }

    /// `show-branch --independent`: the heads that are not an ancestor of another head.
    pub fn show_branch_independent(&self, heads: &[SbHead]) -> Result<Vec<ObjectId>> {
        let mut out: Vec<ObjectId> = Vec::new();
        for h in heads {
            if out.contains(&h.id) {
                continue;
            }
            let mut dominated = false;
            for o in heads {
                if o.id != h.id && self.is_ancestor(h.id, o.id)? {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                out.push(h.id);
            }
        }
        Ok(out)
    }
}

/// `base`, `base^`, `base~N` (N > 1), as git names the first-parent chain.
fn sb_name(base: &str, gen: u32) -> String {
    if gen == 0 {
        base.to_owned()
    } else if gen == 1 {
        format!("{base}^")
    } else {
        format!("{base}~{gen}")
    }
}
