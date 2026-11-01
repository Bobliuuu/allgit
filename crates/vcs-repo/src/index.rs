//! Staging index: tracked path -> (blob id, mode, stat cache).

use crate::error::{Error, Result};
use crate::meta::{frame, unframe};
use std::collections::BTreeMap;
use std::ops::Deref;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_path, validate_path_after};
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
    /// Tracked names directly inside `dir` ("" = root): `files` are entries that are exactly
    /// `dir/name`, `dirs` the names with something tracked beneath them. Both sorted.
    pub fn children(&self, dir: &str) -> Children<'_> {
        use std::ops::Bound::{Included, Unbounded};
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut out = Children::default();
        let mut start = prefix.clone();
        'seek: loop {
            for (k, _) in self.map.range::<str, _>((Included(start.as_str()), Unbounded)) {
                let Some(rest) = k.strip_prefix(prefix.as_str()) else { break 'seek };
                match rest.find('/') {
                    None => out.files.push(rest),
                    Some(i) => {
                        let name = &rest[..i];
                        out.dirs.push(name);
                        // everything under `name/` is one contiguous run: jump past it
                        start = format!("{prefix}{name}0"); // '0' == '/' + 1
                        continue 'seek;
                    }
                }
            }
            break;
        }
        out.dirs.sort_unstable();
        out
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

/// See [`Entries::children`].
#[derive(Debug, Default)]
pub struct Children<'a> {
    pub files: Vec<&'a str>,
    pub dirs: Vec<&'a str>,
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

    /// Serialise as `AGI3`: entries in fixed-size blocks behind a length table, so a reader can
    /// decode the blocks on several threads. `AGI2` and `AGIX` (older layouts) are still read.
    pub fn encode(&self) -> Vec<u8> {
        let n = self.entries.len();
        // All blocks go into one buffer (sized for ~100 bytes an entry; untouched capacity costs
        // nothing), then the header, which needs their lengths, is put in front.
        let mut body: Vec<u8> = Vec::with_capacity(n * 100 + 64);
        let mut lens: Vec<usize> = Vec::with_capacity(n / BLOCK + 1);
        let (mut in_block, mut block_start) = (0usize, 0usize);
        for (p, e) in &self.entries {
            put_varint(&mut body, p.len() as u64);
            body.extend_from_slice(p.as_bytes());
            body.push(e.mode as u8);
            body.extend_from_slice(e.id.as_bytes());
            put_varint(&mut body, e.size);
            put_varint(&mut body, e.mtime_ns);
            put_varint(&mut body, e.ctime_ns);
            in_block += 1;
            if in_block == BLOCK {
                lens.push(body.len() - block_start);
                block_start = body.len();
                in_block = 0;
            }
        }
        if in_block > 0 {
            lens.push(body.len() - block_start);
        }
        // Optional cache-tree section (count 0 = absent).
        let mut tail = Writer::new();
        match (&self.entries.trees_fp, self.entries.trees.is_empty()) {
            (Some(fp), false) => {
                tail.varint(self.entries.trees.len() as u64).id(fp);
                for (d, id) in &self.entries.trees {
                    tail.str(d).id(id);
                }
            }
            _ => {
                tail.varint(0);
            }
        }
        let tail = tail.finish();
        let mut head = Writer::new();
        head.varint(self.written_ns)
            .varint(n as u64)
            .varint(BLOCK as u64)
            .varint(lens.len() as u64);
        for l in &lens {
            head.varint(*l as u64);
        }
        let head = head.finish();
        let mut out = Vec::with_capacity(5 + head.len() + body.len() + tail.len());
        out.extend_from_slice(b"AGI3");
        out.push(crate::meta::STATE_VERSION);
        out.extend_from_slice(&head);
        out.extend_from_slice(&body);
        out.extend_from_slice(&tail);
        out
    }

    /// The legacy `AGI2` encoding (kept for tests and for readers of older repositories).
    #[cfg(test)]
    fn encode_v2(&self) -> Vec<u8> {
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
        let c = |e: vcs_objects::DecodeError| Error::Corrupt(format!("index: {e}"));
        let v3 = bytes.starts_with(b"AGI3");
        // AGIX: legacy entries without a ctime.
        let legacy = bytes.starts_with(b"AGIX");
        let body = unframe(
            if v3 {
                b"AGI3"
            } else if legacy {
                b"AGIX"
            } else {
                b"AGI2"
            },
            bytes,
            "index",
        )?;
        let mut r = Reader::new(body);
        let written_ns = r.varint().map_err(c)?;
        let n = r.count(36).map_err(c)?;
        let mut idx = Index {
            entries: Entries::default(),
            written_ns,
        };
        let mut list: Vec<(String, IndexEntry)>;
        if v3 {
            let per = r.varint().map_err(c)? as usize;
            let nblocks = r.count(1).map_err(c)?;
            if per == 0 || per > (1 << 20) || nblocks != n.div_ceil(per) {
                return Err(Error::Corrupt("index: bad block table".into()));
            }
            let mut lens = Vec::with_capacity(nblocks);
            for _ in 0..nblocks {
                lens.push(r.varint().map_err(c)? as usize);
            }
            let mut slices: Vec<(&[u8], usize)> = Vec::with_capacity(nblocks);
            for (i, l) in lens.iter().enumerate() {
                let cnt = if i + 1 == nblocks { n - per * i } else { per };
                slices.push((r.raw(*l).map_err(c)?, cnt));
            }
            list = decode_blocks(&slices)?;
        } else {
            list = Vec::with_capacity(n);
            for _ in 0..n {
                let e = read_entry(&mut r, legacy, list.last().map_or("", |l| l.0.as_str()))?;
                list.push(e);
            }
        }
        // Entries are written in key order, so build the map in one pass and check file/directory
        // conflicts afterwards: a conflict is always "an ancestor directory of K is itself a key".
        let sorted = list.windows(2).all(|w| w[0].0 < w[1].0);
        if sorted {
            check_no_conflicts(&list)?;
            idx.entries.map = list.into_iter().collect();
        } else {
            for (p, e) in list {
                if idx.entries.map.contains_key(&p) {
                    return Err(Error::Corrupt(format!("index: duplicate path {p:?}")));
                }
                idx.insert(&p, e)?;
            }
        }
        if r.remaining() > 0 {
            let n = if v3 {
                r.varint().map_err(c)? as usize
            } else {
                r.count(33).map_err(c)?
            };
            if !v3 && n == 0 {
                return Err(Error::Corrupt("index: empty tree section".into()));
            }
            if n > 0 {
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
        } else if v3 {
            return Err(Error::Corrupt("index: missing tree section".into()));
        }
        r.finish().map_err(c)?;
        idx.entries.rev = 0;
        Ok(idx)
    }
}

