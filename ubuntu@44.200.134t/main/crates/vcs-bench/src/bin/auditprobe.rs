//! Read-only measurement probe for the storage audit (docs/research/storage/architecture-audit.md).
//! It only calls public APIs of the existing crates and changes no default behaviour.
//! Every subcommand prints one JSON object per line on stdout. Usage: auditprobe <exp> [args].
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;
use vcs_bench::{content_of, generate, path_of, Layout, Rng};
use vcs_objects::{id_of_encoded, Object, ObjectId, Signature};
use vcs_remote::{Backend, BackendError, FsBackend, MemBackend, Versioned};
use vcs_repo::Repo;
use vcs_store::{pack::write_pack, LocalStore, ObjectStore};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "B".into(),
        email: "b@example.invalid".into(),
        timestamp: ts,
    }
}

/// Per-method request and byte counters (CountingBackend only has a total).
struct Typed<'a> {
    inner: &'a dyn Backend,
    get: AtomicU64,
    range: AtomicU64,
    head: AtomicU64,
    put_abs: AtomicU64,
    put_cas: AtomicU64,
    list: AtomicU64,
    down: AtomicU64,
    up: AtomicU64,
}
impl<'a> Typed<'a> {
    fn new(inner: &'a dyn Backend) -> Self {
        let z = || AtomicU64::new(0);
        Typed {
            inner,
            get: z(),
            range: z(),
            head: z(),
            put_abs: z(),
            put_cas: z(),
            list: z(),
            down: z(),
            up: z(),
        }
    }
    fn json(&self) -> String {
        let g = |a: &AtomicU64| a.load(Relaxed);
        let total = g(&self.get) + g(&self.range) + g(&self.head) + g(&self.put_abs) + g(&self.put_cas) + g(&self.list);
        format!(
            "{{\"requests\":{total},\"get\":{},\"get_range\":{},\"head\":{},\"put_if_absent\":{},\"put_if_match\":{},\"list\":{},\"bytes_down\":{},\"bytes_up\":{}}}",
            g(&self.get), g(&self.range), g(&self.head), g(&self.put_abs), g(&self.put_cas), g(&self.list), g(&self.down), g(&self.up)
        )
    }
}
impl Backend for Typed<'_> {
    fn get(&self, k: &str) -> Result<Option<Versioned>, BackendError> {
        self.get.fetch_add(1, Relaxed);
        let r = self.inner.get(k)?;
        if let Some(v) = &r {
            self.down.fetch_add(v.data.len() as u64, Relaxed);
        }
        Ok(r)
    }
    fn get_range(&self, k: &str, o: u64, l: u64) -> Result<Option<Vec<u8>>, BackendError> {
        self.range.fetch_add(1, Relaxed);
        let r = self.inner.get_range(k, o, l)?;
        if let Some(v) = &r {
            self.down.fetch_add(v.len() as u64, Relaxed);
        }
        Ok(r)
    }
    fn exists(&self, k: &str) -> Result<bool, BackendError> {
        self.head.fetch_add(1, Relaxed);
        self.inner.exists(k)
    }
    fn put_if_absent(&self, k: &str, d: &[u8]) -> Result<bool, BackendError> {
        self.put_abs.fetch_add(1, Relaxed);
        self.up.fetch_add(d.len() as u64, Relaxed);
        self.inner.put_if_absent(k, d)
    }
    fn put_if_match(&self, k: &str, d: &[u8], t: &str) -> Result<Option<String>, BackendError> {
        self.put_cas.fetch_add(1, Relaxed);
        self.up.fetch_add(d.len() as u64, Relaxed);
        self.inner.put_if_match(k, d, t)
    }
    fn list(&self, p: &str) -> Result<Vec<String>, BackendError> {
        self.list.fetch_add(1, Relaxed);
        self.inner.list(p)
    }
}

