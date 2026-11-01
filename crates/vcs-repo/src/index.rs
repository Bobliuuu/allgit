//! Staging index: tracked path -> (blob id, mode, stat cache).

use crate::error::{Error, Result};
use crate::meta::{frame, unframe};
use std::collections::BTreeMap;
use std::ops::Deref;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::validate_path;
use vcs_objects::{EntryMode, ObjectId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: ObjectId,
    pub mode: EntryMode,
    pub size: u64,
    pub mtime_ns: u64,
    /// Inode change time when the entry was recorded; 0 = unknown (legacy index, or an entry
    /// written without looking at the file). A rename-over or `utimes` restore changes it even
    /// when size and mtime come out equal.
    pub ctime_ns: u64,
}

impl IndexEntry {
    /// The later of the two timestamps: stat equality is only trusted once it is this old.
    pub fn stat_edge(&self, racy: u64) -> u64 {
        self.mtime_ns.max(self.ctime_ns).saturating_add(racy)
    }
}

/// The tracked-path map plus a cache-tree: the tree id of directories whose contents have not
/// changed since the id was computed. Every mutation goes through methods here, so a change
/// beneath a directory always drops the cached ids of all its ancestors; reads go through
/// `Deref` to the plain map.
#[derive(Clone, Debug, Default)]
pub struct Entries {
    map: BTreeMap<String, IndexEntry>,
    /// Directory path ("" = root) -> id of the tree built from the entries beneath it.
    trees: BTreeMap<String, ObjectId>,
    /// Fingerprint of the zone layout the cached trees were computed under.
    trees_fp: Option<ObjectId>,
    /// Bumped on every real change to the map (not persisted).
    rev: u64,
}

impl PartialEq for Entries {
    fn eq(&self, other: &Self) -> bool {
        self.map == other.map
    }
}
impl Eq for Entries {}

