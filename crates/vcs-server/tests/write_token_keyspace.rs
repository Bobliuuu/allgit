//! A write token may only push objects and manifest updates; it must not be able to plant coordination keys
//! (`gc/lock`) that stall the collector. Issue #573.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

#[test]
fn write_token_cannot_plant_gc_lock() {
    let table = TokenTable::from_entries(vec![(
        "t-w",
        Principal {
            name: "w".into(),
            role: Role::Write,
            zones: Some(BTreeSet::from(["only-this-zone".to_string()])),
        },
    )]);
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table), "127.0.0.1:0").unwrap();
    let addr = srv.local_addr().to_string();
    let body = "evil 18446744073709551615";
    let req = format!(
        "PUT /v1/o/gc/lock HTTP/1.1\r\nConnection: close\r\nAuthorization: Bearer t-w\r\nIf-None-Match: *\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut s = TcpStream::connect(&addr).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(
        out.starts_with("HTTP/1.1 403"),
        "write token planted gc/lock: {}",
        out.lines().next().unwrap_or("")
    );
}

fn put(addr: &str, tok: &str, key: &str, cond: &str, body: &str) -> String {
    let req = format!(
        "PUT /v1/o/{key} HTTP/1.1\r\nConnection: close\r\nAuthorization: Bearer {tok}\r\n{cond}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.lines().next().unwrap_or("").to_string()
}

#[test]
fn write_token_keyspace_allowlist() {
    let w = |n: &str, r| Principal {
        name: n.into(),
        role: r,
        zones: Some(BTreeSet::from(["z".to_string()])),
    };
    let table = TokenTable::from_entries(vec![("t-w", w("w", Role::Write)), ("t-a", w("a", Role::Admin))]);
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table), "127.0.0.1:0").unwrap();
    let addr = srv.local_addr().to_string();
    let inm = "If-None-Match: *\r\n";
    for k in ["gc/lock", "tomb/ab", "compact/x"] {
        assert!(
            put(&addr, "t-w", k, inm, "x 1").starts_with("HTTP/1.1 403"),
            "write token wrote {k}"
        );
    }
    // packs are create-only for a write token
    assert!(put(&addr, "t-w", "packs/p.pack", "If-Match: \"abc\"\r\n", "x").starts_with("HTTP/1.1 403"));
    assert!(put(&addr, "t-w", "packs/p.pack", inm, "x").starts_with("HTTP/1.1 2"));
    // strict allowlist: unknown prefixes are refused, real key shapes still work
    for k in ["k/one", "misc/a", "services/x", "gcx", "manifest/x", "objects"] {
        assert!(
            put(&addr, "t-w", k, inm, "x").starts_with("HTTP/1.1 403"),
            "write token wrote {k}"
        );
    }
    assert!(put(&addr, "t-w", "tx/abc", inm, "x").starts_with("HTTP/1.1 2"));
    assert!(put(&addr, "t-w", "tx/abc", "If-Match: \"a\"\r\n", "x").starts_with("HTTP/1.1 403"));
    assert!(put(&addr, "t-a", "k/one", inm, "x").starts_with("HTTP/1.1 2"), "admin keeps ad-hoc keys");
    // an admin keeps the coordination keys
    assert!(put(&addr, "t-a", "gc/lock", inm, "me 1").starts_with("HTTP/1.1 2"));
}