fn layout(s: &str) -> Layout {
    Layout::parse(s).expect("layout flat|deep|bigdir")
}
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}
fn spread(v: &[f64]) -> (f64, f64) {
    (v.iter().cloned().fold(f64::MAX, f64::min), v.iter().cloned().fold(0.0, f64::max))
}
fn dir_size(d: &Path) -> (u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let (mut app, mut alloc, mut n) = (0, 0, 0);
    let mut st = vec![d.to_owned()];
    while let Some(x) = st.pop() {
        for e in std::fs::read_dir(&x).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                st.push(e.path());
            } else {
                app += m.len();
                alloc += m.blocks() * 512;
                n += 1;
            }
        }
    }
    (app, alloc, n)
}
fn rchar() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("rchar: ").map(|v| v.trim().parse().unwrap()))
        .unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let exp = a.first().map(String::as_str).unwrap_or("");
    let n = |i: usize| -> usize { a[i].parse().expect("number") };
    match exp {
        // publish-req <layout> <files>: backend requests of a first publish and of a 1-file incremental publish.
        "publish-req" => {
            let t = tempfile::tempdir().unwrap();
            let paths = generate(t.path(), layout(&a[1]), n(2), 7).unwrap();
            let repo = Repo::init(t.path()).unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("c1", sig(1)).unwrap();
            let mem = MemBackend::new();
            let c1 = Typed::new(&mem);
            repo.publish(&c1, None, sig(2)).unwrap();
            std::fs::write(t.path().join(&paths[paths.len() / 2]), b"changed once").unwrap();
            repo.add(&[paths[paths.len() / 2].clone()]).unwrap();
            repo.commit("c2", sig(3)).unwrap();
            let c2 = Typed::new(&mem);
            repo.publish(&c2, None, sig(4)).unwrap();
            let packs = mem.list("packs/").unwrap().len();
            println!("{{\"exp\":\"publish-req\",\"layout\":\"{}\",\"files\":{},\"first\":{},\"incremental_1_file\":{},\"remote_pack_keys\":{packs}}}", a[1], n(2), c1.json(), c2.json());
        }
        // sparse <zones> <files-per-zone> <commits>: clone one zone vs all zones; fetch after one new commit.
        "sparse" => {
            let (z, fpz, commits) = (n(1), n(2), n(3));
            let t = tempfile::tempdir().unwrap();
            let repo = Repo::init(t.path()).unwrap();
            for i in 0..z {
                std::fs::create_dir_all(t.path().join(format!("z{i}"))).unwrap();
                repo.create_zone(&format!("z{i}"), &format!("z{i}")).unwrap();
            }
            for i in 0..z {
                for f in 0..fpz {
                    let p = t.path().join(format!("z{i}/d{}/f{f}.txt", f / 50));
                    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                    std::fs::write(p, content_of(i as u64, f)).unwrap();
                }
            }
            repo.add(&[String::new()]).unwrap();
            repo.commit("c0", sig(1)).unwrap();
            for c in 1..commits {
                for i in 0..z {
                    std::fs::write(t.path().join(format!("z{i}/d0/f0.txt")), format!("rev {c}")).unwrap();
                }
                repo.add(&[String::new()]).unwrap();
                repo.commit(&format!("c{c}"), sig(1 + c as u64)).unwrap();
            }
            let mem = MemBackend::new();
            repo.publish(&mem, None, sig(99)).unwrap();
            let man = mem.get("manifest").unwrap().unwrap().data.len();
            let all_keys = mem.list("objects/").unwrap().len();
            let mut res = Vec::new();
            for (label, zs) in [("one_zone", Some(vec!["z0".to_string()])), ("all_zones", None)] {
                let d = tempfile::tempdir().unwrap();
                let ty = Typed::new(&mem);
                let t0 = Instant::now();
                let r = Repo::clone_from(&ty, "mem", &d.path().join("c"), zs.as_deref()).unwrap();
                let ms = t0.elapsed().as_secs_f64() * 1e3;
                let (app, alloc, files) = dir_size(&d.path().join("c/.vcs/objects"));
                let wt_files = r.status().map(|s| s.zones.len()).unwrap_or(0);
                res.push(format!("\"{label}\":{{\"backend\":{},\"ms\":{ms:.1},\"local_objects_files\":{files},\"local_bytes\":{app},\"local_alloc\":{alloc},\"subscribed_zones\":{wt_files}}}", ty.json()));
            }
            // incremental fetch after one new commit in z0 (published by the origin)
            std::fs::write(t.path().join("z0/d0/f0.txt"), "new head").unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("cN", sig(500)).unwrap();
            repo.publish(&mem, None, sig(501)).unwrap();
            let d = tempfile::tempdir().unwrap();
            let r = Repo::clone_from(&mem, "mem", &d.path().join("c"), Some(&["z0".to_string()])).unwrap();
            // second origin commit then incremental fetch with a counter
            std::fs::write(t.path().join("z0/d0/f0.txt"), "new head 2").unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("cN2", sig(600)).unwrap();
            repo.publish(&mem, None, sig(601)).unwrap();
            let ty = Typed::new(&mem);
            r.fetch(&ty, None).unwrap();
            println!("{{\"exp\":\"sparse\",\"zones\":{z},\"files_per_zone\":{fpz},\"commits\":{commits},\"manifest_bytes\":{man},\"remote_loose_objects\":{all_keys},{},\"incremental_fetch_one_new_commit\":{}}}", res.join(","), ty.json());
        }
        // history <commits> <files>: GETs needed to fetch the commit/tree history lazily, and lazy blob fetch.
        "history" => {
            let (commits, files) = (n(1), n(2));
            let t = tempfile::tempdir().unwrap();
            let paths = generate(t.path(), Layout::Flat, files, 3).unwrap();
            let repo = Repo::init(t.path()).unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("c0", sig(1)).unwrap();
            for c in 1..commits {
                std::fs::write(t.path().join(&paths[c % files]), format!("rev {c}")).unwrap();
                repo.add(&[paths[c % files].clone()]).unwrap();
                repo.commit(&format!("c{c}"), sig(1 + c as u64)).unwrap();
            }
            let mem = MemBackend::new();
            repo.publish(&mem, None, sig(9999)).unwrap();
            let d = tempfile::tempdir().unwrap();
            let ty = Typed::new(&mem);
            Repo::clone_from(&ty, "mem", &d.path().join("c"), None).unwrap();
            println!(
                "{{\"exp\":\"history\",\"commits\":{commits},\"files\":{files},\"clone\":{}}}",
                ty.json()
            );
        }
        // manifest-size <zones> <branches-per-zone> <applied>: bytes of the single manifest object.
        "manifest-size" => {
            use vcs_remote::Manifest;
            let (z, b, ap) = (n(1), n(2), n(3));
            let mut m = Manifest::default();
            for i in 0..z {
                for j in 0..b {
                    m.branches.insert(
                        (format!("zone{i:05}"), format!("branch{j:03}")),
                        ObjectId::from_bytes([(i + j) as u8; 32]),
                    );
                }
                m.zones.insert(format!("zone{i:05}"), format!("path/to/zone{i:05}"));
            }
            for i in 0..ap {
                let mut x = [0u8; 32];
                x[..8].copy_from_slice(&(i as u64).to_le_bytes());
                m.applied.push(ObjectId::from_bytes(x));
            }
            println!(
                "{{\"exp\":\"manifest-size\",\"zones\":{z},\"branches_per_zone\":{b},\"applied\":{ap},\"bytes\":{}}}",
                m.encode().len()
            );
        }
        // mkpacks <dir> <objects> <packs>: write `packs` packs holding `objects` 200-byte blobs in total.
        "mkpacks" => {
            let (dir, total, packs) = (Path::new(&a[1]), n(2), n(3));
            let per = total / packs;
            let mut rng = Rng::new(5);
            std::fs::create_dir_all(dir.join("tmp")).unwrap();
            for _ in 0..packs {
                let objs: Vec<_> = (0..per)
                    .map(|_| {
                        let e = Object::Blob(rng.bytes(200)).encode();
                        (id_of_encoded(&e), e)
                    })
                    .collect();
                write_pack(&dir.join("pack"), &dir.join("tmp"), objs).unwrap();
            }
        }
        // openpacks <dir>: LocalStore::open time (parses + blake3-verifies every .idx) and miss-lookup cost.
        "openpacks" => {
            let dir = Path::new(&a[1]);
            let idx_bytes: u64 = std::fs::read_dir(dir.join("pack"))
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "idx"))
                .map(|e| e.metadata().unwrap().len())
                .sum();
            let mut open = Vec::new();
            for _ in 0..7 {
                let t0 = Instant::now();
                let s = LocalStore::open(dir).unwrap();
                open.push(t0.elapsed().as_secs_f64() * 1e3);
                drop(s);
            }
            let s = LocalStore::open(dir).unwrap();
            let miss = ObjectId::from_bytes([0xEE; 32]);
            let mut lk = Vec::new();
            for _ in 0..7 {
                let t0 = Instant::now();
                for i in 0..20000u32 {
                    let mut x = [0u8; 32];
                    x[..4].copy_from_slice(&i.to_le_bytes());
                    let _ = s.contains(&ObjectId::from_bytes(x));
                }
                let _ = s.contains(&miss);
                lk.push(t0.elapsed().as_secs_f64() * 1e9 / 20001.0);
            }
            let (olo, ohi) = spread(&open);
            let (llo, lhi) = spread(&lk);
            println!("{{\"exp\":\"openpacks\",\"dir\":\"{}\",\"idx_bytes\":{idx_bytes},\"open_ms_median\":{:.2},\"open_ms_min\":{olo:.2},\"open_ms_max\":{ohi:.2},\"contains_miss_ns_median\":{:.0},\"contains_miss_ns_min\":{llo:.0},\"contains_miss_ns_max\":{lhi:.0}}}", a[1], median(&mut open), median(&mut lk));
        }
        // mkloose <dir> <n>: n small loose objects, synced; with arg 3 = "pack" then repacked.
        "mkloose" => {
            let s = LocalStore::open(Path::new(&a[1])).unwrap();
            let mut rng = Rng::new(11);
            let mut ids = Vec::new();
            for _ in 0..n(2) {
                ids.push(s.put_blob(&rng.bytes(300)).unwrap());
            }
            s.sync().unwrap();
            if a.get(3).map(String::as_str) == Some("pack") {
                s.repack().unwrap();
            }
            let txt: String = ids.iter().map(|i| i.to_hex() + "\n").collect();
            std::fs::write(Path::new(&a[1]).join("ids.txt"), txt).unwrap();
            let (app, alloc, files) = dir_size(Path::new(&a[1]));
            println!("{{\"exp\":\"mkloose\",\"objects\":{},\"packed\":{},\"files\":{files},\"apparent_bytes\":{app},\"allocated_bytes\":{alloc}}}", n(2), a.get(3).is_some());
        }
        // getall <dir> <k>: get() the first k ids of ids.txt (for strace -c).
        "getall" => {
            let s = LocalStore::open(Path::new(&a[1])).unwrap();
            let ids: Vec<ObjectId> = std::fs::read_to_string(Path::new(&a[1]).join("ids.txt"))
                .unwrap()
                .lines()
                .map(|l| ObjectId::from_hex(l).unwrap())
                .collect();
            let t0 = Instant::now();
            for id in ids.iter().take(n(2)) {
                s.get(id).unwrap();
            }
            println!(
                "{{\"exp\":\"getall\",\"gets\":{},\"us_per_get\":{:.1}}}",
                n(2).min(ids.len()),
                t0.elapsed().as_secs_f64() * 1e6 / n(2).min(ids.len()) as f64
            );
        }
        // putdup <dir> <k>: put the same k existing objects again (dedup path), for strace -c.
        "putdup" => {
            let s = LocalStore::open(Path::new(&a[1])).unwrap();
            let ids: Vec<ObjectId> = std::fs::read_to_string(Path::new(&a[1]).join("ids.txt"))
                .unwrap()
                .lines()
                .map(|l| ObjectId::from_hex(l).unwrap())
                .collect();
            let mut rng = Rng::new(11);
            for _ in ids.iter().take(n(2)) {
                s.put_blob(&rng.bytes(300)).unwrap();
            }
            s.sync().unwrap();
            println!("{{\"exp\":\"putdup\",\"puts\":{}}}", n(2));
        }
        // gcread <files>: bytes read by gc_plan (rchar) versus bytes in the store.
        "gcread" => {
            let t = tempfile::tempdir().unwrap();
            let mut rng = Rng::new(21);
            for i in 0..n(1) {
                let p = t.path().join(format!("d{}/f{i}.bin", i / 100));
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, rng.bytes(if i % 50 == 0 { 300_000 } else { 2_000 })).unwrap();
            }
            let repo = Repo::init(t.path()).unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("c", sig(1)).unwrap();
            let (app, _, files) = dir_size(&t.path().join(".vcs/objects"));
            let before = rchar();
            let t0 = Instant::now();
            let rep = repo.gc_plan(std::time::Duration::from_secs(14 * 86400)).unwrap();
            let ms = t0.elapsed().as_secs_f64() * 1e3;
            let read = rchar() - before;
            println!("{{\"exp\":\"gcread\",\"files\":{},\"store_objects\":{files},\"store_bytes\":{app},\"gc_plan_rchar\":{read},\"ratio\":{:.2},\"ms\":{ms:.1},\"total\":{},\"reachable\":{}}}", n(1), read as f64 / app as f64, rep.total, rep.reachable);
        }
        // crash: leftovers after an unsynced drop and after a pack without index.
        "crash" => {
            let t = tempfile::tempdir().unwrap();
            let dir = t.path().join("o");
            {
                let s = LocalStore::open(&dir).unwrap();
                for i in 0..10u8 {
                    s.put_blob(&[i; 64]).unwrap();
                }
                // process "dies" here: no sync()
            }
            let tmp_after_crash = std::fs::read_dir(dir.join("tmp")).unwrap().count();
            let s2 = LocalStore::open(&dir).unwrap();
            let tmp_after_reopen = std::fs::read_dir(dir.join("tmp")).unwrap().count();
            let readable = s2.contains(&vcs_objects::blob_id(&[3u8; 64])).unwrap();
            s2.put_blob(b"x").unwrap();
            s2.sync().unwrap();
            let tmp_after_next_sync = std::fs::read_dir(dir.join("tmp")).unwrap().count();
            s2.repack().unwrap();
            let pack = std::fs::read_dir(dir.join("pack"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|x| x == "idx"))
                .unwrap();
            std::fs::remove_file(&pack).unwrap(); // crash between the .pack rename and the .idx rename
            let s3 = LocalStore::open(&dir).unwrap();
            s3.put_blob(b"y").unwrap();
            s3.sync().unwrap();
            let orphan = std::fs::read_dir(dir.join("pack")).unwrap().count();
            println!("{{\"exp\":\"crash\",\"tmp_files_after_crash\":{tmp_after_crash},\"tmp_files_after_reopen\":{tmp_after_reopen},\"unsynced_object_visible_after_restart\":{readable},\"tmp_files_after_next_sync\":{tmp_after_next_sync},\"pack_dir_entries_with_idx_missing_after_reopen_and_sync\":{orphan}}}");
        }
        // opgrowth <branches> <commits>: bytes of Operation objects (full branch view per operation).
        "opgrowth" => {
            let (b, c) = (n(1), n(2));
            let t = tempfile::tempdir().unwrap();
            std::fs::write(t.path().join("f"), "0").unwrap();
            let repo = Repo::init(t.path()).unwrap();
            repo.add(&[String::new()]).unwrap();
            repo.commit("c0", sig(1)).unwrap();
            for i in 0..b {
                repo.create_branch("root", &format!("b{i}")).ok();
            }
            let base = repo.op_log(1_000_000).unwrap().len();
            for i in 0..c {
                std::fs::write(t.path().join("f"), format!("v{i}")).unwrap();
                repo.add(&[String::new()]).unwrap();
                repo.commit(&format!("m{i}"), sig(10 + i as u64)).unwrap();
            }
            let ops = repo.op_log(1_000_000).unwrap();
            let bytes: usize = ops.iter().map(|(_, o)| Object::Operation(o.clone()).encode().len()).sum();
            let last = Object::Operation(ops[0].1.clone()).encode().len();
            println!("{{\"exp\":\"opgrowth\",\"branches\":{b},\"commits\":{c},\"ops_before\":{base},\"ops_total\":{},\"op_bytes_total\":{bytes},\"last_op_bytes\":{last}}}", ops.len());
        }
        // dedup: identical content in two zones/paths and a chunked file.
        "dedup" => {
            let t = tempfile::tempdir().unwrap();
            let repo = Repo::init(t.path()).unwrap();
            std::fs::create_dir_all(t.path().join("za")).unwrap();
            std::fs::create_dir_all(t.path().join("zb")).unwrap();
            repo.create_zone("za", "za").unwrap();
            repo.create_zone("zb", "zb").unwrap();
            let body = Rng::new(1).bytes(5000);
            for p in ["za/x.bin", "zb/x.bin", "zb/copy.bin", "root.bin"] {
                std::fs::write(t.path().join(p), &body).unwrap();
            }
            repo.add(&[String::new()]).unwrap();
            repo.commit("dup", sig(1)).unwrap();
            let blobs = repo
                .store()
                .list()
                .unwrap()
                .into_iter()
                .filter(|i| matches!(repo.store().get(i), Ok(Object::Blob(b)) if b == body))
                .count();
            let big = Rng::new(2).bytes(3 << 20);
            std::fs::write(t.path().join("big.bin"), &big).unwrap();
            let before = repo.store().list().unwrap().len();
            repo.add(&["big.bin".into()]).unwrap();
            let after = repo.store().list().unwrap();
            let chunk_objs = after.len() - before;
            let whole_stored = after
                .iter()
                .any(|i| matches!(repo.store().get(i), Ok(Object::Blob(b)) if b.len() == big.len()));
            // identical bytes under a different file name / second add: no new objects
            std::fs::write(t.path().join("big2.bin"), &big).unwrap();
            repo.add(&["big2.bin".into()]).unwrap();
            let after2 = repo.store().list().unwrap().len();
            println!("{{\"exp\":\"dedup\",\"copies_of_same_5000B_file\":4,\"blob_objects_holding_it\":{blobs},\"chunk_objects_for_3MiB_file\":{chunk_objs},\"whole_3MiB_blob_also_stored\":{whole_stored},\"new_objects_for_second_identical_big_file\":{}}}", after2 - after.len());
        }
        // fslist <n>: FsBackend::list cost versus number of keys under objects/.
        "fslist" => {
            let t = tempfile::tempdir().unwrap();
            let b = FsBackend::open(t.path().join("r")).unwrap();
            for i in 0..n(1) {
                b.put_if_absent(&format!("objects/{:02x}/{i:060}", i % 256), b"x").unwrap();
            }
            let mut ts = Vec::new();
            for _ in 0..5 {
                let t0 = Instant::now();
                let l = b.list("packs/").unwrap();
                assert!(l.is_empty());
                ts.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            let (lo, hi) = spread(&ts);
            println!(
                "{{\"exp\":\"fslist\",\"keys\":{},\"list_packs_prefix_ms_median\":{:.2},\"min\":{lo:.2},\"max\":{hi:.2}}}",
                n(1),
                median(&mut ts)
            );
        }
        // hashbytes <MiB>: time for content_id (chunk+hash) versus put_content into a memory store (copies+hash).
        "putcontent" => {
            let data = Rng::new(3).bytes(n(1) << 20);
            let (mut t_id, mut t_put, mut t_h) = (vec![], vec![], vec![]);
            for _ in 0..7 {
                let t0 = Instant::now();
                std::hint::black_box(vcs_objects::content_id(&data));
                t_id.push(t0.elapsed().as_secs_f64() * 1e3);
                let s = vcs_store::MemoryStore::new();
                let t0 = Instant::now();
                s.put_content(&data).unwrap();
                t_put.push(t0.elapsed().as_secs_f64() * 1e3);
                let t0 = Instant::now();
                std::hint::black_box(blake3::hash(&data));
                t_h.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            println!("{{\"exp\":\"putcontent\",\"mib\":{},\"blake3_one_pass_ms\":{:.2},\"content_id_ms\":{:.2},\"put_content_memstore_ms\":{:.2}}}", n(1), median(&mut t_h), median(&mut t_id), median(&mut t_put));
        }
        _ => {
            eprintln!("unknown experiment; see source header");
            std::process::exit(2);
        }
    }
    let _: BTreeMap<u8, u8> = BTreeMap::new();
    let _ = path_of;
}
