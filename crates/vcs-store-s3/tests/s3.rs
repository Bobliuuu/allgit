//! Tests against a fake HTTP server (always run) and a real MinIO (gated on
//! `VCS_TEST_S3_ENDPOINT`, see docs/s3.md).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::backend::conformance;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};
use vcs_store_s3::{S3Backend, S3Config};

// ---------- fake server (no docker needed) ----------

/// Serves every request with the given canned response (ignores conditional headers).
fn fake_server(status_line: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            std::thread::spawn(move || {
                // read headers and the declared body so closing does not reset the connection
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let Ok(n) = s.read(&mut buf) else { return };
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_lowercase();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let len = text[..h]
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if data.len() >= h + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        return;
                    }
                }
                let r = format!("HTTP/1.1 {status_line}\r\nETag: \"abc\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    format!("http://{addr}")
}

fn cfg(endpoint: &str) -> S3Config {
    let mut c = S3Config::new(endpoint, "b", "k", "s");
    c.timeout = std::time::Duration::from_secs(2);
    c
}

#[test]
fn connect_refuses_provider_ignoring_conditional_writes() {
    let ep = fake_server("200 OK");
    let e = S3Backend::connect(cfg(&ep)).err().expect("must refuse");
    assert!(e.to_string().contains("conditional"), "{e}");
}

#[test]
fn server_errors_and_connection_failures_are_unavailable() {
    let ep = fake_server("503 Service Unavailable");
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    assert!(matches!(b.get("a/b"), Err(BackendError::Unavailable(_))));
    assert!(matches!(b.put_if_absent("a/b", b"x"), Err(BackendError::Unavailable(_))));
    assert!(matches!(S3Backend::connect(cfg(&ep)), Err(BackendError::Unavailable(_))));
    // nothing listening
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let b = S3Backend::new_unchecked(cfg(&dead)).unwrap();
    assert!(matches!(b.exists("a/b"), Err(BackendError::Unavailable(_))));
}

#[test]
fn precondition_statuses_are_not_errors() {
    let ep = fake_server("412 Precondition Failed");
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    assert!(!b.put_if_absent("a/b", b"x").unwrap());
    assert_eq!(b.put_if_match("a/b", b"x", "\"t\"").unwrap(), None);
    let ep = fake_server("409 Conflict");
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    assert!(!b.put_if_absent("a/b", b"x").unwrap());
    assert_eq!(b.put_if_match("a/b", b"x", "\"t\"").unwrap(), None);
}

// ---------- MinIO (gated) ----------

fn minio(test: &str) -> Option<S3Backend> {
    let Ok(ep) = std::env::var("VCS_TEST_S3_ENDPOINT") else {
        eprintln!("skipping {test}: VCS_TEST_S3_ENDPOINT not set");
        return None;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
    let mut c = S3Config::new(
        &ep,
        &var("VCS_TEST_S3_BUCKET", "vcs-test"),
        &var("VCS_TEST_S3_ACCESS_KEY", "minioadmin"),
        &var("VCS_TEST_S3_SECRET_KEY", "minioadmin"),
    );
    // unique prefix per test run so tests are isolated and re-runnable
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    c.prefix = format!("{test}-{t}/");
    let b = S3Backend::new_unchecked(c.clone()).unwrap();
    b.ensure_bucket().unwrap();
    Some(S3Backend::connect(c).expect("MinIO must honour conditional writes"))
}

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn chain(s: &MemoryStore, zone: &str, n: usize, size: usize) -> ObjectId {
    let mut parent = None;
    for i in 0..n {
        let blob = s.put_blob(&vec![(i % 251) as u8 ^ zone.len() as u8; size + i]).unwrap();
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
        parent = Some(
            s.put(&Object::Commit(Commit {
                zone: zone.into(),
                parents: parent.into_iter().collect(),
                root: t,
                change_id: ChangeId::from_bytes([i as u8; 16]),
                author: sig(),
                message: format!("{zone} {i}"),
            }))
            .unwrap(),
        );
    }
    parent.unwrap()
}

fn publish(b: &dyn Backend, s: &MemoryStore, ups: &[(&str, Option<ObjectId>, ObjectId)]) -> Result<Outcome, PublishError> {
    let mut u: Vec<RefUpdate> = ups
        .iter()
        .map(|(a, o, n)| RefUpdate {
            zone: (*a).into(),
            branch: "main".into(),
            old: *o,
            new: *n,
        })
        .collect();
    u.sort();
    Publisher::new(
        b,
        s,
        Transaction {
            updates: u,
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        },
    )
    .run()
}

#[test]
fn minio_conformance() {
    let Some(b) = minio("conformance") else { return };
    conformance(&b);
}

#[test]
fn minio_list_paginates_past_1000_keys() {
    let Some(b) = minio("list") else { return };
    let b = Arc::new(b);
    let hs: Vec<_> = (0..8)
        .map(|t| {
            let b = b.clone();
            std::thread::spawn(move || {
                for i in 0..130 {
                    assert!(b.put_if_absent(&format!("many/{t}-{i:04}"), b"x").unwrap());
                }
            })
        })
        .collect();
    hs.into_iter().for_each(|h| h.join().unwrap());
    let before = b.requests();
    let keys = b.list("many/").unwrap();
    assert_eq!(keys.len(), 1040);
    assert!(keys.windows(2).all(|w| w[0] < w[1]));
    assert!(b.requests() - before >= 2, "needed more than one page");
}

#[test]
fn minio_publish_fetch_and_sparse_request_counts() {
    let Some(remote) = minio("sparse") else { return };
    let src = MemoryStore::new();
    let eng = chain(&src, "engine", 5, 100);
    let assets = chain(&src, "assets", 5, 100_000);
    publish(&remote, &src, &[("engine", None, eng), ("assets", None, assets)]).unwrap();

    let (r0, b0) = (remote.requests(), remote.bytes_read());
    let dst = MemoryStore::new();
    let heads = fetch_heads(&remote, &dst, Some(&["engine".to_string()])).unwrap();
    assert_eq!(heads.len(), 1);
    let (sparse_reqs, sparse_bytes) = (remote.requests() - r0, remote.bytes_read() - b0);
    assert!(
        sparse_bytes < 10_000,
        "sparse fetch read {sparse_bytes} bytes; assets (500KB) must not be touched"
    );
    assert!(dst.contains(&eng).unwrap());
    assert!(!dst.contains(&assets).unwrap());

    // full fetch of everything costs strictly more requests and bytes than the sparse one
    let (r1, b1) = (remote.requests(), remote.bytes_read());
    let full = MemoryStore::new();
    fetch_heads(&remote, &full, None).unwrap();
    assert!(full.contains(&assets).unwrap());
    assert!(remote.requests() - r1 > sparse_reqs);
    assert!(remote.bytes_read() - b1 > 400_000);

    // subscribing later fetches only what is missing
    fetch_heads(&remote, &dst, Some(&["assets".to_string()])).unwrap();
    assert!(dst.contains(&assets).unwrap());
    // re-fetch with nothing new: manifest read only, no object downloads
    let (r2, b2) = (remote.requests(), remote.bytes_read());
    fetch_heads(&remote, &dst, None).unwrap();
    assert!(remote.requests() - r2 <= 3, "no-op fetch used {} requests", remote.requests() - r2);
    assert!(remote.bytes_read() - b2 < 2_000);
}

#[test]
fn minio_diverged_publish_reports_both_heads() {
    let Some(remote) = minio("diverged") else { return };
    let (s1, s2) = (MemoryStore::new(), MemoryStore::new());
    let x = chain(&s1, "a", 1, 1);
    let y = chain(&s2, "a", 2, 1);
    publish(&remote, &s1, &[("a", None, x)]).unwrap();
    match publish(&remote, &s2, &[("a", None, y)]).unwrap_err() {
        PublishError::Diverged { found, expected, .. } => {
            assert_eq!(found, Some(x));
            assert_eq!(expected, None);
        }
        e => panic!("{e}"),
    }
}

#[test]
fn minio_concurrent_publishers_all_commit() {
    let Some(remote) = minio("concurrent-publish") else { return };
    let remote = Arc::new(remote);
    let hs: Vec<_> = (0..6)
        .map(|i| {
            let remote = remote.clone();
            std::thread::spawn(move || {
                let s = MemoryStore::new();
                let zone = format!("zone{i}");
                let h = chain(&s, &zone, 2, 10);
                publish(remote.as_ref(), &s, &[(zone.as_str(), None, h)]).unwrap();
                h
            })
        })
        .collect();
    let heads: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    let (m, _) = load_manifest(remote.as_ref()).unwrap();
    assert_eq!(m.generation, 6);
    for (i, h) in heads.iter().enumerate() {
        assert_eq!(m.head(&format!("zone{i}"), "main"), Some(*h));
    }
    assert_eq!(fetch_heads(remote.as_ref(), &MemoryStore::new(), None).unwrap().len(), 6);
}

#[test]
fn minio_concurrent_cas_has_exactly_one_winner_per_round() {
    let Some(b) = minio("cas") else { return };
    let b = Arc::new(b);
    assert!(b.put_if_absent("ctr", b"0").unwrap());
    for round in 0..10 {
        let tag = b.get("ctr").unwrap().unwrap().tag;
        let hs: Vec<_> = (0..8)
            .map(|t| {
                let (b, tag) = (b.clone(), tag.clone());
                std::thread::spawn(move || b.put_if_match("ctr", format!("{round}-{t}").as_bytes(), &tag).unwrap())
            })
            .collect();
        let wins: Vec<_> = hs.into_iter().filter_map(|h| h.join().unwrap()).collect();
        assert_eq!(wins.len(), 1, "round {round}: exactly one CAS may win");
        assert_eq!(b.get("ctr").unwrap().unwrap().tag, wins[0]);
    }
    // concurrent create-only: exactly one creator
    let hs: Vec<_> = (0..8)
        .map(|t| {
            let b = b.clone();
            std::thread::spawn(move || b.put_if_absent("once", &[t]).unwrap())
        })
        .collect();
    assert_eq!(hs.into_iter().filter(|_| true).map(|h| h.join().unwrap()).filter(|w| *w).count(), 1);
}

/// Records "METHOD path" of each request and answers `status_line`.
fn recording_server(status_line: &'static str) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let log = log.clone();
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                while !String::from_utf8_lossy(&data).contains("\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&data).to_string();
                let mut it = text.lines().next().unwrap_or("").split(' ');
                log.lock()
                    .unwrap()
                    .push(format!("{} {}", it.next().unwrap_or(""), it.next().unwrap_or("")));
                let r = format!("HTTP/1.1 {status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                let _ = s.write_all(r.as_bytes());
            });
        }
    });
    (format!("http://{addr}"), seen)
}

