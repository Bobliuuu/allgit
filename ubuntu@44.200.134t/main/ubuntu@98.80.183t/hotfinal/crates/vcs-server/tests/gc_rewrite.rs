//! #232: remote pack rewriting and lease renewal against the directory backend and the HTTP
//! backend (a server over a directory). Real clocks: tiny grace, short sleeps.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use vcs_objects::*;
use vcs_remote::gc::{remote_gc_with, GcPoint, RemoteGcOptions};
use vcs_remote::protocol::GcQuirks;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};
use vcs_store::pack::{build_pack, PackIndex};
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit_over(s: &MemoryStore, content: &[u8], tag: u8) -> ObjectId {
    let blob = s.put_blob(content).unwrap();
    let root = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents: vec![],
        root,
        change_id: ChangeId::from_bytes([tag; 16]),
        author: sig(),
        message: "m".into(),
    }))
    .unwrap()
}

fn opts() -> RemoteGcOptions {
    RemoteGcOptions {
        grace_ms: 1500,
        hold_ms: 100,
        dry_run: false,
        lock_ttl_ms: 600,
        owner: "sweeper".into(),
        allow_short_grace: true,
        rewrite_packs: true,
    }
}

fn has(b: &dyn Backend, stem: &str, id: &ObjectId) -> bool {
    let idx = b.get(&format!("{stem}.idx")).unwrap().unwrap();
    PackIndex::parse(&idx.data, std::path::Path::new(stem)).unwrap().find(id).is_some()
}

fn scenario(b: &dyn Backend) {
    let s = MemoryStore::new();
    let head = commit_over(&s, b"head", 1);
    let orphan = commit_over(&s, b"orphan", 2);
    let tx = Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old: None,
            new: head,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    };
    Publisher::new(b, &s, tx).run().unwrap();
    // one pack with a live object (a duplicate of the head) and the orphan commit
    let built = build_pack(vec![
        (head, s.get(&head).unwrap().encode()),
        (orphan, s.get(&orphan).unwrap().encode()),
    ]);
    let stem = format!("packs/{}", built.name);
    b.put_if_absent(&format!("{stem}.pack"), &built.pack).unwrap();
    b.put_if_absent(&format!("{stem}.idx"), &built.idx).unwrap();
    std::thread::sleep(Duration::from_millis(1700));

    let dry = remote_gc_with(b, &RemoteGcOptions { dry_run: true, ..opts() }, GcQuirks::default(), &mut |_| {}).unwrap();
    assert_eq!(dry.packs_rewritten.len(), 1, "{dry:?}");
    assert!(has(b, &stem, &orphan));

    // the lease is renewed: stall past the original 600 ms TTL between phases, a rival is still locked out
    let mut blocked = false;
    let mut rewritten = false;
    let mut hook = |p: GcPoint| match p {
        GcPoint::Tombed => std::thread::sleep(Duration::from_millis(400)),
        GcPoint::Remarked => {
            std::thread::sleep(Duration::from_millis(400));
            blocked = matches!(vcs_remote::remote_gc(b, &opts()), Err(vcs_remote::GcError::Locked(_)));
        }
        GcPoint::Rewritten => rewritten = true,
        _ => {}
    };
    let rep = remote_gc_with(b, &opts(), GcQuirks::default(), &mut hook).unwrap();
    assert!(blocked, "lease was not renewed");
    assert!(rewritten);
    let (old, new) = &rep.packs_rewritten[0];
    assert_eq!(old, &stem);
    assert!(!b.exists(&format!("{stem}.idx")).unwrap() && !b.exists(&format!("{stem}.pack")).unwrap());
    assert!(has(b, new, &head) && !has(b, new, &orphan));
    assert!(b.list("tomb/").unwrap().is_empty());
    // the head still fetches completely
    let (m, _) = load_manifest(b).unwrap();
    assert_eq!(m.branches.values().copied().collect::<Vec<_>>(), vec![head]);
    for id in [head] {
        fetch_object(b, &id).unwrap();
    }
}

#[test]
fn rewrite_and_lease_on_a_directory_backend() {
    let d = tempfile::tempdir().unwrap();
    scenario(&FsBackend::open(d.path().join("r")).unwrap());
}

#[test]
fn rewrite_and_lease_over_http() {
    let d = tempfile::tempdir().unwrap();
    let fs = Arc::new(FsBackend::open(d.path().join("r")).unwrap());
    let table = TokenTable::from_entries(vec![(
        "t-admin",
        Principal {
            name: "root".into(),
            role: Role::Admin,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let srv = Server::start(fs, Config::new(table), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    scenario(&HttpBackend::new(&url, "t-admin").unwrap());
}
