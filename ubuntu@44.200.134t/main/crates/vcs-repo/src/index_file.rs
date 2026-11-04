//! Reading and patching the on-disk index without decoding all of it.
//!
//! [`IndexFile`] is the lazy face of [`crate::indeximage::IndexImage`]: the `AGI5` base is
//! memory mapped and searched in place, edits made since it was written come from `index.log`.
//! Commands that change a handful of entries (`add` of a file, `rm`, `mv`, `restore --staged`,
//! `reset <path>`, `add -u` on a mostly clean tree) cost a few lookups and one appended log frame,
//! not a decode and rewrite of every entry.
//!
//! A patch keeps `written_ns` as it was: entries it adds are then "racy" at worst (re-read once
//! by the next status), never wrongly trusted.

use crate::error::{Error, Result};
use crate::index::IndexEntry;
use crate::indeximage::{IndexImage, Op};
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::codec::Reader;
use vcs_objects::ObjectId;
use vcs_store::ObjectStore;

thread_local! {
    /// Set by `prepare_index_patch`, consumed by the `write_index_patch` that follows it.
    static PENDING: std::cell::RefCell<Option<Pending>> = const { std::cell::RefCell::new(None) };
}

/// What `prepare_index_patch` leaves for the `write_index_patch` after it: the paths the patch touches,
/// and (when a filesystem-monitor state exists) the fingerprint of the index it starts from.
struct Pending {
    touched: Vec<String>,
    old_fp: Option<[u8; 32]>,
    zones: Vec<u8>,
    workspace: Vec<u8>,
}

/// A parsed index: the `AGI5` base and its log; entries are read on demand.
pub(crate) struct IndexFile {
    pub(crate) img: IndexImage,
    pub written_ns: u64,
}

/// Edits to apply: `Some(entry)` sets (adds or replaces), `None` removes.
pub(crate) type Edits = BTreeMap<String, Option<IndexEntry>>;

/// The log ops that carry an [`Edits`] out, ready for [`crate::repo::Repo::write_index_patch`].
pub(crate) struct Patch {
    ops: Vec<Op>,
}

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("index: {e}"))
}

impl IndexFile {
    pub(crate) fn from_image(img: IndexImage) -> Option<IndexFile> {
        img.is_v5().then(|| IndexFile {
            written_ns: img.written_ns,
            img,
        })
    }

    /// `None` for the older layouts (`AGI2`, `AGIX`, `AGI3`, `AGI4`), which have no in-place form.
    #[cfg(test)]
    pub(crate) fn parse(bytes: Vec<u8>) -> Result<Option<IndexFile>> {
        Ok(Self::from_image(IndexImage::from_bytes(&bytes)?))
    }

    /// No entries at all (a patch has nothing to look at).
    pub(crate) fn is_empty(&self) -> bool {
        self.img.next_ge("").ok().flatten().is_none()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.img.list().unwrap().len()
    }

    /// The entry for exactly `path`.
    pub(crate) fn get(&self, path: &str) -> Result<Option<IndexEntry>> {
        self.img.get(path)
    }

    /// First tracked path that sorts at or after `key`.
    fn lower_bound(&self, key: &str) -> Result<Option<String>> {
        Ok(self.img.next_ge(key)?.map(|x| x.0))
    }

    /// Entries equal to `prefix` or beneath `prefix/` (all of them for an empty prefix), in order.
    pub(crate) fn under(&self, prefix: &str) -> Result<Vec<(String, IndexEntry)>> {
        self.img.under(prefix)
    }

    /// The cached tree id of directory `dir` ("" = root) from the cache-tree section, when that
    /// section was computed under the zone layout `fp`. Reads only the tail, never an entry.
    pub(crate) fn cached_tree(&self, fp: &ObjectId, dir: &str) -> Result<Option<ObjectId>> {
        self.img.cached_tree(fp, dir)
    }