#[test]
fn delete_sends_a_signed_unconditional_delete_and_tolerates_404() {
    for status in ["204 No Content", "404 Not Found"] {
        let (ep, seen) = recording_server(status);
        let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
        b.delete("objects/ab/cdef").unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["DELETE /b/objects/ab/cdef"]);
        assert!(!b.supports_gc(), "S3 touch/age/clock are not implemented: remote GC must refuse it");
    }
    let (ep, _) = recording_server("403 Forbidden");
    assert!(S3Backend::new_unchecked(cfg(&ep)).unwrap().delete("k/x").is_err());
    let (ep, _) = recording_server("503 Service Unavailable");
    assert!(matches!(
        S3Backend::new_unchecked(cfg(&ep)).unwrap().delete("k/x"),
        Err(BackendError::Unavailable(_))
    ));
    assert!(S3Backend::new_unchecked(cfg(&ep)).unwrap().delete("../x").is_err());
}

/// Serves one object honoring `Range`; records the Range header of each request.
fn range_server(obj: Vec<u8>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let (obj, log) = (obj.clone(), log.clone());
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                while !String::from_utf8_lossy(&data).contains("\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => data.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&data).to_string();
                let range = text
                    .lines()
                    .find_map(|l| l.to_lowercase().strip_prefix("range: bytes=").map(str::to_owned));
                log.lock().unwrap().push(range.clone().unwrap_or_default());
                let resp = match range.and_then(|r| {
                    let (a, b) = r.split_once('-')?;
                    Some((a.parse::<usize>().ok()?, b.trim().parse::<usize>().ok()?))
                }) {
                    Some((a, _)) if a >= obj.len() => {
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .as_bytes()
                            .to_vec()
                    }
                    Some((a, b)) => {
                        let part = &obj[a..=b.min(obj.len() - 1)];
                        let mut r = format!(
                            "HTTP/1.1 206 Partial Content\r\nETag: \"e\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            part.len()
                        )
                        .into_bytes();
                        r.extend_from_slice(part);
                        r
                    }
                    None => "HTTP/1.1 200 OK\r\nETag: \"e\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .as_bytes()
                        .to_vec(),
                };
                let _ = s.write_all(&resp);
            });
        }
    });
    (format!("http://{addr}"), seen)
}

