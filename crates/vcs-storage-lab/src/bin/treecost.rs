//! Experiment H3: measured cost of ONE path-limited tree diff (commit vs first parent) in the real object store,
//! plus the cost of a commit-graph-less `log` step. Builds a real repo with vcs-repo and vcs-bench generators.
//! usage: treecost [--files 5000] [--commits 300] [--out f.jsonl]
use std::hint::black_box;
use std::path::Path;
use vcs_bench::{content_of, generate, path_of, Layout, Rng};
use vcs_objects::{EntryMode, Object, ObjectId, Signature};
use vcs_repo::Repo;
use vcs_storage_lab::util::*;

fn changed(store: &dyn vcs_store::ObjectStore, a: &ObjectId, b: &ObjectId, comps: &[&str]) -> (bool, u32) {
    // path-limited: descend one component at a time; stop as soon as ids are equal. Counts tree reads.
    let (mut x, mut y) = (*a, *b);
    let mut reads = 0;
    for c in comps {
        if x == y {
            return (false, reads);
        }
        let (tx, ty) = match (store.get(&x), store.get(&y)) {
            (Ok(Object::Tree(tx)), Ok(Object::Tree(ty))) => (tx, ty),
            _ => return (true, reads),
        };
        reads += 2;
        match (tx.get(c), ty.get(c)) {
            (Some(ex), Some(ey)) => {
                x = ex.id;
                y = ey.id;
                if ex.mode != EntryMode::Directory {
                    return (ex.id != ey.id, reads);
                }
            }
            (None, None) => return (false, reads),
            _ => return (true, reads),
        }
    }
    (x != y, reads)
}

fn main() {
    let files: usize = arg_or("--files", 5000);
    let commits: usize = arg_or("--commits", 300);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()));
    let d = tempfile::tempdir_in(std::env::var("LAB_DIR").unwrap_or_else(|_| "/tmp".into())).unwrap();
    let all = generate(d.path(), Layout::Deep, files, 42).unwrap();
    let repo = Repo::init(d.path()).unwrap();
    let sig = |t| Signature { name: "L".into(), email: "l@example.invalid".into(), timestamp: t };
    repo.add(&[String::new()]).unwrap();
    repo.commit("c0", sig(1)).unwrap();
    let mut r = Rng::new(9);
    let mut touched = vec![0u32; files];
    let t0 = std::time::Instant::now();
    for c in 1..commits {
        for _ in 0..3 {
            // skewed choice (square of a uniform) so some files are hot
            let u = (r.next_u64() % 1_000_000) as f64 / 1e6;
            let i = ((u * u) * files as f64) as usize % files;
            touched[i] += 1;
            std::fs::write(d.path().join(&all[i]), content_of(1000 + c as u64, i)).unwrap();
        }
        repo.add(&[String::new()]).unwrap();
        repo.commit(&format!("c{c}"), sig(1 + c as u64)).unwrap();
    }
    let build_s = t0.elapsed().as_secs_f64();
    let log = repo.log("root", None, commits + 1).unwrap();
    let store = repo.store();
    let hot = (0..files).max_by_key(|&i| touched[i]).unwrap();
    let target = path_of(Layout::Deep, hot);
    let comps: Vec<&str> = target.split('/').collect();
    let n = log.len();
    let measure = |label: &str, out: &mut Out| {
        let mut reads = 0u64;
        let mut hits = 0;
        let tr = trials(5, n - 1, || {
            for w in log.windows(2) {
                let (c, p) = (&w[0].1, &w[1].1);
                let (ch, rd) = changed(store, &p.root, &c.root, &comps);
                black_box(ch);
                reads = rd as u64 + reads;
            }
        });
        for w in log.windows(2) {
            hits += changed(store, &w[1].1.root, &w[0].1.root, &comps).0 as u32;
        }
        out.row(Row::new().s("exp", "tree_diff_cost").s("storage", label).i("files", files as u64).i("commits", n as u64).i("path_depth", comps.len() as u64)
            .n("ns_per_commit_diff_med", tr.median).n("ns_min", tr.min).n("ns_max", tr.max).n("tree_reads_per_diff_avg", reads as f64 / (5 * (n - 1)) as f64)
            .i("commits_where_path_changed", hits as u64).s("state", "warm page cache, hash verified on every read"));
    };
    measure("loose", &mut out);
    let _ = repo.repack();
    measure("packed(one pack)", &mut out);
    // cost to read a commit object alone (what a graph-less log step pays)
    let tr = trials(5, n, || {
        for (id, _) in &log {
            black_box(store.get(id).unwrap());
        }
    });
    out.row(Row::new().s("exp", "commit_read_cost").s("storage", "packed").n("ns_per_commit_read_med", tr.median).n("ns_max", tr.max));
    out.row(Row::new().s("exp", "repo_build").n("seconds", build_s).i("commits", n as u64));
    let _ = Path::new("");
}
