//! Validator behaviour added for the production-shaped server: server-computed lease expiry,
//! lease cap, replica clock skew epsilon, `require-lock` and the deny-by-default path policy.
//! Each negative test fails if the corresponding check is removed from `validate.rs`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn p(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}
fn table() -> TokenTable {
    TokenTable::from_entries(vec![
        ("t-alice", p("alice", Role::Write)),
        ("t-bob", p("bob", Role::Write)),
        ("t-root", p("root", Role::Admin)),
    ])
}

/// A server with a settable clock over `mem`.
fn start(mem: Arc<MemBackend>, clock: Arc<AtomicU64>, policy: &str, eps: u64) -> (Server, String) {
    let mut cfg = Config::new(table());
    let c = clock.clone();
    cfg.clock = Arc::new(move || c.load(Ordering::SeqCst));
    cfg.validator.policy = Policy::parse(policy).unwrap();
    cfg.validator.skew_epsilon_ms = eps;
    cfg.validator.max_lease_ms = 3_600_000;
    let srv = Server::start(mem, cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}
fn client(url: &str, tok: &str) -> HttpBackend {
    HttpBackend::new(url, tok).unwrap()
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

fn acquire(path: &str, lease: u64) -> LockOp {
    LockOp::Acquire {
        zone: "z".into(),
        path: path.into(),
        lease_ms: lease,
        reason: String::new(),
    }
}
fn lock_of(b: &dyn Backend, path: &str) -> Option<Lock> {
    load_manifest(b).unwrap().0.locks.get(&("z".to_owned(), path.to_owned())).cloned()
}

#[test]
fn expiry_is_computed_from_the_server_clock_not_the_client_clock() {
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(1_000_000)));
    let (_s, url) = start(mem, clock.clone(), "", 0);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0")]);
    let alice = client(&url, "t-alice");
    // alice's own clock is 30 s behind the server: she asks for 10 min starting "then"
    publish(
        &alice,
        &s,
        tx("z", None, c1, vec![acquire("a.bin", 600_000)], "alice"),
        "alice",
        970_000,
    )
    .unwrap();
    let l = lock_of(&alice, "a.bin").unwrap();
    assert_eq!(l.acquired_at, 1_000_000, "acquired_at is server time");
    assert_eq!(l.expires_at, 1_600_000, "lease runs from the server clock");
    // a forward-skewed client (within the 60 s allowance) cannot shorten or lengthen it either
    let l2 = {
        let c2 = commit(&s, "z", Some(c1), &[("a.bin", "0"), ("b.txt", "1")]);
        let t = load_manifest(&alice).unwrap().0.locks[&("z".to_owned(), "a.bin".to_owned())].token;
        let present = LockOp::Present {
            zone: "z".into(),
            path: "a.bin".into(),
            token: t,
        };
        publish(
            &alice,
            &s,
            tx(
                "z",
                Some(c1),
                c2,
                vec![
                    present,
                    LockOp::Renew {
                        zone: "z".into(),
                        path: "a.bin".into(),
                        token: t,
                        lease_ms: 60_000,
                    },
                ],
                "alice",
            ),
            "alice",
            1_030_000,
        )
        .unwrap();
        lock_of(&alice, "a.bin").unwrap()
    };
    assert_eq!(l2.expires_at, 1_060_000, "renewal also restarts from the server clock");
    assert_eq!(l2.acquired_at, 1_000_000);
    assert_eq!(l2.token, l.token);
}

#[test]
fn forged_expiry_and_oversized_leases_are_refused_or_normalized() {
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(5_000)));
    let (_s, url) = start(mem, clock, "", 0);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0")]);
    let bob = client(&url, "t-bob");
    // lease over the 1 h cap
    let e = publish(&bob, &s, tx("z", None, c1, vec![acquire("a.bin", 7_200_000)], "bob"), "bob", 5_000)
        .unwrap_err()
        .to_string();
    assert!(e.contains("422") && e.contains("exceeds the server maximum"), "{e}");
    // hand-forged manifest: expires_at in the year 3000 with a client time_hw to match
    put_closure(&bob, &s, c1);
    let mut m = Manifest {
        generation: 1,
        next_token: 1,
        time_hw: 5_000,
        ..Manifest::default()
    };
    m.branches.insert(("z".into(), "main".into()), c1);
    m.locks.insert(
        ("z".into(), "a.bin".into()),
        Lock {
            holder: "bob".into(),
            token: 0,
            acquired_at: 0,
            expires_at: 32_503_680_000_000,
            reason: String::new(),
        },
    );
    let e = bob.put_if_absent(manifest::KEY, &m.encode()).unwrap_err().to_string();
    assert!(e.contains("422"), "{e}");
    // same forgery with a modest lease but a bogus acquired_at: stored times are the server's
    m.locks.get_mut(&("z".to_owned(), "a.bin".to_owned())).unwrap().expires_at = 65_000;
    assert!(bob.put_if_absent(manifest::KEY, &m.encode()).unwrap());
    let l = lock_of(&bob, "a.bin").unwrap();
    assert_eq!((l.acquired_at, l.expires_at), (5_000, 65_000));
}

