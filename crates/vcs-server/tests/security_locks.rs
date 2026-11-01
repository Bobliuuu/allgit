//! Security regression (docs/security-review.md): fails without the matching fix.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};

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

#[test]
fn write_principal_cannot_renew_or_forge_another_principals_expired_lock() {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let mut cfg = Config::new(table());
    let c2 = clock.clone();
    cfg.clock = Arc::new(move || c2.load(Ordering::SeqCst));
    let (_s, _m, url) = start_cfg(cfg);
    let alice = HttpBackend::new(&url, "t-alice").unwrap();
    let bob = HttpBackend::new(&url, "t-bob").unwrap();
    let lock = |holder: &str, now: u64, lease: u64| Lock {
        holder: holder.into(),
        token: 1,
        acquired_at: now,
        expires_at: now + lease,
        reason: String::new(),
    };
    let mut m = Manifest::default();
    m.next_token = 2;
    m.time_hw = 1_000_000;
    m.locks.insert(("main".into(), "a.bin".into()), lock("alice", 1_000_000, 1000));
    assert!(alice.put_if_absent(manifest::KEY, &m.encode()).unwrap());
    // alice's lease runs out
    clock.store(1_100_000, Ordering::SeqCst);
    let cur = bob.get(manifest::KEY).unwrap().unwrap();
    let mut m2 = Manifest::decode(&cur.data).unwrap();
    m2.time_hw = 1_100_000;
    // bob "renews" the lock but leaves alice's name on it
    m2.locks.insert(("main".into(), "a.bin".into()), lock("alice", 1_100_000, 600_000));
    let e = bob.put_if_match(manifest::KEY, &m2.encode(), &cur.tag).unwrap_err().to_string();
    assert!(e.contains("403"), "bob must not write lock records in alice's name: {e}");
}
