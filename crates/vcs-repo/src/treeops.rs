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
