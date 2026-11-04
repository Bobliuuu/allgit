//! Experiment H4: tree/path indexes. Nested sorted directories (what trees are) versus a flat sorted path array, an FST and a hash map.
//! usage: treeidx [--files 200000] [--out f.jsonl]
use std::collections::HashMap;
use std::hint::black_box;
use vcs_storage_lab::history::{build_zone, Params, Zone};
use vcs_storage_lab::util::*;

#[global_allocator]
static A: Counting = Counting;

struct Dir {
    names: Vec<String>,
    kids: Vec<Kid>,
}
enum Kid {
    Dir(Dir),
    File(u32),
}
fn build_nested(z: &Zone, files: &[(String, u32)]) -> Dir {
    fn ins(d: &mut Dir, comps: &[&str], id: u32) {
        let c = comps[0];
        match d.names.binary_search_by(|n| n.as_str().cmp(c)) {
            Ok(i) => {
                if let Kid::Dir(sub) = &mut d.kids[i] {
                    ins(sub, &comps[1..], id)
                }
            }
            Err(i) => {
                d.names.insert(i, c.to_string());
                d.kids.insert(i, if comps.len() == 1 { Kid::File(id) } else { let mut s = Dir { names: vec![], kids: vec![] }; ins(&mut s, &comps[1..], id); Kid::Dir(s) });
            }
        }
    }
    let _ = z;
    let mut root = Dir { names: vec![], kids: vec![] };
    for (p, id) in files {
        let comps: Vec<&str> = p.split('/').collect();
        ins(&mut root, &comps, *id);
    }
    root
}
fn lookup_nested(d: &Dir, comps: &[&str]) -> Option<u32> {
    let i = d.names.binary_search_by(|n| n.as_str().cmp(comps[0])).ok()?;
    match &d.kids[i] {
        Kid::File(id) => (comps.len() == 1).then_some(*id),
        Kid::Dir(s) => lookup_nested(s, &comps[1..]),
    }
}
fn count_under(d: &Dir) -> usize {
    d.kids.iter().map(|k| match k { Kid::File(_) => 1, Kid::Dir(s) => count_under(s) }).sum()
}
fn subtree<'a>(d: &'a Dir, comps: &[&str]) -> Option<&'a Dir> {
    if comps.is_empty() { return Some(d); }
    let i = d.names.binary_search_by(|n| n.as_str().cmp(comps[0])).ok()?;
    match &d.kids[i] { Kid::Dir(s) => subtree(s, &comps[1..]), _ => None }
}

