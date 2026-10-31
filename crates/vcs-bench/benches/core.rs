//! Criterion benches over the public APIs of vcs-objects / vcs-store / vcs-repo.
//!
//! Sizes are capped by `VCS_BENCH_MAX` (default 10000 files). Set `VCS_BENCH_MAX=100000`
//! for the full 1k/10k/100k matrix (slow). Run with `cargo bench -p vcs-bench`.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use std::fs;
use std::path::Path;
use std::time::Duration;
use vcs_bench::{generate, large_text_pair, modify, Layout, Rng};
use vcs_objects::{blob_id, EntryMode, ObjectId, Signature};
use vcs_repo::{diff, treeops, Repo, ROOT_ZONE};
use vcs_store::{MemoryStore, ObjectStore};

const SEED: u64 = 42;

fn sizes() -> Vec<usize> {
    let max: usize = std::env::var("VCS_BENCH_MAX").ok().and_then(|s| s.parse().ok()).unwrap_or(10_000);
    [1_000, 10_000, 100_000].into_iter().filter(|n| *n <= max).collect()
}

fn layouts() -> [(&'static str, Layout); 3] {
    [("flat", Layout::Flat), ("deep", Layout::Deep), ("bigdir", Layout::BigDir)]
}

fn sig(ts: u64) -> Signature {
    Signature {
        name: "Bench".into(),
        email: "bench@example.invalid".into(),
        timestamp: ts,
    }
}

fn fresh(dir: &Path) -> Repo {
    let _ = fs::remove_dir_all(dir.join(".vcs"));
    Repo::init(dir).unwrap()
}

fn bench_hash(c: &mut Criterion) {
    let mut g = c.benchmark_group("hash_blob");
    g.sample_size(10).measurement_time(Duration::from_secs(3));
    for (name, n) in [
        ("1KB", 1usize << 10),
        ("64KB", 1 << 16),
        ("1MB", 1 << 20),
        ("16MB", 1 << 24),
        ("64MB", 1 << 26),
    ] {
        let data = Rng::new(SEED).bytes(n);
        g.throughput(Throughput::Bytes(n as u64));
        g.bench_function(name, |b| b.iter(|| blob_id(black_box(&data))));
    }
    g.finish();
}

fn tree_inputs(layout: Layout, n: usize) -> Vec<(String, EntryMode, ObjectId)> {
    (0..n)
        .map(|i| {
            let p = vcs_bench::path_of(layout, i);
            (p, EntryMode::File, blob_id(&vcs_bench::content_of(SEED, i)))
        })
        .collect()
}

fn bench_tree(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree");
    g.sample_size(10).measurement_time(Duration::from_secs(5));
    for n in sizes() {
        for (name, layout) in layouts() {
            let inputs = tree_inputs(layout, n);
            let id = format!("{name}/{n}");
            g.bench_with_input(BenchmarkId::new("build", &id), &inputs, |b, inp| {
                b.iter_batched(
                    MemoryStore::new,
                    |s| treeops::build(&s, inp.iter().map(|(p, m, i)| (p.as_str(), *m, *i))).unwrap(),
                    BatchSize::LargeInput,
                )
            });
            let s = MemoryStore::new();
            let root = treeops::build(&s, inputs.iter().map(|(p, m, i)| (p.as_str(), *m, *i))).unwrap();
            g.bench_with_input(BenchmarkId::new("flatten", &id), &root, |b, r| {
                b.iter(|| treeops::flatten(&s as &dyn ObjectStore, Some(*r), "").unwrap())
            });
        }
    }
    g.finish();
}

/// Generated working tree with an initialised repo. Returns (tempdir, paths).
fn dataset(layout: Layout, n: usize) -> (tempfile::TempDir, Vec<String>) {
    let d = tempfile::tempdir().unwrap();
    let paths = generate(d.path(), layout, n, SEED).unwrap();
    (d, paths)
}

fn bench_repo(c: &mut Criterion) {
    let mut g = c.benchmark_group("repo");
    g.sample_size(10)
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(1));
    for n in sizes() {
        for (name, layout) in layouts() {
            let id = format!("{name}/{n}");
            let (d, paths) = dataset(layout, n);

            // add: fresh repo each iteration (objects re-written; nothing in the index)
            g.bench_function(BenchmarkId::new("add_all", &id), |b| {
                b.iter_batched(|| fresh(d.path()), |r| r.add(&[String::new()]).unwrap(), BatchSize::PerIteration)
            });

            // persistent staged+committed repo for status / commit
            let r = fresh(d.path());
            r.add(&[String::new()]).unwrap();
            r.commit("initial", sig(1)).unwrap();
            // Let the generated files age past the racy-entry window (2 s) and re-stage so the
            // index is written after that. Without this, a fast `add` leaves every entry "racy"
            // and each `status` re-hashes every file, which would measure hashing, not the scan.
            std::thread::sleep(Duration::from_millis(2_200));
            r.add(&[String::new()]).unwrap();

            g.bench_function(BenchmarkId::new("status_clean", &id), |b| b.iter(|| r.status().unwrap()));

            // re-add of an unchanged, already-stored tree: measures the "object already exists" check
            g.bench_function(BenchmarkId::new("add_unchanged", &id), |b| {
                b.iter(|| r.add(&[String::new()]).unwrap())
            });

            // commit with one changed file staged
            let mut ts = 2u64;
            g.bench_function(BenchmarkId::new("commit_1_change", &id), |b| {
                b.iter_batched(
                    || {
                        ts += 1;
                        fs::write(d.path().join(&paths[0]), format!("rev {ts}\n")).unwrap();
                        r.add(std::slice::from_ref(&paths[0])).unwrap();
                        ts
                    },
                    |t| r.commit("bench", sig(t)).unwrap(),
                    BatchSize::PerIteration,
                )
            });

            // status with 1% modified (rewritten, so content really differs)
            let k = modify(d.path(), &paths, 100, SEED).unwrap();
            assert!(k >= n / 100);
            g.bench_function(BenchmarkId::new("status_1pct_modified", &id), |b| b.iter(|| r.status().unwrap()));
        }
    }
    g.finish();
}