#[test]
fn get_range_sends_range_header_and_returns_only_that_slice() {
    let (ep, seen) = range_server((0u8..=99).collect());
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    assert_eq!(b.get_range("p/x.pack", 10, 5).unwrap().unwrap(), vec![10, 11, 12, 13, 14]);
    assert_eq!(b.get_range("p/x.pack", 98, 10).unwrap().unwrap(), vec![98, 99], "clamped at end");
    assert_eq!(b.get_range("p/x.pack", 500, 4).unwrap().unwrap(), Vec::<u8>::new(), "416 is empty");
    assert_eq!(*seen.lock().unwrap(), vec!["10-14", "98-107", "500-503"]);
}

// ---------- connection reuse and conditional GET (#243, #240), no docker needed ----------

type Seen = Arc<std::sync::Mutex<Vec<(String, std::collections::HashMap<String, String>)>>>;

/// A keep-alive HTTP/1.1 server holding one object `k` with ETag `"t1"`. Counts accepted TCP
/// connections and records every request as (request line, lowercase headers). A GET with a
/// matching `If-None-Match` is answered 304; PUTs answer 200 with a fresh ETag.
fn keepalive_server() -> (String, Arc<std::sync::atomic::AtomicUsize>, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen: Seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (c2, s2) = (conns.clone(), seen.clone());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let log = s2.clone();
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    // one request at a time on this connection
                    let end = loop {
                        if let Some(h) = String::from_utf8_lossy(&data).find("\r\n\r\n") {
                            break h;
                        }
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => data.extend_from_slice(&buf[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&data[..end]).to_string();
                    let mut lines = head.lines();
                    let reqline = lines.next().unwrap_or("").to_owned();
                    let hdrs: std::collections::HashMap<String, String> = lines
                        .filter_map(|l| l.split_once(':'))
                        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                        .collect();
                    let len: usize = hdrs.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                    while data.len() < end + 4 + len {
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => data.extend_from_slice(&buf[..n]),
                        }
                    }
                    data.drain(..end + 4 + len);
                    let method = reqline.split(' ').next().unwrap_or("").to_owned();
                    let inm = hdrs.get("if-none-match").cloned();
                    log.lock().unwrap().push((reqline, hdrs));
                    let r = match method.as_str() {
                        "GET" if inm.as_deref() == Some("\"t1\"") => "HTTP/1.1 304 Not Modified\r\nETag: \"t1\"\r\n\r\n".to_owned(),
                        "GET" => "HTTP/1.1 200 OK\r\nETag: \"t1\"\r\nContent-Length: 5\r\n\r\nhello".to_owned(),
                        "HEAD" => "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n".to_owned(),
                        _ => "HTTP/1.1 200 OK\r\nETag: \"t2\"\r\nContent-Length: 0\r\n\r\n".to_owned(),
                    };
                    if s.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("http://{addr}"), conns, seen)
}

