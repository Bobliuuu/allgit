//! Many-writer load driver for a running `vcs-server`: N simulated clients (threads, one connection pool each) of ONE
//! process push K commits each through the real publish protocol (`Publisher` over `HttpBackend`). No CLI process per
//! push, so hundreds of clients fit in a few processes. Run several processes (`--first-client`) to simulate more.
//!
//! usage: writerbench <url> <token> --mode same|zones|branches --clients N --pushes K [--first-client I] [--seed-zone Z]
//!        [--ticket] [--decorrelated [--retry-base-ms 5 --retry-cap-ms 2000]] [--no-backoff] [--tag T] [--think-ms MS]
//!   same      all clients push to zone `shared`, branch `main` (one branch, true serialization)
//!   branches  one zone `shared`, branch `b<i>` per client
//!   --ticket  take a per-branch ticket from the server before each push (waits instead of polling; same mode)
//!   zones     zone `z<i>`, branch `main` per client (disjoint zones)
//! Output: one JSON line: latencies (ms, per acked push), conflicts (Diverged), unknowns, errors, acked heads for audit.
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};
use vcs_objects::*;
use vcs_remote::*;
use vcs_server::HttpBackend;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "w".into(),
        email: "w@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, tag: &str) -> ObjectId {
    let blob = s.put_blob(tag.as_bytes()).unwrap();
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
    let mut cid = [0u8; 16];
    cid.copy_from_slice(&blake3::hash(tag.as_bytes()).as_bytes()[..16]);
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes(cid),
        author: sig(),
        message: tag.into(),
    }))
    .unwrap()
}

