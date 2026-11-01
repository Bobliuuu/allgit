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
fn walk(b: &dyn Backend, starts: Vec<ObjectId>, seen: &mut HashSet<ObjectId>) -> Result<(), BackendError> {
    // (id, may be a plain blob: probe its kind with a 1-byte read before downloading)
    let mut stack: Vec<(ObjectId, bool)> = starts.into_iter().map(|i| (i, false)).collect();
    while let Some((id, probe)) = stack.pop() {
        if seen.contains(&id) {
            continue;
        }
        if probe {
            if let Some(k) = b.get_range(&object_key(&id), 0, 1)? {
                if k.first() == Some(&KIND_BLOB) {
                    seen.insert(id);
                    continue;
                }
            }
        }
        seen.insert(id);
        let Ok(obj) = fetch_object(b, &id) else { continue };
        match obj {
            Object::Commit(c) => {
                stack.extend(c.parents.iter().map(|p| (*p, false)));
                stack.push((c.root, false));
            }
            Object::Tree(t) => {
                for e in t.entries() {
                    stack.push((e.id, !matches!(e.mode, EntryMode::Directory)));
                }
            }
            Object::Chunked(m) => stack.extend(m.chunks.iter().map(|c| (c.id, true))),
            Object::Tag(t) => stack.push((t.target, false)),
            Object::Snapshot(_) | Object::Transaction(_) | Object::Operation(_) | Object::Blob(_) => {}
        }
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
