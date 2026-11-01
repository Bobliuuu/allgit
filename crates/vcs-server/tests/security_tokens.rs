//! Security regression (docs/security-review.md): the lock fencing counter `next_token` may only
//! move by what a write can plausibly have granted, and lock tokens must come from below it.
//! Without bounds one write principal can set `next_token = u64::MAX`; it can never decrease, so
//! every later lock acquisition overflows (panic in debug, wrapped and reused tokens in release).

use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};

fn start() -> (Server, String) {
    let table = TokenTable::from_entries(vec![(
        "t-alice",
        Principal {
            name: "alice".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}

fn lock(token: u64) -> Lock {
    Lock {
        holder: "alice".into(),
        token,
        acquired_at: 0,
        expires_at: 1000,
        reason: String::new(),
    }
}

fn put_manifest(c: &HttpBackend, m: &Manifest) -> Result<bool, BackendError> {
    match c.get(manifest::KEY)? {
        None => c.put_if_absent(manifest::KEY, &m.encode()),
        Some(cur) => Ok(c.put_if_match(manifest::KEY, &m.encode(), &cur.tag)?.is_some()),
    }
}

#[test]
fn next_token_cannot_be_exhausted_in_one_write() {
    let (_s, url) = start();
    let c = HttpBackend::new(&url, "t-alice").unwrap();
    let mut m = Manifest::default();
    m.next_token = u64::MAX;
    let e = put_manifest(&c, &m).unwrap_err().to_string();
    assert!(e.contains("422"), "{e}");
    // a plausible jump (a batch of acquisitions) is fine
    m.next_token = 3;
    m.locks.insert(("main".into(), "a".into()), lock(0));
    m.locks.insert(("main".into(), "b".into()), lock(1));
    m.locks.insert(("main".into(), "c".into()), lock(2));
    assert!(put_manifest(&c, &m).unwrap());
}

#[test]
fn lock_tokens_must_be_below_next_token() {
    let (_s, url) = start();
    let c = HttpBackend::new(&url, "t-alice").unwrap();
    let mut m = Manifest::default();
    m.next_token = 1;
    m.locks.insert(("main".into(), "a".into()), lock(9_999));
    let e = put_manifest(&c, &m).unwrap_err().to_string();
    assert!(e.contains("422"), "{e}");
}