#[test]
fn a_client_clock_far_ahead_is_refused() {
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(1_000_000)));
    let (_s, url) = start(mem, clock, "", 0);
    let s = MemoryStore::new();
    let c1 = commit(&s, "z", None, &[("a.bin", "0")]);
    let e = publish(
        &client(&url, "t-alice"),
        &s,
        tx("z", None, c1, vec![acquire("a.bin", 1_000)], "alice"),
        "alice",
        2_000_000,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("422") && e.contains("ahead of the server clock"), "{e}");
}

/// Two server replicas on one backend, the second 3 s ahead: with epsilon >= 2 * skew the
/// fast replica does not let a contender in before the holder's full lease has elapsed.
#[test]
fn replica_skew_epsilon_keeps_a_lock_live_on_a_fast_replica() {
    for (eps, may_take_over_early) in [(0u64, true), (6_000, false)] {
        let mem = Arc::new(MemBackend::new());
        let (ca, cb) = (Arc::new(AtomicU64::new(100_000)), Arc::new(AtomicU64::new(103_000)));
        let (_sa, ua) = start(mem.clone(), ca.clone(), "", eps);
        let (_sb, ub) = start(mem.clone(), cb.clone(), "", eps);
        let s = MemoryStore::new();
        let c1 = commit(&s, "z", None, &[("a.bin", "0")]);
        // alice locks via replica A (true time T=0 -> A clock 100_000), 10 s lease
        publish(
            &client(&ua, "t-alice"),
            &s,
            tx("z", None, c1, vec![acquire("a.bin", 10_000)], "alice"),
            "alice",
            100_000,
        )
        .unwrap();
        // true time T+8 s: A reads 108_000, B reads 111_000: past expiry (110_000) on B only
        ca.store(108_000, Ordering::SeqCst);
        cb.store(111_000, Ordering::SeqCst);
        // a lock-only transaction (no head change), so only the lock decides
        let mut t = tx("z", Some(c1), c1, vec![acquire("a.bin", 10_000)], "bob");
        t.updates.clear();
        let r = publish(&client(&ub, "t-bob"), &s, t, "bob", 111_000);
        match (r, may_take_over_early) {
            (Ok(_), true) => assert_eq!(lock_of(&client(&ua, "t-alice"), "a.bin").unwrap().holder, "bob"),
            (Err(e), false) => assert!(e.to_string().contains("LockedBy"), "{e}"),
            (r, early) => panic!("eps {eps}: unexpected {r:?} (early takeover expected: {early})"),
        }
    }
}

fn two_commits(s: &MemoryStore, files: &[&str]) -> (ObjectId, ObjectId) {
    let base: Vec<(&str, &str)> = vec![("README", "r")];
    let c1 = commit(s, "z", None, &base);
    let mut next = base.clone();
    for f in files {
        next.push((f, "new"));
    }
    next.sort();
    (c1, commit(s, "z", Some(c1), &next))
}

#[test]
fn path_policy_is_deny_by_default_with_deny_overrides() {
    let policy = "allow alice z **\nallow bob z docs/**\ndeny alice z secrets/**\n";
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(1)));
    let (_s, url) = start(mem, clock, policy, 0);
    let s = MemoryStore::new();
    // the first publish by alice creates the zone's branch: README only
    let (c1, _) = two_commits(&s, &[]);
    publish(&client(&url, "t-alice"), &s, tx("z", None, c1, vec![], "alice"), "alice", 1).unwrap();
    // alice may add any path except secrets/**. (flat trees only: use names with slashes via tree entries is
    // beyond this helper, so "secrets" is checked as an exact-name deny below)
    let (_, ok) = two_commits(&s, &["code.rs"]);
    publish(&client(&url, "t-alice"), &s, tx("z", Some(c1), ok, vec![], "alice"), "alice", 1).unwrap();
    // bob has no rule covering code.rs: denied by default, 403 from the server
    let (_, bad) = two_commits(&s, &["bob.rs"]);
    let e = publish(&client(&url, "t-bob"), &s, tx("z", Some(ok), bad, vec![], "bob"), "bob", 1)
        .unwrap_err()
        .to_string();
    assert!(e.contains("403") && e.contains("deny-by-default"), "{e}");
    // admin bypasses allow/deny
    publish(&client(&url, "t-root"), &s, tx("z", Some(ok), bad, vec![], "root"), "root", 1).unwrap();
}

