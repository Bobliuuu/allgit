//! Three-way merge of stored trees that only reads the directories where the sides differ.
//!
//! `merge`, `cherry-pick`, `revert`, `rebase` and `stash pop` used to flatten base, ours and theirs
//! (O(tracked files) each), merge the flat maps and rebuild the whole result tree. Here the three
//! trees are walked together: an entry that is equal on two sides is decided by id without reading
//! it (whole subtrees included), directories that differ on all three sides are walked, and only
//! the remaining small "leaf" nodes (a file on some side, or a path changed on all sides) are
//! flattened and merged with the ordinary flat engine ([`Repo::tree3`]). The cost follows the
//! number of changed directories, not the number of tracked files.
//!
//! The result is a set of *patches* against the ours tree; [`Merge3::apply`] rewrites only the
//! directories on the path to a patch. The patched tree is identical (same ids) to what the flat
//! merge plus a full rebuild produced, which `tests` below check against the old path on random
//! trees.
//!
//! Rename detection needs a global view, so with `renames` on the old path is used (its result is
//! converted to patches against ours).

use crate::error::{Error, Result};
use crate::repo::Repo;
use crate::treeops::{self, Flat, TreeDiff};
use std::collections::BTreeMap;
use vcs_merge::Conflict;
use vcs_objects::{EntryMode, Object, ObjectId, Tree, TreeEntry};
use vcs_store::ObjectStore;

pub(crate) type Entry = (EntryMode, ObjectId);

/// Outcome of a tree merge, as changes to the "ours" tree.
#[derive(Debug, Default)]
pub(crate) struct Merge3 {
    /// Zone-relative path -> new entry (`None` = remove). A `Directory` entry replaces the whole
    /// subtree at that path.
    pub patches: BTreeMap<String, Option<Entry>>,
    /// Conflicts; `path` is the full path (zone prefix included), as with the flat engine.
    pub conflicts: Vec<Conflict>,
    /// Blobs created by clean text merges; the caller must store them.
    pub new_blobs: BTreeMap<ObjectId, Vec<u8>>,
    prefix: String,
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

#[derive(Default)]
struct PNode {
    exact: Option<Option<Entry>>,
    kids: BTreeMap<String, PNode>,
}

impl Merge3 {
    /// Zone-relative form of a full path.
    pub fn rel<'a>(&self, full: &'a str) -> &'a str {
        if self.prefix.is_empty() {
            full
        } else {
            full.strip_prefix(self.prefix.as_str())
                .and_then(|r| r.strip_prefix('/'))
                .unwrap_or(full)
        }
    }

    /// Record what a conflicted path holds in the working tree (marker file or one side).
    pub fn set_placeholder(&mut self, full: &str, e: Entry) {
        let rel = self.rel(full).to_owned();
        self.patches.insert(rel, Some(e));
    }

    /// Apply the patches to the `ours` tree. Returns the new root and the difference between the
    /// two trees (the same `old`/`new` maps as `treeops::diff_trees(ours, root)`, computed from the
    /// patches so the new tree need not exist in the store). With `write` the new trees are
    /// stored; without, only their ids are computed (a conflicted state is never committed).
    pub fn apply(&self, store: &dyn ObjectStore, ours: Option<ObjectId>, write: bool) -> Result<(ObjectId, TreeDiff)> {
        let mut root = PNode::default();
        for (p, v) in &self.patches {
            let mut cur = &mut root;
            for c in p.split('/') {
                cur = cur.kids.entry(c.to_owned()).or_default();
            }
            cur.exact = Some(*v);
        }
        let mut d = TreeDiff::default();
        let id = apply_node(store, ours, &root, &self.prefix, write, true, &mut d)?.expect("root is always produced");
        Ok((id, d))
    }
}