#[test]
fn sequential_reads_reuse_one_connection_and_each_request_is_signed() {
    let (ep, conns, seen) = keepalive_server();
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    // mix of safe request kinds, 300 requests in all
    for i in 0..100 {
        assert_eq!(b.get("k").unwrap().unwrap().data, b"hello");
        assert_eq!(b.get_range("k", 0, 2).unwrap().unwrap(), b"hello"); // fake server ignores Range
        assert!(b.exists(&format!("k{}", i % 3)).unwrap());
    }
    let n = conns.load(std::sync::atomic::Ordering::SeqCst);
    println!("300 sequential safe requests used {n} TCP connection(s) (was 300 with max_idle_connections(0))");
    assert_eq!(b.requests(), 300);
    assert!(n <= 1, "reads must reuse the pooled connection, opened {n}");
    // signing: every request carries its own date, content hash and a well-formed signature
    let log = seen.lock().unwrap();
    assert_eq!(log.len(), 300);
    for (line, h) in log.iter() {
        let auth = h.get("authorization").unwrap_or_else(|| panic!("{line}: unsigned"));
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=k/"), "{auth}");
        let sig = auth.rsplit("Signature=").next().unwrap();
        assert_eq!(sig.len(), 64, "{auth}");
        assert!(h.contains_key("x-amz-date") && h.contains_key("x-amz-content-sha256"), "{line}");
        assert_eq!(h["host"], ep.trim_start_matches("http://"));
    }
    // different keys sign differently even on the same pooled connection
    let sigs: std::collections::HashSet<_> = log
        .iter()
        .filter(|(l, _)| l.starts_with("HEAD"))
        .map(|(l, h)| (l.clone(), h["authorization"].rsplit("Signature=").next().unwrap().to_owned()))
        .collect();
    assert!(sigs.len() >= 3);
}