/// Entries per `AGI3` block.
const BLOCK: usize = 2048;
/// Below this many entries a decode stays on the calling thread.
const PAR_MIN: usize = 16 * 1024;

/// Read one entry. `prev` is the previous (already validated) path of the same block, or "":
/// path components shared with it are not validated again.
fn read_entry(r: &mut Reader<'_>, legacy: bool, prev: &str) -> Result<(String, IndexEntry)> {
    let c = |e: vcs_objects::DecodeError| Error::Corrupt(format!("index: {e}"));
    let p = r.str(4096).map_err(c)?;
    validate_path_after(prev, &p)?;
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
    Ok((
        p,
        IndexEntry {
            id,
            mode,
            size,
            mtime_ns,
            ctime_ns,
        },
    ))
}

#[inline]
fn put_varint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

/// Worker threads for decoding `n` entries. CPU time is the scarce resource, and every extra
/// thread costs allocator-arena setup and page faults: on 100k entries (16-core box) one thread
/// takes 28 ms of CPU, four 31 ms (12 ms wall), eight 35 ms, sixteen 48 ms (10 ms wall). So
/// roughly one thread per 12.5k entries, at most four.
fn threads_for(n: usize) -> usize {
    if n < PAR_MIN {
        1
    } else {
        let cores = std::thread::available_parallelism().map_or(1, |t| t.get());
        (n / 12_500).clamp(1, 4).min(cores)
    }
}

