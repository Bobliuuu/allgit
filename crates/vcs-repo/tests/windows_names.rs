//! Windows-unsafe names are refused on checkout when `VCS_PROTECT_WINDOWS` is set (always on Windows).
#![cfg(unix)]

use std::fs;
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Tree, TreeEntry};
use vcs_repo::Repo;

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}

fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}

/// Publish a crafted root-zone commit as a branch without going through `add`.
fn plant_branch(r: &Repo, branch: &str, entries: Vec<(&str, EntryMode, &[u8])>) {
    let mut es = Vec::new();
    for (name, mode, data) in entries {
        let id = r.store().put(&Object::Blob(data.to_vec())).unwrap();
        es.push(TreeEntry {
            name: name.into(),
            mode,
            id,
        });
    }
    let root = r.store().put(&Object::Tree(Tree::new(es).unwrap())).unwrap();
    let c = Commit {
        zone: "root".into(),
        parents: vec![],
        root,
        change_id: ChangeId::from_bytes([7; 16]),
        author: sig(),
        message: "crafted".into(),
    };
    let cid: ObjectId = r.store().put(&Object::Commit(c)).unwrap();
    r.store().sync().unwrap();
    let mut refs = r.refs().unwrap();
    refs.apply(&[RefUpdate {
        zone: "root".into(),
        branch: branch.into(),
        old: None,
        new: cid,
    }])
    .unwrap();
    fs::write(r.worktree().join(".vcs/refs"), refs.encode()).unwrap();
}

fn clean_main_then_checkout(r: &Repo, br: &str) -> Result<(), vcs_repo::Error> {
    r.checkout("root", br).map(|_| ())
}

#[test]
fn windows_unsafe_names_are_refused_on_checkout_only_when_enabled() {
    let (_d, r) = setup();
    write(&r, "keep", "k");
    r.add(&["".into()]).unwrap();
    r.commit("base", sig()).unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("w0", "aux.txt".into()),
        ("w1", "CON".into()),
        ("w2", "trailing.".into()),
        ("w3", "a:b".into()),
        ("w4", "d".repeat(250)),
    ];
    for (i, (br, name)) in cases.iter().enumerate() {
        let entries: Vec<(&str, EntryMode, &[u8])> = vec![(name.as_str(), EntryMode::File, b"x")];
        plant_branch(&r, br, entries);
        // disabled: allowed (Linux can hold these names)
        std::env::remove_var("VCS_PROTECT_WINDOWS");
        clean_main_then_checkout(&r, br).unwrap_or_else(|e| panic!("{name:?} must be allowed when disabled: {e}"));
        clean_main_then_checkout(&r, "main").unwrap();
        // enabled: refused, nothing written, still on main
        std::env::set_var("VCS_PROTECT_WINDOWS", "1");
        let e = clean_main_then_checkout(&r, br).unwrap_err().to_string();
        std::env::remove_var("VCS_PROTECT_WINDOWS");
        assert!(e.contains("refusing to check out") && e.contains("Windows"), "{i} {name:?}: {e}");
        assert!(!r.worktree().join(name).exists(), "{name:?} was written");
        assert!(r.worktree().join("keep").exists());
    }
}
