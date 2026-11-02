//! Store requests of a one-file push through the validating server (client -> server -> store).
//! Counts and asserts the requests the server's backing store sees; run with `--nocapture` for
//! the per-request list. EMULATED store (in memory, no latency): request counts only.
use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;
use vcs_objects::*;
use vcs_remote::timeline::TimelineBackend;
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

fn start(store: Arc<dyn Backend>) -> (Server, String) {
    let table = TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    cfg.clock = Arc::new({
        let c = Arc::new(AtomicU64::new(1));
        move || c.load(std::sync::atomic::Ordering::SeqCst)
    });
    let srv = Server::start(store, cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}

/// Commit whose root holds `files` blobs `f0..`, file `changed` carrying `n`.
fn commit(s: &MemoryStore, parent: Option<ObjectId>, files: usize, n: u8) -> ObjectId {
    let mut ents = Vec::new();
    for i in 0..files {
        let body = if i == 0 { vec![n] } else { vec![i as u8, 7] };
        ents.push(TreeEntry {
            name: format!("f{i}"),
            mode: EntryMode::File,
            id: s.put_blob(&body).unwrap(),
        });
    }
    let t = s.put(&Object::Tree(Tree::new(ents).unwrap())).unwrap();
    let c = Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([n; 16]),
        author: sig(),
        message: format!("c{n}\n"),
    };
    s.put(&Object::Commit(c)).unwrap()
}

fn tx(old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: "publish".into(),
        lock_ops: vec![],
    }
}

fn publish(b: &dyn Backend, s: &MemoryStore, t: Transaction) -> Result<Outcome, PublishError> {
    Publisher::new(b, s, t).with_principal("alice").with_clock(|| 1).run()
}

/// Client -> server -> store: the store sees the manifest read, the pack, its index, the tx record
/// and the CAS, nothing else (no existence probe, no second manifest read, no pack listing or
/// object read to validate the new commit).
#[test]
fn one_file_push_store_requests() {
    let mem: Arc<dyn Backend> = Arc::new(MemBackend::new());
    let tl = Arc::new(TimelineBackend::new(mem, Duration::ZERO));
    let (_srv, url) = start(tl.clone());
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 50, 1);
    let b = HttpBackend::new(&url, "t-alice").unwrap();
    publish(&b, &s, tx(None, c1)).unwrap();
    let c2 = commit(&s, Some(c1), 50, 2);
    // what `vcs publish` does: one conditional manifest read, the publish reuses it
    let b = HttpBackend::new(&url, "t-alice").unwrap();
    let cb = CountingBackend::new(&b);
    let (m, tag) = load_manifest(&cb).unwrap();
    tl.clear();
    Publisher::new(&cb, &s, tx(Some(c1), c2))
        .with_principal("alice")
        .with_clock(|| 1)
        .with_manifest_hint(m, tag)
        .run()
        .unwrap();
    let ops: Vec<String> = tl
        .events()
        .iter()
        .map(|e| format!("{} {}", e.op, e.key.split('/').next().unwrap()))
        .collect();
    println!("store requests: {} {ops:?}", tl.requests());
    assert_eq!(tl.requests(), 4, "{ops:?}"); // pack, idx, tx, CAS (the manifest read came before)
    assert!(ops.iter().all(|o| o.starts_with("PUT")), "{ops:?}");
}

/// A second server on the same store (another replica) moves the manifest; the first replica still
/// holds the old version in its cache. A write naming the old tag is refused (412 -> Diverged),
/// the cache cannot make a stale validation commit.
#[test]
fn stale_manifest_cache_cannot_commit() {
    let store: Arc<dyn Backend> = Arc::new(MemBackend::new());
    let (_a, url_a) = start(store.clone());
    let (_b, url_b) = start(store.clone());
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 5, 1);
    let ha = HttpBackend::new(&url_a, "t-alice").unwrap();
    let hb = HttpBackend::new(&url_b, "t-alice").unwrap();
    publish(&ha, &s, tx(None, c1)).unwrap();
    let c2 = commit(&s, Some(c1), 5, 2);
    let c3 = commit(&s, Some(c1), 5, 3);
    // replica A learns the manifest (cache = version after c1) ...
    let (m_old, tag_old) = load_manifest(&ha).unwrap();
    // ... replica B moves main to c3
    publish(&hb, &s, tx(Some(c1), c3)).unwrap();
    // a publisher that read the OLD manifest through A and writes through A: its CAS carries the old tag
    let e = Publisher::new(&ha, &s, tx(Some(c1), c2))
        .with_principal("alice")
        .with_clock(|| 1)
        .with_manifest_hint(m_old, tag_old)
        .run();
    // it retries from a fresh manifest, sees main at c3 != c1 and reports divergence; c2 never lands
    assert!(matches!(e, Err(PublishError::Diverged { .. })), "{e:?}");
    assert_eq!(load_manifest(&ha).unwrap().0.head("z", "main"), Some(c3));
}
