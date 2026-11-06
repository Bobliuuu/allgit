//! Un-ranged GET of a large object from a directory backend is streamed in windows (docs/known-limits.md):
//! bytes and ETag match what a whole-object read gives, conditional GET still answers 304, small objects and
//! memory backends fall back to the whole read.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use vcs_remote::*;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

fn get(addr: &str, key: &str, extra: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET /v1/o/{key} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer t\r\n{extra}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap();
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&out[..split]).to_string();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head, out[split + 4..].to_vec())
}

fn start(b: Arc<dyn Backend>, min: u64) -> (Server, String) {
    let table = TokenTable::from_entries(vec![(
        "t",
        Principal {
            name: "a".into(),
            role: Role::Write,
            zones: None::<BTreeSet<String>>,
        },
    )]);
    let mut cfg = Config::new(table);
    cfg.stream_min_bytes = min;
    let srv = Server::start(b, cfg, "127.0.0.1:0").unwrap();
    let a = srv.local_addr().to_string();
    (srv, a)
}

#[test]
fn directory_backend_streams_large_gets_with_correct_bytes_and_etag() {
    let d = tempfile::tempdir().unwrap();
    let fsb = Arc::new(FsBackend::open(d.path()).unwrap());
    // not a multiple of the window size, so the last window is partial
    let data: Vec<u8> = (0..(3 * 256 * 1024 + 12345)).map(|i: usize| (i * 31 % 251) as u8).collect();
    fsb.put_if_absent("packs/big.pack", &data).unwrap();
    fsb.put_if_absent("packs/small.pack", b"tiny").unwrap();
    let want_tag = fsb.get("packs/big.pack").unwrap().unwrap().tag;
    // the backend itself reports a stream only at/above the threshold
    assert!(fsb.get_stream("packs/small.pack", 1024).unwrap().is_none());
    assert!(fsb.get_stream("packs/none", 0).unwrap().is_none());
    let s = fsb.get_stream("packs/big.pack", 1024).unwrap().unwrap();
    assert_eq!((s.len, s.tag.as_str()), (data.len() as u64, want_tag.as_str()));

    let (_srv, addr) = start(fsb, 1024);
    let (st, head, body) = get(&addr, "packs/big.pack", "");
    assert_eq!(st, 200);
    assert!(head.contains(&format!("ETag: \"{want_tag}\"")), "{head}");
    assert_eq!(body, data);
    let (st, _, body) = get(&addr, "packs/big.pack", &format!("If-None-Match: \"{want_tag}\"\r\n"));
    assert_eq!((st, body.len()), (304, 0));
    // below the threshold: ordinary whole read, same answer
    let (st, _, body) = get(&addr, "packs/small.pack", "");
    assert_eq!((st, body.as_slice()), (200, &b"tiny"[..]));
    assert_eq!(get(&addr, "packs/absent", "").0, 404);
}

#[test]
fn memory_backend_falls_back_to_whole_read() {
    let m = Arc::new(MemBackend::new());
    m.put_if_absent("packs/p.pack", &vec![7u8; 5000]).unwrap();
    let (_srv, addr) = start(m, 0);
    let (st, _, body) = get(&addr, "packs/p.pack", "");
    assert_eq!((st, body.len()), (200, 5000));
}
