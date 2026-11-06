//! History-truncated fetch (shallow clone / deepen).
//!
//! Walks commits breadth-first from `starts` (generation 1 = a start), stops at a depth or a
//! date, fetches the commits that are inside the limit plus their trees (never file content),
//! and reports which fetched commits were cut short (their parents were left on the remote).
//! Every byte is verified against its id by [`crate::packfetch::fetch_many_with`] before use.

use crate::packfetch::{fetch_many_with, PackCatalog};
use crate::protocol::PublishError;
use std::collections::{BTreeSet, HashMap, HashSet};
use vcs_objects::{EntryMode, Object, ObjectId};
use vcs_store::ObjectStore;

/// Where to cut history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cut {
    /// Keep this many commits along every path from a start (`1` = only the starts).
    Depth(u32),
    /// Keep commits whose author time is at least this (unix seconds). A start is always kept.
    Since(u64),
}

fn remote_err(s: String) -> PublishError {
    PublishError::Remote(s)
}

/// Fetch commits (and their trees) reachable from `starts` within `cut` (`None`: all of it)
/// into `local`, stopping at commits `local` already has. Returns the fetched commits that
/// still have parents left behind (the new shallow boundary).
pub fn fetch_commits_cut(
    backend: &dyn crate::Backend,
    local: &dyn ObjectStore,
    cat: &PackCatalog,
    starts: &[ObjectId],
    cut: Option<Cut>,
) -> Result<BTreeSet<ObjectId>, PublishError> {
    // (commit to read, the included child that named it, the zone it must be in)
    let mut level: Vec<(ObjectId, Option<ObjectId>, Option<String>)> = starts.iter().map(|s| (*s, None, None)).collect();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut boundary = BTreeSet::new();
    let mut included: Vec<(ObjectId, Vec<u8>, ObjectId)> = Vec::new();
    let mut gen = 1u32;
    while !level.is_empty() {
        let mut want = Vec::new();
        for (id, _, _) in &level {
            if !seen.contains(id) && !local.contains(id)? {
                want.push(*id);
            }
        }
        want.sort();
        want.dedup();
        let got: HashMap<ObjectId, Vec<u8>> = fetch_many_with(backend, cat, &want)?.into_iter().collect();
        let mut next = Vec::new();
        for (id, child, zone) in level {
            if !seen.insert(id) || local.contains(&id)? {
                continue;
            }
            let bytes = got.get(&id).ok_or_else(|| remote_err(format!("commit {id} missing on remote")))?;
            let Object::Commit(c) = Object::decode(bytes).map_err(|e| remote_err(format!("object {id}: {e}")))? else {
                return Err(remote_err(format!("object {id} is not a commit")));
            };
            if zone.as_deref().is_some_and(|z| z != c.zone) {
                return Err(remote_err(format!("commit {id} has parent from different zone")));
            }
            if let (Some(Cut::Since(t)), Some(ch)) = (cut, child) {
                if c.author.timestamp < t {
                    boundary.insert(ch); // too old: the child keeps its parent on the remote
                    continue;
                }
            }
            included.push((id, bytes.clone(), c.root));
            if matches!(cut, Some(Cut::Depth(n)) if gen >= n) && !c.parents.is_empty() {
                boundary.insert(id);
            } else {
                next.extend(c.parents.iter().map(|p| (*p, Some(id), Some(c.zone.clone()))));
            }
        }
        level = next;
        gen += 1;
    }
    // trees of the included commits, level by level, pruned at what is already stored
    let mut trees: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    let mut done: HashSet<ObjectId> = HashSet::new();
    let mut lvl: Vec<ObjectId> = included.iter().map(|(_, _, r)| *r).collect();
    while !lvl.is_empty() {
        lvl.sort();
        lvl.dedup();
        let mut want = Vec::new();
        for id in lvl {
            if done.insert(id) && !local.contains(&id)? {
                want.push(id);
            }
        }
        let mut next = Vec::new();
        for (id, bytes) in fetch_many_with(backend, cat, &want)? {
            match Object::decode(&bytes).map_err(|e| remote_err(format!("object {id}: {e}")))? {
                Object::Tree(t) => next.extend(t.entries().iter().filter(|e| e.mode == EntryMode::Directory).map(|e| e.id)),
                _ => return Err(remote_err(format!("object {id} is not a tree"))),
            }
            trees.push((id, bytes));
        }
        lvl = next;
    }
    // Everything is verified and in memory: store trees, then commits, then make it durable.
    for (id, b) in &trees {
        local.put_encoded_prehashed(id, b)?;
    }
    for (id, b, _) in &included {
        local.put_encoded_prehashed(id, b)?;
    }
    local.sync()?;
    Ok(boundary)
}