fn tx(zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId, msg: &str) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: msg.into(),
        lock_ops: vec![],
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: writerbench <url> <token> --mode same|zones|branches --clients N --pushes K [--first-client I] [--tag T]");
        std::process::exit(2);
    }
    let (url, token) = (a[1].clone(), a[2].clone());
    let opt = |n: &str| a.iter().position(|x| x == n).and_then(|i| a.get(i + 1)).cloned();
    let mode = opt("--mode").unwrap_or("same".into());
    let clients: usize = opt("--clients").unwrap().parse().unwrap();
    let pushes: usize = opt("--pushes").unwrap().parse().unwrap();
    let first: usize = opt("--first-client").map_or(0, |x| x.parse().unwrap());
    let think: u64 = opt("--think-ms").map_or(0, |x| x.parse().unwrap());
    let tag = opt("--tag").unwrap_or("r".into());
    let decor = a.iter().any(|x| x == "--decorrelated");
    let base_ms: u64 = opt("--retry-base-ms").map_or(5, |x| x.parse().unwrap());
    let cap_ms: u64 = opt("--retry-cap-ms").map_or(2000, |x| x.parse().unwrap());
    let nobackoff = a.iter().any(|x| x == "--no-backoff");
    let use_ticket = a.iter().any(|x| x == "--ticket");
    let audit_b = HttpBackend::new(&url, &token).unwrap();
    let chain_before = if mode == "same" { chain_len(&audit_b, "shared", "main") } else { 0 };
    let bar = Arc::new(Barrier::new(clients + 1));
    let out: Arc<Mutex<(Vec<f64>, u64, u64, Vec<String>, Vec<(String, String, String)>)>> = Arc::default();
    let mut hs = vec![];
    for c in 0..clients {
        let (url, token, mode, tag, bar, out) = (url.clone(), token.clone(), mode.clone(), tag.clone(), bar.clone(), out.clone());
        hs.push(
            std::thread::Builder::new()
                .stack_size(512 * 1024)
                .spawn(move || {
                    let id = first + c;
                    let (zone, branch) = match mode.as_str() {
                        "same" => ("shared".to_string(), "main".to_string()),
                        "branches" => ("shared".to_string(), format!("b{id}")),
                        _ => (format!("z{id}"), "main".to_string()),
                    };
                    let b = HttpBackend::new(&url, &token).unwrap();
                    let store = MemoryStore::new();
                    let mut lat = vec![];
                    let (mut conflicts, mut unknown) = (0u64, 0u64);
                    let mut prev_wait = 0u64;
                    let mut errs = vec![];
                    let mut head: Option<ObjectId> = if mode == "same" {
                        load_manifest_head(&b, &zone, &branch)
                    } else {
                        None
                    };
                    bar.wait();
                    for i in 0..pushes {
                        let t0 = Instant::now();
                        let mut tries = 0;
                        let ticket = if use_ticket {
                            let t = b.ticket_acquire(&zone, &branch, Duration::from_secs(900)).expect("ticket");
                            head = load_manifest_head(&b, &zone, &branch);
                            Some(t)
                        } else {
                            None
                        };
                        loop {
                            tries += 1;
                            let new = commit(&store, &zone, head, &format!("{tag}-{id}-{i}-{tries}"));
                            let mut p = Publisher::new(&b, &store, tx(&zone, &branch, head, new, &format!("{tag}-{id}-{i}")))
                                .with_principal("bench")
                                .with_clock(|| 1);
                            if nobackoff {
                                p = p.without_backoff();
                            }
                            match p.run() {
                                Ok(_) => {
                                    head = Some(new);
                                    lat.push(t0.elapsed().as_secs_f64() * 1000.0);
                                    break;
                                }
                                Err(PublishError::Diverged { found, .. }) => {
                                    conflicts += 1;
                                    head = found;
                                    // jitter before refetching: spread the retry storm of a hot branch
                                    let w = if decor {
                                        // decorrelated jitter: uniform in [base, min(cap, 3 * previous wait)]
                                        let hi = (prev_wait.max(base_ms) * 3).min(cap_ms).max(base_ms + 1);
                                        let r = (id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                                            ^ (tries as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
                                            ^ (Instant::now().elapsed().subsec_nanos() as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
                                            ^ std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .map_or(0, |d| d.subsec_nanos() as u64);
                                        let r = (r ^ (r >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                                        let w = base_ms + (r >> 11) % (hi - base_ms + 1);
                                        prev_wait = w;
                                        w
                                    } else {
                                        (2u64 << tries.min(5)) + (id as u64 * 7919 + tries as u64 * 104729) % 40
                                    };
                                    std::thread::sleep(Duration::from_millis(w));
                                    if tries > 2000 {
                                        errs.push(format!("{id}: gave up after {tries}"));
                                        break;
                                    }
                                }
                                Err(PublishError::Unknown(..)) => {
                                    unknown += 1;
                                    if tries > 40 {
                                        errs.push(format!("{id}: unknown x{tries}"));
                                        break;
                                    }
                                }
                                Err(e) => {
                                    errs.push(format!("{id}: {e}"));
                                    break;
                                }
                            }
                        }
                        if let Some(t) = ticket {
                            let _ = b.ticket_release(&zone, &branch, t);
                        }
                        if think > 0 {
                            std::thread::sleep(Duration::from_millis(think));
                        }
                    }
                    let mut o = out.lock().unwrap();
                    o.0.extend(lat);
                    o.1 += conflicts;
                    o.2 += unknown;
                    o.3.extend(errs);
                    if mode != "same" {
                        if let Some(h) = head {
                            o.4.push((zone, branch, h.to_string()));
                        }
                    }
                })
                .unwrap(),
        );
    }
    bar.wait();
    let t0 = Instant::now();
    for h in hs {
        let _ = h.join();
    }
    let wall = t0.elapsed().as_secs_f64();
    let o = out.lock().unwrap();
    let mut s = String::from("{");
    s += &format!(
        "\"wall_s\":{wall:.3},\"acked\":{},\"conflicts\":{},\"unknowns\":{},\"errors\":{:?},",
        o.0.len(),
        o.1,
        o.2,
        o.3.iter().take(5).collect::<Vec<_>>()
    );
    s += &format!(
        "\"lat_ms\":[{}],",
        o.0.iter().map(|x| format!("{x:.1}")).collect::<Vec<_>>().join(",")
    );
    if mode == "same" {
        s += &format!(
            "\"chain_before\":{chain_before},\"chain_after\":{},",
            chain_len(&audit_b, "shared", "main")
        );
    }
    s += &format!(
        "\"heads\":[{}]}}",
        o.4.iter()
            .map(|(z, b, h)| format!("[\"{z}\",\"{b}\",\"{h}\"]"))
            .collect::<Vec<_>>()
            .join(",")
    );
    println!("{s}");
}

/// Number of commits behind `zone`/`branch` in the server's manifest (first parents), 0 when the branch does not exist.
/// Audit for `--mode same`: after a run, the chain must have grown by exactly the number of acked pushes.
fn chain_len(b: &HttpBackend, zone: &str, branch: &str) -> usize {
    // one RemoteSource keeps the pack indexes: fetching each commit with fetch_object re-reads every index of the
    // store (thousands of packs after a run), which made this walk quadratic
    let mut src = vcs_remote::packfetch::RemoteSource::new(b, vcs_remote::packfetch::ReadAhead::None);
    let mut n = 0;
    let mut cur = load_manifest_head(b, zone, branch);
    while let Some(id) = cur {
        n += 1;
        cur = match src.fetch(&id) {
            Ok(Some(enc)) => match Object::decode(&enc) {
                Ok(Object::Commit(c)) => c.parents.first().copied(),
                _ => break,
            },
            _ => break,
        };
    }
    n
}

fn load_manifest_head(b: &HttpBackend, zone: &str, branch: &str) -> Option<ObjectId> {
    use vcs_remote::Backend;
    let g = b.get(manifest::KEY).ok().flatten()?;
    Manifest::decode(&g.data).ok()?.head(zone, branch)
}
