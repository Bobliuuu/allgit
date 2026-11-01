use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::backend::conformance;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn p(name: &str, role: Role, zones: Option<&[&str]>) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: zones.map(|z| z.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()),
    }
}

fn table() -> TokenTable {
    TokenTable::from_entries(vec![
        ("t-reader", p("rita", Role::Read, None)),
        ("t-alice", p("alice", Role::Write, Some(&["z", "other"]))),
        ("t-bob", p("bob", Role::Write, Some(&["z"]))),
        ("t-carol", p("carol", Role::Write, Some(&["other"]))),
        ("t-admin", p("root", Role::Admin, None)),
    ])
}

fn start(clock: u64) -> (Server, Arc<MemBackend>, String) {
    let mem = Arc::new(MemBackend::new());
    let mut cfg = Config::new(table());
    cfg.clock = Arc::new(move || clock);
    let srv = Server::start(mem.clone(), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, mem, url)
}

fn client(url: &str, tok: &str) -> HttpBackend {
    HttpBackend::new(url, tok).unwrap()
}

#[test]
fn conformance_over_http_mem_and_fs() {
    let (_s, _m, url) = start(1);
    // conformance includes delete, which needs an admin token
    conformance(&client(&url, "t-admin"));
    let d = tempfile::tempdir().unwrap();
    let fs = Arc::new(FsBackend::open(d.path().join("r")).unwrap());
    let srv = Server::start(fs, Config::new(table()), "127.0.0.1:0").unwrap();
    conformance(&client(&format!("http://{}", srv.local_addr()), "t-admin"));
}

#[test]
fn delete_needs_admin_and_never_deletes_the_manifest() {
    let (_s, mem, url) = start(1);
    mem.put_if_absent("objects/aa/bb", b"x").unwrap();
    mem.put_if_absent(manifest::KEY, b"m").unwrap();
    for tok in ["t-reader", "t-alice"] {
        let e = client(&url, tok).delete("objects/aa/bb").unwrap_err().to_string();
        assert!(e.contains("403"), "{tok}: {e}");
    }
    assert!(mem.exists("objects/aa/bb").unwrap());
    let e = client(&url, "t-admin").delete(manifest::KEY).unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
    assert!(mem.exists(manifest::KEY).unwrap());
    client(&url, "t-admin").delete("objects/aa/bb").unwrap();
    assert!(!mem.exists("objects/aa/bb").unwrap());
    assert!(!client(&url, "t-admin").supports_gc());
}

#[test]
fn auth_failures_and_whoami() {
    let (_s, _m, url) = start(1);
    for tok in ["", "nope"] {
        let e = client(&url, tok).get("k/one").unwrap_err().to_string();
        assert!(e.contains("401"), "{e}");
    }
    let r = ureq::get(&format!("{url}/v1/whoami")).call();
    assert!(matches!(r, Err(ureq::Error::Status(401, _))));
    assert_eq!(ureq::get(&format!("{url}/healthz")).call().unwrap().status(), 200);
    assert_eq!(client(&url, "t-bob").whoami().unwrap(), "bob write z");
    assert_eq!(client(&url, "t-admin").whoami().unwrap(), "root admin *");
}

#[test]
fn roles_read_cannot_write() {
    let (_s, _m, url) = start(1);
    client(&url, "t-alice").put_if_absent("k/one", b"x").unwrap();
    let r = client(&url, "t-reader");
    assert_eq!(r.get("k/one").unwrap().unwrap().data, b"x");
    assert!(r.exists("k/one").unwrap());
    assert_eq!(r.list("k/").unwrap(), vec!["k/one"]);
    assert!(r.put_if_absent("k/two", b"y").unwrap_err().to_string().contains("403"));
    let t = r.get("k/one").unwrap().unwrap().tag;
    assert!(r.put_if_match("k/one", b"y", &t).unwrap_err().to_string().contains("403"));
    assert_eq!(r.get("k/two").unwrap(), None);
}