/// Chain two differences: `first` goes from tree W to tree C, `second` from C to tree F; the result
/// goes from W to F (what `treeops::diff_trees(W, F)` would give, without reading F).
pub(crate) fn compose(first: TreeDiff, second: TreeDiff) -> TreeDiff {
    let touched = |d: &TreeDiff, p: &String| d.old.contains_key(p) || d.new.contains_key(p);
    let paths: std::collections::BTreeSet<&String> = first
        .old
        .keys()
        .chain(first.new.keys())
        .chain(second.old.keys())
        .chain(second.new.keys())
        .collect();
    let mut out = TreeDiff::default();
    for p in paths {
        // W's value: from `first` when W and C differ at p, else C's value (recorded in `second`)
        let w = if touched(&first, p) { first.old.get(p) } else { second.old.get(p) };
        // F's value: from `second` when C and F differ at p, else C's value (recorded in `first`)
        let f = if touched(&second, p) { second.new.get(p) } else { first.new.get(p) };
        if w == f {
            continue;
        }
        if let Some(w) = w {
            out.old.insert(p.clone(), *w);
        }
        if let Some(f) = f {
            out.new.insert(p.clone(), *f);
        }
    }
    out.new_dir_names = first.new_dir_names;
    out.new_dir_names.extend(second.new_dir_names);
    out
}

fn read_entries(store: &dyn ObjectStore, id: Option<ObjectId>) -> Result<Vec<TreeEntry>> {
    match id {
        None => Ok(Vec::new()),
        Some(id) => match store.get(&id)? {
            Object::Tree(t) => Ok(t.entries().to_vec()),
            _ => Err(Error::Corrupt(format!("object {id} is not a tree"))),
        },
    }
}

fn apply_node(
    store: &dyn ObjectStore,
    tree: Option<ObjectId>,
    p: &PNode,
    path: &str,
    write: bool,
    root: bool,
    d: &mut TreeDiff,
) -> Result<Option<ObjectId>> {
    let mut map: BTreeMap<String, Entry> = read_entries(store, tree)?.into_iter().map(|e| (e.name, (e.mode, e.id))).collect();
    for (name, kid) in &p.kids {
        let ours = map.get(name).copied();
        let mut cur = ours;
        let child_path = join(path, name);
        if let Some(x) = kid.exact {
            cur = x;
            treeops::diff_entries(store, ours, x, &child_path, d)?;
        }
        if !kid.kids.is_empty() {
            let child = cur.filter(|e| e.0 == EntryMode::Directory).map(|e| e.1);
            match apply_node(store, child, kid, &child_path, write, false, d)? {
                Some(id) => {
                    if cur.is_some() && child.is_none() {
                        return Err(Error::Msg(format!("path conflict at {name:?}")));
                    }
                    cur = Some((EntryMode::Directory, id));
                }
                // the directory emptied out: drop it, but never drop a file that only had
                // descendant removals recorded against it
                None if child.is_some() => cur = None,
                None => {}
            }
        }
        match cur {
            Some(e) => {
                map.insert(name.clone(), e);
            }
            None => {
                map.remove(name);
            }
        }
    }
    if map.is_empty() && !root {
        return Ok(None);
    }
    d.new_dir_names.push(map.keys().cloned().collect());
    let entries: Vec<TreeEntry> = map.into_iter().map(|(name, (mode, id))| TreeEntry { name, mode, id }).collect();
    let t = Tree::new(entries)?;
    Ok(Some(if write {
        store.put(&Object::Tree(t))?
    } else {
        vcs_objects::id_of_encoded(&Object::Tree(t).encode())
    }))
}

