//! Security regression (docs/security-review.md): fails without the matching fix.

use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{AuditLog, Config, HttpBackend, Principal, Role, Server, TokenTable};

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
fn delete_is_audited() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("audit.jsonl");
    let mut cfg = Config::new(table());
    cfg.audit = Some(Arc::new(AuditLog::open(&path, None).unwrap()));
    let (_s, _m, url) = start_cfg(cfg);
    let root = HttpBackend::new(&url, "t-root").unwrap();
    root.put_if_absent("k/one", b"1").unwrap();
    root.delete("k/one").unwrap();
    // a refused delete (write token) is a mutating attempt too
    let _ = HttpBackend::new(&url, "t-alice").unwrap().delete("k/two");
    let text = std::fs::read_to_string(&path).unwrap();
    let dels: Vec<&str> = text.lines().filter(|l| l.contains("\"op\":\"DELETE\"")).collect();
    assert_eq!(dels.len(), 2, "every DELETE must be audited: {text}");
    assert!(dels[0].contains("\"principal\":\"root\"") && dels[0].contains("\"outcome\":\"204\""));
    assert!(dels[1].contains("\"principal\":\"alice\"") && dels[1].contains("\"outcome\":\"403\""));
}