#[test]
fn unconditional_put_and_bad_keys_rejected() {
    let (_s, _m, url) = start(1);
    let r = ureq::put(&format!("{url}/v1/o/k/one"))
        .set("Authorization", "Bearer t-alice")
        .send_bytes(b"x");
    assert!(matches!(r, Err(ureq::Error::Status(428, _))));
    let r = ureq::get(&format!("{url}/v1/o/k/%5Cx"))
        .set("Authorization", "Bearer t-alice")
        .call();
    assert!(matches!(r, Err(ureq::Error::Status(400, _))));
}

#[test]
fn token_file_parsing() {
    let h = vcs_server::auth::hash_token("s3cret");
    let t = TokenTable::parse(&format!("# c\nann write a,b {h}\n")).unwrap();
    let who = t.authenticate("s3cret").unwrap();
    assert!(who.may_write_zone("a") && !who.may_write_zone("c"));
    assert!(t.authenticate("other").is_none());
    assert!(TokenTable::parse("x write * short").is_err());
    assert!(TokenTable::parse(&format!("x boss * {h}")).is_err());
}

fn sig(who: &str) -> Signature {
    Signature {
        name: who.into(),
        email: who.into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, files: &[(&str, &str)]) -> ObjectId {
    let entries = files
        .iter()
        .map(|(n, c)| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: s.put_blob(c.as_bytes()).unwrap(),
        })
        .collect();
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([files.len() as u8; 16]),
        author: sig("c"),
        message: format!("{files:?}"),
    }))
    .unwrap()
}

fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId, lock_ops: Vec<LockOp>, who: &str) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(who),
        message: "m".into(),
        lock_ops,
    }
}

fn publish(b: &dyn Backend, s: &MemoryStore, t: Transaction, principal: &str, now: u64) -> Result<Outcome, PublishError> {
    Publisher::new(b, s, t).with_principal(principal).with_clock(move || now).run()
}

fn put_closure(b: &dyn Backend, s: &MemoryStore, id: ObjectId) {
    let o = s.get(&id).unwrap();
    let deps: Vec<ObjectId> = match &o {
        Object::Tree(t) => t.entries().iter().map(|e| e.id).collect(),
        Object::Commit(c) => c.parents.iter().copied().chain([c.root]).collect(),
        _ => vec![],
    };
    for d in deps {
        put_closure(b, s, d);
    }
    b.put_if_absent(&object_key(&id), &o.encode()).unwrap();
}

fn lock_op(path: &str, lease: u64) -> LockOp {
    LockOp::Acquire {
        zone: "z".into(),
        path: path.into(),
        lease_ms: lease,
        reason: String::new(),
    }
}

#[test]
fn publish_applies_over_http_and_zone_policy_is_enforced() {
    let (_s, _m, url) = start(1_000);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0"), ("b.txt", "0")]);
    let alice = client(&url, "t-alice");
    let out = publish(&alice, &s, tx("z", None, c1, vec![], "alice"), "alice", 1_000).unwrap();
    assert!(matches!(out, Outcome::Applied { .. }));
    let out = publish(&alice, &s, tx("z", None, c1, vec![], "alice"), "alice", 1_000).unwrap();
    assert!(matches!(out, Outcome::AlreadyApplied));
    // carol may only write zone "other"
    let c2 = commit(&s, "z", Some(c1), &[("a.bin", "1"), ("b.txt", "0")]);
    let e = publish(&client(&url, "t-carol"), &s, tx("z", Some(c1), c2, vec![], "carol"), "carol", 1_000).unwrap_err();
    assert!(e.to_string().contains("403"), "{e}");
    let (m, _) = load_manifest(&client(&url, "t-reader")).unwrap();
    assert_eq!(m.head("z", "main"), Some(c1));
}