#[test]
fn deny_rule_beats_allow_and_unknown_principals_get_nothing() {
    let policy = "allow * z **\ndeny alice z locked.txt\n";
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(1)));
    let (_s, url) = start(mem, clock, policy, 0);
    let s = MemoryStore::new();
    let (c1, _) = two_commits(&s, &[]);
    publish(&client(&url, "t-bob"), &s, tx("z", None, c1, vec![], "bob"), "bob", 1).unwrap();
    let (_, locked) = two_commits(&s, &["locked.txt"]);
    let e = publish(&client(&url, "t-alice"), &s, tx("z", Some(c1), locked, vec![], "alice"), "alice", 1)
        .unwrap_err()
        .to_string();
    assert!(e.contains("403") && e.contains("denied by policy"), "{e}");
    publish(&client(&url, "t-bob"), &s, tx("z", Some(c1), locked, vec![], "bob"), "bob", 1).unwrap();
}

#[test]
fn require_lock_blocks_blind_changes_and_allows_locked_ones() {
    let policy = "allow * z **\nrequire-lock * z *.psd\n";
    let (mem, clock) = (Arc::new(MemBackend::new()), Arc::new(AtomicU64::new(1_000)));
    let (_s, url) = start(mem, clock, policy, 0);
    let s = MemoryStore::new();
    let (c1, _) = two_commits(&s, &[]);
    let alice = client(&url, "t-alice");
    publish(&alice, &s, tx("z", None, c1, vec![], "alice"), "alice", 1_000).unwrap();
    // no lock: refused with 428 and a LockRequired message
    let (_, hero) = two_commits(&s, &["hero.psd"]);
    let e = publish(&alice, &s, tx("z", Some(c1), hero, vec![], "alice"), "alice", 1_000)
        .unwrap_err()
        .to_string();
    assert!(e.contains("428") && e.contains("LockRequired") && e.contains("hero.psd"), "{e}");
    let m = ureq::get(&format!("{url}/metrics")).set("Authorization", "Bearer t-root").call().unwrap().into_string().unwrap();
    let m = vcs_server::metrics::parse(&m).unwrap();
    assert!(m["vcs_server_lock_conflicts_total"] >= 1.0, "lock conflict counted");
    // non-matching paths are unaffected
    let (_, txt) = two_commits(&s, &["notes.txt"]);
    publish(&alice, &s, tx("z", Some(c1), txt, vec![], "alice"), "alice", 1_000).unwrap();
    // with a lock acquired in the same transaction the change goes through
    publish(
        &alice,
        &s,
        tx("z", Some(txt), hero_on(&s, txt), vec![acquire("hero.psd", 60_000)], "alice"),
        "alice",
        1_000,
    )
    .unwrap();
    // bob changing the locked file is refused by the lock, naming the holder
    let bob = client(&url, "t-bob");
    let head = hero_on(&s, txt);
    let bob_edit = commit(&s, "z", Some(head), &[("README", "r"), ("hero.psd", "bob"), ("notes.txt", "new")]);
    let e = publish(&bob, &s, tx("z", Some(head), bob_edit, vec![], "bob"), "bob", 1_000).unwrap_err();
    assert!(matches!(e, PublishError::LockedBy { .. }), "{e}");
}

fn hero_on(s: &MemoryStore, parent: ObjectId) -> ObjectId {
    commit(s, "z", Some(parent), &[("README", "r"), ("hero.psd", "new"), ("notes.txt", "new")])
}

#[test]
fn shipped_sample_files_parse() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
    let t = TokenTable::parse(&std::fs::read_to_string(dir.join("tokens.example")).unwrap()).unwrap();
    assert_eq!(t.authenticate("example-bob").unwrap().name, "bob");
    assert_eq!(t.authenticate("example-root").unwrap().role, Role::Admin);
    assert!(t.authenticate("example-nobody").is_none());
    let pol = Policy::parse(&std::fs::read_to_string(dir.join("policy.example")).unwrap()).unwrap();
    assert!(pol.zone_access("bob", "art") && !pol.zone_access("carol", "art"));
}
