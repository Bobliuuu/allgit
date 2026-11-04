//! `vcs-server` over a sharded repository: disjoint writers all land, one-branch contention still
//! has exactly one winner, shard/decision records are not client-addressable, and a server started
//! on a repository that was migrated while it was running (marker appears under a live server) keeps working.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier};
use vcs_objects::*;
use vcs_remote::sharded::{migrate_to_sharded, ShardedBackend};
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
fn cfg() -> Config {
    let table = TokenTable::from_entries(vec![
        (
            "t-a",
            Principal {
                name: "a".into(),
                role: Role::Write,
                zones: None::<BTreeSet<String>>,
            },
        ),
        (
            "t-admin",
            Principal {
                name: "root".into(),
                role: Role::Admin,
                zones: None::<BTreeSet<String>>,
            },
        ),
    ]);
    let mut cfg = Config::new(table);
    cfg.clock = Arc::new({
        let c = Arc::new(AtomicU64::new(1));
        move || c.load(std::sync::atomic::Ordering::SeqCst)
    });
    cfg.max_connections = 1024;
    cfg
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
fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

#[test]
fn sharded_server_disjoint_writers_conflicts_and_isolation() {
    let mem = Arc::new(MemBackend::new().with_gc());
    // a single-manifest repository with a few zones, served by a server that is already running
    let srv = Server::start(Arc::new(ShardedBackend::from_arc(mem.clone())), cfg(), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    let seed = MemoryStore::new();
    let b0 = HttpBackend::new(&url, "t-a").unwrap();
    let mut base = vec![];
    for z in 0..4 {
        let zone = format!("w{z:02}");
        let c = commit(&seed, &zone, None, 0);
        Publisher::new(&b0, &seed, tx(&zone, None, c))
            .with_principal("a")
            .with_clock(|| 1)
            .run()
            .unwrap();
        base.push(c);
    }
    // migrate UNDER the live server (the server's wrapper notices the marker on its next manifest read)
    let rep = migrate_to_sharded(&*mem).unwrap();
    assert_eq!(rep.zones, 4);

    let (writers, each) = (16usize, 4usize);
    let bar = Barrier::new(writers);
    let heads: Vec<ObjectId> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..writers)
            .map(|w| {
                let (url, bar, base) = (&url, &bar, &base);
                sc.spawn(move || {
                    let b = HttpBackend::new(url, "t-a").unwrap();
                    let store = MemoryStore::new();
                    let zone = format!("w{w:02}");
                    let mut head = base.get(w).copied();
                    bar.wait();
                    for i in 1..=each {
                        let new = commit(&store, &zone, head, i);
                        let out = Publisher::new(&b, &store, tx(&zone, head, new))
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
    let m = load_manifest(&HttpBackend::new(&url, "t-a").unwrap()).unwrap().0;
    for (w, h) in heads.iter().enumerate() {
        assert_eq!(m.head(&format!("w{w:02}"), "main"), Some(*h), "writer {w} lost an update");
    }
    // one branch, many writers: exactly one wins
    let hot = commit(&seed, "hot", None, 0);
    Publisher::new(&b0, &seed, tx("hot", None, hot))
        .with_principal("a")
        .with_clock(|| 1)
        .run()
        .unwrap();
    let n = 8usize;
    let bar = Barrier::new(n);
    let wins: usize = std::thread::scope(|sc| {
        (0..n)
            .map(|w| {
                let (url, bar) = (&url, &bar);
                sc.spawn(move || {
                    let b = HttpBackend::new(url, "t-a").unwrap();
                    let store = MemoryStore::new();
                    let new = commit(&store, "hot", Some(hot), w + 1);
                    bar.wait();
                    matches!(
                        Publisher::new(&b, &store, tx("hot", Some(hot), new))
                            .with_principal("a")
                            .with_clock(|| 1)
                            .run(),
                        Ok(Outcome::Applied { .. })
                    ) as usize
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum()
    });
    assert_eq!(wins, 1);
    // internal records are not client-addressable
    assert!(b0.get("shard/~root").is_err() || b0.get("shard/~root").unwrap().is_none());
}
