//! Zone-level read restriction (policy `allow-read` / `deny-read`).
//!
//! Objects are content-addressed and shared, so an object id says nothing about the zone it came
//! from. The guard therefore works from the manifest: for every zone it walks everything reachable
//! from that zone's branch heads and tags (commits, trees, chunk manifests; blobs are leaves and
//! are never downloaded in full) and remembers the ids. A restricted principal is refused an object
//! that is reachable from a zone it may not read and from none it may read. Everything else is
//! served: objects in no zone (not yet published, or snapshot/transaction/operation objects), and
//! objects shared with a readable zone (the principal can legitimately read that content anyway).
//!
//! The per-zone sets only grow while the process runs, an `ObjectId` (32 bytes) per reachable
//! object, and a manifest change costs a walk of the new history plus one 1-byte ranged GET per
//! new file entry. Nothing is persisted. This is documented, with the residual leakage, in
//! docs/server.md.

use std::collections::{BTreeSet, HashMap, HashSet};
use vcs_objects::{EntryMode, Object, ObjectId};
use vcs_remote::protocol::object_key;
use vcs_remote::{fetch_object, Backend, BackendError, Manifest};

/// Kind byte of a chunk manifest (first byte of the encoding); blobs are 1.
const KIND_BLOB: u8 = 1;

#[derive(Default)]
pub struct ReadIndex {
    tag: Option<String>,
    /// zone id -> its display label, if renamed
    labels: HashMap<String, String>,
    zones: HashMap<String, HashSet<ObjectId>>,
}

/// Canonical `objects/<2 hex>/<62 hex>` key (lowercase) to its id.
pub fn canonical_object_id(key: &str) -> Option<ObjectId> {
    let rest = key.strip_prefix("objects/")?;
    let (a, b) = rest.split_once('/')?;
    if a.len() != 2 || b.len() != 62 || !rest.bytes().all(|c| c == b'/' || c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return None;
    }
    ObjectId::from_hex(&format!("{a}{b}")).ok()
}

impl ReadIndex {
    /// Bring the per-zone sets up to date with the stored manifest (cheap when its tag is unchanged).
    pub fn refresh(&mut self, backend: &dyn Backend) -> Result<(), BackendError> {
        let Some(v) = backend.get(vcs_remote::manifest::KEY)? else {
            return Ok(());
        };
        if self.tag.as_deref() == Some(v.tag.as_str()) {
            return Ok(());
        }
        let m = Manifest::decode(&v.data).map_err(|e| BackendError::Other(format!("stored manifest invalid: {e}")))?;
        let mut roots: HashMap<String, Vec<ObjectId>> = HashMap::new();
        for ((z, _), id) in &m.branches {
            roots.entry(z.clone()).or_default().push(*id);
        }
        for ((z, _), id) in &m.tags {
            roots.entry(z.clone()).or_default().push(*id);
        }
        for z in m.zones.keys() {
            roots.entry(z.clone()).or_default();
        }
        for (zone, starts) in roots {
            let set = self.zones.entry(zone).or_default();
            walk(backend, starts, set)?;
        }
        self.labels = m.labels.into_iter().collect();
        self.tag = Some(v.tag);
        Ok(())
    }

    /// Is `id` reachable only from zones for which `readable(name)` is false?
    pub fn denied_only(&self, id: &ObjectId, readable: &dyn Fn(&str) -> bool) -> bool {
        let (mut in_denied, mut in_allowed) = (false, false);
        for (zone, set) in &self.zones {
            if !set.contains(id) {
                continue;
            }
            // a zone is readable only if both its id and its display label are
            let ok = readable(zone) && self.labels.get(zone).is_none_or(|l| readable(l));
            if ok {
                in_allowed = true;
            } else {
                in_denied = true;
            }
        }
        in_denied && !in_allowed
    }

    pub fn zone_names(&self) -> BTreeSet<&str> {
        self.zones.keys().map(String::as_str).collect()
    }
}

/// Add everything reachable from `starts` to `seen`. A missing object ends that branch of the walk.
/// Level by level: each level's objects are read concurrently (`VCS_REMOTE_PARALLEL`, default 8), so
/// the walk costs round trips per tree depth, not one per file.
fn walk(b: &dyn Backend, starts: Vec<ObjectId>, seen: &mut HashSet<ObjectId>) -> Result<(), BackendError> {
    // (id, may be a plain blob: probe its kind with a 1-byte read before downloading)
    let mut level: Vec<(ObjectId, bool)> = starts.into_iter().map(|i| (i, false)).collect();
    let limit = vcs_remote::par::parallelism();
    while !level.is_empty() {
        level.retain(|(id, _)| seen.insert(*id));
        let next = vcs_remote::par::map_bounded(&level, limit, |&(id, probe)| -> Result<Vec<(ObjectId, bool)>, BackendError> {
            if probe {
                if let Some(k) = b.get_range(&object_key(&id), 0, 1)? {
                    if k.first() == Some(&KIND_BLOB) {
                        return Ok(Vec::new());
                    }
                }
            }
            let Ok(obj) = fetch_object(b, &id) else { return Ok(Vec::new()) };
            Ok(match obj {
                Object::Commit(c) => c.parents.iter().map(|p| (*p, false)).chain([(c.root, false)]).collect(),
                Object::Tree(t) => t
                    .entries()
                    .iter()
                    .map(|e| (e.id, !matches!(e.mode, EntryMode::Directory)))
                    .collect(),
                Object::Chunked(m) => m.chunks.iter().map(|c| (c.id, true)).collect(),
                Object::Tag(t) => vec![(t.target, false)],
                Object::Snapshot(_) | Object::Transaction(_) | Object::Operation(_) | Object::Blob(_) => Vec::new(),
            })
        })?;
        level = next.into_iter().flatten().collect();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_keys_only() {
        let h = "ab".to_owned() + &"0".repeat(62);
        assert!(canonical_object_id(&format!("objects/ab/{}", &h[2..])).is_some());
        assert!(canonical_object_id(&format!("objects/AB/{}", &h[2..])).is_none());
        assert!(canonical_object_id(&format!("objects/ab/{}x", &h[2..])).is_none());
        assert!(canonical_object_id("objects/ab/").is_none());
        assert!(canonical_object_id("packs/x.pack").is_none());
    }
}