#[test]
fn writes_never_ride_a_pooled_connection() {
    let (ep, conns, _) = keepalive_server();
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    for _ in 0..5 {
        b.get("k").unwrap();
    }
    assert_eq!(conns.load(std::sync::atomic::Ordering::SeqCst), 1);
    for i in 0..5 {
        b.put_if_match("k", b"x", "\"t1\"").unwrap();
        // each write opened its own connection; the pooled read connection is still the one used for reads
        assert_eq!(conns.load(std::sync::atomic::Ordering::SeqCst), 2 + i);
    }
    b.get("k").unwrap();
    assert_eq!(
        conns.load(std::sync::atomic::Ordering::SeqCst),
        6,
        "reads still reuse their pooled connection"
    );
}

#[test]
fn a_dead_pooled_connection_is_retried_for_reads() {
    // server closes each connection after one response without saying so
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut buf = [0u8; 4096];
            let mut data = Vec::new();
            while !String::from_utf8_lossy(&data).contains("\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
            }
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nETag: \"t1\"\r\nContent-Length: 5\r\n\r\nhello");
            // dropped: closed under the client's pooled connection
        }
    });
    let b = S3Backend::new_unchecked(cfg(&format!("http://{addr}"))).unwrap();
    for _ in 0..10 {
        assert_eq!(b.get("k").unwrap().unwrap().data, b"hello");
    }
}

#[test]
fn conditional_get_sends_if_none_match_and_handles_304() {
    let (ep, _, seen) = keepalive_server();
    let b = S3Backend::new_unchecked(cfg(&ep)).unwrap();
    let before = b.bytes_read();
    assert_eq!(b.get_conditional("k", "\"t1\"").unwrap(), Conditional::NotModified);
    assert_eq!(b.bytes_read(), before, "304 carries no body");
    match b.get_conditional("k", "\"stale\"").unwrap() {
        Conditional::Modified(v) => assert_eq!((v.data.as_slice(), v.tag.as_str()), (&b"hello"[..], "\"t1\"")),
        other => panic!("{other:?}"),
    }
    let log = seen.lock().unwrap();
    assert_eq!(log[0].1["if-none-match"], "\"t1\"");
    assert_eq!(log[1].1["if-none-match"], "\"stale\"");
}
