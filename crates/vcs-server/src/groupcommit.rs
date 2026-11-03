//! Server-side reconciliation of stale manifest writes ("group commit").
//!
//! Every publish is a compare-and-swap of the one `manifest` object. When many clients publish at once
//! all but one lose the swap and retry over the network (docs/scalability.md: 512 writers, 92.8 percent of
//! attempts lost, p95 25 s). The server is already the validating writer, so a write whose `If-Match` names
//! an OLDER version can be reconciled here, in-process, exactly as the client's own retry would: the
//! client's change is "one transaction on top of version B", and if nothing it touches has changed between B
//! and the current version C, applying the same change to C is what the retry computes (same `old` heads
//! check, same generation + 1, same txid appended), and the full validator then runs against C.
//!
//! The server remembers, per version it wrote, which branch heads changed (key, old, new) and whether
//! anything else (zones, labels, tags, snapshots, locks, tokens) changed. A stale write is merged only if
//!
//! * the chain from B to C is known and contains no change to anything but branch heads,
//! * the write is exactly one generation above B and appends one txid that C has not applied,
//! * the write itself changes nothing but branch heads (all other fields equal C's),
//! * none of the branch keys the write changed was changed between B and C.
//!
//! Anything else falls back to 412 and the client retries as before, so correctness never depends on the
//! merge. Linearizability: the merged manifest is validated against C and replaces C by compare-and-swap on
//! C's tag, so the history is the same as if the client had retried after seeing C.

use std::collections::{BTreeMap, HashMap, VecDeque};
use vcs_objects::ObjectId;
use vcs_remote::Manifest;

type Key = (String, String);

/// Versions remembered (each costs the size of its branch delta, a few hundred bytes).
const CHAIN_MAX: usize = 8192;

struct Link {
    next: String,
    gen_from: u64,
    /// Branch heads changed by the step from this version to `next`: key -> (old, new).
    delta: Vec<(Key, Option<ObjectId>, Option<ObjectId>)>,
    /// Something other than branch heads (and generation, applied ids, validator time) changed.
    other: bool,
}

#[derive(Default)]
pub struct Chain {
    links: HashMap<String, Link>,
    order: VecDeque<String>,
}

fn same_except_heads(a: &Manifest, b: &Manifest) -> bool {
    a.snapshots == b.snapshots
        && a.zones == b.zones
        && a.labels == b.labels
        && a.locks == b.locks
        && a.tags == b.tags
        && a.next_token == b.next_token
}

impl Chain {
    /// Record that version `from_tag` (decoded as `from`) was replaced by `to_tag` (decoded as `to`).
    pub fn record(&mut self, from_tag: &str, from: &Manifest, to_tag: &str, to: &Manifest) {
        let mut delta = vec![];
        let mut a = from.branches.iter().peekable();
        let mut b = to.branches.iter().peekable();
        loop {
            match (a.peek(), b.peek()) {
                (None, None) => break,
                (Some(&(k, v)), None) => {
                    delta.push((k.clone(), Some(*v), None));
                    a.next();
                }
                (None, Some(&(k, v))) => {
                    delta.push((k.clone(), None, Some(*v)));
                    b.next();
                }
                (Some(&(ka, va)), Some(&(kb, vb))) => match ka.cmp(kb) {
                    std::cmp::Ordering::Less => {
                        delta.push((ka.clone(), Some(*va), None));
                        a.next();
                    }
                    std::cmp::Ordering::Greater => {
                        delta.push((kb.clone(), None, Some(*vb)));
                        b.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if va != vb {
                            delta.push((ka.clone(), Some(*va), Some(*vb)));
                        }
                        a.next();
                        b.next();
                    }
                },
            }
        }
        let link = Link {
            next: to_tag.to_owned(),
            gen_from: from.generation,
            delta,
            other: !same_except_heads(from, to),
        };
        if self.links.insert(from_tag.to_owned(), link).is_none() {
            self.order.push_back(from_tag.to_owned());
        }
        while self.order.len() > CHAIN_MAX {
            if let Some(old) = self.order.pop_front() {
                self.links.remove(&old);
            }
        }
    }

    /// Reconcile `n`, a write based on version `base_tag`, with the current version `cur` (tag `cur_tag`).
    /// Returns the manifest to validate and store instead, or `None` when the write cannot be merged.
    pub fn merge(&self, base_tag: &str, cur_tag: &str, cur: &Manifest, n: &Manifest) -> Option<Manifest> {
        // Walk B -> ... -> C, composing the head changes: key -> (value at B, value at C).
        let mut changed: BTreeMap<&Key, Option<ObjectId>> = BTreeMap::new();
        let mut tag = base_tag;
        let base_gen = self.links.get(tag)?.gen_from;
        let mut steps = 0usize;
        while tag != cur_tag {
            let l = self.links.get(tag)?;
            if l.other {
                return None;
            }
            for (k, old, _) in &l.delta {
                changed.entry(k).or_insert(*old); // value at B = oldest `old`
            }
            tag = &l.next;
            steps += 1;
            if steps > CHAIN_MAX {
                return None;
            }
        }
        if n.generation != base_gen + 1 || !same_except_heads(n, cur) {
            return None;
        }
        let txid = *n.applied.last()?;
        if cur.applied.contains(&txid) {
            return None;
        }
        let mut merged = cur.clone();
        // Keys the write changed relative to B: those where n differs from C and C still equals B, plus keys
        // in `changed` where n differs from the value at B (a conflict).
        for (k, v) in &n.branches {
            match changed.get(k) {
                Some(at_b) => {
                    if Some(*v) != *at_b {
                        return None; // the write changed a head that also moved since B
                    }
                }
                None => {
                    if cur.branches.get(k) != Some(v) {
                        merged.branches.insert(k.clone(), *v);
                    }
                }
            }
        }
        for k in cur.branches.keys() {
            if !n.branches.contains_key(k) {
                match changed.get(k) {
                    // absent at B and added since: the write simply does not know it
                    Some(None) => {}
                    // present at B, the write deleted it, and it also moved since B
                    Some(Some(_)) => return None,
                    // untouched since B: the write deleted it
                    None => {
                        merged.branches.remove(k);
                    }
                }
            }
        }
        merged.generation = cur.generation + 1;
        merged.applied.push(txid);
        if merged.applied.len() > vcs_remote::manifest::MAX_APPLIED {
            let excess = merged.applied.len() - vcs_remote::manifest::MAX_APPLIED;
            merged.applied.drain(..excess);
        }
        Some(merged)
    }
}
