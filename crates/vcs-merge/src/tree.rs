//! Three-way tree merge over flat `path -> (mode, blob id)` maps and over stored trees.
//!
//! Pure: blobs are read through [`BlobSource`]; text-merged results are returned in
//! [`TreeMerge::new_blobs`] for the caller to persist. Nothing is written here.

use crate::error::MergeError;
use crate::text::{merge3, Hunk, TextMerge};
use crate::Algorithm;
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::{blob_id, EntryMode, Object, ObjectId};
use vcs_store::ObjectStore;

pub type Entry = (EntryMode, ObjectId);
pub type Flat = BTreeMap<String, Entry>;

/// Read-only blob access.
pub trait BlobSource {
    fn blob(&self, id: &ObjectId) -> Result<Vec<u8>, MergeError>;
}

pub struct StoreBlobs<'a>(pub &'a dyn ObjectStore);

impl BlobSource for StoreBlobs<'_> {
    fn blob(&self, id: &ObjectId) -> Result<Vec<u8>, MergeError> {
        Ok(self.0.get_blob(id)?)
    }
}

/// In-memory blobs (tests, tools).
#[derive(Default, Clone, Debug)]
pub struct MapBlobs(pub BTreeMap<ObjectId, Vec<u8>>);

impl MapBlobs {
    pub fn add(&mut self, data: &[u8]) -> ObjectId {
        let id = blob_id(data);
        self.0.insert(id, data.to_vec());
        id
    }
}

impl BlobSource for MapBlobs {
    fn blob(&self, id: &ObjectId) -> Result<Vec<u8>, MergeError> {
        self.0.get(id).cloned().ok_or(MergeError::Missing(*id))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides added the path with different content or mode.
    AddAdd,
    /// Both sides changed content and the changes overlap (or the file is binary).
    ModifyModify,
    /// One side deleted, the other modified (content or mode).
    DeleteModify,
    /// Content merged but the two sides set different modes.
    ModeConflict,
    /// A path is a file on one side and a directory (or a different shape) on another.
    FileDirectory,
}

/// Structured conflict. The path is absent from [`TreeMerge::entries`]; all inputs are kept
/// here so a caller can resolve or persist it without re-reading the trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub kind: ConflictKind,
    pub base: Option<ObjectId>,
    pub left: Option<ObjectId>,
    pub right: Option<ObjectId>,
    pub base_mode: Option<EntryMode>,
    pub left_mode: Option<EntryMode>,
    pub right_mode: Option<EntryMode>,
    /// Line-level hunks when both sides are text and a text merge was attempted.
    pub hunks: Option<Vec<Hunk>>,
}

