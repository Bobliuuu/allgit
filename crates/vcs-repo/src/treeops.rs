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

    #[test]
    fn build_rejects_file_dir_conflict() {
        let s = MemoryStore::new();
        let b = s.put_blob(b"1").unwrap();
        assert!(build(&s, [("a", EntryMode::File, b), ("a/b", EntryMode::File, b)]).is_err());
    }
}
