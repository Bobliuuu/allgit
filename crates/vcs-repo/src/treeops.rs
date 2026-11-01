//! Tree <-> flat path map conversion.
//!
//! MVP: whole-tree flatten/build (O(n) in tracked files of a zone). Subtree-skipping
//! diff and lazy loading are Milestone 5 work; benchmark first.

use crate::error::{Error, Result};
use std::collections::BTreeMap;
use vcs_objects::names::MAX_PATH;
use vcs_objects::{EntryMode, Object, ObjectId, Tree, TreeEntry};
use vcs_store::ObjectStore;

pub type Flat = BTreeMap<String, (EntryMode, ObjectId)>;

/// Flatten `root` into files keyed by `prefix + relative path`. `None` = empty tree.
pub fn flatten(store: &dyn ObjectStore, root: Option<ObjectId>, prefix: &str) -> Result<Flat> {
    let mut out = Flat::new();
    if let Some(r) = root {
        walk(store, r, prefix, &mut out)?;
    }
    Ok(out)
}

fn walk(store: &dyn ObjectStore, tree: ObjectId, prefix: &str, out: &mut Flat) -> Result<()> {
    let Object::Tree(t) = store.get(&tree)? else {
        return Err(Error::Corrupt(format!("object {tree} is not a tree")));
    };
    for e in t.entries() {
        let p = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{prefix}/{}", e.name)
        };
        if p.len() > MAX_PATH {
            return Err(Error::Corrupt("tree path too long".into()));
        }
        match e.mode {
            EntryMode::Directory => walk(store, e.id, &p, out)?,
            m => {
                out.insert(p, (m, e.id));
            }
        }
    }
    Ok(())
}

/// The parts of two trees that differ: files of `old` and `new` (full paths) that are not
/// identical on both sides, plus the entry names of every `new` directory that was read, for
/// the case-folding collision check.
#[derive(Default)]
pub struct TreeDiff {
    pub old: Flat,
    pub new: Flat,
    pub new_dir_names: Vec<Vec<String>>,
    /// Do not collect `new_dir_names` (callers that only want the changed files).
    lean: bool,
}

/// Diff two trees without reading subtrees whose ids are equal, so the cost follows the number of
/// changed directories rather than tracked files. The result's `old`/`new` maps are exactly
/// `flatten(old)`/`flatten(new)` with every path whose entry is equal on both sides removed.
pub fn diff_trees(store: &dyn ObjectStore, old: Option<ObjectId>, new: Option<ObjectId>, prefix: &str) -> Result<TreeDiff> {
    diff_trees_with(store, old, new, prefix, false)
}

/// [`diff_trees`] for callers that only look at `old` and `new`: `new_dir_names` stays empty, which
/// saves cloning every entry name of every changed directory.
pub fn diff_trees_files(store: &dyn ObjectStore, old: Option<ObjectId>, new: Option<ObjectId>, prefix: &str) -> Result<TreeDiff> {
    diff_trees_with(store, old, new, prefix, true)
}

fn diff_trees_with(store: &dyn ObjectStore, old: Option<ObjectId>, new: Option<ObjectId>, prefix: &str, lean: bool) -> Result<TreeDiff> {
    let mut d = TreeDiff {
        lean,
        ..Default::default()
    };
    if old != new {
        diff_pair(store, old, new, prefix, &mut d)?;
    }
    Ok(d)
}

/// Add to `d` the difference between the entry `old` and the entry `new` at `path` (either may be
/// absent; a directory entry stands for its whole subtree). Equal subtrees are not read.
pub fn diff_entries(
    store: &dyn ObjectStore,
    old: Option<(EntryMode, ObjectId)>,
    new: Option<(EntryMode, ObjectId)>,
    path: &str,
    d: &mut TreeDiff,
) -> Result<()> {
    if old == new {
        return Ok(());
    }
    if let (Some((EntryMode::Directory, a)), Some((EntryMode::Directory, b))) = (old, new) {
        return diff_pair(store, Some(a), Some(b), path, d);
    }
    if let Some((mode, id)) = old {
        add_side(
            store,
            &TreeEntry {
                name: String::new(),
                mode,
                id,
            },
            path.to_owned(),
            &mut d.old,
            None,
        )?;
    }
    if let Some((mode, id)) = new {
        add_side(
            store,
            &TreeEntry {
                name: String::new(),
                mode,
                id,
            },
            path.to_owned(),
            &mut d.new,
            Some(&mut d.new_dir_names),
        )?;
    }
    Ok(())
}