impl Deref for Entries {
    type Target = BTreeMap<String, IndexEntry>;
    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<'a> IntoIterator for &'a Entries {
    type Item = (&'a String, &'a IndexEntry);
    type IntoIter = std::collections::btree_map::Iter<'a, String, IndexEntry>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl Entries {
    fn invalidate(&mut self, path: &str) {
        if self.trees.is_empty() {
            return;
        }
        self.trees.remove(path);
        let mut end = path.len();
        while let Some(i) = path[..end].rfind('/') {
            end = i;
            self.trees.remove(&path[..end]);
        }
        self.trees.remove("");
    }
    pub fn insert(&mut self, path: String, e: IndexEntry) -> Option<IndexEntry> {
        let old = self.map.insert(path.clone(), e.clone());
        // A stat-only refresh (same id and mode) does not change any tree.
        if old.as_ref().is_none_or(|o| o.id != e.id || o.mode != e.mode) {
            self.invalidate(&path);
        }
        if old.as_ref() != Some(&e) {
            self.rev += 1;
        }
        old
    }
    pub fn remove(&mut self, path: &str) -> Option<IndexEntry> {
        let old = self.map.remove(path);
        if old.is_some() {
            self.invalidate(path);
            self.rev += 1;
        }
        old
    }
    pub fn retain(&mut self, mut f: impl FnMut(&String, &IndexEntry) -> bool) {
        let before = self.map.len();
        self.map.retain(|k, v| f(k, v));
        if self.map.len() != before {
            self.trees.clear();
            self.rev += 1;
        }
    }
    /// Mutable access to one entry; conservatively drops the cached trees above it.
    pub fn get_mut(&mut self, path: &str) -> Option<&mut IndexEntry> {
        self.invalidate(path);
        self.rev += 1;
        self.map.get_mut(path)
    }
    /// Change counter (not persisted): differs iff some entry was added, removed or altered.
    pub fn revision(&self) -> u64 {
        self.rev
    }
    /// Cached tree ids, valid only under `fp` (a fingerprint of the zone layout).
    pub fn cached_trees(&self, fp: &ObjectId) -> Option<&BTreeMap<String, ObjectId>> {
        (self.trees_fp.as_ref() == Some(fp)).then_some(&self.trees)
    }
    /// Merge freshly computed tree ids (drops anything cached under another layout).
    pub fn store_trees(&mut self, fp: ObjectId, fresh: BTreeMap<String, ObjectId>) {
        if self.trees_fp != Some(fp) {
            self.trees.clear();
            self.trees_fp = Some(fp);
        }
        self.trees.extend(fresh);
    }
    pub fn tree_count(&self) -> usize {
        self.trees.len()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub entries: Entries,
    /// Wall-clock time of last write; stat-cache entries newer than this - 2s are "racy"
    /// and always re-hashed.
    pub written_ns: u64,
}

impl Index {
    /// Insert, refusing file/directory conflicts (`a` vs `a/b`).
    pub fn insert(&mut self, path: &str, e: IndexEntry) -> Result<()> {
        let mut pos = 0;
        while let Some(i) = path[pos..].find('/') {
            let anc = &path[..pos + i];
            if self.entries.contains_key(anc) {
                return Err(Error::Msg(format!("cannot track {path:?}: {anc:?} is tracked as a file")));
            }
            pos += i + 1;
        }
        let lo = format!("{path}/");
        let hi = format!("{path}0"); // '0' == '/' + 1
        if let Some((k, _)) = self.entries.range(lo..hi).next() {
            return Err(Error::Msg(format!("cannot track file {path:?}: {k:?} is tracked beneath it")));
        }
        self.entries.insert(path.to_owned(), e);
        Ok(())
    }

    /// Paths equal to `prefix` or beneath `prefix/`. Empty prefix = all.
    pub fn under(&self, prefix: &str) -> Vec<String> {
        if prefix.is_empty() {
            return self.entries.keys().cloned().collect();
        }
        let mut v = Vec::new();
        if self.entries.contains_key(prefix) {
            v.push(prefix.to_owned());
        }
        v.extend(
            self.entries
                .range(format!("{prefix}/")..format!("{prefix}0"))
                .map(|(k, _)| k.clone()),
        );
        v
    }

    /// `!under(prefix).is_empty()` without allocating: is `prefix` or anything beneath it tracked?
    pub fn has_under(&self, prefix: &str) -> bool {
        if prefix.is_empty() {
            return !self.entries.is_empty();
        }
        if self.entries.contains_key(prefix) {
            return true;
        }
        self.entries.range(format!("{prefix}/")..format!("{prefix}0")).next().is_some()
    }

    /// The `written_ns` to record when saving at time `now`. An entry is trusted by its stat
    /// alone only when `mtime + racy <= written_ns`. Advancing `written_ns` past the end of an
    /// entry's racy window is sound only if that entry's content was verified after the window
    /// closed (`verified`); every other entry whose window is still open under the current
    /// value keeps it open. Symlinks are never trusted by stat, so they never hold it back.
    /// The result never goes backwards.
    pub fn settled_written_ns(&self, now: u64, racy: u64, verified: impl Fn(&str) -> bool) -> u64 {
        let old = self.written_ns;
        let mut cap = now;
        for (p, e) in &self.entries {
            if e.mode == EntryMode::Symlink {
                continue;
            }
            let edge = e.stat_edge(racy);
            if edge > old && edge <= cap && !verified(p) {
                cap = edge - 1;
            }
        }
        old.max(cap)
    }

    /// Would moving `written_ns` to `next` take any entry out of its racy window?
    pub fn unlocks(&self, next: u64, racy: u64) -> bool {
        self.entries.values().any(|e| {
            let edge = e.stat_edge(racy);
            e.mode != EntryMode::Symlink && edge > self.written_ns && edge <= next
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.written_ns).varint(self.entries.len() as u64);
        for (p, e) in &self.entries {
            w.str(p)
                .u8(e.mode as u8)
                .id(&e.id)
                .varint(e.size)
                .varint(e.mtime_ns)
                .varint(e.ctime_ns);
        }
        // Optional trailing cache-tree section (absent in indexes written without one).
        if !self.entries.trees.is_empty() {
            if let Some(fp) = &self.entries.trees_fp {
                w.varint(self.entries.trees.len() as u64).id(fp);
                for (d, id) in &self.entries.trees {
                    w.str(d).id(id);
                }
            }
        }
        frame(b"AGI2", w.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        // AGIX: legacy entries without a ctime.
        let legacy = bytes.starts_with(b"AGIX");
        let body = unframe(if legacy { b"AGIX" } else { b"AGI2" }, bytes, "index")?;
        let mut r = Reader::new(body);
        let c = |e: vcs_objects::DecodeError| Error::Corrupt(format!("index: {e}"));
        let written_ns = r.varint().map_err(c)?;
        let n = r.count(36).map_err(c)?;
        let mut idx = Index {
            entries: Entries::default(),
            written_ns,
        };
        // Entries are written in key order, so build the map in one pass and check file/directory
        // conflicts afterwards: a conflict is always "an ancestor directory of K is itself a key".
        let mut list: Vec<(String, IndexEntry)> = Vec::with_capacity(n);
        let mut sorted = true;
        for _ in 0..n {
            let p = r.str(4096).map_err(c)?;
            validate_path(&p)?;
            let mode = match r.u8().map_err(c)? {
                1 => EntryMode::File,
                2 => EntryMode::Executable,
                4 => EntryMode::Symlink,
                _ => return Err(Error::Corrupt("index: bad mode".into())),
            };
            let id = r.id().map_err(c)?;
            let size = r.varint().map_err(c)?;
            let mtime_ns = r.varint().map_err(c)?;
            let ctime_ns = if legacy { 0 } else { r.varint().map_err(c)? };
            if let Some((prev, _)) = list.last() {
                sorted &= prev.as_str() < p.as_str();
            }
            list.push((
                p,
                IndexEntry {
                    id,
                    mode,
                    size,
                    mtime_ns,
                    ctime_ns,
                },
            ));
        }
        if sorted {
            idx.entries.map = list.into_iter().collect();
            for k in idx.entries.map.keys() {
                let mut pos = 0;
                while let Some(i) = k[pos..].find('/') {
                    let anc = &k[..pos + i];
                    if idx.entries.map.contains_key(anc) {
                        return Err(Error::Msg(format!("cannot track {k:?}: {anc:?} is tracked as a file")));
                    }
                    pos += i + 1;
                }
            }
        } else {
            for (p, e) in list {
                if idx.entries.map.contains_key(&p) {
                    return Err(Error::Corrupt(format!("index: duplicate path {p:?}")));
                }
                idx.insert(&p, e)?;
            }
        }
        if r.remaining() > 0 {
            let n = r.count(33).map_err(c)?;
            let fp = r.id().map_err(c)?;
            let mut trees = BTreeMap::new();
            for _ in 0..n {
                let d = r.str(4096).map_err(c)?;
                if !d.is_empty() {
                    validate_path(&d)?;
                }
                trees.insert(d, r.id().map_err(c)?);
            }
            idx.entries.trees = trees;
            idx.entries.trees_fp = Some(fp);
        }
        r.finish().map_err(c)?;
        idx.entries.rev = 0;
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::frame;
    fn e() -> IndexEntry {
        IndexEntry {
            id: ObjectId::from_bytes([1; 32]),
            mode: EntryMode::File,
            size: 1,
            mtime_ns: 2,
            ctime_ns: 3,
        }
    }
    #[test]
    fn file_dir_conflicts() {
        let mut i = Index::default();
        i.insert("a/b", e()).unwrap();
        assert!(i.insert("a", e()).is_err());
        assert!(i.insert("a/b/c", e()).is_err());
        i.insert("a/c", e()).unwrap();
        i.insert("a0", e()).unwrap(); // sorts right after "a/": must not confuse range logic
        assert_eq!(i.under("a"), vec!["a/b", "a/c"]);
    }
    #[test]
    fn roundtrip() {
        let mut i = Index::default();
        i.insert("x/y", e()).unwrap();
        i.written_ns = 99;
        let j = Index::decode(&i.encode()).unwrap();
        assert_eq!(j.entries, i.entries);
        assert_eq!(j.written_ns, 99);
    }

    fn raw(magic: &[u8; 4], paths: &[&str], ctime: bool) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(5).varint(paths.len() as u64);
        for p in paths {
            w.str(p).u8(1).id(&ObjectId::from_bytes([1; 32])).varint(1).varint(2);
            if ctime {
                w.varint(3);
            }
        }
        frame(magic, w.finish())
    }

    #[test]
    fn decode_rejects_conflicts_duplicates_and_accepts_unsorted_and_legacy() {
        // file/dir conflict, sorted: "a" < "a-x" < "a/b"
        assert!(Index::decode(&raw(b"AGI2", &["a", "a-x", "a/b"], true)).is_err());
        // conflict, unsorted
        assert!(Index::decode(&raw(b"AGI2", &["a/b", "a"], true)).is_err());
        // duplicate
        assert!(Index::decode(&raw(b"AGI2", &["a", "a"], true)).is_err());
        assert!(Index::decode(&raw(b"AGI2", &["b", "a", "b"], true)).is_err());
        // unsorted but valid
        let i = Index::decode(&raw(b"AGI2", &["b", "a/x", "a-y"], true)).unwrap();
        assert_eq!(i.entries.len(), 3);
        // legacy frame (no ctime) decodes with ctime 0
        let l = Index::decode(&raw(b"AGIX", &["a", "b/c"], false)).unwrap();
        assert!(l.entries.values().all(|e| e.ctime_ns == 0 && e.mtime_ns == 2));
        assert_eq!(l.written_ns, 5);
        // truncated and trailing garbage
        let ok = raw(b"AGI2", &["a"], true);
        assert!(Index::decode(&ok[..ok.len() - 1]).is_err());
        let mut t = ok.clone();
        t.push(0);
        assert!(Index::decode(&t).is_err());
    }

    #[test]
    fn cache_tree_survives_roundtrip_and_is_invalidated_by_changes() {
        let mut i = Index::default();
        for p in ["a/b/c", "a/b/d", "a/e", "z"] {
            i.insert(p, e()).unwrap();
        }
        let fp = ObjectId::from_bytes([9; 32]);
        let t = |n: u8| ObjectId::from_bytes([n; 32]);
        i.entries.store_trees(
            fp,
            [("".to_string(), t(1)), ("a".to_string(), t(2)), ("a/b".to_string(), t(3))]
                .into_iter()
                .collect(),
        );
        let j = Index::decode(&i.encode()).unwrap();
        assert_eq!(j.entries.cached_trees(&fp).map(|m| m.len()), Some(3));
        assert!(j.entries.cached_trees(&ObjectId::from_bytes([8; 32])).is_none(), "other layout");
        let mut j = j;
        j.entries.remove("z"); // only the root is above `z`
        assert_eq!(j.entries.cached_trees(&fp).unwrap().len(), 2);
        let mut e2 = e();
        e2.mtime_ns = 77; // stat-only refresh keeps trees
        j.entries.insert("a/e".into(), e2);
        assert_eq!(j.entries.cached_trees(&fp).unwrap().len(), 2);
        let mut e3 = e();
        e3.id = t(5);
        j.entries.insert("a/b/c".into(), e3);
        assert_eq!(j.entries.cached_trees(&fp).unwrap().len(), 0);
    }

    /// Measurement for #112, not a regression test: `cargo test -p vcs-repo --release index_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn index_cost() {
        use std::io::Write;
        use std::time::Instant;
        let med = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        for n in [5_000usize, 100_000] {
            let mut ix = Index::default();
            for i in 0..n {
                let p = format!("d{}/d{}/d{}/file{i}.txt", i % 7, i % 31, i % 211);
                ix.insert(
                    &p,
                    IndexEntry {
                        id: ObjectId::from_bytes([(i % 251) as u8; 32]),
                        mode: EntryMode::File,
                        size: 600 + i as u64,
                        mtime_ns: 1_790_000_000_000_000_000 + i as u64 * 7919,
                        ctime_ns: 1_790_000_000_000_000_000 + i as u64 * 7919,
                    },
                )
                .unwrap();
            }
            let bytes = ix.encode();
            let (mut enc, mut dec, mut wr) = (vec![], vec![], vec![]);
            let dir = std::env::temp_dir().join(format!("ixcost{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for _ in 0..9 {
                let t = Instant::now();
                let b = ix.encode();
                enc.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let d = Index::decode(&b).unwrap();
                dec.push(t.elapsed().as_secs_f64() * 1e3);
                assert_eq!(d.entries.len(), n);
                let t = Instant::now();
                let tmp = dir.join("index.tmp");
                let mut f = std::fs::File::create(&tmp).unwrap();
                f.write_all(&b).unwrap();
                f.sync_all().unwrap();
                std::fs::rename(&tmp, dir.join("index")).unwrap();
                wr.push(t.elapsed().as_secs_f64() * 1e3);
            }
            std::fs::remove_dir_all(&dir).unwrap();
            println!(
                "entries={n} bytes={} encode_ms={:.2} decode_ms={:.2} write+fsync+rename_ms={:.2}",
                bytes.len(),
                med(enc),
                med(dec),
                med(wr)
            );
        }
    }
}