    /// Check `edits` and turn them into log ops plus how many entries really change.
    pub(crate) fn patch(&self, edits: &Edits) -> Result<(Patch, usize)> {
        let mut changed = 0usize;
        let mut added: Vec<&str> = Vec::new();
        let mut invalid: BTreeSet<String> = BTreeSet::new(); // dirs whose cached trees go
        let mut ops: Vec<Op> = Vec::with_capacity(edits.len() + 1);
        for (k, new) in edits {
            let cur = self.img.get(k)?;
            match (cur, new) {
                (Some(_), None) => {
                    changed += 1;
                    self.invalidate(k, &mut invalid);
                    ops.push(Op::Remove(k.clone()));
                }
                (None, None) => {}
                (Some(o), Some(n)) => {
                    if o != *n {
                        changed += 1;
                        ops.push(Op::Set(k.clone(), n.clone()));
                    }
                    if o.id != n.id || o.mode != n.mode {
                        self.invalidate(k, &mut invalid);
                    }
                }
                (None, Some(n)) => {
                    changed += 1;
                    added.push(k);
                    self.invalidate(k, &mut invalid);
                    ops.push(Op::Set(k.clone(), n.clone()));
                }
            }
        }
        if changed == 0 {
            return Ok((Patch { ops: Vec::new() }, 0));
        }
        // file/directory conflicts for brand-new paths, judged on the state after the edits
        for p in added {
            self.check_conflicts(p, edits)?;
        }
        if !invalid.is_empty() && self.img.trees()?.is_some_and(|t| !t.1.is_empty()) {
            ops.push(Op::DropTrees(invalid.into_iter().collect()));
        }
        Ok((Patch { ops }, changed))
    }