fn read_tree(store: &dyn ObjectStore, id: Option<ObjectId>) -> Result<Vec<TreeEntry>> {
    match id {
        None => Ok(Vec::new()),
        Some(id) => match store.get(&id)? {
            Object::Tree(t) => Ok(t.entries().to_vec()),
            _ => Err(Error::Corrupt(format!("object {id} is not a tree"))),
        },
    }
}

fn add_side(store: &dyn ObjectStore, e: &TreeEntry, p: String, flat: &mut Flat, names: Option<&mut Vec<Vec<String>>>) -> Result<()> {
    if p.len() > MAX_PATH {
        return Err(Error::Corrupt("tree path too long".into()));
    }
    if e.mode == EntryMode::Directory {
        match names {
            Some(names) => walk_names(store, e.id, &p, flat, names),
            None => walk(store, e.id, &p, flat),
        }
    } else {
        flat.insert(p, (e.mode, e.id));
        Ok(())
    }
}

fn walk_names(store: &dyn ObjectStore, tree: ObjectId, prefix: &str, out: &mut Flat, names: &mut Vec<Vec<String>>) -> Result<()> {
    let ents = read_tree(store, Some(tree))?;
    names.push(ents.iter().map(|e| e.name.clone()).collect());
    for e in &ents {
        add_side(store, e, join(prefix, &e.name), out, Some(names))?;
    }
    Ok(())
}

fn diff_pair(store: &dyn ObjectStore, old: Option<ObjectId>, new: Option<ObjectId>, prefix: &str, d: &mut TreeDiff) -> Result<()> {
    let lean = d.lean;
    let (oe, ne) = (read_tree(store, old)?, read_tree(store, new)?);
    if new.is_some() && !d.lean {
        d.new_dir_names.push(ne.iter().map(|e| e.name.clone()).collect());
    }
    let (mut i, mut j) = (0, 0);
    while i < oe.len() || j < ne.len() {
        let ord = match (oe.get(i), ne.get(j)) {
            (Some(a), Some(b)) => a.name.cmp(&b.name),
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match ord {
            std::cmp::Ordering::Less => {
                let a = &oe[i];
                add_side(store, a, join(prefix, &a.name), &mut d.old, None)?;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                let b = &ne[j];
                add_side(store, b, join(prefix, &b.name), &mut d.new, if lean { None } else { Some(&mut d.new_dir_names) })?;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let (a, b) = (&oe[i], &ne[j]);
                i += 1;
                j += 1;
                if a.mode == b.mode && a.id == b.id {
                    continue;
                }
                let p = join(prefix, &a.name);
                match (a.mode == EntryMode::Directory, b.mode == EntryMode::Directory) {
                    (true, true) => diff_pair(store, Some(a.id), Some(b.id), &p, d)?,
                    _ => {
                        add_side(store, a, p.clone(), &mut d.old, None)?;
                        add_side(store, b, p, &mut d.new, if lean { None } else { Some(&mut d.new_dir_names) })?;
                    }
                }
            }
        }
    }
    Ok(())
}

enum Node {
    File(EntryMode, ObjectId),
    Dir(BTreeMap<String, Node>),
}

/// Build and store a tree from zone-relative `(path, mode, blob)` entries.
pub fn build<'a>(store: &dyn ObjectStore, entries: impl IntoIterator<Item = (&'a str, EntryMode, ObjectId)>) -> Result<ObjectId> {
    let mut root = BTreeMap::new();
    for (path, mode, id) in entries {
        let parts: Vec<&str> = path.split('/').collect();
        let (name, dirs) = parts.split_last().expect("split yields >=1");
        let mut cur = &mut root;
        for d in dirs {
            let n = cur.entry((*d).to_owned()).or_insert_with(|| Node::Dir(BTreeMap::new()));
            match n {
                Node::Dir(m) => cur = m,
                Node::File(..) => return Err(Error::Msg(format!("path conflict at {path:?}"))),
            }
        }
        if cur.insert((*name).to_owned(), Node::File(mode, id)).is_some() {
            return Err(Error::Msg(format!("path conflict at {path:?}")));
        }
    }
    write_dir(store, root)
}

