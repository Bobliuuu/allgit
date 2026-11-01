//! `GET /v1/audit` (admin only, paginated) over the hash-chained log, and the per-outcome records:
//! accepted, rejected by policy, rejected by lock, rejected for a missing lock, force, branch delete.
//! Every manifest decision carries `reason`, the policy hash and the validator time.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::audit::{verify_file, AuditLog};
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

const POLICY: &str = "allow * z **\ndeny alice z secret-*\nrequire-lock * z *.psd\n";

fn pr(n: &str, r: Role) -> Principal {
    Principal {
        name: n.into(),
        role: r,
        zones: None::<BTreeSet<String>>,
    }
}

fn start() -> (Server, String, std::path::PathBuf, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("audit.jsonl");
    let table = TokenTable::from_entries(vec![
        ("t-alice", pr("alice", Role::Write)),
        ("t-bob", pr("bob", Role::Write)),
        ("t-root", pr("root", Role::Admin)),
    ]);
    let mut cfg = Config::new(table);
    cfg.clock = Arc::new({
        let c = Arc::new(AtomicU64::new(1000));
        move || c.load(std::sync::atomic::Ordering::SeqCst)
    });
    cfg.audit = Some(Arc::new(AuditLog::open(&path, None).unwrap()));
    cfg.validator.policy = Policy::parse(POLICY).unwrap();
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url, path, d)
}

fn sig() -> Signature {
    Signature {
        name: "a".into(),
        email: "a".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, parent: Option<ObjectId>, files: &[&str], tag: u8) -> ObjectId {
    let entries = files
        .iter()
        .map(|n| TreeEntry {
            name: (*n).into(),
            mode: EntryMode::File,
            id: s.put_blob(&[tag]).unwrap(),
        })
        .collect();
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([tag; 16]),
        author: sig(),
        message: format!("c{tag}\n"),
    }))
    .unwrap()
}

fn tx(branch: &str, old: Option<ObjectId>, new: ObjectId, ops: Vec<LockOp>) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: ops,
    }
}

fn run(url: &str, tok: &str, who: &str, s: &MemoryStore, t: Transaction) -> Result<Outcome, PublishError> {
    let b = HttpBackend::new(url, tok).unwrap();
    Publisher::new(&b, s, t).with_principal(who).with_clock(|| 1000).run()
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
    b.put_if_absent(&vcs_remote::protocol::object_key(&id), &o.encode()).unwrap();
}

fn get(url: &str, tok: &str, q: &str) -> Result<(String, Option<String>, Option<String>), u16> {
    match ureq::get(&format!("{url}/v1/audit{q}"))
        .set("Authorization", &format!("Bearer {tok}"))
        .call()
    {
        Ok(r) => {
            let next = r.header("X-Audit-Next").map(str::to_owned);
            let head = r.header("X-Audit-Head").map(str::to_owned);
            Ok((r.into_string().unwrap(), next, head))
        }
        Err(ureq::Error::Status(c, _)) => Err(c),
        Err(e) => panic!("{e}"),
    }
}

fn line_with<'a>(lines: &[&'a str], needle: &str) -> &'a str {
    lines
        .iter()
        .copied()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no {needle} in {lines:#?}"))
}

