//! A write token may only push objects and manifest updates; it must not be able to plant coordination keys
//! (`gc/lock`) that stall the collector. Known gap, see the GitHub issue: ignored until fixed.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

#[test]
#[ignore = "known gap: a write token can create gc/lock with an expiry that never passes"]
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
    assert!(out.starts_with("HTTP/1.1 403"), "write token planted gc/lock: {}", out.lines().next().unwrap_or(""));
}