#[test]
fn lock_violation_is_rejected_server_side() {
    let (_s, _m, url) = start(1_000);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0"), ("b.txt", "0")]);
    let alice = client(&url, "t-alice");
    let bob = client(&url, "t-bob");
    publish(&alice, &s, tx("z", None, c1, vec![], "alice"), "alice", 1_000).unwrap();
    let c1b = commit(&s, "z", Some(c1), &[("a.bin", "0"), ("b.txt", "0"), ("c.txt", "0")]);
    publish(
        &alice,
        &s,
        tx("z", Some(c1), c1b, vec![lock_op("a.bin", 600_000)], "alice"),
        "alice",
        1_000,
    )
    .unwrap();
    let (m, tag) = load_manifest(&bob).unwrap();
    assert_eq!(m.locks.len(), 1);
    let tag = tag.unwrap();

    // An honest bob is stopped by the client-side check already.
    let c2 = commit(&s, "z", Some(c1b), &[("a.bin", "BOB"), ("b.txt", "0"), ("c.txt", "0")]);
    let e = publish(&bob, &s, tx("z", Some(c1b), c2, vec![], "bob"), "bob", 1_000).unwrap_err();
    assert!(matches!(e, PublishError::LockedBy { .. }), "{e}");

    // A malicious bob skips it and CASes the manifest directly: the server rejects.
    put_closure(&bob, &s, c2);
    let mut evil = m.clone();
    evil.generation += 1;
    evil.branches.insert(("z".into(), "main".into()), c2);
    let e = bob.put_if_match(manifest::KEY, &evil.encode(), &tag).unwrap_err().to_string();
    assert!(
        e.contains("409") && e.contains("LockedBy") && e.contains("a.bin") && e.contains("alice"),
        "{e}"
    );

    // Claiming to be the holder is caught too (principal comes from the token): the client
    // builds a lock/holder-consistent manifest as "alice" but authenticates as bob.
    let token = m.locks[&("z".into(), "a.bin".into())].token;
    let present = LockOp::Present {
        zone: "z".into(),
        path: "a.bin".into(),
        token,
    };
    let e = publish(&bob, &s, tx("z", Some(c1b), c2, vec![present], "alice"), "alice", 1_000)
        .unwrap_err()
        .to_string();
    assert!(e.contains("409") && e.contains("LockedBy"), "{e}");

    // Dropping the lock record is forbidden for bob, allowed for an admin.
    let mut nolock = m.clone();
    nolock.generation += 1;
    nolock.locks.clear();
    let e = bob.put_if_match(manifest::KEY, &nolock.encode(), &tag).unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
    client(&url, "t-admin")
        .put_if_match(manifest::KEY, &nolock.encode(), &tag)
        .unwrap()
        .expect("admin may break");
    assert!(load_manifest(&alice).unwrap().0.locks.is_empty());
}

#[test]
fn holder_may_publish_locked_path_others_may_not() {
    let (_s, _m, url) = start(1_000);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0")]);
    let alice = client(&url, "t-alice");
    let bob = client(&url, "t-bob");
    publish(
        &alice,
        &s,
        tx("z", None, c1, vec![lock_op("a.bin", 5_000)], "alice"),
        "alice",
        1_000,
    )
    .unwrap();
    let (m, _) = load_manifest(&alice).unwrap();
    let token = m.locks[&("z".into(), "a.bin".into())].token;
    let c2 = commit(&s, "z", Some(c1), &[("a.bin", "alice-edit")]);
    let present = LockOp::Present {
        zone: "z".into(),
        path: "a.bin".into(),
        token,
    };
    publish(&alice, &s, tx("z", Some(c1), c2, vec![present], "alice"), "alice", 1_500).unwrap();
    let c3 = commit(&s, "z", Some(c2), &[("a.bin", "bob")]);
    let e = publish(&bob, &s, tx("z", Some(c2), c3, vec![], "bob"), "bob", 1_600).unwrap_err();
    assert!(matches!(e, PublishError::LockedBy { .. }), "{e}");
}