fn bench_log(c: &mut Criterion) {
    let mut g = c.benchmark_group("log");
    g.sample_size(10).measurement_time(Duration::from_secs(5));
    let commits: usize = std::env::var("VCS_BENCH_COMMITS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(d.path()).unwrap();
    for i in 0..commits {
        fs::write(d.path().join("f.txt"), format!("rev {i}\n")).unwrap();
        r.add(&["f.txt".to_string()]).unwrap();
        r.commit(&format!("c{i}"), sig(1_000 + i as u64)).unwrap();
    }
    for limit in [10, 1_000, commits] {
        g.bench_function(BenchmarkId::new("log", format!("limit{limit}_of_{commits}")), |b| {
            b.iter(|| {
                let l = r.log(ROOT_ZONE, None, limit).unwrap();
                assert_eq!(l.len(), limit);
            })
        });
    }
    g.finish();
}

fn bench_diff(c: &mut Criterion) {
    let mut g = c.benchmark_group("unified_diff");
    g.sample_size(10).measurement_time(Duration::from_secs(5));
    for lines in [1_000, 10_000, 100_000] {
        let (a, b) = large_text_pair(lines, SEED);
        g.throughput(Throughput::Bytes(a.len() as u64));
        g.bench_function(BenchmarkId::new("1pct_changed_lines", lines), |bch| {
            bch.iter(|| diff::unified("f.txt", Some(black_box(&a)), Some(black_box(&b))))
        });
    }
    g.finish();
}

criterion_group!(benches, bench_hash, bench_tree, bench_repo, bench_log, bench_diff);
criterion_main!(benches);
