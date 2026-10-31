//! Staging index: tracked path -> (blob id, mode, stat cache).

use crate::error::{Error, Result};
use crate::meta::{frame, unframe};
use std::collections::BTreeMap;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::validate_path;
use vcs_objects::{EntryMode, ObjectId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: ObjectId,
    pub mode: EntryMode,
    pub size: u64,
    pub mtime_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub entries: BTreeMap<String, IndexEntry>,
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

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.written_ns).varint(self.entries.len() as u64);
        for (p, e) in &self.entries {
            w.str(p).u8(e.mode as u8).id(&e.id).varint(e.size).varint(e.mtime_ns);
        }
        frame(b"AGIX", w.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(b"AGIX", bytes, "index")?;
        let mut r = Reader::new(body);
        let c = |e: vcs_objects::DecodeError| Error::Corrupt(format!("index: {e}"));
        let written_ns = r.varint().map_err(c)?;
        let n = r.count(36).map_err(c)?;
        let mut idx = Index {
            entries: BTreeMap::new(),
            written_ns,
        };
        for _ in 0..n {
            let p = r.str(4096).map_err(c)?;
            validate_path(&p)?;
            let mode = match r.u8().map_err(c)? {
                1 => EntryMode::File,
                2 => EntryMode::Executable,
                _ => return Err(Error::Corrupt("index: bad mode".into())),
            };
            let id = r.id().map_err(c)?;
            let size = r.varint().map_err(c)?;
            let mtime_ns = r.varint().map_err(c)?;
            idx.insert(&p, IndexEntry { id, mode, size, mtime_ns })?;
        }
        r.finish().map_err(c)?;
        Ok(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e() -> IndexEntry {
        IndexEntry {
            id: ObjectId::from_bytes([1; 32]),
            mode: EntryMode::File,
            size: 1,
            mtime_ns: 2,
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
}
