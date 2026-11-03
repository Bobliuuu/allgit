//! Path policy over the wire: a rename is checked on both sides (delete of the old path, add of the
//! new one), nested directories, and a case-folded spelling of a denied directory.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn start(policy: &str) -> (Server, String) {
    let pr = |n: &str, r| Principal {
        name: n.into(),
        role: r,
        zones: None::<BTreeSet<String>>,
    };
    let table = TokenTable::from_entries(vec![
        ("t-alice", pr("alice", Role::Write)),
        ("t-bob", pr("bob", Role::Write)),
        ("t-root", pr("root", Role::Admin)),
    ]);
    let mut cfg = Config::new(table);
    cfg.clock = Arc::new({
        let c = Arc::new(AtomicU64::new(1));
        move || c.load(std::sync::atomic::Ordering::SeqCst)
    });
    cfg.validator.policy = Policy::parse(policy).unwrap();
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}

fn sig() -> Signature {
    Signature {
        name: "a".into(),
        email: "a".into(),
        timestamp: 1,
    }
}

/// Build a commit whose tree holds `files` (`dir/sub/name` paths, content = the same bytes for a path).
fn commit(s: &MemoryStore, parent: Option<ObjectId>, files: &[(&str, &str)]) -> ObjectId {
    fn build(s: &MemoryStore, files: &[(&str, &str)]) -> ObjectId {
        let mut dirs: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
        let mut entries = vec![];
        for (p, c) in files {
            match p.split_once('/') {
                Some((d, rest)) => dirs.entry(d).or_default().push((rest, c)),
                None => entries.push(TreeEntry {
                    name: (*p).into(),
                    mode: EntryMode::File,
                    id: s.put_blob(c.as_bytes()).unwrap(),
                }),
            }
        }
        for (d, sub) in dirs {
            entries.push(TreeEntry {
                name: d.into(),
                mode: EntryMode::Directory,
                id: build(s, &sub),
            });
        }
        s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap()
    }
    let root = build(s, files);
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes([files.len() as u8; 16]),
        author: sig(),
        message: format!("{files:?}"),
    }))
    .unwrap()
}

fn publish(url: &str, tok: &str, who: &str, s: &MemoryStore, old: Option<ObjectId>, new: ObjectId) -> Result<Outcome, PublishError> {
    let t = Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    };
    let b = HttpBackend::new(url, tok).unwrap();
    Publisher::new(&b, s, t).with_principal(who).with_clock(|| 1).run()
}

fn head(url: &str) -> Option<ObjectId> {
    load_manifest(&HttpBackend::new(url, "t-root").unwrap())
        .unwrap()
        .0
        .head("z", "main")
}

#[test]
fn a_rename_is_checked_on_both_sides() {
    // bob may write pub/** only; secrets/** is nobody's but the admin's
    let (_srv, url) = start("allow alice z **\nallow bob z pub/**\nallow bob z README\n");
    let s = MemoryStore::new();
    let base = commit(&s, None, &[("README", "r"), ("pub/a", "1"), ("secrets/k", "2")]);
    publish(&url, "t-alice", "alice", &s, None, base).unwrap();

    // bob renames secrets/k -> pub/k: the add side is allowed, the delete side is not
    let out = commit(&s, Some(base), &[("README", "r"), ("pub/a", "1"), ("pub/k", "2")]);
    let e = publish(&url, "t-bob", "bob", &s, Some(base), out).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("secrets/k"), "delete side must be checked: {e}");
    assert_eq!(head(&url), Some(base));

    // bob renames pub/a -> secrets/a: the delete side is allowed, the add side is not
    let into = commit(&s, Some(base), &[("README", "r"), ("secrets/a", "1"), ("secrets/k", "2")]);
    let e = publish(&url, "t-bob", "bob", &s, Some(base), into).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("secrets/a"), "add side must be checked: {e}");
    assert_eq!(head(&url), Some(base));

    // a rename inside bob's area is fine
    let ok = commit(&s, Some(base), &[("README", "r"), ("pub/b", "1"), ("secrets/k", "2")]);
    publish(&url, "t-bob", "bob", &s, Some(base), ok).unwrap();
    assert_eq!(head(&url), Some(ok));

    // deny rules apply to a rename's delete side too, even where allow covers everything
    let (_srv2, url2) = start("allow * z **\ndeny bob z secrets/**\n");
    let s2 = MemoryStore::new();
    let base2 = commit(&s2, None, &[("secrets/k", "2")]);
    publish(&url2, "t-alice", "alice", &s2, None, base2).unwrap();
    let moved = commit(&s2, Some(base2), &[("open/k", "2")]);
    let e = publish(&url2, "t-bob", "bob", &s2, Some(base2), moved).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("denied by policy"), "{e}");
    // an admin may
    publish(&url2, "t-root", "root", &s2, Some(base2), moved).unwrap();
}

#[test]
fn a_case_folded_spelling_of_a_denied_directory_is_denied() {
    let (_srv, url) = start("allow * z **\ndeny bob z secrets/**\n");
    let s = MemoryStore::new();
    let base = commit(&s, None, &[("README", "r")]);
    publish(&url, "t-alice", "alice", &s, None, base).unwrap();
    for dir in ["Secrets", "SECRETS", "secrets."] {
        let c = commit(&s, Some(base), &[("README", "r"), (&format!("{dir}/k"), "x")]);
        let e = publish(&url, "t-bob", "bob", &s, Some(base), c).unwrap_err().to_string();
        assert!(e.contains("403") && e.contains("denied by policy"), "{dir}: {e}");
    }
    assert_eq!(head(&url), Some(base));
    let ok = commit(&s, Some(base), &[("README", "r"), ("docs/k", "x")]);
    publish(&url, "t-bob", "bob", &s, Some(base), ok).unwrap();
}
