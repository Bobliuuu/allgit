//! Measurements for the per-zone commit-graph and automatic repack (issues #241, #247).
//!
//!   zonebench graph <commits-per-zone> [zones=8] [new=10]   graph refresh cost after new commits
//!   zonebench loose <objects>                               disk overhead before/after repack
//! Prints one JSON line per run. The `graph` mode only uses API present before and after the
//! per-zone change, so the same file can be run on both trees.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use vcs_objects::{ObjectId, Signature};
use vcs_repo::Repo;
use vcs_store::{LocalStore, ObjectStore};

fn sig(ts: u64) -> Signature {
    Signature {
        name: "B".into(),
        email: "b@x".into(),
        timestamp: ts,
    }
}

fn graph_files(vcs: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let old = vcs.join("commit-graph");
    if let Ok(b) = fs::read(&old) {
        out.insert(old, b);
    }
    if let Ok(rd) = fs::read_dir(vcs.join("commit-graphs")) {
        for e in rd.flatten() {
            if !e.file_name().to_string_lossy().contains('.') {
                out.insert(e.path(), fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

fn head(r: &Repo, z: &str) -> ObjectId {
    r.branches(z).unwrap().into_iter().find_map(|(_, h, _)| h).unwrap()
}

fn commit(r: &Repo, z: &str, i: usize) {
    fs::write(r.worktree().join(z).join(format!("f{i}")), format!("{z} {i}")).unwrap();
    r.add(&[format!("{z}/f{i}")]).unwrap();
    r.commit(&format!("c{i}"), sig(1000 + i as u64)).unwrap();
}

fn alloc_bytes(dir: &Path) -> (u64, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let (mut app, mut alloc, mut files) = (0, 0, 0);
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                app += m.len();
                alloc += m.blocks() * 512;
                files += 1;
            }
        }
    }
    (app, alloc, files)
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let n = |i: usize, d: usize| a.get(i).and_then(|x| x.parse().ok()).unwrap_or(d);
    match a.first().map(String::as_str) {
        Some("graph") => {
            let (per, zones, new, touched) = (n(1, 1000), n(2, 8), n(3, 10), n(4, 8));
            let d = tempfile::tempdir().unwrap();
            let r = Repo::init(&d.path().join("r")).unwrap();
            let names: Vec<String> = (0..zones).map(|i| format!("z{i}")).collect();
            for z in &names {
                fs::create_dir_all(r.worktree().join(z)).unwrap();
                r.create_zone(z, z).unwrap();
            }
            for i in 0..per {
                for z in &names {
                    commit(&r, z, i);
                }
            }
            let refresh = |r: &Repo| {
                for z in &names {
                    let h = head(r, z);
                    r.merge_bases(&h, &h).unwrap();
                }
            };
            refresh(&r);
            let vcs = r.worktree().join(".vcs");
            let warm = graph_files(&vcs);
            for i in 0..new {
                commit(&r, &names[i % touched.min(zones)], per + i);
            }
            let t0 = Instant::now();
            refresh(&r);
            let dt = t0.elapsed();
            let after = graph_files(&vcs);
            let rewritten: u64 = after
                .iter()
                .filter(|(p, b)| warm.get(*p) != Some(*b))
                .map(|(_, b)| b.len() as u64)
                .sum();
            let files_rewritten = after.iter().filter(|(p, b)| warm.get(*p) != Some(*b)).count();
            let total: u64 = after.values().map(|b| b.len() as u64).sum();
            println!(
                "{{\"exp\":\"graph\",\"zones\":{zones},\"commits_per_zone\":{per},\"new_commits\":{new},\"zones_touched\":{},\"refresh_ms\":{:.2},\"graph_files\":{},\"graph_bytes_total\":{total},\"graph_bytes_rewritten\":{rewritten},\"files_rewritten\":{files_rewritten}}}",
                touched.min(zones),
                dt.as_secs_f64() * 1e3,
                after.len()
            );
        }
        Some("loose") => {
            let count = n(1, 20000);
            let d = tempfile::tempdir().unwrap();
            let s = LocalStore::open(d.path()).unwrap();
            let mut logical = 0u64;
            for i in 0..count {
                let body = format!("object {i} {}", blake3::hash(&i.to_le_bytes()).to_hex());
                logical += body.len() as u64;
                s.put_blob(body.as_bytes()).unwrap();
            }
            s.sync().unwrap();
            let (app0, al0, f0) = alloc_bytes(d.path());
            let t0 = Instant::now();
            let packed = s
                .repack_older_than(SystemTime::now() + Duration::from_secs(7200), Duration::from_secs(3600))
                .unwrap();
            let dt = t0.elapsed();
            let (app1, al1, f1) = alloc_bytes(d.path());
            println!(
                "{{\"exp\":\"loose\",\"objects\":{count},\"logical_bytes\":{logical},\"before\":{{\"files\":{f0},\"apparent\":{app0},\"allocated\":{al0},\"x_logical\":{:.2}}},\"after\":{{\"files\":{f1},\"apparent\":{app1},\"allocated\":{al1},\"x_logical\":{:.2}}},\"packed\":{packed},\"repack_ms\":{:.1}}}",
                al0 as f64 / logical as f64,
                al1 as f64 / logical as f64,
                dt.as_secs_f64() * 1e3
            );
        }
        _ => eprintln!("usage: zonebench graph <commits-per-zone> [zones] [new] | loose <objects>"),
    }
}
