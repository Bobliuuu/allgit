//! Experiment H2: commit reachability, changed-path filters, zone partitioning of the commit graph.
//! usage: histbench [--zones 8] [--commits 50000] [--files 20000] [--queries 2000] [--out f.jsonl]
use std::hint::black_box;
use vcs_storage_lab::history::*;
use vcs_storage_lab::util::*;

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let p = Params {
        zones: arg_or("--zones", 8),
        commits_per_zone: arg_or("--commits", 50_000),
        files_per_zone: arg_or("--files", 20_000),
        branch_prob: 0.08,
        mean_branch_len: 6.0,
        mean_files_per_commit: 3.0,
        refactor_prob: 0.01,
        skew_prob: arg_or("--skew", 0.02),
        seed: arg_or("--seed", 1),
    };
    let nq: usize = arg_or("--queries", 2000);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()).s("model", &p.describe()));
    let t = std::time::Instant::now();
    let zones: Vec<Zone> = (0..p.zones).map(|zi| build_zone(&p, zi)).collect();
    let gen_s = t.elapsed().as_secs_f64();
    let total_commits: usize = zones.iter().map(|z| z.commits.len()).sum();
    let merges: usize = zones.iter().map(|z| z.commits.iter().filter(|c| c.parents.len() > 1).count()).sum();
    let ch: Vec<usize> = zones.iter().flat_map(|z| z.commits.iter().map(|c| c.changed.len())).collect();
    let mut chs = ch.clone();
    chs.sort();
    out.row(Row::new().s("exp", "history_shape").i("commits", total_commits as u64).i("merges", merges as u64)
        .n("mean_changed_entries", ch.iter().sum::<usize>() as f64 / ch.len() as f64).i("median_changed", chs[chs.len() / 2] as u64)
        .i("p99_changed", chs[chs.len() * 99 / 100] as u64).i("max_changed", *chs.last().unwrap() as u64).n("gen_s", gen_s));

    // ---------- reachability ----------
    let z = &zones[0];
    let n = z.commits.len();
    let g1 = gen_v1(z);
    let g2 = gen_v2(z);
    let t = std::time::Instant::now();
    let _ = gen_v1(z);
    let g1_s = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let gr = Grail::build(z, 3, 5);
    let grail_s = t.elapsed().as_secs_f64();
    // query mix: near ancestor, far ancestor, sibling (non ancestor), newer-than (non ancestor)
    let mut r = Rng::new(77);
    let mut qs: Vec<(&str, u32, u32)> = vec![];
    for _ in 0..nq {
        let d = (n as u64 - 1 - r.next_u64() % (n as u64 / 4)) as u32; // a recent descendant
        let near = d.saturating_sub(1 + (r.next_u64() % 20) as u32);
        let far = (r.next_u64() % (n as u64 / 2)) as u32;
        qs.push(("near", near, d));
        qs.push(("far", far, d));
        qs.push(("newer_than", d, far)); // desc older than anc: false
        qs.push(("random_pair", (r.next_u64() % n as u64) as u32, (r.next_u64() % n as u64) as u32));
    }
    let mut seen = vec![0u32; n];
    let mut stamp = 0u32;
    let mut truth = vec![];
    for (_, a, d) in &qs {
        stamp += 1;
        truth.push(is_ancestor(z, *a, *d, &Prune::None, &mut seen, stamp).answer);
    }
    for (name, prune, extra_bytes, build_s) in [
        ("no_index_full_walk", Prune::None, 0usize, 0.0),
        ("commit_time_cutoff", Prune::Time, 8 * n, 0.0),
        ("gen_v1_topological", Prune::GenV1(&g1), 4 * n, g1_s),
        ("gen_v2_corrected_date", Prune::GenV2(&g2), 8 * n, g1_s),
        ("grail_k3_plus_gen_v1", Prune::Grail(&gr, &g1), 4 * n + gr.bytes(), g1_s + grail_s),
    ] {
        for kind in ["near", "far", "newer_than", "random_pair"] {
            let mut visited = 0u64;
            let mut wrong = 0u64;
            let mut cnt = 0u64;
            let t = std::time::Instant::now();
            for (i, (k, a, d)) in qs.iter().enumerate() {
                if *k != kind {
                    continue;
                }
                stamp += 1;
                let w = is_ancestor(z, *a, *d, &prune, &mut seen, stamp);
                visited += w.visited;
                cnt += 1;
                wrong += (w.answer != truth[i]) as u64;
            }
            let el = t.elapsed().as_nanos() as f64 / cnt as f64;
            black_box(visited);
            out.row(Row::new().s("exp", "reachability").s("strategy", name).s("query", kind).i("commits", n as u64)
                .n("visited_per_query", visited as f64 / cnt as f64).n("ns_per_query", el).i("wrong_answers", wrong).i("queries", cnt)
                .n("extra_bytes_per_commit", extra_bytes as f64 / n as f64).n("build_s", build_s)
                .s("note", "visited = commits popped; in the product each is an object read+decode unless the graph file supplies parents"));
        }
    }

    // ---------- commit-graph file size/build/update: monolithic (product) vs per-zone ----------
    {
        use vcs_merge::CommitGraphFile;
        use vcs_objects::ObjectId;
        type Prep = (Vec<ObjectId>, std::collections::HashMap<ObjectId, (Vec<ObjectId>, u64)>);
        let prep = |zs: &[(usize, &Zone)], trim: usize| -> Prep {
            let mut tips = vec![];
            let mut map = std::collections::HashMap::new();
            for (zi, z) in zs {
                let lim = z.commits.len() - if *zi == 0 { trim } else { 0 };
                let mut has_child = vec![false; lim];
                for (i, c) in z.commits.iter().enumerate().take(lim) {
                    map.insert(ObjectId::from_bytes(commit_id(*zi, i)), (c.parents.iter().map(|&p| ObjectId::from_bytes(commit_id(*zi, p as usize))).collect::<Vec<_>>(), c.time));
                    for &p in &c.parents {
                        has_child[p as usize] = true;
                    }
                }
                for i in 0..lim {
                    if !has_child[i] {
                        tips.push(ObjectId::from_bytes(commit_id(*zi, i)));
                    }
                }
            }
            (tips, map)
        };
        // only `extend` (the product's graph construction) is timed; the synthetic loader map is built beforehand
        let build_cg = |zs: &[(usize, &Zone)], trim: usize, base: &CommitGraphFile| -> (CommitGraphFile, f64) {
            let (tips, map) = prep(zs, trim);
            let t = std::time::Instant::now();
            let g = base.extend(&tips, |id| Ok(map[id].clone())).unwrap();
            (g, t.elapsed().as_secs_f64())
        };
        let all: Vec<(usize, &Zone)> = zones.iter().enumerate().collect();
        let (mono, mono_s) = build_cg(&all, 0, &CommitGraphFile::default());
        let enc = mono.encode();
        let t = std::time::Instant::now();
        let _ = CommitGraphFile::decode(&enc).unwrap();
        let dec_s = t.elapsed().as_secs_f64();
        let (z0, z0_s) = build_cg(&all[..1], 0, &CommitGraphFile::default());
        let enc0 = z0.encode();
        let t = std::time::Instant::now();
        let _ = CommitGraphFile::decode(&enc0).unwrap();
        let dec0_s = t.elapsed().as_secs_f64();
        out.row(Row::new().s("exp", "commit_graph_file").s("layout", "monolithic_all_zones(product)").i("commits", mono.len() as u64)
            .i("file_bytes", enc.len() as u64).n("bytes_per_commit", enc.len() as f64 / mono.len() as f64).n("build_s", mono_s).n("decode_s", dec_s)
            .n("bytes_rewritten_per_publish", enc.len() as f64));
        // incremental publish: graph already holds all but the newest 10 commits of zone 0
        let (base_m, _) = build_cg(&all, 10, &CommitGraphFile::default());
        let (base_z, _) = build_cg(&all[..1], 10, &CommitGraphFile::default());
        let (m2, inc_m) = build_cg(&all, 0, &base_m);
        let (z2, inc_z) = build_cg(&all[..1], 0, &base_z);
        out.row(Row::new().s("exp", "commit_graph_incremental").s("layout", "monolithic").i("existing_commits", base_m.len() as u64).i("new_commits", (m2.len() - base_m.len()) as u64).n("extend_s", inc_m).i("bytes_to_write", m2.encode().len() as u64));
        out.row(Row::new().s("exp", "commit_graph_incremental").s("layout", "per_zone").i("existing_commits", base_z.len() as u64).i("new_commits", (z2.len() - base_z.len()) as u64).n("extend_s", inc_z).i("bytes_to_write", z2.encode().len() as u64).s("note", "extend() rebuilds the whole BTreeMap of existing commits even for 10 new ones: O(existing)"));
        out.row(Row::new().s("exp", "commit_graph_file").s("layout", "per_zone_file(one zone shown)").i("commits", z0.len() as u64)
            .i("file_bytes", enc0.len() as u64).n("bytes_per_commit", enc0.len() as f64 / z0.len() as f64).n("build_s", z0_s).n("decode_s", dec0_s)
            .n("bytes_rewritten_per_publish", enc0.len() as f64).s("note", "a publish touching one zone rewrites only that zone's file; a query in one zone decodes only it"));
    }

    if std::env::args().any(|a| a == "--skip-paths") {
        return;
    }
    // ---------- changed-path filters ----------
    // query set: for zone 0, file paths sampled (a) by popularity (hot), (b) uniformly (cold), plus directories and absent paths
    let zi = 0;
    let z = &zones[zi];
    let hashes: Vec<u64> = (0..z.node_parent.len() as u32).map(|nn| node_hash(zi, z, nn)).collect();
    let mut qr = Rng::new(3);
    let mut qnodes: Vec<(&str, u32)> = vec![];
    for _ in 0..nq {
        qnodes.push(("file_uniform", z.files[(qr.next_u64() as usize) % z.files.len()]));
        let nd = z.node_parent.len() as u64;
        let mut d = (qr.next_u64() % nd) as u32;
        while z.node_is_file[d as usize] {
            d = (qr.next_u64() % nd) as u32
        }
        qnodes.push(("dir", d));
    }
    // hot files: the 200 most-changed files
    let mut cnt = vec![0u32; z.node_parent.len()];
    for c in &z.commits {
        for &nn in &c.changed {
            cnt[nn as usize] += 1;
        }
    }
    let mut hot: Vec<u32> = z.files.clone();
    hot.sort_by_key(|&f| std::cmp::Reverse(cnt[f as usize]));
    for i in 0..nq {
        qnodes.push(("file_hot", hot[i % 200.min(hot.len())]));
    }
    let nc = z.commits.len();
    let exact_total: u64 = z.commits.iter().map(|c| c.changed.len() as u64).sum();
    // exact answers
    let mut exact_hits = |nn: u32| z.commits.iter().filter(|c| c.changed.binary_search(&nn).is_ok()).count();
    let truth_counts: Vec<usize> = qnodes.iter().map(|(_, nn)| exact_hits(*nn)).collect();
    // baseline: no filter => a tree diff per commit
    for kind in ["file_uniform", "dir", "file_hot"] {
        let idx: Vec<usize> = (0..qnodes.len()).filter(|&i| qnodes[i].0 == kind).collect();
        let hits: usize = idx.iter().map(|&i| truth_counts[i]).sum();
        out.row(Row::new().s("exp", "path_history").s("strategy", "no_filter").s("query", kind).i("queries", idx.len() as u64).n("tree_diffs_per_query", nc as f64)
            .n("true_matches_per_query", hits as f64 / idx.len() as f64).n("filter_bytes_per_commit", 0.0).s("note", "one tree diff per commit walked"));
    }
    for (bpe, k) in [(6.0, 4u32), (10.0, 7), (16.0, 11)] {
        let t = std::time::Instant::now();
        let bf = CommitBloom::build(z, zi, &hashes, bpe, k, 512);
        let bs = t.elapsed().as_secs_f64();
        for kind in ["file_uniform", "dir", "file_hot"] {
            let idx: Vec<usize> = (0..qnodes.len()).filter(|&i| qnodes[i].0 == kind).collect();
            let mut diffs = 0u64;
            let mut trues = 0u64;
            let t = std::time::Instant::now();
            for &i in &idx {
                let h = hashes[qnodes[i].1 as usize];
                for c in 0..nc {
                    if bf.maybe(c, h) {
                        diffs += 1;
                    }
                }
                trues += truth_counts[i] as u64;
            }
            let el = t.elapsed().as_nanos() as f64 / idx.len() as f64;
            let fpd = diffs - trues;
            let neg = (idx.len() * nc) as u64 - trues;
            out.row(Row::new().s("exp", "path_history").s("strategy", &format!("commit_bloom_{bpe}bpe_k{k}_max512")).s("query", kind).i("queries", idx.len() as u64)
                .n("tree_diffs_per_query", diffs as f64 / idx.len() as f64).n("true_matches_per_query", trues as f64 / idx.len() as f64)
                .n("false_pos_rate_over_negatives", fpd as f64 / neg as f64).n("filter_bytes_per_commit", bf.bytes() as f64 / nc as f64)
                .n("build_s", bs).n("scan_ns_per_query", el).n("exact_entries_per_commit", exact_total as f64 / nc as f64));
        }
    }
    // exact inverted index
    let t = std::time::Instant::now();
    let inv = Inverted::build(z);
    let bs = t.elapsed().as_secs_f64();
    for kind in ["file_uniform", "dir", "file_hot"] {
        let idx: Vec<usize> = (0..qnodes.len()).filter(|&i| qnodes[i].0 == kind).collect();
        let t = std::time::Instant::now();
        let mut s = 0usize;
        for &i in &idx {
            s += inv.postings[qnodes[i].1 as usize].len();
        }
        black_box(s);
        out.row(Row::new().s("exp", "path_history").s("strategy", "inverted_index_exact").s("query", kind).i("queries", idx.len() as u64)
            .n("tree_diffs_per_query", s as f64 / idx.len() as f64).n("true_matches_per_query", s as f64 / idx.len() as f64)
            .n("filter_bytes_per_commit", inv.bytes() as f64 / nc as f64).n("build_s", bs).n("scan_ns_per_query", t.elapsed().as_nanos() as f64 / idx.len() as f64)
            .s("note", "no per-commit scan at all; tree diffs only for true matches (merge-commit caveat applies to all strategies)"));
    }

    // ---------- zone-aware: one filter set per zone vs one global set ----------
    // A zone-scoped query ("log zone Z -- path") must scan only that zone's commits when the graph is partitioned; a global
    // layout (commits of all zones interleaved by time) scans all and discards by zone id.
    let per_zone_scan = nc as u64;
    let global_scan = total_commits as u64;
    out.row(Row::new().s("exp", "zone_partition").i("zones", p.zones as u64).i("commits_scanned_zone_partitioned", per_zone_scan).i("commits_scanned_global_interleaved", global_scan)
        .n("ratio", global_scan as f64 / per_zone_scan as f64));
}
