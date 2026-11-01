//! Security regression (docs/security-review.md): fails without the matching fix.

use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

fn who(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}

fn table() -> TokenTable {
    TokenTable::from_entries(vec![
        ("t-alice", who("alice", Role::Write)),
        ("t-bob", who("bob", Role::Write)),
        ("t-root", who("root", Role::Admin)),
    ])
}

fn start_cfg(cfg: Config) -> (Server, Arc<MemBackend>, String) {
    let mem = Arc::new(MemBackend::new());
    let srv = Server::start(mem.clone(), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, mem, url)
}

fn put_status(url: &str, tok: &str, key: &str) -> u16 {
    match ureq::put(&format!("{url}/v1/o/{key}"))
        .set("Authorization", &format!("Bearer {tok}"))
        .set("If-None-Match", "*")
        .send_bytes(b"x")
    {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(c, _)) => c,
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn key_spellings_of_the_manifest_cannot_bypass_validation() {
    // On a case-insensitive filesystem backend `MANIFEST` is the manifest file; the validator only
    // looks at the exact key, so those spellings must never reach the backend.
    let (_s, mem, url) = start_cfg(Config::new(table()));
    for k in ["Manifest", "MANIFEST", "manifest%20", "manifest.", "manifest::$DATA", "ManiFest"] {
        let c = put_status(&url, "t-alice", k);
        assert!((400..500).contains(&c), "{k}: {c}");
    }
    assert!(mem.list("").unwrap().is_empty());
}

#[test]
fn control_characters_in_keys_are_refused() {
    // a newline in a key would forge extra lines in `/v1/list` output
    let (_s, mem, url) = start_cfg(Config::new(table()));
    for k in ["k/a%0Ab", "k/a%0Db", "k/a%09b", "k/a%7Fb", "k/a%1Bb"] {
        let c = put_status(&url, "t-alice", k);
        assert_eq!(c, 400, "{k}");
    }
    assert!(mem.list("").unwrap().is_empty());
}