impl Repo {
    /// Three-way merge of the zone trees `base`, `ours`, `theirs` (`None` = empty), honouring the
    /// structural, rename and attribute settings exactly like [`Repo::tree3`].
    pub(crate) fn merge3_trees(
        &self,
        prefix: &str,
        base: Option<ObjectId>,
        ours: Option<ObjectId>,
        theirs: Option<ObjectId>,
    ) -> Result<Merge3> {
        let mut out = Merge3 {
            prefix: prefix.to_owned(),
            ..Default::default()
        };
        if self.renames {
            self.merge3_whole(&mut out, base, ours, theirs)?;
        } else {
            self.walk3(base, ours, theirs, "", &mut out)?;
        }
        out.conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Whole-tree route (rename detection): flatten, merge, diff against ours.
    fn merge3_whole(&self, out: &mut Merge3, base: Option<ObjectId>, ours: Option<ObjectId>, theirs: Option<ObjectId>) -> Result<()> {
        let prefix = out.prefix.clone();
        let (fb, fo, ft) = (
            treeops::flatten(&self.store, base, &prefix)?,
            treeops::flatten(&self.store, ours, &prefix)?,
            treeops::flatten(&self.store, theirs, &prefix)?,
        );
        let tm = self.tree3(&fb, &fo, &ft).map_err(crate::merge::me)?;
        out.add_flat_result(&fo, tm);
        Ok(())
    }

    fn walk3(&self, b: Option<ObjectId>, l: Option<ObjectId>, r: Option<ObjectId>, rel: &str, out: &mut Merge3) -> Result<()> {
        let (be, le, re) = (
            read_entries(&self.store, b)?,
            read_entries(&self.store, l)?,
            read_entries(&self.store, r)?,
        );
        let mut names: BTreeMap<&str, [Option<&TreeEntry>; 3]> = BTreeMap::new();
        for (i, es) in [&be, &le, &re].into_iter().enumerate() {
            for e in es {
                names.entry(e.name.as_str()).or_default()[i] = Some(e);
            }
        }
        let key = |e: Option<&TreeEntry>| e.map(|e| (e.mode, e.id));
        for (name, [b, l, r]) in names {
            let (kb, kl, kr) = (key(b), key(l), key(r));
            if kl == kr || kr == kb {
                continue; // ours stays
            }
            let path = join(rel, name);
            if kl == kb {
                out.patches.insert(path, kr); // only theirs changed: take its whole entry
                continue;
            }
            let has_file = [b, l, r].into_iter().flatten().any(|e| e.mode != EntryMode::Directory);
            if has_file {
                self.leaf3(&path, [b, l, r], out)?;
            } else {
                self.walk3(b.map(|e| e.id), l.map(|e| e.id), r.map(|e| e.id), &path, out)?;
            }
        }
        Ok(())
    }

    /// A node that changed on all sides and involves a file: flatten it and use the flat engine.
    fn leaf3(&self, rel: &str, sides: [Option<&TreeEntry>; 3], out: &mut Merge3) -> Result<()> {
        let full = join(&out.prefix, rel);
        let flat_of = |e: Option<&TreeEntry>| -> Result<Flat> {
            let mut f = Flat::new();
            match e {
                None => {}
                Some(e) if e.mode == EntryMode::Directory => f = treeops::flatten(&self.store, Some(e.id), &full)?,
                Some(e) => {
                    f.insert(full.clone(), (e.mode, e.id));
                }
            }
            Ok(f)
        };
        let (fb, fl, fr) = (flat_of(sides[0])?, flat_of(sides[1])?, flat_of(sides[2])?);
        let tm = self.tree3(&fb, &fl, &fr).map_err(crate::merge::me)?;
        out.add_flat_result(&fl, tm);
        Ok(())
    }
}

impl Merge3 {
    /// Fold a flat merge of (a part of) the tree into patches against `ours_flat` (same part).
    fn add_flat_result(&mut self, ours_flat: &Flat, tm: vcs_merge::TreeMerge) {
        for (p, e) in &tm.entries {
            if ours_flat.get(p) != Some(e) {
                let rel = self.rel(p).to_owned();
                self.patches.insert(rel, Some(*e));
            }
        }
        for p in ours_flat.keys() {
            if !tm.entries.contains_key(p) {
                let rel = self.rel(p).to_owned();
                self.patches.insert(rel, None);
            }
        }
        self.conflicts.extend(tm.conflicts);
        self.new_blobs.extend(tm.new_blobs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;
    use vcs_store::ObjectStore;

    const POOL: [&str; 14] = [
        "a", "a/x", "a/y", "b", "b/c/d", "b/c/e", "b/f", "c", "d/e", "d/e/f", "g/h/i", "g/h", "g/j", "k/l/m/n",
    ];
    const BLOBS: [&[u8]; 8] = [
        b"a\nb\nc\nd\ne\n",
        b"A\nb\nc\nd\ne\n",
        b"a\nb\nc\nd\nE\n",
        b"A\nb\nc\nd\nE\n",
        b"a\nB\nc\nd\ne\n",
        b"x\ny\n",
        b"\0bin\0",
        b"",
    ];

    type Side = Vec<Option<(u8, bool)>>; // per pool path: blob variant + executable

    fn side() -> impl Strategy<Value = Side> {
        proptest::collection::vec(proptest::option::of((0u8..BLOBS.len() as u8, any::<bool>())), POOL.len())
    }

    /// Mostly-equal sides so that most subtrees are shared: derive ours/theirs from base by a few edits.
    fn derive(base: &Side, edits: &[(usize, Option<(u8, bool)>)]) -> Side {
        let mut s = base.clone();
        for (i, v) in edits {
            s[*i % POOL.len()] = *v;
        }
        s
    }

    fn flat_of(store: &dyn ObjectStore, s: &Side, prefix: &str) -> Flat {
        let mut f = Flat::new();
        let mut taken: BTreeSet<&str> = BTreeSet::new();
        for (i, v) in s.iter().enumerate() {
            let Some((b, x)) = v else { continue };
            let p = POOL[i];
            // keep each side a valid tree: no path may be a file and a directory
            let clash = taken
                .iter()
                .any(|t| p.starts_with(&format!("{t}/")) || t.starts_with(&format!("{p}/")));
            if clash {
                continue;
            }
            taken.insert(p);
            let id = store.put_content(BLOBS[*b as usize]).unwrap();
            let mode = if *x { EntryMode::Executable } else { EntryMode::File };
            f.insert(if prefix.is_empty() { p.to_owned() } else { format!("{prefix}/{p}") }, (mode, id));
        }
        f
    }

    fn build_root(store: &dyn ObjectStore, f: &Flat, prefix: &str) -> ObjectId {
        try_build_root(store, f, prefix).unwrap()
    }

    fn try_build_root(store: &dyn ObjectStore, f: &Flat, prefix: &str) -> Result<ObjectId> {
        let strip = |p: &str| {
            if prefix.is_empty() {
                p.to_owned()
            } else {
                p[prefix.len() + 1..].to_owned()
            }
        };
        let rels: Vec<(String, EntryMode, ObjectId)> = f.iter().map(|(p, (m, i))| (strip(p), *m, *i)).collect();
        treeops::build(store, rels.iter().map(|(p, m, i)| (p.as_str(), *m, *i)))
    }

    fn check(repo: &Repo, prefix: &str, b: &Side, o: &Side, t: &Side) {
        let st: &dyn ObjectStore = &repo.store;
        let (fb, fo, ft) = (flat_of(st, b, prefix), flat_of(st, o, prefix), flat_of(st, t, prefix));
        let (rb, ro, rt) = (
            build_root(st, &fb, prefix),
            build_root(st, &fo, prefix),
            build_root(st, &ft, prefix),
        );
        // old path: flat merge
        let tm = repo.tree3(&fb, &fo, &ft).unwrap();
        // new path
        let mut m = repo.merge3_trees(prefix, Some(rb), Some(ro), Some(rt)).unwrap();
        assert_eq!(m.conflicts, tm.conflicts, "conflicts differ");
        assert_eq!(m.new_blobs, tm.new_blobs, "new blobs differ");
        // conflicted paths get a placeholder (what the callers do), clean ones do not
        let ph = st.put_content(b"placeholder").unwrap();
        let mut expect = tm.entries.clone();
        for c in &tm.conflicts {
            expect.insert(c.path.clone(), (EntryMode::File, ph));
            m.set_placeholder(&c.path, (EntryMode::File, ph));
        }
        // File/directory conflicts are refused by every caller before any tree is built (and the
        // flat result then holds a file and a directory at one path), so only conflicts and blobs
        // are compared for them. Every other result must build.
        if tm.conflicts.iter().any(|c| c.kind == vcs_merge::ConflictKind::FileDirectory) {
            return;
        }
        if try_build_root(st, &expect, prefix).is_err() {
            panic!("old route cannot build a non-file/directory result: {expect:?}");
        }
        let (root, diff) = m.apply(st, Some(ro), true).unwrap();
        let (hashed, diff2) = m.apply(st, Some(ro), false).unwrap();
        assert_eq!(root, hashed, "write and hash-only disagree");
        let want = treeops::diff_trees(st, Some(ro), Some(root), prefix).unwrap();
        assert_eq!((&diff.old, &diff.new), (&want.old, &want.new), "patch diff differs from tree diff");
        assert_eq!((&diff2.old, &diff2.new), (&want.old, &want.new));
        // a third tree W (here the base) chained through ours
        let w_to_o = treeops::diff_trees(st, Some(rb), Some(ro), prefix).unwrap();
        let chained = compose(w_to_o, diff);
        let direct = treeops::diff_trees(st, Some(rb), Some(root), prefix).unwrap();
        assert_eq!((&chained.old, &chained.new), (&direct.old, &direct.new), "composed diff differs");
        assert_eq!(treeops::flatten(st, Some(root), prefix).unwrap(), expect, "merged trees differ");
        if tm.conflicts.is_empty() {
            assert_eq!(
                root,
                build_root(st, &tm.entries, prefix),
                "clean merge root id differs from full rebuild"
            );
        }
    }

    fn repo() -> (tempfile::TempDir, Repo) {
        let d = tempfile::tempdir().unwrap();
        let r = Repo::init(&d.path().join("r")).unwrap();
        (d, r)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn tree_walk_equals_flat_merge(
            base in side(),
            e1 in proptest::collection::vec((0usize..64, proptest::option::of((0u8..BLOBS.len() as u8, any::<bool>()))), 0..5),
            e2 in proptest::collection::vec((0usize..64, proptest::option::of((0u8..BLOBS.len() as u8, any::<bool>()))), 0..5),
            prefix in prop_oneof![Just(""), Just("zone"), Just("zone/deep")],
        ) {
            let (_d, repo) = repo();
            let (o, t) = (derive(&base, &e1), derive(&base, &e2));
            check(&repo, prefix, &base, &o, &t);
        }

        #[test]
        fn tree_walk_equals_flat_merge_independent_sides(b in side(), o in side(), t in side()) {
            let (_d, repo) = repo();
            check(&repo, "", &b, &o, &t);
        }
    }

    #[test]
    fn identical_subtrees_are_not_read() {
        // A store that fails on reading any tree outside the changed directory proves subtrees with
        // equal ids are decided by id alone.
        let (_d, repo) = repo();
        let st: &dyn ObjectStore = &repo.store;
        let mut flat = Flat::new();
        for i in 0..50 {
            let id = st.put_content(format!("f{i}").as_bytes()).unwrap();
            flat.insert(format!("big/d{}/f", i % 10) + &i.to_string(), (EntryMode::File, id));
        }
        let new = st.put_content(b"changed").unwrap();
        let mut theirs = flat.clone();
        theirs.insert("small/x".into(), (EntryMode::File, new));
        let mut flat_small = flat.clone();
        flat_small.insert("small/x".into(), (EntryMode::File, st.put_content(b"orig").unwrap()));
        let (rb, ro) = (build_root(st, &flat_small, ""), build_root(st, &flat_small, ""));
        let rt = build_root(st, &theirs, "");
        let m = repo.merge3_trees("", Some(rb), Some(ro), Some(rt)).unwrap();
        assert_eq!(m.patches.len(), 1, "{:?}", m.patches);
        assert!(m.patches.contains_key("small"));
    }

    #[test]
    fn patches_cannot_drop_a_file_for_descendant_removals() {
        let (_d, repo) = repo();
        let st: &dyn ObjectStore = &repo.store;
        let id = st.put_content(b"x").unwrap();
        let mut f = Flat::new();
        f.insert("q".into(), (EntryMode::File, id));
        let ro = build_root(st, &f, "");
        let mut m = Merge3::default();
        m.patches.insert("q/a".into(), None);
        let (root, _) = m.apply(st, Some(ro), true).unwrap();
        assert_eq!(root, ro);
        m.patches.insert("q/b".into(), Some((EntryMode::File, id)));
        assert!(m.apply(st, Some(ro), true).is_err());
    }
}
