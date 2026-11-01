//! `require-signed <zone> <branch-glob>`: the validating writer refuses publishes whose new commits
//! lack a valid signature from an allowed signer. Each negative test fails if the check in
//! `validate.rs` (`check_signed`) is removed.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use vcs_objects::signing::fixtures::{KEY1, KEY2};
use vcs_objects::signing::{self, SigningKey, Trust};
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn start(policy: &str, allowed: &str) -> (Server, String) {
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
    cfg.validator.policy = Policy::parse(&format!("allow alice z **\nallow alice z2 **\n{policy}")).unwrap();
    let mut t = Trust::default();
    t.parse_allowed(allowed).unwrap();
    cfg.validator.signers = t;
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}

fn key(text: &str) -> SigningKey {
    SigningKey::from_openssh(text).unwrap()
}
fn allow(k: &SigningKey) -> String {
    format!("alice {}\n", k.public_line())
}

fn sig() -> Signature {
    Signature {
        name: "a".into(),
        email: "a".into(),
        timestamp: 1,
    }
}

/// Commit on `parent` (zone `z`), signed with `k` when given; `mangle` edits the signed commit.
fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, n: u8, k: Option<&SigningKey>, mangle: impl Fn(&mut Commit)) -> ObjectId {
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
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([n; 16]),
        author: sig(),
        message: format!("c{n}\n"),
    };
    if let Some(k) = k {
        c = signing::sign_commit(&c, k);
    }
    mangle(&mut c);
    s.put(&Object::Commit(c)).unwrap()
}

fn publish(url: &str, s: &MemoryStore, zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId) -> Result<Outcome, PublishError> {
    let b = HttpBackend::new(url, "t-alice").unwrap();
    let t = Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    };
    Publisher::new(&b, s, t).with_principal("alice").with_clock(|| 1).run()
}

const POLICY: &str = "require-signed z main\n";

#[test]
fn unsigned_commit_is_rejected_and_signed_accepted() {
    let k1 = key(KEY1);
    let (_srv, url) = start(POLICY, &allow(&k1));
    let s = MemoryStore::new();
    let bad = commit(&s, "z", None, 1, None, |_| {});
    let e = publish(&url, &s, "z", "main", None, bad).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("require-signed") && e.contains("not signed"), "{e}");
    let good = commit(&s, "z", None, 2, Some(&k1), |_| {});
    publish(&url, &s, "z", "main", None, good).unwrap();
}

#[test]
fn bad_signature_is_rejected() {
    let k1 = key(KEY1);
    let (_srv, url) = start(POLICY, &allow(&k1));
    let s = MemoryStore::new();
    // signed, then the message altered: signature no longer matches
    let tampered = commit(&s, "z", None, 1, Some(&k1), |c| c.message = c.message.replacen("c1", "c9", 1));
    let e = publish(&url, &s, "z", "main", None, tampered).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("invalid signature"), "{e}");
    // signed for a different tree
    let other_root = commit(&s, "z", None, 3, Some(&k1), |c| c.root = ObjectId::from_bytes([9; 32]));
    assert!(publish(&url, &s, "z", "main", None, other_root).is_err());
}

#[test]
fn forged_signer_is_rejected() {
    let (k1, k2) = (key(KEY1), key(KEY2));
    let (_srv, url) = start(POLICY, &allow(&k1));
    let s = MemoryStore::new();
    // signed by a valid but non-allowed key
    let stranger = commit(&s, "z", None, 1, Some(&k2), |_| {});
    let e = publish(&url, &s, "z", "main", None, stranger).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("not allowed"), "{e}");
    // claims the allowed key's identity but carries key2's signature bytes
    let forged = commit(&s, "z", None, 2, Some(&k2), |c| {
        let (base, line) = signing::split_commit_signature(&c.message);
        let sig2 = line.unwrap().split(' ').nth(2).unwrap().to_owned();
        let line = format!("ssh-ed25519 {} {}", k1.public_line().split(' ').nth(1).unwrap(), sig2);
        c.message = format!("{base}\n-----BEGIN ALLGIT SIGNATURE-----\n{line}\n-----END ALLGIT SIGNATURE-----\n");
    });
    let e = publish(&url, &s, "z", "main", None, forged).unwrap_err().to_string();
    assert!(e.contains("403") && e.contains("invalid signature"), "{e}");
}

#[test]
fn only_new_commits_are_checked_and_one_unsigned_ancestor_fails() {
    let k1 = key(KEY1);
    // main is not covered by the rule; `rel*` is
    let (_srv2, url2) = start("require-signed z rel*\n", &allow(&k1));
    let s2 = MemoryStore::new();
    let u1 = commit(&s2, "z", None, 1, None, |_| {});
    publish(&url2, &s2, "z", "main", None, u1).unwrap(); // main is not covered
    let mid = commit(&s2, "z", Some(u1), 2, None, |_| {});
    let top = commit(&s2, "z", Some(mid), 3, Some(&k1), |_| {});
    // rel is new: its whole history (u1, mid, top) is new to it, and u1/mid are unsigned
    let e = publish(&url2, &s2, "z", "rel", None, top).unwrap_err().to_string();
    assert!(e.contains("not signed"), "{e}");
    // a signed-only chain is accepted, and extending it with a signed commit works too
    let a = commit(&s2, "z", None, 10, Some(&k1), |_| {});
    publish(&url2, &s2, "z", "rel", None, a).unwrap();
    let b = commit(&s2, "z", Some(a), 11, Some(&k1), |_| {});
    publish(&url2, &s2, "z", "rel", Some(a), b).unwrap();
    // an unsigned commit on top is refused even though its parent is signed
    let u = commit(&s2, "z", Some(b), 12, None, |_| {});
    assert!(publish(&url2, &s2, "z", "rel", Some(b), u).is_err());
    // zones without a rule are unaffected
    let z2 = commit(&s2, "z2", None, 20, None, |_| {});
    publish(&url2, &s2, "z2", "rel", None, z2).unwrap();
}

#[test]
fn revoked_key_is_rejected() {
    let k1 = key(KEY1);
    let table = TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    cfg.validator.policy = Policy::parse(&format!("allow alice z **\n{POLICY}")).unwrap();
    let mut t = Trust::default();
    t.parse_allowed(&allow(&k1)).unwrap();
    t.parse_revoked(&k1.public_line()).unwrap();
    cfg.validator.signers = t;
    let srv = Server::start(Arc::new(MemBackend::new()), cfg, "127.0.0.1:0").unwrap();
    let url2 = format!("http://{}", srv.local_addr());
    let s = MemoryStore::new();
    let c = commit(&s, "z", None, 1, Some(&k1), |_| {});
    let e = publish(&url2, &s, "z", "main", None, c).unwrap_err().to_string();
    assert!(e.contains("revoked"), "{e}");
}
