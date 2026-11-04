//! Per-branch ticket queue: writers that take a turn before pushing to one branch never see `Diverged`.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier};
use std::time::Duration;
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "a".into(),
        email: "a".into(),
        timestamp: 1,
    }
}
fn commit(s: &MemoryStore, parent: Option<ObjectId>, i: usize) -> ObjectId {
    let blob = s.put_blob(format!("c{i}").as_bytes()).unwrap();
    let t = s
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
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([i as u8; 16]),
        author: sig(),
        message: format!("c{i}"),
    }))
    .unwrap()
}
fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "feature/x".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

#[test]
fn ticketed_writers_on_one_branch_never_diverge() {
    let table = TokenTable::from_entries(vec![(
        "t-a",
        Principal {
            name: "a".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    cfg.clock = Arc::new({
        let c = Arc::new(AtomicU64::new(1));
        move || c.load(std::sync::atomic::Ordering::SeqCst)
    });
    cfg.max_connections = 1024;
    cfg.ticket_wait = Duration::from_millis(300); // exercises the "ask again" path too
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    let n = 24usize;
    let bar = Barrier::new(n);
    let heads: Vec<ObjectId> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..n)
            .map(|w| {
                let (url, bar) = (&url, &bar);
                sc.spawn(move || {
                    let b = HttpBackend::new(url, "t-a").unwrap();
                    let store = MemoryStore::new();
                    bar.wait();
                    let t = b.ticket_acquire("z", "feature/x", Duration::from_secs(60)).unwrap();
                    let head = load_manifest(&b).unwrap().0.head("z", "feature/x");
                    let new = commit(&store, head, w);
                    let out = Publisher::new(&b, &store, tx(head, new)).with_principal("a").with_clock(|| 1).run();
                    b.ticket_release("z", "feature/x", t).unwrap();
                    assert!(matches!(out, Ok(Outcome::Applied { .. })), "writer {w}: {out:?}");
                    new
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // a chain: every writer's commit is the parent of the next one's, so no push was lost or reordered
    let b = HttpBackend::new(&url, "t-a").unwrap();
    let m = load_manifest(&b).unwrap().0;
    assert!(heads.contains(&m.head("z", "feature/x").unwrap()));
    assert_eq!(m.generation as usize, n);
}
