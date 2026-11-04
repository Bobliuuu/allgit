//! Measurement harness for remote compaction (issues #248, #249). `#[ignore]`d: it needs a real
//! repository and (optionally) an S3 server, and prints numbers instead of asserting them.
//!
//! ```text
//! MEASURE_REPO=/path/to/imported.vcs MEASURE_PUBLISHES=60 MEASURE_BACKEND=dir:/tmp/remote \
//!   cargo test --release -p vcs-cli --test compact_measure -- --ignored --nocapture
//! MEASURE_BACKEND=s3:http://127.0.0.1:19400/bucket/prefix  (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY)
//! ```
//!
//! It publishes N evenly spaced prefixes of the branch's history (one pack each, like N pushes),
//! then reports request counts and bytes for publishing, a full fetch, compaction, and a full
//! fetch again; and how many bytes the union of objects takes under several pack layouts.

use std::path::Path;
use vcs_objects::*;
use vcs_remote::compact::{compact, load_packs, CompactOptions};
use vcs_remote::*;
use vcs_store::MemoryStore;

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_owned())
}

fn tx(zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: Signature {
            name: "m".into(),
            email: "m@x".into(),
            timestamp: 1,
        },
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn open_backend(spec: &str) -> Box<dyn Backend> {
    if let Some(d) = spec.strip_prefix("dir:") {
        let _ = std::fs::remove_dir_all(d);
        return Box::new(FsBackend::open(std::path::PathBuf::from(d)).unwrap());
    }
    let rest = spec
        .strip_prefix("s3:")
        .expect("MEASURE_BACKEND = dir:<path> | s3:<endpoint>/<bucket>/<prefix>");
    let (ep, tail) = rest
        .rsplit_once('/')
        .and_then(|(a, p)| a.rsplit_once('/').map(|(e, b)| (e, (b, p))))
        .unwrap();
    let (ak, sk) = (env("AWS_ACCESS_KEY_ID", "rustfsadmin"), env("AWS_SECRET_ACCESS_KEY", "rustfsadmin"));
    let mut cfg = vcs_store_s3::S3Config::new(ep, tail.0, &ak, &sk);
    cfg.prefix = tail.1.to_owned();
    let b = vcs_store_s3::S3Backend::new_unchecked(cfg).unwrap();
    b.ensure_bucket().unwrap();
    Box::new(b)
}

fn stats_line(what: &str, s: TransferStats) {
    println!(
        "  {what:<34} {:>7} requests {:>12} B down {:>12} B up",
        s.requests, s.bytes_down, s.bytes_up
    );
}

fn remote_bytes(b: &dyn Backend) -> (usize, u64) {
    let packs = load_packs(b).unwrap();
    (packs.len(), packs.iter().map(|p| p.0.len + p.1.len() as u64 * 44).sum())
}

fn full_fetch(b: &dyn Backend) -> TransferStats {
    let c = CountingBackend::new(b);
    let dst = MemoryStore::new();
    fetch_heads(&c, &dst, None).unwrap();
    c.stats()
}

/// One object fetched by id (what a lazy checkout of one file costs per object).
fn one_fetch(b: &dyn Backend, id: &ObjectId) -> TransferStats {
    let c = CountingBackend::new(b);
    vcs_remote::packfetch::fetch_from_any_pack(&c, id).unwrap().expect("present");
    c.stats()
}

#[test]
#[ignore]
fn measure() {
    let repo_path = env("MEASURE_REPO", "");
    assert!(!repo_path.is_empty(), "set MEASURE_REPO");
    let (zone, branch) = (env("MEASURE_ZONE", "root"), env("MEASURE_BRANCH", "master"));
    let n: usize = env("MEASURE_PUBLISHES", "60").parse().unwrap();
    let repo = vcs_repo::Repo::discover(Path::new(&repo_path)).unwrap();
    let mut chain: Vec<ObjectId> = repo
        .log(&zone, Some(&branch), usize::MAX)
        .unwrap()
        .into_iter()
        .map(|c| c.0)
        .collect();
    chain.reverse();
    println!("history: {} commits, publishing {n} prefixes", chain.len());
    let backend = open_backend(&env("MEASURE_BACKEND", "dir:/tmp/compact-measure"));
    let raw: &dyn Backend = backend.as_ref();

    let pub_c = CountingBackend::new(raw);
    let mut old = None;
    for k in 1..=n {
        let new = chain[(k * chain.len() / n).max(1) - 1];
        if Some(new) == old {
            continue;
        }
        Publisher::new(&pub_c, repo.store(), tx(&zone, &branch, old, new)).run().unwrap();
        old = Some(new);
    }
    stats_line("publish x N", pub_c.stats());
    let (np, bytes) = remote_bytes(raw);
    println!("  remote: {np} packs, {bytes} bytes (packs + 44 B/object index)");
    let before = full_fetch(raw);
    stats_line("full fetch, before compaction", before);

    // the union of objects, for the layout experiment (read through the uncounted handle)
    let mut ids = vec![];
    for (_, idx) in load_packs(raw).unwrap() {
        ids.extend(idx.iter().map(|e| e.0));
    }
    ids.sort();
    ids.dedup();
    let objs: Vec<(ObjectId, Vec<u8>)> = fetch_many(raw, &ids).unwrap();
    // a mid-sized file blob from the middle of the history
    let probe = objs
        .iter()
        .filter(|o| o.1[0] == 1 && o.1.len() > 4000)
        .nth(300)
        .or(objs.iter().find(|o| o.1[0] == 1))
        .unwrap()
        .0;
    stats_line("one blob by id, before", one_fetch(raw, &probe));
    let raw_total: usize = objs.iter().map(|o| o.1.len()).sum();
    println!("  union: {} objects, {raw_total} bytes uncompressed", objs.len());

    let cc = CountingBackend::new(raw);
    let rep = compact(
        &cc,
        &CompactOptions {
            dry_run: false,
            ..Default::default()
        },
    )
    .unwrap();
    stats_line("compaction (tiered, default)", cc.stats());
    println!(
        "  merges {}, packs {} -> {}, duplicates dropped {}",
        rep.merges.len(),
        rep.packs_before,
        rep.packs_after,
        rep.duplicates
    );
    let (np, bytes2) = remote_bytes(raw);
    println!("  remote after tiered: {np} packs, {bytes2} bytes");
    stats_line("full fetch, after tiered", full_fetch(raw));
    stats_line("one blob by id, after tiered", one_fetch(raw, &probe));
    let ca = CountingBackend::new(raw);
    let rep = compact(
        &ca,
        &CompactOptions {
            dry_run: false,
            all: true,
            ..Default::default()
        },
    )
    .unwrap();
    stats_line("compaction (all into one)", ca.stats());
    let (np, bytes3) = remote_bytes(raw);
    println!(
        "  remote after all: {np} packs, {bytes3} bytes (merged {} packs)",
        rep.merges.first().map(|m| m.0.len()).unwrap_or(0)
    );
    stats_line("full fetch, after all", full_fetch(raw));
    stats_line("one blob by id, after all", one_fetch(raw, &probe));

    // layout experiment: stored bytes of the same objects
    let frame = |o: &(ObjectId, Vec<u8>)| vcs_store::pack::encode_payload(vcs_store::pack::PackFormat::V2, &o.1).len() + 4;
    let per_object: usize = objs.iter().map(frame).sum();
    println!(
        "  per-object zstd-3 entries (current v2): {per_object} bytes ({:.2}x of raw)",
        raw_total as f64 / per_object as f64
    );
    let ordered = vcs_store::layout::locality_order(objs.clone());
    for (name, list) in [("id order", &objs), ("commits, trees, files by path", &ordered)] {
        for block in [64 * 1024usize, 1 << 20, 4 << 20] {
            let mut total = 0usize;
            let mut buf: Vec<u8> = Vec::new();
            let flush = |buf: &mut Vec<u8>, total: &mut usize| {
                if !buf.is_empty() {
                    *total += zstd::bulk::compress(buf, 3).unwrap().len();
                    buf.clear();
                }
            };
            for o in list.iter() {
                if !buf.is_empty() && buf.len() + o.1.len() + 4 > block {
                    flush(&mut buf, &mut total);
                }
                buf.extend_from_slice(&(o.1.len() as u32).to_le_bytes());
                buf.extend_from_slice(&o.1);
            }
            flush(&mut buf, &mut total);
            println!(
                "  blocks {:>7} KiB, {name:<30} {total:>12} bytes ({:.2}x vs per-object)",
                block / 1024,
                per_object as f64 / total as f64
            );
        }
    }
}