fn write_dir(store: &dyn ObjectStore, m: BTreeMap<String, Node>) -> Result<ObjectId> {
    let mut entries = Vec::with_capacity(m.len());
    for (name, n) in m {
        entries.push(match n {
            Node::File(mode, id) => TreeEntry { name, mode, id },
            Node::Dir(sub) => TreeEntry {
                name,
                mode: EntryMode::Directory,
                id: write_dir(store, sub)?,
            },
        });
    }
    Ok(store.put(&Object::Tree(Tree::new(entries)?))?)
}

/// Hashed in-memory image of the tree an index would commit (nothing is stored).
enum INode {
    File(EntryMode, ObjectId),
    Dir(ObjectId, BTreeMap<String, INode>),
}

fn hash_dir(m: BTreeMap<String, Node>) -> Result<(ObjectId, BTreeMap<String, INode>)> {
    let mut kids = BTreeMap::new();
    let mut entries = Vec::with_capacity(m.len());
    for (name, n) in m {
        match n {
            Node::File(mode, id) => {
                entries.push(TreeEntry {
                    name: name.clone(),
                    mode,
                    id,
                });
                kids.insert(name, INode::File(mode, id));
            }
            Node::Dir(sub) => {
                let (id, k) = hash_dir(sub)?;
                entries.push(TreeEntry {
                    name: name.clone(),
                    mode: EntryMode::Directory,
                    id,
                });
                kids.insert(name, INode::Dir(id, k));
            }
        }
    }
    let id = vcs_objects::id_of_encoded(&Object::Tree(Tree::new(entries)?).encode());
    Ok((id, kids))
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn all_inode(prefix: &str, kids: &BTreeMap<String, INode>, out: &mut Vec<String>) {
    for (n, k) in kids {
        let p = join(prefix, n);
        match k {
            INode::File(..) => out.push(p),
            INode::Dir(_, sub) => all_inode(&p, sub, out),
        }
    }
}

/// Staged changes between HEAD's tree `head` and the tree `entries` (zone-relative
/// `(path, mode, blob)`) would build, as `(prefix + path, kind)` sorted by path. Subtrees whose
/// ids are equal on both sides are skipped without reading them, so the cost follows the number
/// of *changed* directories, not the number of tracked files.
pub fn diff_index<'a>(
    store: &dyn ObjectStore,
    head: Option<ObjectId>,
    entries: impl IntoIterator<Item = (&'a str, EntryMode, ObjectId)>,
    prefix: &str,
) -> Result<Vec<(String, crate::ChangeKind)>> {
    let mut root = BTreeMap::new();
    for (path, mode, id) in entries {
        let parts: Vec<&str> = path.split('/').collect();
        let (name, dirs) = parts.split_last().expect("split yields >=1");
        let mut cur = &mut root;
        for d in dirs {
            let n = cur.entry((*d).to_owned()).or_insert_with(|| Node::Dir(BTreeMap::new()));
            match n {
                Node::Dir(m) => cur = m,
                Node::File(..) => return Err(Error::Msg(format!("path conflict at {path:?}"))),
            }
        }
        if cur.insert((*name).to_owned(), Node::File(mode, id)).is_some() {
            return Err(Error::Msg(format!("path conflict at {path:?}")));
        }
    }
    let (root_id, kids) = hash_dir(root)?;
    let mut out = Vec::new();
    if head != Some(root_id) {
        diff_dir(store, head, &kids, prefix, &mut out)?;
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn diff_dir(
    store: &dyn ObjectStore,
    head: Option<ObjectId>,
    kids: &BTreeMap<String, INode>,
    prefix: &str,
    out: &mut Vec<(String, crate::ChangeKind)>,
) -> Result<()> {
    use crate::ChangeKind::{Added, Deleted, Modified};
    let mut hm: BTreeMap<String, TreeEntry> = BTreeMap::new();
    if let Some(h) = head {
        let Object::Tree(t) = store.get(&h)? else {
            return Err(Error::Corrupt(format!("object {h} is not a tree")));
        };
        for e in t.entries() {
            hm.insert(e.name.clone(), e.clone());
        }
    }
    let deleted_under = |e: &TreeEntry, p: &str, out: &mut Vec<(String, crate::ChangeKind)>| -> Result<()> {
        if e.mode == EntryMode::Directory {
            let mut f = Flat::new();
            walk(store, e.id, p, &mut f)?;
            out.extend(f.into_keys().map(|k| (k, Deleted)));
        } else {
            out.push((p.to_owned(), Deleted));
        }
        Ok(())
    };
    let added_under = |n: &INode, p: &str, out: &mut Vec<(String, crate::ChangeKind)>| match n {
        INode::File(..) => out.push((p.to_owned(), Added)),
        INode::Dir(_, sub) => {
            let mut v = Vec::new();
            all_inode(p, sub, &mut v);
            out.extend(v.into_iter().map(|k| (k, Added)));
        }
    };
    for (name, n) in kids {
        let p = join(prefix, name);
        match (hm.remove(name), n) {
            (None, n) => added_under(n, &p, out),
            (Some(h), INode::Dir(id, sub)) if h.mode == EntryMode::Directory => {
                if h.id != *id {
                    diff_dir(store, Some(h.id), sub, &p, out)?;
                }
            }
            (Some(h), n @ INode::Dir(..)) => {
                deleted_under(&h, &p, out)?;
                added_under(n, &p, out);
            }
            (Some(h), INode::File(m, id)) => {
                if h.mode == EntryMode::Directory {
                    deleted_under(&h, &p, out)?;
                    out.push((p, Added));
                } else if h.mode != *m || h.id != *id {
                    out.push((p, Modified));
                }
            }
        }
    }
    for (name, h) in hm {
        deleted_under(&h, &join(prefix, &name), out)?;
    }
    Ok(())
}

/// Smallest subtree (in files) whose tree id is kept in the index.
const CACHE_MIN_FILES: u32 = 8;

/// Tree construction straight from the index, reusing cached directory ids (cache-tree).
///
/// A directory's tree id is looked up in `cached` (ids stored in the index for directories that
/// have not changed) or in `fresh` (computed during this walk), and is only derived from the
/// entries beneath it on a miss. Everything the walk computes ends up in `fresh`, which the
/// caller may merge back into the index.
pub struct IndexTrees<'a> {
    pub store: &'a dyn ObjectStore,
    pub map: &'a BTreeMap<String, crate::index::IndexEntry>,
    pub zones: &'a crate::meta::Zones,
    pub zone: &'a str,
    cached: Option<&'a BTreeMap<String, ObjectId>>,
    fresh: BTreeMap<String, (ObjectId, u32)>,
    /// Trees written with `put` by this walk.
    pub puts: usize,
    /// Directories skipped because their id is known and equals the base tree's.
    pub reused: usize,
}

enum Child {
    File(EntryMode, ObjectId),
    Dir(String),
}

impl<'a> IndexTrees<'a> {
    pub fn new(
        store: &'a dyn ObjectStore,
        map: &'a BTreeMap<String, crate::index::IndexEntry>,
        zones: &'a crate::meta::Zones,
        zone: &'a str,
        cached: Option<&'a BTreeMap<String, ObjectId>>,
    ) -> Self {
        IndexTrees {
            store,
            map,
            zones,
            zone,
            cached,
            fresh: BTreeMap::new(),
            puts: 0,
            reused: 0,
        }
    }

    /// Immediate children of directory `d` (full path, "" = root) in this zone's view, by name.
    fn children(&self, d: &str) -> Vec<(String, Child)> {
        use std::ops::Bound::{Included, Unbounded};
        let lo = if d.is_empty() { String::new() } else { format!("{d}/") };
        let mut out = Vec::new();
        let mut it = self.map.range::<str, _>((Included(lo.as_str()), Unbounded));
        while let Some((k, e)) = it.next() {
            if !k.starts_with(&lo) {
                break;
            }
            let rest = &k[lo.len()..];
            match rest.find('/') {
                None => out.push((rest.to_owned(), Child::File(e.mode, e.id))),
                Some(p) => {
                    let name = &rest[..p];
                    let full = format!("{lo}{name}");
                    // A directory that is itself another zone's root is not part of this tree.
                    if self.zones.owner(&format!("{full}/_")) == self.zone {
                        out.push((name.to_owned(), Child::Dir(full)));
                    }
                    let skip = format!("{lo}{name}0"); // '0' == '/' + 1: first key after "full/..."
                    it = self.map.range::<str, _>((Included(skip.as_str()), Unbounded));
                }
            }
        }
        out
    }

    /// Id (and a lower bound on the number of files beneath) of the tree for directory `d`;
    /// `None` when it has no entries. Nothing is stored.
    fn id_of(&mut self, d: &str) -> Result<Option<(ObjectId, u32)>> {
        if let Some(x) = self.fresh.get(d) {
            return Ok(Some(*x));
        }
        if let Some(id) = self.cached.and_then(|c| c.get(d)) {
            return Ok(Some((*id, CACHE_MIN_FILES)));
        }
        let mut entries = Vec::new();
        let mut count = 0u32;
        for (name, ch) in self.children(d) {
            match ch {
                Child::File(mode, id) => {
                    count = count.saturating_add(1);
                    entries.push(TreeEntry { name, mode, id });
                }
                Child::Dir(full) => {
                    if let Some((id, n)) = self.id_of(&full)? {
                        count = count.saturating_add(n);
                        entries.push(TreeEntry {
                            name,
                            mode: EntryMode::Directory,
                            id,
                        });
                    }
                }
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let id = vcs_objects::id_of_encoded(&Object::Tree(Tree::new(entries)?).encode());
        self.fresh.insert(d.to_owned(), (id, count));
        Ok(Some((id, count)))
    }

    /// Store directory `d`, whose tree id is `id`, and every changed subtree. Where an id equals
    /// the one in `base` (the same directory in a tree known to be stored) nothing is read or
    /// written.
    fn write(&mut self, d: &str, id: ObjectId, base: Option<ObjectId>) -> Result<()> {
        if base == Some(id) {
            self.reused += 1;
            return Ok(());
        }
        if self.cached.is_some_and(|c| c.get(d) == Some(&id)) && self.store.contains(&id)? {
            return Ok(()); // the index remembers this id from an earlier commit and it is stored
        }
        let base_kids: BTreeMap<String, TreeEntry> = match base {
            Some(b) => match self.store.get(&b)? {
                Object::Tree(t) => t.entries().iter().map(|e| (e.name.clone(), e.clone())).collect(),
                _ => return Err(Error::Corrupt(format!("object {b} is not a tree"))),
            },
            None => BTreeMap::new(),
        };
        let mut entries = Vec::new();
        for (name, ch) in self.children(d) {
            match ch {
                Child::File(mode, fid) => entries.push(TreeEntry { name, mode, id: fid }),
                Child::Dir(full) => {
                    if let Some((cid, _)) = self.id_of(&full)? {
                        let cb = base_kids.get(&name).filter(|e| e.mode == EntryMode::Directory).map(|e| e.id);
                        self.write(&full, cid, cb)?;
                        entries.push(TreeEntry {
                            name,
                            mode: EntryMode::Directory,
                            id: cid,
                        });
                    }
                }
            }
        }
        let obj = Object::Tree(Tree::new(entries)?);
        debug_assert_eq!(vcs_objects::id_of_encoded(&obj.encode()), id);
        self.store.put(&obj)?;
        self.puts += 1;
        Ok(())
    }

    /// Does the zone rooted at `root_path` have any entries?
    pub fn is_empty(&mut self, root_path: &str) -> Result<bool> {
        Ok(self.id_of(root_path)?.is_none())
    }

    /// Root tree of the zone rooted at `root_path`, stored; an empty zone gives the empty tree.
    pub fn build_root(&mut self, root_path: &str, base: Option<ObjectId>) -> Result<ObjectId> {
        match self.id_of(root_path)? {
            Some((id, _)) => {
                self.write(root_path, id, base)?;
                Ok(id)
            }
            None => Ok(self.store.put(&Object::Tree(Tree::new(vec![])?))?),
        }
    }

    /// Tree ids worth remembering in the index: directories with enough files beneath that
    /// skipping them pays for the space the index spends on them.
    pub fn cacheable(&self) -> BTreeMap<String, ObjectId> {
        self.fresh
            .iter()
            .filter(|(_, (_, n))| *n >= CACHE_MIN_FILES)
            .map(|(d, (id, _))| (d.clone(), *id))
            .collect()
    }

    /// Staged changes of this zone against HEAD's root tree, like [`diff_index`], descending only
    /// into directories whose ids differ.
    pub fn diff(&mut self, root_path: &str, head: Option<ObjectId>) -> Result<Vec<(String, crate::ChangeKind)>> {
        let mut out = Vec::new();
        let root = self.id_of(root_path)?.map(|x| x.0);
        if root != head {
            self.diff_dir(root_path, head, &mut out)?;
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn added_under(&self, d: &str, out: &mut Vec<(String, crate::ChangeKind)>) {
        for (name, ch) in self.children(d) {
            let p = join(d, &name);
            match ch {
                Child::File(..) => out.push((p, crate::ChangeKind::Added)),
                Child::Dir(full) => self.added_under(&full, out),
            }
        }
    }

    fn diff_dir(&mut self, d: &str, head: Option<ObjectId>, out: &mut Vec<(String, crate::ChangeKind)>) -> Result<()> {
        use crate::ChangeKind::{Added, Deleted, Modified};
        let mut hm: BTreeMap<String, TreeEntry> = BTreeMap::new();
        if let Some(h) = head {
            let Object::Tree(t) = self.store.get(&h)? else {
                return Err(Error::Corrupt(format!("object {h} is not a tree")));
            };
            for e in t.entries() {
                hm.insert(e.name.clone(), e.clone());
            }
        }
        let deleted_under = |store: &dyn ObjectStore, e: &TreeEntry, p: &str, out: &mut Vec<(String, crate::ChangeKind)>| -> Result<()> {
            if e.mode == EntryMode::Directory {
                let mut f = Flat::new();
                walk(store, e.id, p, &mut f)?;
                out.extend(f.into_keys().map(|k| (k, Deleted)));
            } else {
                out.push((p.to_owned(), Deleted));
            }
            Ok(())
        };
        for (name, ch) in self.children(d) {
            let p = join(d, &name);
            match ch {
                Child::File(m, id) => match hm.remove(&name) {
                    None => out.push((p, Added)),
                    Some(h) if h.mode == EntryMode::Directory => {
                        deleted_under(self.store, &h, &p, out)?;
                        out.push((p, Added));
                    }
                    Some(h) => {
                        if h.mode != m || h.id != id {
                            out.push((p, Modified));
                        }
                    }
                },
                Child::Dir(full) => {
                    let Some((id, _)) = self.id_of(&full)? else {
                        continue; // empty in this view: HEAD's entry (if any) is reported deleted below
                    };
                    match hm.remove(&name) {
                        None => self.added_under(&full, out),
                        Some(h) if h.mode == EntryMode::Directory => {
                            if h.id != id {
                                self.diff_dir(&full, Some(h.id), out)?;
                            }
                        }
                        Some(h) => {
                            deleted_under(self.store, &h, &p, out)?;
                            self.added_under(&full, out);
                        }
                    }
                }
            }
        }
        for (name, h) in hm {
            deleted_under(self.store, &h, &join(d, &name), out)?;
        }
        Ok(())
    }
}

/// The entry (mode and id) at zone-relative `rel` below `root`, or `None` when nothing is
/// there. `rel == ""` is the root itself (a directory). A file in the middle of the path means
/// "absent", not an error. Reads one tree per path component.
pub fn entry_at(store: &dyn ObjectStore, root: ObjectId, rel: &str) -> Result<Option<(EntryMode, ObjectId)>> {
    let mut cur = (EntryMode::Directory, root);
    if rel.is_empty() {
        return Ok(Some(cur));
    }
    for part in rel.split('/') {
        if cur.0 != EntryMode::Directory {
            return Ok(None);
        }
        let Object::Tree(t) = store.get(&cur.1)? else {
            return Err(Error::Corrupt(format!("object {} is not a tree", cur.1)));
        };
        match t.get(part) {
            Some(e) => cur = (e.mode, e.id),
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// Does `entry` hold at least one file? (A file does; a directory does when its tree is not empty.)
pub fn entry_has_files(store: &dyn ObjectStore, entry: (EntryMode, ObjectId)) -> Result<bool> {
    if entry.0 != EntryMode::Directory {
        return Ok(true);
    }
    match store.get(&entry.1)? {
        Object::Tree(t) => Ok(!t.entries().is_empty()),
        _ => Err(Error::Corrupt(format!("object {} is not a tree", entry.1))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcs_store::MemoryStore;

    #[test]
    fn build_flatten_roundtrip_and_determinism() {
        let s = MemoryStore::new();
        let b1 = s.put_blob(b"1").unwrap();
        let b2 = s.put_blob(b"2").unwrap();
        let a = build(
            &s,
            [
                ("a/b/c", EntryMode::File, b1),
                ("z", EntryMode::Executable, b2),
                ("a/d", EntryMode::File, b2),
            ],
        )
        .unwrap();
        let b = build(
            &s,
            [
                ("a/d", EntryMode::File, b2),
                ("z", EntryMode::Executable, b2),
                ("a/b/c", EntryMode::File, b1),
            ],
        )
        .unwrap();
        assert_eq!(a, b);
        let f = flatten(&s, Some(a), "pre").unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f["pre/a/b/c"], (EntryMode::File, b1));
        assert_eq!(f["pre/z"].0, EntryMode::Executable);
    }

    fn oracle(s: &MemoryStore, head: Option<ObjectId>, ents: &[(String, EntryMode, ObjectId)]) -> Vec<(String, crate::ChangeKind)> {
        use crate::ChangeKind::*;
        let h = flatten(s, head, "").unwrap();
        let m: BTreeMap<&str, (EntryMode, ObjectId)> = ents.iter().map(|(p, m, i)| (p.as_str(), (*m, *i))).collect();
        let mut v = Vec::new();
        for (p, (mo, id)) in &m {
            match h.get(*p) {
                None => v.push((p.to_string(), Added)),
                Some(x) if x != &(*mo, *id) => v.push((p.to_string(), Modified)),
                _ => {}
            }
        }
        for p in h.keys() {
            if !m.contains_key(p.as_str()) {
                v.push((p.clone(), Deleted));
            }
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    #[test]
    fn diff_index_matches_flatten_oracle_on_random_trees() {
        // Tiny deterministic LCG; names drawn from a small alphabet so paths collide and
        // file/dir swaps, deletions and additions all occur.
        let mut x: u64 = 0x2545F4914F6CDD1D;
        let mut rnd = move |n: u64| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) % n
        };
        for round in 0..300 {
            let s = MemoryStore::new();
            let blobs: Vec<ObjectId> = (0..4).map(|i| s.put_blob(&[i as u8]).unwrap()).collect();
            let gen = |rnd: &mut dyn FnMut(u64) -> u64| {
                let mut set: BTreeMap<String, (EntryMode, ObjectId)> = BTreeMap::new();
                for _ in 0..rnd(12) {
                    let depth = 1 + rnd(3);
                    let p: Vec<String> = (0..depth).map(|_| format!("n{}", rnd(3))).collect();
                    let p = p.join("/");
                    // keep file/dir consistent inside one side
                    let clash = set
                        .keys()
                        .any(|k| k.starts_with(&format!("{p}/")) || p.starts_with(&format!("{k}/")));
                    if clash {
                        continue;
                    }
                    let mode = if rnd(4) == 0 { EntryMode::Executable } else { EntryMode::File };
                    set.insert(p, (mode, blobs[rnd(4) as usize]));
                }
                set.into_iter().map(|(p, (m, i))| (p, m, i)).collect::<Vec<_>>()
            };
            let a = gen(&mut rnd);
            let b = gen(&mut rnd);
            let head = if a.is_empty() && round % 2 == 0 {
                None
            } else {
                Some(build(&s, a.iter().map(|(p, m, i)| (p.as_str(), *m, *i))).unwrap())
            };
            let got = diff_index(&s, head, b.iter().map(|(p, m, i)| (p.as_str(), *m, *i)), "").unwrap();
            assert_eq!(got, oracle(&s, head, &b), "round {round}");
        }
    }

    #[test]
    fn diff_index_skips_equal_subtrees() {
        let s = MemoryStore::new();
        let b1 = s.put_blob(b"1").unwrap();
        let b2 = s.put_blob(b"2").unwrap();
        let head = build(&s, [("a/x", EntryMode::File, b1), ("b/y", EntryMode::File, b1)]).unwrap();
        // Remove the stored subtree "a": if diff_index read it, it would fail with NotFound.
        let Object::Tree(t) = s.get(&head).unwrap() else { panic!() };
        let a_id = t.entries().iter().find(|e| e.name == "a").unwrap().id;
        s.remove_for_test(&a_id);
        let d = diff_index(&s, Some(head), [("a/x", EntryMode::File, b1), ("b/y", EntryMode::File, b2)], "pre").unwrap();
        assert_eq!(d, vec![("pre/b/y".to_string(), crate::ChangeKind::Modified)]);
    }

    #[test]
    fn build_rejects_file_dir_conflict() {
        let s = MemoryStore::new();
        let b = s.put_blob(b"1").unwrap();
        assert!(build(&s, [("a", EntryMode::File, b), ("a/b", EntryMode::File, b)]).is_err());
    }
}
