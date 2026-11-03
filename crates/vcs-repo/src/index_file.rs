//! Reading and patching the on-disk index without decoding all of it.
//!
//! The `AGI3`/`AGI4` layouts store entries in blocks behind a length table. A point lookup only
//! needs the first path of every block (to binary-search) and one decoded block; a patch (a few
//! entries added, changed or removed) re-encodes only the blocks it touches and copies every
//! other block's bytes as they are. Commands that change a handful of entries (`rm`,
//! `restore --staged`, `reset <path>`, `add -u` on a mostly clean tree) therefore cost a file
//! read, a block or two of work and a file write, instead of decoding and encoding every entry.
//!
//! Patched files are written as `AGI4`, whose table carries each block's own entry count (a
//! removal leaves an uneven block); `Index::decode` reads both. A patch keeps `written_ns` as it
//! was: entries it adds are then "racy" at worst (re-read once by the next status), never
//! wrongly trusted.

use crate::error::{Error, Result};
use crate::index::{put_entry, put_varint, read_entry, IndexEntry, BLOCK};
use crate::meta::{unframe, STATE_VERSION};
use std::collections::BTreeMap;
use vcs_objects::codec::Reader;
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

struct Blk {
    off: usize,
    len: usize,
    count: usize,
    first: String,
}

/// A parsed index file: header and block table only; entries are decoded on demand.
pub(crate) struct IndexFile {
    bytes: Vec<u8>,
    pub written_ns: u64,
    blocks: Vec<Blk>,
    /// Offset of the cache-tree section.
    tail: usize,
}

/// Edits to apply: `Some(entry)` sets (adds or replaces), `None` removes.
pub(crate) type Edits = BTreeMap<String, Option<IndexEntry>>;

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("index: {e}"))
}

impl IndexFile {
    /// Parse the header. `None` for the older layouts (`AGI2`, `AGIX`), which have no blocks.
    pub(crate) fn parse(bytes: Vec<u8>) -> Result<Option<IndexFile>> {
        let v4 = bytes.starts_with(b"AGI4");
        if !v4 && !bytes.starts_with(b"AGI3") {
            return Ok(None);
        }
        let body = unframe(if v4 { b"AGI4" } else { b"AGI3" }, &bytes, "index")?;
        let mut r = Reader::new(body);
        let written_ns = r.varint().map_err(corrupt)?;
        let n = r.count(36).map_err(corrupt)?;
        let mut meta: Vec<(usize, usize)> = Vec::new();
        if v4 {
            let nblocks = r.count(2).map_err(corrupt)?;
            for _ in 0..nblocks {
                let len = r.varint().map_err(corrupt)? as usize;
                let cnt = r.varint().map_err(corrupt)? as usize;
                meta.push((len, cnt));
            }
        } else {
            let per = r.varint().map_err(corrupt)? as usize;
            let nblocks = r.count(1).map_err(corrupt)?;
            if per == 0 || per > (1 << 20) || nblocks != n.div_ceil(per) {
                return Err(corrupt("bad block table"));
            }
            for i in 0..nblocks {
                let len = r.varint().map_err(corrupt)? as usize;
                meta.push((len, if i + 1 == nblocks { n - per * i } else { per }));
            }
        }
        if meta.iter().map(|m| m.1).sum::<usize>() != n || meta.iter().any(|m| m.1 == 0) {
            return Err(corrupt("bad block table"));
        }
        let mut off = bytes.len() - r.remaining();
        let mut blocks = Vec::with_capacity(meta.len());
        for (len, count) in meta {
            let b = r.raw(len).map_err(corrupt)?;
            let first = Reader::new(b).str(4096).map_err(corrupt)?;
            blocks.push(Blk { off, len, count, first });
            off += len;
        }
        if r.remaining() == 0 {
            return Err(corrupt("missing tree section"));
        }
        Ok(Some(IndexFile {
            bytes,
            written_ns,
            blocks,
            tail: off,
        }))
    }

