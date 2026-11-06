//! The minimum-reader marker through `vcs-server` (issue #567): a server binary below the repository's
//! level refuses to start, writers raise the level over HTTP, and lowering needs an admin token.
use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_remote::format::{self, Format};
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Principal, Role, Server, TokenTable};

fn cfg() -> Config {
    let p = |name: &str, role| Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    };
    Config::new(TokenTable::from_entries(vec![
        ("t-w", p("w", Role::Write)),
        ("t-admin", p("root", Role::Admin)),
    ]))
}

fn marker(level: u32) -> Vec<u8> {
    Format {
        min_reader: level,
        writer: "vcs test".into(),
    }
    .encode()
}

#[test]
fn server_refuses_to_start_on_a_repository_newer_than_itself() {
    let mem = Arc::new(MemBackend::new());
    mem.put_if_absent(format::KEY, &marker(format::LEVEL + 1)).unwrap();
    let e = Server::start(mem, cfg(), "127.0.0.1:0").err().expect("must refuse");
    let m = e.to_string();
    assert!(
        m.contains("requires vcs format level") && m.contains("docs/upgrade-rollback.md"),
        "{m}"
    );
}

#[test]
fn server_starts_on_unmarked_and_current_repositories() {
    let mem = Arc::new(MemBackend::new());
    let mut s = Server::start(mem.clone(), cfg(), "127.0.0.1:0").unwrap();
    s.shutdown();
    mem.put_if_absent(format::KEY, &marker(format::LEVEL)).unwrap();
    let mut s = Server::start(mem, cfg(), "127.0.0.1:0").unwrap();
    s.shutdown();
}

#[test]
fn raise_over_http_and_lower_needs_admin() {
    let mem = Arc::new(MemBackend::new());
    let mut srv = Server::start(mem.clone(), cfg(), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    let w = HttpBackend::new(&url, "t-w").unwrap();
    assert!(format::raise(&w, 3).unwrap(), "a write token may raise");
    assert_eq!(format::level(&*mem).unwrap(), 3);
    // garbage is rejected
    assert!(w
        .put_if_match(format::KEY, b"junk", &mem.get(format::KEY).unwrap().unwrap().tag)
        .is_err());
    // lowering: refused for a write token, accepted for an admin
    let tag = mem.get(format::KEY).unwrap().unwrap().tag;
    assert!(w.put_if_match(format::KEY, &marker(2), &tag).is_err());
    assert_eq!(format::level(&*mem).unwrap(), 3);
    let a = HttpBackend::new(&url, "t-admin").unwrap();
    assert!(a.put_if_match(format::KEY, &marker(2), &tag).unwrap().is_some());
    assert_eq!(format::level(&*mem).unwrap(), 2);
    // a reader sees the marker, so an old client would refuse
    assert!(format::check_as(&w, 1).is_err());
    srv.shutdown();
}