/// Decode `AGI3` blocks (slice, entry count), on several threads for big indexes.
fn decode_blocks(blocks: &[(&[u8], usize)]) -> Result<Vec<(String, IndexEntry)>> {
    let total: usize = blocks.iter().map(|b| b.1).sum();
    let one = |(bytes, cnt): &(&[u8], usize)| -> Result<Vec<(String, IndexEntry)>> {
        let mut r = Reader::new(bytes);
        let mut v = Vec::with_capacity(*cnt);
        for _ in 0..*cnt {
            let e = read_entry(&mut r, false, v.last().map_or("", |l: &(String, IndexEntry)| l.0.as_str()))?;
            v.push(e);
        }
        r.finish().map_err(|e| Error::Corrupt(format!("index: {e}")))?;
        Ok(v)
    };
    let threads = threads_for(total).min(blocks.len().max(1));
    if threads <= 1 {
        let mut out = Vec::with_capacity(total);
        for b in blocks {
            out.extend(one(b)?);
        }
        return Ok(out);
    }
    let per = blocks.len().div_ceil(threads);
    let parts: Vec<Result<Vec<(String, IndexEntry)>>> = std::thread::scope(|sc| {
        let one = &one;
        let hs: Vec<_> = blocks
            .chunks(per)
            .map(|chunk| {
                sc.spawn(move || {
                    let mut out = Vec::new();
                    for b in chunk {
                        out.extend(one(b)?);
                    }
                    Ok(out)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("index decode worker panicked")).collect()
    });
    let mut out = Vec::with_capacity(total);
    for p in parts {
        out.extend(p?);
    }
    Ok(out)
}

/// `list` is strictly sorted: fail if some key has an ancestor directory that is itself a key.
fn check_no_conflicts(list: &[(String, IndexEntry)]) -> Result<()> {
    let check = |part: &[(String, IndexEntry)]| -> Result<()> {
        // Sorted keys cluster by directory: the ancestors of a key that are also ancestors of
        // the previous key were already looked up, so only the new ones are searched.
        let mut prev_dir: &str = "";
        for (k, _) in part {
            let Some(dir_end) = k.rfind('/') else {
                prev_dir = "";
                continue;
            };
            let dir = &k[..dir_end];
            if dir == prev_dir {
                continue;
            }
            let mut pos = 0;
            while let Some(i) = k[pos..].find('/') {
                let anc = &k[..pos + i];
                pos += i + 1;
                let seen = prev_dir.len() >= anc.len()
                    && prev_dir.as_bytes().starts_with(anc.as_bytes())
                    && (prev_dir.len() == anc.len() || prev_dir.as_bytes()[anc.len()] == b'/');
                if !seen && list.binary_search_by(|(p, _)| p.as_str().cmp(anc)).is_ok() {
                    return Err(Error::Msg(format!("cannot track {k:?}: {anc:?} is tracked as a file")));
                }
            }
            prev_dir = dir;
        }
        Ok(())
    };
    let threads = threads_for(list.len());
    if threads <= 1 {
        return check(list);
    }
    let per = list.len().div_ceil(threads);
    let rs: Vec<Result<()>> = std::thread::scope(|sc| {
        let check = &check;
        let hs: Vec<_> = list.chunks(per).map(|part| sc.spawn(move || check(part))).collect();
        hs.into_iter().map(|h| h.join().expect("index check worker panicked")).collect()
    });
    rs.into_iter().collect()
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
    fn conflict_check_matches_a_naive_scan() {
        // Directory-clustered keys, so the "already checked ancestor" shortcut is exercised.
        let naive = |keys: &[&str]| {
            keys.iter().any(|k| {
                let mut pos = 0;
                while let Some(i) = k[pos..].find('/') {
                    if keys.contains(&&k[..pos + i]) {
                        return true;
                    }
                    pos += i + 1;
                }
                false
            })
        };
        let names = [
            "a", "a-x", "a.b", "a/b", "a/b/c", "a/b/d", "a/c", "a0", "b/c", "b/c/d/e", "b/d", "b",
        ];
        for mask in 0u32..(1 << names.len()) {
            let mut keys: Vec<&str> = (0..names.len()).filter(|i| mask >> i & 1 == 1).map(|i| names[i]).collect();
            keys.sort_unstable();
            let list: Vec<(String, IndexEntry)> = keys.iter().map(|k| (k.to_string(), e())).collect();
            assert_eq!(check_no_conflicts(&list).is_err(), naive(&keys), "{keys:?}");
        }
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

    #[test]
    fn v3_roundtrip_parallel_and_legacy_read() {
        let mut i = Index::default();
        for k in 0..40_000u64 {
            let p = format!("d{}/s{}/f{k}.txt", k % 13, k % 5);
            let mut en = e();
            en.size = k;
            en.mtime_ns = k * 3;
            i.insert(&p, en).unwrap();
        }
        i.written_ns = 7;
        i.entries.store_trees(
            ObjectId::from_bytes([4; 32]),
            [("".to_string(), ObjectId::from_bytes([5; 32]))].into_iter().collect(),
        );
        let b3 = i.encode();
        assert!(b3.starts_with(b"AGI3"));
        let d3 = Index::decode(&b3).unwrap();
        assert_eq!(d3.entries, i.entries);
        assert_eq!(d3.written_ns, 7);
        assert_eq!(d3.entries.tree_count(), 1);
        // the previous on-disk layout still reads, and re-encodes as the new one
        let d2 = Index::decode(&i.encode_v2()).unwrap();
        assert_eq!(d2.entries, i.entries);
        assert_eq!(d2.entries.tree_count(), 1);
        // a conflict hidden in a late block is still caught
        let mut c = Index::default();
        for k in 0..40_000u64 {
            c.insert(&format!("x{k:06}"), e()).unwrap();
        }
        let mut bad = c.clone();
        bad.entries.insert("x039999/y".into(), e()); // bypass the conflict check
        assert!(Index::decode(&bad.encode()).is_err());
        // truncation anywhere in the body is an error, never a panic
        for cut in [5, 20, b3.len() / 2, b3.len() - 1] {
            assert!(Index::decode(&b3[..cut]).is_err());
        }
    }

    /// The pre-optimisation `AGI3` writer, verbatim: the new encoder must produce the same bytes.
    fn encode_v3_reference(ix: &Index) -> Vec<u8> {
        let mut blocks: Vec<Vec<u8>> = vec![];
        let mut w = Writer::new();
        let mut in_block = 0usize;
        for (p, e) in &ix.entries {
            w.str(p)
                .u8(e.mode as u8)
                .id(&e.id)
                .varint(e.size)
                .varint(e.mtime_ns)
                .varint(e.ctime_ns);
            in_block += 1;
            if in_block == BLOCK {
                blocks.push(std::mem::replace(&mut w, Writer::new()).finish());
                in_block = 0;
            }
        }
        if in_block > 0 {
            blocks.push(w.finish());
        }
        let mut w = Writer::new();
        w.varint(ix.written_ns)
            .varint(ix.entries.len() as u64)
            .varint(BLOCK as u64)
            .varint(blocks.len() as u64);
        for b in &blocks {
            w.varint(b.len() as u64);
        }
        for b in &blocks {
            w.raw(b);
        }
        match (&ix.entries.trees_fp, ix.entries.trees.is_empty()) {
            (Some(fp), false) => {
                w.varint(ix.entries.trees.len() as u64).id(fp);
                for (d, id) in &ix.entries.trees {
                    w.str(d).id(id);
                }
            }
            _ => {
                w.varint(0);
            }
        }
        frame(b"AGI3", w.finish())
    }

    #[test]
    fn encode_is_byte_identical_to_the_reference_writer() {
        for n in [0usize, 1, 2047, 2048, 2049, 5000] {
            let mut i = Index::default();
            for k in 0..n as u64 {
                let mut en = e();
                en.size = k * k * 1000;
                en.mtime_ns = 1_790_000_000_000_000_000 + k;
                en.ctime_ns = if k % 3 == 0 { 0 } else { k };
                en.mode = [EntryMode::File, EntryMode::Executable, EntryMode::Symlink][(k % 3) as usize];
                i.insert(&format!("d{}/é{k}.txt", k % 7), en).unwrap();
            }
            i.written_ns = 123_456_789_012;
            assert_eq!(i.encode(), encode_v3_reference(&i), "n={n}");
            i.entries.store_trees(
                ObjectId::from_bytes([4; 32]),
                [
                    ("".to_string(), ObjectId::from_bytes([5; 32])),
                    ("d1".to_string(), ObjectId::from_bytes([6; 32])),
                ]
                .into_iter()
                .collect(),
            );
            assert_eq!(i.encode(), encode_v3_reference(&i), "n={n} trees");
            assert_eq!(Index::decode(&i.encode()).unwrap().entries, i.entries);
        }
    }

    /// Measurement for #112, not a regression test: `cargo test -p vcs-repo --release index_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn index_cost() {
        use std::io::Write;
        use std::time::Instant;
        let cpu = || {
            let mut u: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
            let t = |x: libc::timeval| x.tv_sec as f64 * 1e3 + x.tv_usec as f64 / 1e3;
            t(u.ru_utime) + t(u.ru_stime)
        };
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
            let (mut enc, mut dec, mut wr, mut deccpu) = (vec![], vec![], vec![], vec![]);
            let dir = std::env::temp_dir().join(format!("ixcost{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for _ in 0..9 {
                let t = Instant::now();
                let b = ix.encode();
                enc.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let c0 = cpu();
                let d = Index::decode(&b).unwrap();
                dec.push(t.elapsed().as_secs_f64() * 1e3);
                deccpu.push(cpu() - c0);
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
                "entries={n} bytes={} encode_ms={:.2} decode_ms={:.2} decode_cpu_ms={:.2} write+fsync+rename_ms={:.2}",
                bytes.len(),
                med(enc),
                med(dec),
                med(deccpu),
                med(wr)
            );
        }
    }
}