fn main() {
    let nf: usize = arg_or("--files", 200_000);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()).i("files", nf as u64));
    let p = Params { zones: 1, commits_per_zone: 10, files_per_zone: nf, branch_prob: 0.0, mean_branch_len: 1.0, mean_files_per_commit: 1.0, refactor_prob: 0.0, skew_prob: 0.0, seed: 3 };
    let z = build_zone(&p, 0);
    let mut files: Vec<(String, u32)> = z.files.iter().enumerate().map(|(i, &n)| (z.path(n), i as u32)).collect();
    files.sort();
    let max_depth = z.files.iter().map(|&f| z.depth(f)).max().unwrap();
    // queries
    let mut r = Rng::new(8);
    let hits: Vec<usize> = (0..200_000).map(|_| (r.next_u64() as usize) % files.len()).collect();
    let miss: Vec<String> = hits.iter().map(|&i| format!("{}x", files[i].0)).collect();
    // directories for subtree listing: pick 200 dir prefixes at depth 2
    let mut dirs: Vec<String> = files.iter().filter(|(p, _)| p.matches('/').count() >= 3).map(|(p, _)| p.splitn(4, '/').take(3).collect::<Vec<_>>().join("/")).collect();
    dirs.dedup();
    dirs.sort();
    dirs.dedup();
    let sub: Vec<String> = dirs.iter().take(200).cloned().collect();

    let listed: usize = sub.iter().map(|d| { let pre = format!("{d}/"); files.iter().filter(|(p, _)| p.starts_with(&pre)).count() }).sum();
    out.row(Row::new().s("exp", "tree_index_setup").i("subtree_queries", sub.len() as u64).n("mean_files_per_subtree", listed as f64 / sub.len() as f64));
    // ---- nested
    let b0 = live_bytes();
    let t = std::time::Instant::now();
    let nested = build_nested(&z, &files);
    let bs = t.elapsed().as_secs_f64();
    let held = live_bytes() - b0;
    let th = trials(5, hits.len(), || {
        let mut s = 0u64;
        for &i in &hits {
            let comps: Vec<&str> = files[i].0.split('/').collect();
            s += lookup_nested(&nested, &comps).unwrap() as u64;
        }
        black_box(s);
    });
    let tm = trials(5, miss.len(), || {
        let mut s = 0u64;
        for m in &miss {
            let comps: Vec<&str> = m.split('/').collect();
            s += lookup_nested(&nested, &comps).is_some() as u64;
        }
        black_box(s);
    });
    let tl = trials(5, sub.len(), || {
        let mut s = 0usize;
        for d in &sub {
            let comps: Vec<&str> = d.split('/').collect();
            s += count_under(subtree(&nested, &comps).unwrap());
        }
        black_box(s);
    });
    // bytes a content-addressed commit must write for a 3-file change: re-encode every ancestor directory (40 B/entry est.)
    let mut upd = 0usize;
    for _ in 0..1000 {
        let i = (r.next_u64() as usize) % files.len();
        let comps: Vec<&str> = files[i].0.split('/').collect();
        let mut d = &nested;
        let mut bytes = 0;
        for c in &comps[..comps.len() - 1] {
            bytes += 40 * d.names.len();
            let k = d.names.binary_search_by(|n| n.as_str().cmp(c)).unwrap();
            if let Kid::Dir(s) = &d.kids[k] { d = s } else { break }
        }
        bytes += 40 * d.names.len();
        upd += bytes;
    }
    out.row(Row::new().s("exp", "tree_index").s("name", "nested_sorted_dirs(in-memory model of trees)").i("files", nf as u64).i("max_depth", max_depth as u64).n("build_s", bs).n("heap_bytes_per_file", held as f64 / nf as f64)
        .n("lookup_hit_ns", th.median).n("lookup_miss_ns", tm.median).n("subtree_list_200dirs_ns_per_dir", tl.median)
        .n("bytes_written_per_one_file_change", upd as f64 / 1000.0).s("note", "bytes written = re-encoded ancestor directories (40 B/entry estimate, not measured against the codec); lookup here excludes object reads, which dominate in the product"));

    // ---- flat sorted
    let b0 = live_bytes();
    let t = std::time::Instant::now();
    let flat: Vec<(String, u32)> = files.clone();
    let bs = t.elapsed().as_secs_f64();
    let held = live_bytes() - b0;
    let th = trials(5, hits.len(), || {
        let mut s = 0u64;
        for &i in &hits {
            s += flat[flat.binary_search_by(|e| e.0.as_str().cmp(files[i].0.as_str())).unwrap()].1 as u64;
        }
        black_box(s);
    });
    let tm = trials(5, miss.len(), || {
        let mut s = 0u64;
        for m in &miss {
            s += flat.binary_search_by(|e| e.0.as_str().cmp(m.as_str())).is_ok() as u64;
        }
        black_box(s);
    });
    let tl = trials(5, sub.len(), || {
        let mut s = 0usize;
        for d in &sub {
            let pre = format!("{d}/");
            let lo = flat.partition_point(|e| e.0.as_str() < pre.as_str());
            s += flat[lo..].iter().take_while(|e| e.0.starts_with(&pre)).count();
        }
        black_box(s);
    });
    out.row(Row::new().s("exp", "tree_index").s("name", "flat_sorted_paths").i("files", nf as u64).n("build_s_excl_sort", bs).n("heap_bytes_per_file", held as f64 / nf as f64)
        .n("lookup_hit_ns", th.median).n("lookup_miss_ns", tm.median).n("subtree_list_200dirs_ns_per_dir", tl.median)
        .n("bytes_written_per_one_file_change", (nf * (held / nf.max(1)).max(1)) as f64).s("note", "a one-file change rewrites the whole derived array (or needs an LSM/patch layer)"));

    // ---- fst
    let t = std::time::Instant::now();
    let b0 = live_bytes();
    let mut bld = fst::MapBuilder::memory();
    for (p, id) in &files {
        bld.insert(p, *id as u64).unwrap();
    }
    let bytes = bld.into_inner().unwrap();
    let bs = t.elapsed().as_secs_f64();
    let map = fst::Map::new(bytes.clone()).unwrap();
    let held = live_bytes() - b0;
    let th = trials(5, hits.len(), || {
        let mut s = 0u64;
        for &i in &hits {
            s += map.get(&files[i].0).unwrap();
        }
        black_box(s);
    });
    let tm = trials(5, miss.len(), || {
        let mut s = 0u64;
        for m in &miss {
            s += map.get(m).is_some() as u64;
        }
        black_box(s);
    });
    let tl = trials(5, sub.len(), || {
        use fst::{IntoStreamer, Streamer};
        let mut s = 0usize;
        for d in &sub {
            let pre = format!("{d}/");
            let mut hi = pre.clone().into_bytes();
            *hi.last_mut().unwrap() += 1;
            let mut st = map.range().ge(pre.as_bytes()).lt(&hi).into_stream();
            while st.next().is_some() {
                s += 1;
            }
        }
        black_box(s);
    });
    out.row(Row::new().s("exp", "tree_index").s("name", "fst_map").i("files", nf as u64).n("build_s_from_sorted", bs).n("fst_bytes_per_file", bytes.len() as f64 / nf as f64).n("heap_bytes_per_file", held as f64 / nf as f64)
        .n("lookup_hit_ns", th.median).n("lookup_miss_ns", tm.median).n("subtree_list_200dirs_ns_per_dir", tl.median)
        .n("bytes_written_per_one_file_change", bytes.len() as f64).s("note", "immutable; a change rebuilds from sorted input (build_s) or layers a small delta FST"));

    // ---- hash map
    let b0 = live_bytes();
    let t = std::time::Instant::now();
    let hm: HashMap<String, u32> = files.iter().cloned().collect();
    let bs = t.elapsed().as_secs_f64();
    let held = live_bytes() - b0;
    let th = trials(5, hits.len(), || {
        let mut s = 0u64;
        for &i in &hits {
            s += hm[&files[i].0] as u64;
        }
        black_box(s);
    });
    let tm = trials(5, miss.len(), || {
        let mut s = 0u64;
        for m in &miss {
            s += hm.contains_key(m) as u64;
        }
        black_box(s);
    });
    out.row(Row::new().s("exp", "tree_index").s("name", "hashmap_string").i("files", nf as u64).n("build_s", bs).n("heap_bytes_per_file", held as f64 / nf as f64).n("lookup_hit_ns", th.median).n("lookup_miss_ns", tm.median)
        .s("note", "no ordered/prefix enumeration, so cannot serve sparse subtree listing"));
}