    /// No entries at all (a patch needs a block to start from).
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.blocks.iter().map(|b| b.count).sum()
    }

    fn decode_block(&self, i: usize) -> Result<Vec<(String, IndexEntry)>> {
        let b = &self.blocks[i];
        let mut r = Reader::new(&self.bytes[b.off..b.off + b.len]);
        let mut v: Vec<(String, IndexEntry)> = Vec::with_capacity(b.count);
        for _ in 0..b.count {
            let e = read_entry(&mut r, false, v.last().map_or("", |l| l.0.as_str()))?;
            v.push(e);
        }
        r.finish().map_err(corrupt)?;
        Ok(v)
    }

    /// Index of the block that holds (or would hold) `key`.
    fn block_for(&self, key: &str) -> usize {
        self.blocks.partition_point(|b| b.first.as_str() <= key).saturating_sub(1)
    }

    /// The entry for exactly `path`.
    pub(crate) fn get(&self, path: &str) -> Result<Option<IndexEntry>> {
        if self.blocks.is_empty() {
            return Ok(None);
        }
        let v = self.decode_block(self.block_for(path))?;
        Ok(v.binary_search_by(|(k, _)| k.as_str().cmp(path)).ok().map(|i| v[i].1.clone()))
    }

    /// First tracked path that sorts at or after `key`.
    fn lower_bound(&self, key: &str) -> Result<Option<String>> {
        if self.blocks.is_empty() {
            return Ok(None);
        }
        let i = self.block_for(key);
        let v = self.decode_block(i)?;
        let at = v.partition_point(|(k, _)| k.as_str() < key);
        if let Some((k, _)) = v.into_iter().nth(at) {
            return Ok(Some(k));
        }
        Ok(self.blocks.get(i + 1).map(|b| b.first.clone()))
    }

    /// Entries equal to `prefix` or beneath `prefix/` (all of them for an empty prefix), in order.
    pub(crate) fn under(&self, prefix: &str) -> Result<Vec<(String, IndexEntry)>> {
        let mut out = Vec::new();
        if self.blocks.is_empty() {
            return Ok(out);
        }
        let (lo, hi) = (format!("{prefix}/"), format!("{prefix}0")); // '0' == '/' + 1
        let first = if prefix.is_empty() { 0 } else { self.block_for(prefix) };
        for i in first..self.blocks.len() {
            if !prefix.is_empty() && self.blocks[i].first.as_str() >= hi.as_str() {
                break;
            }
            for (k, e) in self.decode_block(i)? {
                if prefix.is_empty() || k == prefix || (k.as_str() >= lo.as_str() && k.as_str() < hi.as_str()) {
                    out.push((k, e));
                }
            }
        }
        Ok(out)
    }

    /// The cached tree id of directory `dir` ("" = root) from the cache-tree section, when that
    /// section was computed under the zone layout `fp`. Reads only the tail, never an entry.
    pub(crate) fn cached_tree(&self, fp: &ObjectId, dir: &str) -> Result<Option<ObjectId>> {
        let mut tr = Reader::new(&self.bytes[self.tail..]);
        let tn = tr.varint().map_err(corrupt)? as usize;
        if tn == 0 || tr.id().map_err(corrupt)? != *fp {
            return Ok(None);
        }
        for _ in 0..tn {
            let d = tr.str(4096).map_err(corrupt)?;
            let id = tr.id().map_err(corrupt)?;
            if d == dir {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Apply `edits` and return the new file bytes plus how many entries really changed.
    pub(crate) fn patch(&self, edits: &Edits) -> Result<(Vec<u8>, usize)> {
        // 1. group the edits by block
        let mut per_block: BTreeMap<usize, Vec<(&String, &Option<IndexEntry>)>> = BTreeMap::new();
        for kv in edits {
            per_block.entry(self.block_for(kv.0)).or_default().push(kv);
        }
        // 2. rebuild the touched blocks
        let mut changed = 0usize;
        let mut added: Vec<&str> = Vec::new();
        let mut invalid: Vec<&str> = Vec::new(); // paths whose ancestors' cached trees go
        let mut rebuilt: BTreeMap<usize, Vec<(Vec<u8>, usize)>> = BTreeMap::new();
        for (&bi, list) in &per_block {
            let old = self.decode_block(bi)?;
            let mut out: Vec<(String, IndexEntry)> = Vec::with_capacity(old.len() + list.len());
            let mut oi = old.into_iter().peekable();
            for &(k, new) in list {
                while let Some(o) = oi.next_if(|o| o.0.as_str() < k.as_str()) {
                    out.push(o);
                }
                let cur = oi.next_if(|o| o.0.as_str() == k.as_str());
                match (cur, new) {
                    (Some(_), None) => {
                        changed += 1;
                        invalid.push(k);
                    }
                    (None, None) => {}
                    (Some((_, o)), Some(n)) => {
                        if o != *n {
                            changed += 1;
                        }
                        if o.id != n.id || o.mode != n.mode {
                            invalid.push(k);
                        }
                        out.push((k.clone(), n.clone()));
                    }
                    (None, Some(n)) => {
                        changed += 1;
                        added.push(k);
                        invalid.push(k);
                        out.push((k.clone(), n.clone()));
                    }
                }
            }
            out.extend(oi);
            let mut chunks = Vec::new();
            for part in out.chunks(BLOCK) {
                let mut buf = Vec::with_capacity(part.len() * 100);
                for (p, e) in part {
                    put_entry(&mut buf, p, e);
                }
                chunks.push((buf, part.len()));
            }
            rebuilt.insert(bi, chunks);
        }
        if changed == 0 {
            return Ok((self.bytes.clone(), 0));
        }
        // 3. file/directory conflicts for brand-new paths, judged on the state after the edits
        for p in added {
            self.check_conflicts(p, edits)?;
        }
        // 4. cache-tree section
        let mut tr = Reader::new(&self.bytes[self.tail..]);
        let tn = tr.varint().map_err(corrupt)? as usize;
        let mut fp: Option<ObjectId> = None;
        let mut trees: BTreeMap<String, ObjectId> = BTreeMap::new();
        if tn > 0 {
            fp = Some(tr.id().map_err(corrupt)?);
            for _ in 0..tn {
                let d = tr.str(4096).map_err(corrupt)?;
                trees.insert(d, tr.id().map_err(corrupt)?);
            }
        }
        tr.finish().map_err(corrupt)?;
        if !trees.is_empty() {
            for path in invalid {
                trees.remove(path);
                let mut end = path.len();
                while let Some(i) = path[..end].rfind('/') {
                    end = i;
                    trees.remove(&path[..end]);
                }
                trees.remove("");
            }
        }
        // 5. assemble
        let mut table: Vec<(usize, usize)> = Vec::with_capacity(self.blocks.len() + 1);
        let mut body: Vec<u8> = Vec::with_capacity(self.tail - self.blocks[0].off + 4096);
        let mut n_new = 0usize;
        for (i, b) in self.blocks.iter().enumerate() {
            match rebuilt.get(&i) {
                None => {
                    body.extend_from_slice(&self.bytes[b.off..b.off + b.len]);
                    table.push((b.len, b.count));
                    n_new += b.count;
                }
                Some(chunks) => {
                    for (buf, cnt) in chunks {
                        body.extend_from_slice(buf);
                        table.push((buf.len(), *cnt));
                        n_new += cnt;
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(body.len() + 128 + trees.len() * 40);
        out.extend_from_slice(b"AGI4");
        out.push(STATE_VERSION);
        put_varint(&mut out, self.written_ns);
        put_varint(&mut out, n_new as u64);
        put_varint(&mut out, table.len() as u64);
        for (l, c) in &table {
            put_varint(&mut out, *l as u64);
            put_varint(&mut out, *c as u64);
        }
        out.extend_from_slice(&body);
        match (fp, trees.is_empty()) {
            (Some(fp), false) => {
                put_varint(&mut out, trees.len() as u64);
                out.extend_from_slice(fp.as_bytes());
                for (d, id) in &trees {
                    put_varint(&mut out, d.len() as u64);
                    out.extend_from_slice(d.as_bytes());
                    out.extend_from_slice(id.as_bytes());
                }
            }
            _ => put_varint(&mut out, 0),
        }
        Ok((out, changed))
    }

    /// Is there a tracked path or directory that differs from `path` (not tracked itself) only by case or
    /// Unicode normalization, so that both could not exist on a folding filesystem? Returns the colliding
    /// pair like `portable::find_collision`. Looks only at the names that can clash: walking down `path`,
    /// a component that already exists spelled the same cannot have a variant (the index holds no such
    /// pair), and the first one that does not exist is compared with its parent's children; below a new
    /// directory nothing exists. A big directory costs one scan of its own entries, never of the tree.
    pub(crate) fn find_new_path_collision(&self, path: &str) -> Result<Option<(String, String)>> {
        let mut parent_end = 0usize; // `path[..parent_end]` is the existing parent (0: the root)
        loop {
            let rest = &path[if parent_end == 0 { 0 } else { parent_end + 1 }..];
            let name_end = rest.find('/').map_or(path.len(), |i| path.len() - rest.len() + i);
            let start = path.len() - rest.len();
            let upto = &path[..name_end];
            let is_leaf = name_end == path.len();
            let exists = if is_leaf {
                false // `path` itself is not tracked
            } else {
                let dir = format!("{upto}/");
                self.get(upto)?.is_some() || self.lower_bound(&dir)?.is_some_and(|k| k.starts_with(&dir))
            };
            if exists {
                parent_end = name_end;
                continue;
            }
            let parent = &path[..parent_end];
            let name = &path[start..name_end];
            let want = crate::portable::fold_component(name);
            let (lo, hi) = if parent.is_empty() {
                (String::new(), String::new())
            } else {
                (format!("{parent}/"), format!("{parent}0"))
            };
            let first = if parent.is_empty() { 0 } else { self.block_for(&lo) };
            let mut last_child = String::new();
            for i in first..self.blocks.len() {
                if !parent.is_empty() && self.blocks[i].first.as_str() >= hi.as_str() {
                    break;
                }
                for (k, _) in self.decode_block(i)? {
                    if !k.starts_with(&lo) || (!parent.is_empty() && k.as_str() >= hi.as_str()) {
                        continue;
                    }
                    let child = k[lo.len()..].split('/').next().unwrap_or("");
                    if child == last_child || child == name {
                        continue;
                    }
                    last_child.clear();
                    last_child.push_str(child);
                    if crate::portable::fold_component(child) == want {
                        let a = if parent.is_empty() {
                            child.to_owned()
                        } else {
                            format!("{parent}/{child}")
                        };
                        return Ok(Some((a, upto.to_owned())));
                    }
                }
            }
            return Ok(None);
        }
    }

    /// Would adding `path` (not tracked before) clash with a tracked file/directory once `edits` are applied?
    fn check_conflicts(&self, path: &str, edits: &Edits) -> Result<()> {
        let mut pos = 0;
        while let Some(i) = path[pos..].find('/') {
            let anc = &path[..pos + i];
            let tracked = match edits.get(anc) {
                Some(e) => e.is_some(),
                None => self.get(anc)?.is_some(),
            };
            if tracked {
                return Err(Error::Msg(format!("cannot track {path:?}: {anc:?} is tracked as a file")));
            }
            pos += i + 1;
        }
        let (lo, hi) = (format!("{path}/"), format!("{path}0"));
        if let Some((k, _)) = edits.range(lo.clone()..hi.clone()).find(|(_, e)| e.is_some()) {
            return Err(Error::Msg(format!("cannot track file {path:?}: {k:?} is tracked beneath it")));
        }
        let mut key = lo.clone();
        while let Some(k) = self.lower_bound(&key)? {
            if k.as_str() >= hi.as_str() {
                break;
            }
            if !matches!(edits.get(&k), Some(None)) {
                return Err(Error::Msg(format!("cannot track file {path:?}: {k:?} is tracked beneath it")));
            }
            key = format!("{k}\0");
        }
        Ok(())
    }
}

impl crate::repo::Repo {
    /// The index file, parsed lazily. An index in an older layout is rewritten once in the
    /// current one first.
    pub(crate) fn index_file(&self) -> Result<IndexFile> {
        let raw = self.rd("index")?;
        if let Some(f) = IndexFile::parse(raw.clone())? {
            return Ok(f);
        }
        let mut ix = crate::index::Index::decode(&raw)?;
        self.save_index(&mut ix)?;
        IndexFile::parse(self.rd("index")?)?.ok_or_else(|| corrupt("unreadable layout after rewrite"))
    }

    /// The index bytes with `edits` applied, ready for [`Self::write_index_bytes`], and how many
    /// entries changed (`None` bytes when nothing does). Nothing is written. Commands compute this
    /// first, record their operation, and write the index last: the operation log's fsyncs then
    /// do not have to wait for a freshly written index to reach the disk.
    pub(crate) fn prepare_index_patch(&self, f: &IndexFile, edits: &Edits) -> Result<(Option<Vec<u8>>, usize)> {
        let (bytes, changed) = f.patch(edits)?;
        Ok((if changed == 0 { None } else { Some(bytes) }, changed))
    }

    /// Replace the index file with `bytes` (atomic rename, no fsync of the temp file; the
    /// blobs it names are made durable first).
    pub(crate) fn write_index_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.store.sync()?;
        let clean = self.index_holds_no_attrs();
        crate::meta::write_atomic_nosync(&self.vcs.join("index"), bytes)?;
        self.carry_attrs_marker(clean, bytes);
        Ok(())
    }
}

/// Cheap whole-file sanity check of the index at `path`, reading only its header and tail: true
/// unless the file is an `AGI3`/`AGI4` index that is shorter than its own block table says or whose
/// cache-tree section does not end exactly at the end of the file (a torn write). Other layouts
/// are not checked here (they are decoded in full anyway).
pub(crate) fn index_looks_whole(path: &std::path::Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else { return true };
    let Ok(len) = f.metadata().map(|m| m.len() as usize) else {
        return true;
    };
    // The block table is small (about a byte per 100 entries): read a few KiB first and widen only when
    // the table does not fit.
    let mut want = len.min(16 << 10);
    let (head, v4, r_rem, parsed) = loop {
        let mut head = vec![0u8; want];
        if f.seek(SeekFrom::Start(0)).is_err() || f.read_exact(&mut head).is_err() {
            return false;
        }
        if !head.starts_with(b"AGI3") && !head.starts_with(b"AGI4") {
            return !head.is_empty() && (head.starts_with(b"AGI2") || head.starts_with(b"AGIX"));
        }
        let v4 = head.starts_with(b"AGI4");
        let mut r = Reader::new(&head[5.min(head.len())..]);
        let parsed = (|| -> std::result::Result<usize, vcs_objects::DecodeError> {
            r.varint()?;
            let n = r.varint()? as usize;
            let mut total = 0usize;
            if v4 {
                let nb = r.varint()? as usize;
                for _ in 0..nb {
                    total += r.varint()? as usize;
                    r.varint()?;
                }
            } else {
                r.varint()?;
                let nb = r.varint()? as usize;
                for _ in 0..nb {
                    total += r.varint()? as usize;
                }
            }
            let _ = n;
            Ok(total)
        })();
        if parsed.is_err() && want < len {
            want = (want * 16).min(len);
            continue;
        }
        let rem = r.remaining();
        break (head, v4, rem, parsed);
    };
    let _ = v4;
    let Ok(blocks_len) = parsed else { return false };
    let off = 5 + (head.len() - 5 - r_rem) + blocks_len;
    if off >= len {
        return false;
    }
    let mut tail = vec![0u8; len - off];
    if f.seek(SeekFrom::Start(off as u64)).is_err() || f.read_exact(&mut tail).is_err() {
        return false;
    }
    let mut t = Reader::new(&tail);
    let ok = (|| -> std::result::Result<(), vcs_objects::DecodeError> {
        let tn = t.varint()? as usize;
        if tn > 0 {
            t.id()?;
            for _ in 0..tn {
                t.str(4096)?;
                t.id()?;
            }
        }
        t.finish()
    })();
    ok.is_ok()
}

impl crate::repo::Repo {
    /// Called when a repository is opened: if the index file is torn (short, or its sections do not
    /// add up), rebuild it from the branch heads of the subscribed zones instead of failing or
    /// reading it as empty. Staged changes that were only in the lost index are gone; the files are
    /// untouched and show up as ordinary unstaged changes. Skipped when another process holds the lock.
    pub(crate) fn recover_torn_index(&self) {
        let path = self.vcs.join("index");
        if index_looks_whole(&path) {
            return;
        }
        let Ok(_l) = self.lock() else { return };
        if index_looks_whole(&path) {
            return;
        }
        let rebuilt = (|| -> Result<crate::index::Index> {
            let (zones, ws, refs) = (self.zones()?, self.workspace()?, self.refs()?);
            let mut ix = crate::index::Index::default();
            for zone in ws.subs.keys() {
                for (p, (mode, id)) in self.head_flat(zone, &zones, &ws, &refs)? {
                    let size = self.content_len(&id)?;
                    // mtime 0 never matches the file's, so status re-reads it
                    ix.insert(
                        &p,
                        IndexEntry {
                            id,
                            mode,
                            size,
                            mtime_ns: 0,
                            ctime_ns: 0,
                        },
                    )?;
                }
            }
            Ok(ix)
        })();
        match rebuilt {
            Ok(ix) => {
                if crate::meta::write_atomic(&path, &ix.encode()).is_ok() {
                    eprintln!("warning: the index file was damaged (a torn write); rebuilt it from the branch heads. Staged changes that were not committed are lost; your files are untouched.");
                }
            }
            Err(e) => eprintln!("warning: the index file is damaged and could not be rebuilt: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Index;
    use vcs_objects::EntryMode;

    fn en(n: u8, mtime: u64) -> IndexEntry {
        IndexEntry {
            id: ObjectId::from_bytes([n; 32]),
            mode: EntryMode::File,
            size: n as u64,
            mtime_ns: mtime,
            ctime_ns: mtime + 1,
        }
    }

    fn big() -> Index {
        let mut ix = Index::default();
        for k in 0..9000u32 {
            ix.insert(&format!("d{:02}/f{k:05}.txt", k % 40), en((k % 200) as u8, k as u64 + 10))
                .unwrap();
        }
        ix.insert("top.txt", en(7, 1)).unwrap();
        ix.written_ns = 12345;
        let fp = ObjectId::from_bytes([9; 32]);
        let mut t = BTreeMap::new();
        t.insert(String::new(), ObjectId::from_bytes([1; 32]));
        for d in 0..40 {
            t.insert(format!("d{d:02}"), ObjectId::from_bytes([2; 32]));
        }
        ix.entries.store_trees(fp, t);
        ix
    }

    /// The patched file must decode to exactly what applying the edits to the map gives, with
    /// the same cache-tree invalidation.
    fn apply_in_memory(ix: &Index, edits: &Edits) -> Index {
        let mut j = ix.clone();
        for (k, e) in edits {
            match e {
                Some(e) => {
                    j.entries.insert(k.clone(), e.clone());
                }
                None => {
                    j.entries.remove(k);
                }
            }
        }
        j
    }

    fn check(ix: &Index, edits: Edits) {
        let f = IndexFile::parse(ix.encode()).unwrap().unwrap();
        let (bytes, _) = f.patch(&edits).unwrap();
        let got = Index::decode(&bytes).unwrap();
        let want = apply_in_memory(ix, &edits);
        assert_eq!(got.entries, want.entries);
        assert_eq!(got.written_ns, ix.written_ns);
        let fp = ObjectId::from_bytes([9; 32]);
        assert_eq!(got.entries.cached_trees(&fp), want.entries.cached_trees(&fp));
        // a second patch of the patched (AGI4) file still works, and a full re-encode agrees
        let again = IndexFile::parse(bytes).unwrap().unwrap();
        assert_eq!(again.len(), want.entries.len());
        assert_eq!(Index::decode(&got.encode()).unwrap().entries, want.entries);
    }

    #[test]
    fn patch_equals_in_memory_edit() {
        let ix = big();
        let all: Vec<String> = ix.entries.keys().cloned().collect();
        let mut e = Edits::new();
        check(&ix, e.clone()); // empty
        e.insert(all[3].clone(), None);
        check(&ix, e.clone()); // one removal
        e.insert(all[5000].clone(), Some(en(99, 5)));
        e.insert(all[8999].clone(), None);
        e.insert("aaa.txt".into(), Some(en(1, 1))); // before every block
        e.insert("zzz/new.txt".into(), Some(en(2, 1))); // after every block
        e.insert("d05/brand-new.txt".into(), Some(en(3, 1)));
        e.insert("nonexistent-removal".into(), None);
        check(&ix, e);
        // stat-only refresh keeps the cache tree
        let mut e = Edits::new();
        let mut same = ix.entries[&all[10]].clone();
        same.mtime_ns += 1;
        e.insert(all[10].clone(), Some(same));
        check(&ix, e);
        // remove an entire block's worth and then some
        let mut e = Edits::new();
        for k in &all[100..4300] {
            e.insert(k.clone(), None);
        }
        check(&ix, e);
        // add enough to force a block split
        let mut e = Edits::new();
        for k in 0..5000 {
            e.insert(format!("d03/g{k:05}.txt"), Some(en(4, k)));
        }
        check(&ix, e);
        // everything removed
        let mut e = Edits::new();
        for k in &all {
            e.insert(k.clone(), None);
        }
        let f = IndexFile::parse(ix.encode()).unwrap().unwrap();
        let (bytes, _) = f.patch(&e).unwrap();
        assert!(Index::decode(&bytes).unwrap().entries.is_empty());
    }

    #[test]
    fn lookups_match_the_map() {
        let ix = big();
        let f = IndexFile::parse(ix.encode()).unwrap().unwrap();
        assert_eq!(f.len(), ix.entries.len());
        for (k, v) in ix.entries.iter().step_by(37) {
            assert_eq!(f.get(k).unwrap().as_ref(), Some(v));
        }
        assert!(f.get("d00/nope").unwrap().is_none());
        assert!(f.get("").unwrap().is_none());
        for pre in ["d07", "d07/f00007.txt", "top.txt", "d0", "zz", "", "d39"] {
            let want: Vec<String> = ix.under(pre);
            let got: Vec<String> = f.under(pre).unwrap().into_iter().map(|x| x.0).collect();
            assert_eq!(got, want, "{pre:?}");
        }
    }

    #[test]
    fn patch_refuses_file_dir_conflicts() {
        let ix = big();
        let f = IndexFile::parse(ix.encode()).unwrap().unwrap();
        let mut e = Edits::new();
        e.insert("top.txt/inner".into(), Some(en(1, 1)));
        assert!(f.patch(&e).is_err());
        let mut e = Edits::new();
        e.insert("d01".into(), Some(en(1, 1)));
        assert!(f.patch(&e).is_err());
        // fine once the clashing entries are removed in the same patch
        let mut e = Edits::new();
        for k in ix.under("d01") {
            e.insert(k, None);
        }
        e.insert("d01".into(), Some(en(1, 1)));
        let (b, _) = f.patch(&e).unwrap();
        let got = Index::decode(&b).unwrap();
        assert!(got.entries.contains_key("d01") && !got.entries.contains_key("d01/f00001.txt"));
    }

    #[test]
    fn legacy_layouts_are_not_parsed() {
        let ix = big();
        let enc = ix.encode();
        assert!(IndexFile::parse(enc).unwrap().is_some());
        assert!(IndexFile::parse(b"AGI2\x01\x00\x00\x00".to_vec()).unwrap().is_none());
    }

    #[test]
    fn new_path_collision_matches_the_whole_index_check() {
        let mut ix = Index::default();
        let mut names: Vec<String> = Vec::new();
        for d in ["Src", "docs", "Docs2", "a/B", "a/b2", "ünï", "z"] {
            for k in 0..700u32 {
                names.push(format!("{d}/File{k}.TXT"));
            }
        }
        names.push("Top.txt".into());
        names.push("lib".into());
        names.sort();
        names.dedup();
        for n in &names {
            ix.insert(n, en(1, 1)).unwrap();
        }
        let f = IndexFile::parse(ix.encode()).unwrap().unwrap();
        let probes = [
            "src/new.txt",
            "SRC/File1.TXT",
            "Src/file1.txt",
            "Src/brandnew.txt",
            "Src/sub/x.txt",
            "DOCS/x",
            "docs/x",
            "a/b/x",
            "a/B/File9.txt",
            "a/B/new",
            "A/b2/x",
            "top.txt",
            "Top2.txt",
            "LIB/x",
            "lib2",
            "UNI\u{308}/x",
            "ünï/new",
            "ÜNÏ/new",
            "newdir/deep/er/x",
            "Z/x",
        ];
        for p in probes {
            if ix.entries.contains_key(p) {
                continue;
            }
            let want = crate::portable::find_collision(names.iter().map(String::as_str).chain([p])).is_some();
            let got = f.find_new_path_collision(p).unwrap().is_some();
            assert_eq!(got, want, "{p}");
        }
    }
}
