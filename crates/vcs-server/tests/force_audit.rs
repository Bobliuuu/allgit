//! Forced head moves and branch deletions through the validating server: allowed (they are always a
//! compare-and-swap on the old head), audited as `FORCE` / `DELETE-BRANCH` with the full old head,
//! refused by `protect-branch`, and still subject to `require-signed`.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use vcs_objects::signing::fixtures::KEY1;
use vcs_objects::signing::{self, SigningKey, Trust};
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::audit::{verify_file, AuditLog};
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn start(policy: &str, allowed: &str) -> (Server, String, std::path::PathBuf, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("audit.jsonl");
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
    cfg.audit = Some(Arc::new(AuditLog::open(&path, None).unwrap()));
    cfg.validator.policy = Policy::parse(&format!("allow alice z **\n{policy}")).unwrap();
    let mut t = Trust::default();
    t.parse_allowed(allowed).unwrap();
    cfg.validator.signers = t;
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

fn commit(s: &MemoryStore, parent: Option<ObjectId>, n: u8, k: Option<&SigningKey>) -> ObjectId {
    let blob = s.put_blob(&[n]).unwrap();
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
    let mut c = Commit {
        zone: "z".into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([n; 16]),
        author: sig(),
        message: format!("c{n}\n"),
    };
    if let Some(k) = k {
        c = signing::sign_commit(&c, k);
    }
    s.put(&Object::Commit(c)).unwrap()
}

fn tx(branch: &str, old: Option<ObjectId>, new: ObjectId, msg: &str) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: "z".into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: msg.into(),
        lock_ops: vec![],
    }
}
fn publish(url: &str, s: &MemoryStore, t: Transaction) -> Result<Outcome, PublishError> {
    let b = HttpBackend::new(url, "t-alice").unwrap();
    Publisher::new(&b, s, t).with_principal("alice").with_clock(|| 1).run()
}
fn delete(url: &str, s: &MemoryStore, branch: &str, old: ObjectId, msg: &str) -> Result<Outcome, PublishError> {
    let b = HttpBackend::new(url, "t-alice").unwrap();
    let t = Transaction {
        updates: vec![],
        author: sig(),
        message: msg.into(),
        lock_ops: vec![],
    };
    Publisher::new(&b, s, t)
        .with_principal("alice")
        .with_clock(|| 1)
        .with_deletes(vec![("z".into(), branch.into(), old)])
        .run()
}
fn head(url: &str, branch: &str) -> Option<ObjectId> {
    let b = HttpBackend::new(url, "t-alice").unwrap();
    load_manifest(&b).unwrap().0.head("z", branch)
}
fn audit(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn force_is_audited_with_the_old_head_and_ordinary_publishes_are_not() {
    let (_srv, url, path, _d) = start("", "");
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 1, None);
    let c2 = commit(&s, Some(c1), 2, None);
    publish(&url, &s, tx("main", None, c1, "m1")).unwrap();
    publish(&url, &s, tx("main", Some(c1), c2, "m2")).unwrap();
    assert!(!audit(&path).contains("FORCE"), "a fast-forward is not a force");
    // rewrite: c3 sits on c1, not on c2
    let c3 = commit(&s, Some(c1), 3, None);
    // a stale lease (expects c1 but head is c2) fails and changes nothing
    let e = publish(&url, &s, tx("main", Some(c1), c3, "force")).unwrap_err();
    assert!(matches!(e, PublishError::Diverged { .. }), "{e}");
    assert_eq!(head(&url, "main"), Some(c2));
    // the right lease works
    publish(&url, &s, tx("main", Some(c2), c3, "publish --force-with-lease")).unwrap();
    assert_eq!(head(&url, "main"), Some(c3));
    let text = audit(&path);
    let line = text.lines().find(|l| l.contains("\"op\":\"FORCE\"")).expect(&text);
    assert!(
        line.contains(&c2.to_hex()) && line.contains(&c3.to_hex()) && line.contains("z/main") && line.contains("alice"),
        "{line}"
    );
    assert_eq!(text.lines().filter(|l| l.contains("\"op\":\"FORCE\"")).count(), 1);
    verify_file(&path, None).unwrap();
}

#[test]
fn delete_is_a_lease_and_audited() {
    let (_srv, url, path, _d) = start("", "");
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 1, None);
    let c2 = commit(&s, None, 2, None);
    publish(&url, &s, tx("topic", None, c1, "m")).unwrap();
    let e = delete(&url, &s, "topic", c2, "delete wrong").unwrap_err();
    assert!(matches!(e, PublishError::Diverged { .. }), "{e}");
    assert_eq!(head(&url, "topic"), Some(c1));
    delete(&url, &s, "topic", c1, "delete right").unwrap();
    assert_eq!(head(&url, "topic"), None);
    let text = audit(&path);
    let line = text.lines().find(|l| l.contains("DELETE-BRANCH")).expect(&text);
    assert!(line.contains(&c1.to_hex()) && line.contains("z/topic"), "{line}");
}

#[test]
fn protect_branch_refuses_force_and_delete_but_not_fast_forward() {
    let (_srv, url, path, _d) = start("protect-branch z rel*\n", "");
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 1, None);
    let c2 = commit(&s, Some(c1), 2, None);
    let c3 = commit(&s, Some(c1), 3, None);
    publish(&url, &s, tx("rel1", None, c1, "m")).unwrap();
    publish(&url, &s, tx("rel1", Some(c1), c2, "ff")).unwrap();
    let e = publish(&url, &s, tx("rel1", Some(c2), c3, "force")).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("protect-branch"), "{e}");
    assert_eq!(head(&url, "rel1"), Some(c2));
    let e = delete(&url, &s, "rel1", c2, "del").unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("protect-branch"), "{e}");
    assert_eq!(head(&url, "rel1"), Some(c2));
    // an unprotected branch can still be forced
    publish(&url, &s, tx("dev", None, c2, "m")).unwrap();
    publish(&url, &s, tx("dev", Some(c2), c3, "force")).unwrap();
    assert!(audit(&path).contains("\"op\":\"FORCE\""));
}

#[test]
fn require_signed_still_applies_to_a_forced_head() {
    let k1 = SigningKey::from_openssh(KEY1).unwrap();
    let (_srv, url, _path, _d) = start("require-signed z main\n", &format!("alice {}\n", k1.public_line()));
    let s = MemoryStore::new();
    let c1 = commit(&s, None, 1, Some(&k1));
    let c2 = commit(&s, Some(c1), 2, Some(&k1));
    publish(&url, &s, tx("main", None, c1, "m")).unwrap();
    publish(&url, &s, tx("main", Some(c1), c2, "m")).unwrap();
    let unsigned = commit(&s, Some(c1), 3, None);
    let e = publish(&url, &s, tx("main", Some(c2), unsigned, "force")).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("not signed"), "{e}");
    assert_eq!(head(&url, "main"), Some(c2));
    let signed = commit(&s, Some(c1), 4, Some(&k1));
    publish(&url, &s, tx("main", Some(c2), signed, "force")).unwrap();
    assert_eq!(head(&url, "main"), Some(signed));
}