#[test]
fn audit_endpoint_is_admin_only_paginated_and_records_every_outcome() {
    let (_srv, url, path, _d) = start();
    let s = MemoryStore::new();
    let want_policy = Policy::parse(POLICY).unwrap().hash();

    // accept
    let c1 = commit(&s, None, &["a.txt"], 1);
    run(&url, "t-alice", "alice", &s, tx("main", None, c1, vec![])).unwrap();
    // reject by policy (deny alice z secret-*)
    let bad = commit(&s, Some(c1), &["a.txt", "secret-k"], 2);
    let e = run(&url, "t-alice", "alice", &s, tx("main", Some(c1), bad, vec![]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("403"), "{e}");
    // reject for missing lock (require-lock *.psd)
    let psd = commit(&s, Some(c1), &["a.txt", "h.psd"], 3);
    let e = run(&url, "t-alice", "alice", &s, tx("main", Some(c1), psd, vec![]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("428"), "{e}");
    // alice locks a.txt; bob's change to it is rejected by the lock
    let acq = LockOp::Acquire {
        zone: "z".into(),
        path: "a.txt".into(),
        lease_ms: 60_000,
        reason: String::new(),
    };
    let mut lt = tx("main", Some(c1), c1, vec![acq]);
    lt.updates.clear();
    run(&url, "t-alice", "alice", &s, lt).unwrap();
    // the stock client refuses client-side; a raw manifest write must be refused by the server
    let edit = commit(&s, Some(c1), &["a.txt"], 4);
    let bob = HttpBackend::new(&url, "t-bob").unwrap();
    put_closure(&bob, &s, edit);
    let (mut m, tag) = load_manifest(&bob).unwrap();
    m.branches.insert(("z".into(), "main".into()), edit);
    let e = bob
        .put_if_match(vcs_remote::manifest::KEY, &m.encode(), &tag.unwrap())
        .unwrap_err()
        .to_string();
    assert!(e.contains("409") && e.contains("LockedBy"), "{e}");
    // force: topic moves to a commit that does not descend from the old head; then delete it
    let t1 = commit(&s, None, &["t1"], 5);
    let t2 = commit(&s, None, &["t2"], 6);
    run(&url, "t-root", "root", &s, tx("topic", None, t1, vec![])).unwrap();
    run(&url, "t-root", "root", &s, tx("topic", Some(t1), t2, vec![])).unwrap();
    let b = HttpBackend::new(&url, "t-root").unwrap();
    Publisher::new(
        &b,
        &s,
        Transaction {
            updates: vec![],
            author: sig(),
            message: "d".into(),
            lock_ops: vec![],
        },
    )
    .with_principal("root")
    .with_clock(|| 1000)
    .with_deletes(vec![("z".into(), "topic".into(), t2)])
    .run()
    .unwrap();

    // role gate
    assert_eq!(get(&url, "t-alice", "").unwrap_err(), 403);
    assert_eq!(get(&url, "nope", "").unwrap_err(), 401);
    assert_eq!(get(&url, "t-root", "?limit=0").unwrap_err(), 400);
    assert_eq!(get(&url, "t-root", "?after=x").unwrap_err(), 400);

    let (body, next, head) = get(&url, "t-root", "?limit=1000").unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        body,
        std::fs::read_to_string(&path).unwrap(),
        "endpoint serves exactly the chained file"
    );
    assert_eq!(head.unwrap(), verify_file(&path, None).unwrap().head);
    assert_eq!(next.unwrap(), (lines.len() - 1).to_string());

    let manifest_puts: Vec<&&str> = lines.iter().filter(|l| l.contains("\"key\":\"manifest\"")).collect();
    assert!(manifest_puts.iter().all(|l| l.contains(&format!("\"policy\":\"{want_policy}\""))));
    assert!(
        manifest_puts
            .iter()
            .filter(|l| l.contains("\"reason\":\"accepted\""))
            .all(|l| l.contains("\"vtime\":")),
        "accepted manifest writes carry the validator time"
    );
    let reason = |r: &str| {
        lines
            .iter()
            .filter(|l| l.contains("\"key\":\"manifest\"") && l.contains(&format!("\"reason\":\"{r}\"")))
            .count()
    };
    assert!(reason("accepted") >= 4, "accepts: {body}");
    assert_eq!(reason("policy"), 1, "{body}");
    assert_eq!(reason("lock-required"), 1, "{body}");
    assert_eq!(reason("lock"), 1, "{body}");
    assert!(
        line_with(&lines, "\"reason\":\"policy\"").contains("\"outcome\":\"403\"")
            && line_with(&lines, "\"reason\":\"policy\"").contains("alice")
    );
    assert!(
        line_with(&lines, "\"reason\":\"lock\"").contains("\"outcome\":\"409\"")
            && line_with(&lines, "\"reason\":\"lock\"").contains("bob")
    );
    // the lock acquire stored server time as time_hw in the same write: vtime is that value
    assert!(
        lines.iter().any(|l| l.contains("\"vtime\":1000")),
        "validator time recorded: {body}"
    );
    assert!(line_with(&lines, "\"op\":\"FORCE\"").contains(&t2.to_hex()));
    assert!(line_with(&lines, "\"op\":\"DELETE-BRANCH\"").contains("z/topic"));

    // pagination: pages of 3 concatenate to the whole log, with no gaps or repeats
    let mut all = String::new();
    let mut after: Option<String> = None;
    loop {
        let q = match &after {
            Some(a) => format!("?limit=3&after={a}"),
            None => "?limit=3".into(),
        };
        let (b, next, _) = get(&url, "t-root", &q).unwrap();
        assert!(b.lines().count() <= 3);
        all.push_str(&b);
        match next {
            Some(n) if !b.is_empty() => after = Some(n),
            _ => break,
        }
        if b.lines().count() < 3 {
            break;
        }
    }
    assert_eq!(all, body);
    // past the end: empty page, no next
    let (b, next, _) = get(&url, "t-root", &format!("?after={}", lines.len() - 1)).unwrap();
    assert!((b.is_empty(), next) == (true, None));
}

#[test]
fn audit_endpoint_is_404_when_audit_is_off() {
    let table = TokenTable::from_entries(vec![("t-root", pr("root", Role::Admin))]);
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    assert_eq!(get(&url, "t-root", "").unwrap_err(), 404);
}