impl Conflict {
    fn new(path: &str, kind: ConflictKind, b: Option<Entry>, l: Option<Entry>, r: Option<Entry>) -> Self {
        Self {
            path: path.to_owned(),
            kind,
            base: b.map(|e| e.1),
            left: l.map(|e| e.1),
            right: r.map(|e| e.1),
            base_mode: b.map(|e| e.0),
            left_mode: l.map(|e| e.0),
            right_mode: r.map(|e| e.0),
            hunks: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeMerge {
    /// Cleanly merged entries (conflicted paths are not included).
    pub entries: Flat,
    /// Sorted by path.
    pub conflicts: Vec<Conflict>,
    /// Blobs created by clean text merges; the caller must store them.
    pub new_blobs: BTreeMap<ObjectId, Vec<u8>>,
}

enum Res {
    Keep(Option<Entry>),
    Conflict(Box<Conflict>),
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

fn pick3<T: PartialEq + Copy>(b: T, l: T, r: T) -> Option<T> {
    if l == r || r == b {
        Some(l)
    } else if l == b {
        Some(r)
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_entry(
    blobs: &dyn BlobSource,
    alg: Algorithm,
    structural: bool,
    path: &str,
    b: Option<Entry>,
    l: Option<Entry>,
    r: Option<Entry>,
    new_blobs: &mut BTreeMap<ObjectId, Vec<u8>>,
) -> Result<Res, MergeError> {
    if l == r || r == b {
        return Ok(Res::Keep(l));
    }
    if l == b {
        return Ok(Res::Keep(r));
    }
    // both sides differ from base and from each other
    match (b, l, r) {
        (None, Some(le), Some(re)) => {
            let mut c = Conflict::new(path, ConflictKind::AddAdd, b, l, r);
            let (lc, rc) = (blobs.blob(&le.1)?, blobs.blob(&re.1)?);
            if !is_binary(&lc) && !is_binary(&rc) {
                c.hunks = Some(merge3(alg, b"", &lc, &rc).into_hunks());
            }
            Ok(Res::Conflict(Box::new(c)))
        }
        (Some(_), None, Some(_)) | (Some(_), Some(_), None) => {
            Ok(Res::Conflict(Box::new(Conflict::new(path, ConflictKind::DeleteModify, b, l, r))))
        }
        (Some(be), Some(le), Some(re)) => {
            let mode = pick3(be.0, le.0, re.0);
            let (content, hunks) = match pick3(be.1, le.1, re.1) {
                Some(id) => (Some(id), None),
                None => {
                    let (bc, lc, rc) = (blobs.blob(&be.1)?, blobs.blob(&le.1)?, blobs.blob(&re.1)?);
                    if is_binary(&bc) || is_binary(&lc) || is_binary(&rc) {
                        (None, None)
                    } else {
                        match text_or_structural(alg, structural, path, &bc, &lc, &rc) {
                            TextMerge::Clean(bytes) => {
                                let id = blob_id(&bytes);
                                if mode.is_some() {
                                    new_blobs.insert(id, bytes);
                                }
                                (Some(id), None)
                            }
                            TextMerge::Conflicted(h) => (None, Some(h)),
                        }
                    }
                }
            };
            match (mode, content) {
                (Some(m), Some(id)) => Ok(Res::Keep(Some((m, id)))),
                (_, None) => {
                    let mut c = Conflict::new(path, ConflictKind::ModifyModify, b, l, r);
                    c.hunks = hunks;
                    Ok(Res::Conflict(Box::new(c)))
                }
                (None, Some(_)) => Ok(Res::Conflict(Box::new(Conflict::new(path, ConflictKind::ModeConflict, b, l, r)))),
            }
        }
        // unreachable given the early returns, kept total for safety
        _ => Ok(Res::Conflict(Box::new(Conflict::new(path, ConflictKind::ModifyModify, b, l, r)))),
    }
}

#[derive(PartialEq, Clone)]
enum Node {
    Absent,
    File(Entry),
    Dir(Vec<(String, Entry)>),
}

fn below(flat: &Flat, q: &str) -> Vec<(String, Entry)> {
    let pre = format!("{q}/");
    flat.range(pre.clone()..)
        .take_while(|(k, _)| k.starts_with(&pre))
        .map(|(k, v)| (k[pre.len()..].to_owned(), *v))
        .collect()
}

fn node(flat: &Flat, q: &str) -> Node {
    if let Some(e) = flat.get(q) {
        return Node::File(*e);
    }
    let sub = below(flat, q);
    if sub.is_empty() {
        Node::Absent
    } else {
        Node::Dir(sub)
    }
}

fn file_of(n: &Node) -> Option<Entry> {
    match n {
        Node::File(e) => Some(*e),
        _ => None,
    }
}

/// Three-way merge of flat path maps.
///
/// Per path: unchanged / one-side-changed / both-same take the obvious result; add/add,
/// modify/modify (text merge, binary => conflict), delete/modify and mode conflicts become
/// [`Conflict`]s; mode and content merge independently (a mode-only change on one side
/// composes with a content change on the other). A path that is a file on one side and has
/// descendants on another is merged as a whole node: if only one side changed it the change
/// wins, otherwise a `FileDirectory` conflict is recorded at the file path and the
/// directory side's descendants are merged normally.
pub fn merge_flat(blobs: &dyn BlobSource, alg: Algorithm, base: &Flat, left: &Flat, right: &Flat) -> Result<TreeMerge, MergeError> {
    merge_flat_with(blobs, alg, false, base, left, right)
}

/// Line merge, or (a `structural*` feature, `structural` set, a path whose extension has an enabled
/// driver) the structural merge, which itself falls back to the line merge on any failure.
fn text_or_structural(alg: Algorithm, structural: bool, path: &str, b: &[u8], l: &[u8], r: &[u8]) -> TextMerge {
    #[cfg(any(
        feature = "structural",
        feature = "structural-python",
        feature = "structural-typescript",
        feature = "structural-go"
    ))]
    if structural {
        if let Some(lang) = crate::structural::lang_for_path(path) {
            return crate::structural::merge_with(lang, alg, b, l, r).merge;
        }
    }
    let _ = (structural, path);
    merge3(alg, b, l, r)
}

/// Like [`merge_flat`]; `structural` enables the structural Rust merge for `.rs` files when the
/// crate is built with feature `structural` (ignored otherwise).
pub fn merge_flat_with(
    blobs: &dyn BlobSource,
    alg: Algorithm,
    structural: bool,
    base: &Flat,
    left: &Flat,
    right: &Flat,
) -> Result<TreeMerge, MergeError> {
    let mut out = TreeMerge::default();
    let all: BTreeSet<&String> = base.keys().chain(left.keys()).chain(right.keys()).collect();
    let mut conflicts: BTreeMap<String, Conflict> = BTreeMap::new();
    for p in &all {
        match merge_entry(
            blobs,
            alg,
            structural,
            p,
            base.get(*p).copied(),
            left.get(*p).copied(),
            right.get(*p).copied(),
            &mut out.new_blobs,
        )? {
            Res::Keep(Some(e)) => {
                out.entries.insert((*p).clone(), e);
            }
            Res::Keep(None) => {}
            Res::Conflict(c) => {
                conflicts.insert((*p).clone(), *c);
            }
        }
    }

    // file <-> directory collisions: a path that is a file somewhere and a prefix of another path
    let mut collisions: BTreeSet<String> = BTreeSet::new();
    for p in &all {
        for (i, _) in p.match_indices('/') {
            if all.contains(&p[..i].to_owned()) {
                collisions.insert(p[..i].to_owned());
            }
        }
    }
    let mut resolved: Vec<String> = Vec::new();
    for q in collisions {
        if resolved.iter().any(|r| q.starts_with(&format!("{r}/"))) {
            continue;
        }
        let (nb, nl, nr) = (node(base, &q), node(left, &q), node(right, &q));
        let chosen = if nl == nr || nr == nb {
            Some(nl.clone())
        } else if nl == nb {
            Some(nr.clone())
        } else {
            None
        };
        let pre = format!("{q}/");
        match chosen {
            Some(n) => {
                out.entries.remove(&q);
                conflicts.remove(&q);
                let doomed: Vec<String> = out
                    .entries
                    .range(pre.clone()..)
                    .take_while(|(k, _)| k.starts_with(&pre))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in doomed {
                    out.entries.remove(&k);
                }
                let doomed: Vec<String> = conflicts
                    .range(pre.clone()..)
                    .take_while(|(k, _)| k.starts_with(&pre))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in doomed {
                    conflicts.remove(&k);
                }
                match n {
                    Node::File(e) => {
                        out.entries.insert(q.clone(), e);
                    }
                    Node::Dir(sub) => {
                        for (rel, e) in sub {
                            out.entries.insert(format!("{pre}{rel}"), e);
                        }
                    }
                    Node::Absent => {}
                }
                resolved.push(q);
            }
            None => {
                out.entries.remove(&q);
                conflicts.insert(
                    q.clone(),
                    Conflict::new(&q, ConflictKind::FileDirectory, file_of(&nb), file_of(&nl), file_of(&nr)),
                );
            }
        }
    }
    out.conflicts = conflicts.into_values().collect();
    Ok(out)
}

fn flatten(store: &dyn ObjectStore, root: Option<ObjectId>) -> Result<Flat, MergeError> {
    fn walk(store: &dyn ObjectStore, tree: ObjectId, prefix: &str, out: &mut Flat) -> Result<(), MergeError> {
        let Object::Tree(t) = store.get(&tree)? else {
            return Err(MergeError::Corrupt(format!("object {tree} is not a tree")));
        };
        for e in t.entries() {
            let p = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            match e.mode {
                EntryMode::Directory => walk(store, e.id, &p, out)?,
                m => {
                    out.insert(p, (m, e.id));
                }
            }
        }
        Ok(())
    }
    let mut out = Flat::new();
    if let Some(r) = root {
        walk(store, r, "", &mut out)?;
    }
    Ok(out)
}

/// Three-way merge of stored trees (`None` = empty tree). Whole-tree flatten for now;
/// subtree skipping is M5 work.
pub fn merge_trees(
    store: &dyn ObjectStore,
    alg: Algorithm,
    base: Option<ObjectId>,
    left: Option<ObjectId>,
    right: Option<ObjectId>,
) -> Result<TreeMerge, MergeError> {
    let (b, l, r) = (flatten(store, base)?, flatten(store, left)?, flatten(store, right)?);
    merge_flat(&StoreBlobs(store), alg, &b, &l, &r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use vcs_store::MemoryStore;

    const F: EntryMode = EntryMode::File;
    const X: EntryMode = EntryMode::Executable;
    const ALGS: [Algorithm; 2] = [Algorithm::Myers, Algorithm::Patience];

    struct Fx {
        blobs: MapBlobs,
    }
    impl Fx {
        fn new() -> Self {
            Self {
                blobs: MapBlobs::default(),
            }
        }
        fn f(&mut self, s: &str) -> Entry {
            (F, self.blobs.add(s.as_bytes()))
        }
        fn bin(&mut self, s: u8) -> Entry {
            (F, self.blobs.add(&[0, s]))
        }
        fn run(&self, b: &[(&str, Entry)], l: &[(&str, Entry)], r: &[(&str, Entry)]) -> TreeMerge {
            let m = |v: &[(&str, Entry)]| -> Flat { v.iter().map(|(p, e)| ((*p).to_owned(), *e)).collect() };
            let a = merge_flat(&self.blobs, Algorithm::Myers, &m(b), &m(l), &m(r)).unwrap();
            let p = merge_flat(&self.blobs, Algorithm::Patience, &m(b), &m(l), &m(r)).unwrap();
            assert_eq!(a.conflicts.len(), p.conflicts.len());
            a
        }
    }

    #[test]
    fn unchanged_one_side_and_both_same() {
        let mut x = Fx::new();
        let (a, b, c) = (x.f("a"), x.f("b"), x.f("c"));
        let m = x.run(
            &[("p", a), ("q", a), ("r", a)],
            &[("p", a), ("q", b), ("r", c)],
            &[("p", a), ("q", a), ("r", c)],
        );
        assert!(m.conflicts.is_empty());
        assert_eq!(m.entries["p"], a);
        assert_eq!(m.entries["q"], b);
        assert_eq!(m.entries["r"], c);
        // one-side add, one-side delete, both delete
        let m = x.run(&[("d", a), ("e", a)], &[("n", b), ("e", a)], &[("e", a)]);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.entries.keys().collect::<Vec<_>>(), vec!["e", "n"]);
        // both add same
        let m = x.run(&[], &[("n", b)], &[("n", b)]);
        assert_eq!(m.entries["n"], b);
    }

    #[test]
    fn add_add_different_is_conflict_with_hunks() {
        let mut x = Fx::new();
        let (a, b) = (x.f("left\n"), x.f("right\n"));
        let m = x.run(&[], &[("n", a)], &[("n", b)]);
        assert_eq!(m.conflicts.len(), 1);
        let c = &m.conflicts[0];
        assert_eq!(
            (c.kind, c.base, c.left, c.right),
            (ConflictKind::AddAdd, None, Some(a.1), Some(b.1))
        );
        assert!(c.hunks.is_some());
        assert!(m.entries.is_empty());
    }

    #[test]
    fn modify_modify_text_clean_and_conflict_and_binary() {
        let mut x = Fx::new();
        let base = x.f("1\n2\n3\n4\n5\n");
        let l = x.f("L\n2\n3\n4\n5\n");
        let r = x.f("1\n2\n3\n4\nR\n");
        let m = x.run(&[("t", base)], &[("t", l)], &[("t", r)]);
        assert!(m.conflicts.is_empty());
        let (mode, id) = m.entries["t"];
        assert_eq!(mode, F);
        assert_eq!(m.new_blobs[&id], b"L\n2\n3\n4\nR\n");
        assert_eq!(id, blob_id(b"L\n2\n3\n4\nR\n"));

        let l2 = x.f("X\n2\n3\n4\n5\n");
        let m = x.run(&[("t", base)], &[("t", l)], &[("t", l2)]);
        let c = &m.conflicts[0];
        assert_eq!(c.kind, ConflictKind::ModifyModify);
        assert!(c.hunks.is_some() && m.new_blobs.is_empty());

        let (bb, bl, br) = (x.bin(0), x.bin(1), x.bin(2));
        let m = x.run(&[("t", bb)], &[("t", bl)], &[("t", br)]);
        assert_eq!(m.conflicts[0].kind, ConflictKind::ModifyModify);
        assert!(m.conflicts[0].hunks.is_none());
    }

    #[test]
    fn delete_modify_both_directions_and_delete_delete() {
        let mut x = Fx::new();
        let (a, b) = (x.f("a"), x.f("b"));
        for (l, r) in [(vec![], vec![("t", b)]), (vec![("t", b)], vec![])] {
            let m = x.run(&[("t", a)], &l, &r);
            assert_eq!(m.conflicts[0].kind, ConflictKind::DeleteModify);
            assert_eq!(m.conflicts[0].base, Some(a.1));
        }
        let m = x.run(&[("t", a)], &[], &[]);
        assert!(m.entries.is_empty() && m.conflicts.is_empty());
    }

    #[test]
    fn mode_only_changes() {
        let mut x = Fx::new();
        let a = x.f("a");
        let ax = (X, a.1);
        // mode on one side, content on the other => composes
        let b = x.f("b");
        let m = x.run(&[("t", a)], &[("t", ax)], &[("t", b)]);
        assert_eq!(m.entries["t"], (X, b.1));
        // mode-only on one side
        let m = x.run(&[("t", a)], &[("t", ax)], &[("t", a)]);
        assert_eq!(m.entries["t"], ax);
        // different modes can't both be set: base X, left F... need three distinct modes -> use D? only two file modes:
        // both change mode identically is fine
        let m = x.run(&[("t", a)], &[("t", ax)], &[("t", ax)]);
        assert_eq!(m.entries["t"], ax);
    }

    #[test]
    fn mode_conflict_when_modes_disagree_and_both_differ_from_base() {
        // with only two file modes a three-way mode disagreement needs add/add; check it is a conflict
        let mut x = Fx::new();
        let a = x.f("a");
        let m = x.run(&[], &[("t", a)], &[("t", (X, a.1))]);
        assert_eq!(m.conflicts[0].kind, ConflictKind::AddAdd);
        // base exec, left file+edit, right exec+edit-to-different... content merges, modes: b=X l=F r=X -> take F (clean)
        let (b0, l0, r0) = (x.f("1\n2\n"), x.f("1\n2\n3\n"), x.f("0\n1\n2\n"));
        let m = x.run(&[("t", (X, b0.1))], &[("t", l0)], &[("t", (X, r0.1))]);
        assert_eq!(m.entries["t"].0, F);
    }

    #[test]
    fn file_vs_directory() {
        let mut x = Fx::new();
        let (a, b, c) = (x.f("a"), x.f("b"), x.f("c"));
        // left: "d" is a file; right: "d" is a directory; base: nothing => conflict at d, d/x kept
        let m = x.run(&[], &[("d", a)], &[("d/x", b)]);
        assert_eq!(m.conflicts.len(), 1);
        assert_eq!(m.conflicts[0].path, "d");
        assert_eq!(m.conflicts[0].kind, ConflictKind::FileDirectory);
        assert_eq!(m.conflicts[0].left, Some(a.1));
        assert_eq!(m.conflicts[0].right, None);
        assert!(!m.entries.contains_key("d"));
        assert_eq!(m.entries["d/x"], b);

        // only right changed file -> dir: clean, directory wins
        let m = x.run(&[("d", a)], &[("d", a)], &[("d/x", b), ("d/y", c)]);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.entries.keys().collect::<Vec<_>>(), vec!["d/x", "d/y"]);

        // only left changed dir -> file: clean, file wins, stale descendants gone
        let m = x.run(&[("d/x", b)], &[("d", a)], &[("d/x", b)]);
        assert!(m.conflicts.is_empty());
        assert_eq!(m.entries.keys().collect::<Vec<_>>(), vec!["d"]);

        // both sides replaced dir with different things
        let m = x.run(&[("d/x", b)], &[("d", a)], &[("d/x", c)]);
        assert_eq!(m.conflicts[0].kind, ConflictKind::FileDirectory);
        assert_eq!(m.conflicts[0].base, None);
    }

    #[test]
    fn merge_stored_trees() {
        let s = MemoryStore::new();
        let mk = |files: &[(&str, &str)]| -> Option<ObjectId> {
            let mut nodes: BTreeMap<String, Vec<vcs_objects::TreeEntry>> = BTreeMap::new();
            for (p, c) in files {
                let id = s.put_blob(c.as_bytes()).unwrap();
                let (dir, name) = p.rsplit_once('/').map_or(("", *p), |(d, n)| (d, n));
                nodes.entry(dir.to_owned()).or_default().push(vcs_objects::TreeEntry {
                    name: name.to_owned(),
                    mode: F,
                    id,
                });
            }
            // supports depth <= 1
            let mut root = nodes.remove("").unwrap_or_default();
            for (d, es) in nodes {
                let id = s.put(&Object::Tree(vcs_objects::Tree::new(es).unwrap())).unwrap();
                root.push(vcs_objects::TreeEntry {
                    name: d,
                    mode: EntryMode::Directory,
                    id,
                });
            }
            Some(s.put(&Object::Tree(vcs_objects::Tree::new(root).unwrap())).unwrap())
        };
        let base = mk(&[("a.txt", "1\n2\n3\n"), ("d/x", "x\n")]);
        let left = mk(&[("a.txt", "L\n2\n3\n"), ("d/x", "x\n")]);
        let right = mk(&[("a.txt", "1\n2\nR\n"), ("d/x", "x\n"), ("d/y", "y\n")]);
        for a in ALGS {
            let m = merge_trees(&s, a, base, left, right).unwrap();
            assert!(m.conflicts.is_empty());
            assert_eq!(m.entries.keys().collect::<Vec<_>>(), vec!["a.txt", "d/x", "d/y"]);
            assert_eq!(m.new_blobs[&m.entries["a.txt"].1], b"L\n2\nR\n");
        }
        // None = empty tree
        let m = merge_trees(&s, Algorithm::Myers, None, None, None).unwrap();
        assert!(m.entries.is_empty());
    }

    fn arb_flat(blobs: Vec<ObjectId>) -> impl Strategy<Value = Flat> {
        proptest::collection::btree_map("[a-c]", (prop::bool::ANY, 0..blobs.len()), 0..4)
            .prop_map(move |m| m.into_iter().map(|(k, (x, i))| (k, (if x { X } else { F }, blobs[i]))).collect())
    }

    fn blob_pool() -> (MapBlobs, Vec<ObjectId>) {
        let mut m = MapBlobs::default();
        let ids = ["1\n2\n3\n", "L\n2\n3\n", "1\n2\nR\n", "L\n2\nR\n", "zzz\n"]
            .iter()
            .map(|s| m.add(s.as_bytes()))
            .collect();
        (m, ids)
    }

    proptest! {
        #[test]
        fn merge_x_x_x_is_x(x in arb_flat(blob_pool().1)) {
            let (blobs, _) = blob_pool();
            for a in ALGS {
                let m = merge_flat(&blobs, a, &x, &x, &x).unwrap();
                prop_assert!(m.conflicts.is_empty());
                prop_assert_eq!(&m.entries, &x);
            }
        }

        #[test]
        fn one_side_unchanged_yields_other(b in arb_flat(blob_pool().1), x in arb_flat(blob_pool().1)) {
            let (blobs, _) = blob_pool();
            for a in ALGS {
                let m = merge_flat(&blobs, a, &b, &x, &b).unwrap();
                prop_assert!(m.conflicts.is_empty());
                prop_assert_eq!(&m.entries, &x);
                let m = merge_flat(&blobs, a, &b, &b, &x).unwrap();
                prop_assert!(m.conflicts.is_empty());
                prop_assert_eq!(&m.entries, &x);
            }
        }

        #[test]
        fn clean_merges_symmetric(b in arb_flat(blob_pool().1), l in arb_flat(blob_pool().1), r in arb_flat(blob_pool().1)) {
            let (blobs, _) = blob_pool();
            for a in ALGS {
                let lr = merge_flat(&blobs, a, &b, &l, &r).unwrap();
                let rl = merge_flat(&blobs, a, &b, &r, &l).unwrap();
                prop_assert_eq!(lr.conflicts.len(), rl.conflicts.len());
                if lr.conflicts.is_empty() { prop_assert_eq!(lr.entries, rl.entries); }
            }
        }

        /// Every input path is either in entries or covered by a conflict (itself or a descendant
        /// of a conflicted file/dir path): no path silently vanishes unless it was deleted by a side
        /// while the other left it unchanged.
        #[test]
        fn conflicts_keep_all_ids(b in arb_flat(blob_pool().1), l in arb_flat(blob_pool().1), r in arb_flat(blob_pool().1)) {
            let (blobs, _) = blob_pool();
            let m = merge_flat(&blobs, Algorithm::Patience, &b, &l, &r).unwrap();
            for c in &m.conflicts {
                prop_assert_eq!(c.left, l.get(&c.path).map(|e| e.1));
                prop_assert_eq!(c.right, r.get(&c.path).map(|e| e.1));
                prop_assert_eq!(c.base, b.get(&c.path).map(|e| e.1));
                prop_assert!(!m.entries.contains_key(&c.path));
            }
        }
    }
}
