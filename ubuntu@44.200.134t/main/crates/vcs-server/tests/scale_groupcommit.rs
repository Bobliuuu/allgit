//! Stale manifest writes are reconciled by the server (group commit) without losing an update, and
//! conflicting writes still fail.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier};
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

fn start() -> (Server, Arc<MemBackend>, String) {
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
    let mem = Arc::new(MemBackend::new());
    let srv = Server::start(mem.clone(), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, mem, url)
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, i: usize) -> ObjectId {
    let blob = s.put_blob(format!("{zone}-{i}").as_bytes()).unwrap();
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
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([i as u8; 16]),
        author: sig(),
        message: format!("{zone} {i}"),
    }))
    .unwrap()
}

fn tx(zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

#[test]
fn disjoint_writers_all_apply_and_stale_writes_are_merged() {
    let (srv, mem, url) = start();
    let (writers, each) = (24usize, 5usize);
    let bar = Barrier::new(writers);
    let heads: Vec<ObjectId> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let (url, bar) = (&url, &bar);
                sc.spawn(move || {
                    let b = HttpBackend::new(url, "t-a").unwrap();
                    let store = MemoryStore::new();
                    let zone = format!("w{w:02}");
                    let mut head = None;
                    bar.wait();
                    for i in 0..each {
                        let new = commit(&store, &zone, head, i);
                        let out = Publisher::new(&b, &store, tx(&zone, "main", head, new))
                            .with_principal("a")
                            .with_clock(|| 1)
                            .run()
                            .expect("publish");
                        assert!(matches!(out, Outcome::Applied { .. }), "{out:?}");
                        head = Some(new);
                    }
                    head.unwrap()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let m = Manifest::decode(&mem.get(manifest::KEY).unwrap().unwrap().data).unwrap();
    for (w, h) in heads.iter().enumerate() {
        assert_eq!(m.head(&format!("w{w:02}"), "main"), Some(*h), "writer {w} lost an update");
    }
    assert_eq!(
        m.generation as usize,
        writers * each,
        "generation counts every publish exactly once"
    );
    assert_eq!(m.applied.len(), writers * each);
    let merged = srv.merged_writes();
    println!("merged stale writes: {merged} of {}", writers * each);
    assert!(
        merged > 0 || srv.batch_stats().0 > 0,
        "no stale write was reconciled or batched by the server"
    );
    println!("batches: {:?}", srv.batch_stats());
}

#[test]
fn conflicting_writers_on_one_branch_cannot_both_apply() {
    let (_srv, mem, url) = start();
    let n = 12usize;
    let seed_store = MemoryStore::new();
    let b0 = HttpBackend::new(&url, "t-a").unwrap();
    let base = commit(&seed_store, "z", None, 0);
    Publisher::new(&b0, &seed_store, tx("z", "main", None, base))
        .with_principal("a")
        .with_clock(|| 1)
        .run()
        .unwrap();
    let bar = Barrier::new(n);
    let results: Vec<bool> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..n)
            .map(|w| {
                let (url, bar) = (&url, &bar);
                sc.spawn(move || {
                    let b = HttpBackend::new(url, "t-a").unwrap();
                    let store = MemoryStore::new();
                    let new = commit(&store, "z", Some(base), w + 1);
                    bar.wait();
                    matches!(
                        Publisher::new(&b, &store, tx("z", "main", Some(base), new))
                            .with_principal("a")
                            .with_clock(|| 1)
                            .run(),
                        Ok(Outcome::Applied { .. })
                    )
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        results.iter().filter(|x| **x).count(),
        1,
        "exactly one writer may move main from {base}: {results:?}"
    );
    let m = Manifest::decode(&mem.get(manifest::KEY).unwrap().unwrap().data).unwrap();
    assert_eq!(m.generation, 2);
}
