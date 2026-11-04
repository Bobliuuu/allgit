//! Direct-to-store publishers (no server) against an in-memory backend with a fixed per-request latency (an S3-like
//! round trip), single manifest vs per-zone sharded manifest. IN-MEMORY + INJECTED LATENCY, not a real object store.
//!
//! usage: shardbench single|sharded <writers> <pushes-per-writer> <latency-ms> [same]
//!   default workload: writer i pushes zone `z<i>` (disjoint). `same`: all writers share zone `z0`, branch `b<i>`.
//! Output: one JSON line.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};
use vcs_objects::*;
use vcs_remote::sharded::{migrate_to_sharded, ShardedBackend};
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

struct Lat {
    inner: Arc<MemBackend>,
    ms: u64,
    cas_lost: Arc<AtomicU64>,
    requests: Arc<AtomicU64>,
}
impl Lat {
    fn nap(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        if self.ms > 0 {
            std::thread::sleep(Duration::from_millis(self.ms));
        }
    }
}
impl Backend for Lat {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.nap();
        self.inner.get(k)
    }
    fn get_conditional(&self, k: &str, h: &str) -> Result<Conditional, BackendError> {
        self.nap();
        self.inner.get_conditional(k, h)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.nap();
        self.inner.get_range(k, o, l)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.nap();
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.nap();
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.nap();
        let r = self.inner.put_if_match(k, d, t)?;
        if r.is_none() {
            self.cas_lost.fetch_add(1, Ordering::Relaxed);
        }
        Ok(r)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.nap();
        self.inner.list(p)
    }
    fn delete(&self, k: &str) -> Result<(), BackendError> {
        self.inner.delete(k)
    }
}

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
fn tx(zone: &str, branch: &str, old: Option<ObjectId>, new: ObjectId) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old,
            new,
        }],
        author: sig(),
        message: "m".into(),
        lock_ops: vec![],
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: shardbench single|sharded <writers> <pushes> <latency-ms> [same]");
        std::process::exit(2);
    }
    let sharded = a[1] == "sharded";
    let (n, k, ms): (usize, usize, u64) = (a[2].parse().unwrap(), a[3].parse().unwrap(), a[4].parse().unwrap());
    let same = a.get(5).is_some_and(|x| x == "same");
    let mem = Arc::new(MemBackend::new());
    // seed every zone once on the raw store, then migrate (not timed)
    let seed = MemoryStore::new();
    for i in 0..n {
        let zone = if same { "z0".to_owned() } else { format!("z{i}") };
        let branch = if same { format!("b{i}") } else { "main".to_owned() };
        let c = commit(&seed, &zone, None, &format!("seed{i}"));
        Publisher::new(&*mem, &seed, tx(&zone, &branch, None, c)).run().unwrap();
    }
    if sharded {
        migrate_to_sharded(&*mem).unwrap();
    }
    let cas_lost = Arc::new(AtomicU64::new(0));
    let requests = Arc::new(AtomicU64::new(0));
    let bar = Arc::new(Barrier::new(n + 1));
    let mut hs = vec![];
    for i in 0..n {
        let (mem, cas_lost, requests, bar) = (mem.clone(), cas_lost.clone(), requests.clone(), bar.clone());
        hs.push(std::thread::spawn(move || {
            let lat = Lat {
                inner: mem.clone(),
                ms,
                cas_lost,
                requests,
            };
            let backend: Box<dyn Backend> = if sharded {
                Box::new(ShardedBackend::new(Box::new(lat)))
            } else {
                Box::new(lat)
            };
            let store = MemoryStore::new();
            let zone = if same { "z0".to_owned() } else { format!("z{i}") };
            let branch = if same { format!("b{i}") } else { "main".to_owned() };
            let mut head = load_manifest(&*backend).unwrap().0.head(&zone, &branch);
            let mut lats = vec![];
            bar.wait();
            for j in 0..k {
                let t0 = Instant::now();
                let new = commit(&store, &zone, head, &format!("w{i}-{j}"));
                Publisher::new(&*backend, &store, tx(&zone, &branch, head, new))
                    .run()
                    .expect("publish");
                head = Some(new);
                lats.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            lats
        }));
    }
    bar.wait();
    let t0 = Instant::now();
    let mut lat: Vec<f64> = hs.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let wall = t0.elapsed().as_secs_f64();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
    let total = (n * k) as f64;
    println!(
        "{{\"mode\":\"{}\",\"workload\":\"{}\",\"writers\":{n},\"pushes\":{},\"latency_ms\":{ms},\"pps\":{:.1},\"p50_ms\":{:.0},\"p95_ms\":{:.0},\"p99_ms\":{:.0},\"cas_lost\":{},\"requests_per_publish\":{:.1}}}",
        a[1],
        if same { "same-zone" } else { "disjoint-zones" },
        n * k,
        total / wall,
        p(0.5),
        p(0.95),
        p(0.99),
        cas_lost.load(Ordering::Relaxed),
        requests.load(Ordering::Relaxed) as f64 / total
    );
}