    /// `path`, every directory above it and the root: their cached trees are stale after a change.
    fn invalidate(&self, path: &str, into: &mut BTreeSet<String>) {
        into.insert(path.to_owned());
        let mut end = path.len();
        while let Some(i) = path[..end].rfind('/') {
            end = i;
            into.insert(path[..end].to_owned());
        }
        into.insert(String::new());
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
            // Walk the children of `parent` in order. A directory child is stepped over as a whole
            // (seek to the key after its last descendant) so a root with 1,000 directories decodes
            // about 1,000 entries, not all 100,000.
            let mut cur = lo.clone();
            let mut last_child = String::new();
            loop {
                let Some((k, _)) = self.img.next_ge(&cur)? else { break };
                if !k.starts_with(&lo) || (!parent.is_empty() && k.as_str() >= hi.as_str()) {
                    break;
                }
                let rest = &k[lo.len()..];
                let (child, is_dir) = match rest.find('/') {
                    Some(i) => (&rest[..i], true),
                    None => (rest, false),
                };
                if child != last_child && child != name {
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
                cur = if is_dir {
                    format!("{lo}{child}0") // '0' == '/' + 1: first key after the directory
                } else {
                    format!("{k}\0")
                };
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
    /// The index as an image (mapped base plus log), whatever layout it is in.
    pub(crate) fn index_image(&self) -> Result<IndexImage> {
        IndexImage::load(&self.vcs)
    }

    /// The index file, parsed lazily; `None` while the index is in an older layout (the callers
    /// then take their whole-index path, which upgrades it on save).
    pub(crate) fn open_index_file(&self) -> Result<Option<IndexFile>> {
        Ok(IndexFile::from_image(self.index_image()?))
    }

    /// The index file, parsed lazily. An index in an older layout is rewritten once in the
    /// current one first.
    pub(crate) fn index_file(&self) -> Result<IndexFile> {
        if let Some(f) = self.open_index_file()? {
            return Ok(f);
        }
        let mut ix = self.index()?;
        self.save_index(&mut ix)?;
        self.open_index_file()?.ok_or_else(|| corrupt("unreadable layout after rewrite"))
    }

    /// The log ops for `edits`, ready for [`Self::write_index_patch`], and how many
    /// entries changed (`None` when nothing does). Nothing is written. Commands compute this
    /// first, record their operation, and write the index last: the operation log's fsyncs then
    /// do not have to wait for a freshly written index to reach the disk.
    pub(crate) fn prepare_index_patch(&self, f: &IndexFile, edits: &Edits) -> Result<(Option<Patch>, usize)> {
        let (patch, changed) = f.patch(edits)?;
        // Remember what this patch touches (narrows the attribute-file scan, tells the filesystem
        // monitor what moved) and, for the monitor, which index it starts from.
        let pending = (changed > 0).then(|| {
            let mon = if crate::fsmonitor::state_exists(&self.vcs) {
                match (self.rd("zones"), self.rd("workspace")) {
                    (Ok(z), Ok(w)) => Some((z, w)),
                    _ => None,
                }
            } else {
                None
            };
            let (zones, workspace) = mon.clone().unwrap_or_default();
            let old_fp = mon.map(|(z, w)| self.fingerprint_img(&f.img, &z, &w));
            Pending {
                touched: edits.keys().cloned().collect(),
                old_fp,
                zones,
                workspace,
            }
        });
        PENDING.with(|p| *p.borrow_mut() = pending);
        Ok((if changed == 0 { None } else { Some(patch) }, changed))
    }

    /// Persist a patch: one appended log frame (no rewrite of the index), or, when the log is
    /// long or the index is in an older layout, a new base generation (atomic rename). The
    /// blobs the index names are made durable first.
    pub(crate) fn write_index_patch(&self, f: &IndexFile, patch: &Patch) -> Result<()> {
        self.store.sync()?;
        let clean = self.index_holds_no_attrs();
        let pending = PENDING.with(|p| p.borrow_mut().take());
        let after = f.img.commit(&self.vcs, patch.ops.clone())?;
        match &pending {
            // The patch knows every path it changed: only those can have introduced an attribute file.
            Some(p) => self.carry_attrs_marker_touched(clean, &p.touched),
            None => self.carry_attrs_marker_touched(clean, &patch.ops.iter().filter_map(|o| match o { Op::Set(p, _) | Op::Remove(p) => Some(p.clone()), _ => None }).collect::<Vec<_>>()),
        }
        if let Some(p) = pending {
            if let Some(old) = p.old_fp {
                let new = crate::fsmonitor::fingerprint_with(&after.digest, after.len, &p.zones, &p.workspace);
                if new != old {
                    crate::fsmonitor::log_rebind(&self.vcs, &old, &new, &p.touched);
                }
            }
        }
        Ok(())
    }
}

/// Cheap whole-file sanity check of the index at `path`, reading only its header and tail: true
/// unless the file is an `AGI3`/`AGI4` index that is shorter than its own block table says or whose
/// cache-tree section does not end exactly at the end of the file (a torn write). Other layouts
/// are not checked here (they are decoded in full anyway).
pub(crate) fn index_looks_whole(path: &std::path::Path) -> bool {
    if let Some(v) = crate::index5::looks_whole(path) {
        return v;
    }
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
                    let _ = std::fs::remove_file(self.vcs.join("index.log"));
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

    fn open(ix: &Index) -> (tempfile::TempDir, IndexFile) {
        let d = tempfile::tempdir().unwrap();
        crate::indeximage::write_full(d.path(), ix).unwrap();
        let f = IndexFile::from_image(IndexImage::load(d.path()).unwrap()).unwrap();
        (d, f)
    }

    /// Persist the patch (log append or fold) and read it back through a fresh load.
    fn patched(d: &tempfile::TempDir, f: &IndexFile, edits: &Edits) -> Result<Index> {
        let (p, _) = f.patch(edits)?;
        f.img.commit(d.path(), p.ops)?;
        IndexImage::load(d.path())?.to_index()
    }

    fn check(ix: &Index, edits: Edits) {
        let (d, f) = open(ix);
        let got = patched(&d, &f, &edits).unwrap();
        let want = apply_in_memory(ix, &edits);
        assert_eq!(got.entries, want.entries);
        assert_eq!(got.written_ns, ix.written_ns);
        let fp = ObjectId::from_bytes([9; 32]);
        assert_eq!(got.entries.cached_trees(&fp), want.entries.cached_trees(&fp));
        // a second patch on top of the log still works, and a full re-encode agrees
        let again = IndexFile::from_image(IndexImage::load(d.path()).unwrap()).unwrap();
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
        let (d, f) = open(&ix);
        assert!(patched(&d, &f, &e).unwrap().entries.is_empty());
    }

    #[test]
    fn lookups_match_the_map() {
        let ix = big();
        let (_d, f) = open(&ix);
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
        let (d, f) = open(&ix);
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
        let got = patched(&d, &f, &e).unwrap();
        assert!(got.entries.contains_key("d01") && !got.entries.contains_key("d01/f00001.txt"));
    }

    #[test]
    fn legacy_layouts_are_not_parsed() {
        let ix = big();
        assert!(IndexFile::parse(ix.encode()).unwrap().is_some());
        assert!(IndexFile::parse(ix.encode_v3()).unwrap().is_none());
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
        let (_d, f) = open(&ix);
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
