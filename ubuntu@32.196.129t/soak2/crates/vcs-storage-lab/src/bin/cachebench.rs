//! Experiment J1: trace-driven cache policy comparison (hit rate, byte hit rate, modelled network time, policy cost),
//! two-level caches, prefetch, warming. usage: cachebench [--out f.jsonl] [--ops 3000] [--zones 12]
use vcs_storage_lab::cache::*;
use vcs_storage_lab::history::Params;
use vcs_storage_lab::util::*;

#[global_allocator]
static A: Counting = Counting;

fn mk(name: &str, cap: u64, avg: u64) -> Box<dyn Policy> {
    match name {
        "lru" => Box::new(Lru::new(cap)),
        "fifo" => Box::new(Fifo::new(cap)),
        "arc" => Box::new(Arc::new(cap)),
        _ => Box::new(TinyLfu::new(cap, avg)),
    }
}

fn main() {
    let ops: usize = arg_or("--ops", 3000);
    let zones: usize = arg_or("--zones", 12);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()));
    let p = Params { zones, commits_per_zone: arg_or("--commits", 4000), files_per_zone: arg_or("--files", 4000), branch_prob: 0.08, mean_branch_len: 6.0, mean_files_per_commit: 3.0, refactor_prob: 0.01, skew_prob: 0.0, seed: 11 };
    out.row(Row::new().s("exp", "cache_world").s("model", &p.describe()).i("ops", ops as u64));
    let w = World::new(&p);
    // WAN-ish remote: 40 ms RTT, 50 MB/s per client (assumption, stated in the doc); 16 parallel streams when batched
    let net = Net { rtt_s: 0.040, bw_bps: 50e6, par: 16 };
    for kind in ["checkout_sync", "checkout_sync_scan", "ci_fresh"] {
        let tr = gen_trace(&w, &TraceCfg { kind: Box::leak(kind.to_string().into_boxed_str()), ops, subscribed: 3, seed: 5, prefetch_ahead: 0 });
        let uniq: std::collections::HashMap<u64, u32> = tr.iter().map(|a| (a.key, a.size)).collect();
        let uniq_bytes: u64 = uniq.values().map(|&s| s as u64).sum();
        let total_bytes: u64 = tr.iter().map(|a| a.size as u64).sum();
        let once = {
            let mut c: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
            for a in &tr {
                *c.entry(a.key).or_default() += 1;
            }
            c.values().filter(|&&n| n == 1).count()
        };
        out.row(Row::new().s("exp", "trace").s("kind", kind).i("accesses", tr.len() as u64).i("unique_objects", uniq.len() as u64).i("unique_bytes", uniq_bytes).i("total_bytes_read", total_bytes)
            .n("one_hit_wonder_fraction_of_unique", once as f64 / uniq.len() as f64).n("compulsory_miss_rate", uniq.len() as f64 / tr.len() as f64));
        let avg = uniq_bytes / uniq.len() as u64;
        for frac in [0.01, 0.03, 0.10, 0.30] {
            let cap = (uniq_bytes as f64 * frac) as u64;
            for pol in ["fifo", "lru", "arc", "w-tinylfu"] {
                let mut p = mk(pol, cap, avg);
                let t = std::time::Instant::now();
                let s = replay(&tr, p.as_mut(), &net, 0, tr.len());
                let ns = t.elapsed().as_nanos() as f64 / tr.len() as f64;
                out.row(Row::new().s("exp", "policy").s("trace", kind).s("policy", pol).n("cap_fraction_of_unique_bytes", frac).i("cap_bytes", cap)
                    .n("hit_rate", s.hits as f64 / s.accesses as f64).n("byte_hit_rate", s.hit_bytes as f64 / s.bytes as f64)
                    .i("remote_fetches", s.fetches).i("remote_bytes", s.fetch_bytes).n("net_s_on_demand", s.t_on_demand).n("net_s_batched", s.t_batched).n("policy_ns_per_access", ns)
                    .n("peak_policy_heap_mb", peak_bytes() as f64 / 1e6));
            }
            let (bh, bb) = belady(&tr, cap);
            out.row(Row::new().s("exp", "policy").s("trace", kind).s("policy", "belady_offline_bound").n("cap_fraction_of_unique_bytes", frac).i("cap_bytes", cap)
                .n("hit_rate", bh as f64 / tr.len() as f64).n("byte_hit_rate", bb as f64 / total_bytes as f64));
        }
        out.row(Row::new().s("exp", "policy").s("trace", kind).s("policy", "no_cache").n("hit_rate", 0.0).n("net_s_on_demand", replay(&tr, &mut Lru::new(0), &net, 0, tr.len()).t_on_demand)
            .n("net_s_batched", replay(&tr, &mut Lru::new(0), &net, 0, tr.len()).t_batched));

        // ---- warming: second session replays the same workload family (different seed); cache starts cold vs warmed
        if kind != "ci_fresh" {
            // session 2 = the SAME client continuing after a restart with an empty cache: one long trace, split at op `ops`
            let long = gen_trace(&w, &TraceCfg { kind: Box::leak(kind.to_string().into_boxed_str()), ops: ops + ops / 3, subscribed: 3, seed: 5, prefetch_ahead: 0 });
            let split = long.iter().position(|a| a.op as usize >= ops).unwrap();
            let tr2: Vec<Access> = long[split..].to_vec();
            let cap = (uniq_bytes as f64 * 0.10) as u64;
            // warm set candidates from the *previous session's* trace: top-K by frequency within a byte budget
            let mut freq: std::collections::HashMap<u64, (u32, u32)> = std::collections::HashMap::new();
            for a in &tr {
                let e = freq.entry(a.key).or_insert((0, a.size));
                e.0 += 1;
            }
            let mut byf: Vec<(u64, (u32, u32))> = freq.into_iter().collect();
            byf.sort_by_key(|&(k, (f, _))| (std::cmp::Reverse(f), k));
            // most recent objects of the previous session (recency warm set)
            let mut recent: Vec<(u64, u32)> = vec![];
            let mut seen = std::collections::HashSet::new();
            for a in tr.iter().rev() {
                if seen.insert(a.key) {
                    recent.push((a.key, a.size));
                }
            }
            let window = 1500.min(tr2.len());
            for (label, warm) in [("cold", None), ("warm_top_frequency", Some(0)), ("warm_most_recent", Some(1))] {
                let mut pol = mk("lru", cap, avg);
                let mut warm_bytes = 0u64;
                if let Some(mode) = warm {
                    let budget = cap / 2;
                    let items: Vec<(u64, u32)> = if mode == 0 { byf.iter().map(|&(k, (_, s))| (k, s)).collect() } else { recent.clone() };
                    for (k, s) in items {
                        if warm_bytes + s as u64 > budget {
                            break;
                        }
                        pol.access(k, s);
                        warm_bytes += s as u64;
                    }
                }
                let s = replay(&tr2, pol.as_mut(), &net, 0, window);
                out.row(Row::new().s("exp", "warming").s("trace", kind).s("start", label).i("first_accesses", window as u64).n("hit_rate", s.hits as f64 / s.accesses as f64)
                    .i("warm_bytes_prefetched", warm_bytes).n("net_s_batched_first_window", s.t_batched).n("cap_fraction", 0.10).s("note", "warm fetch cost excluded; it is background/off-critical-path in this model"));
            }
        }

        // ---- prefetch (checkout_sync only)
        if kind == "checkout_sync" {
            let cap = (uniq_bytes as f64 * 0.10) as u64;
            for ahead in [0u32, 3, 6] {
                let t = gen_trace(&w, &TraceCfg { kind: "checkout_sync", ops, subscribed: 3, seed: 5, prefetch_ahead: ahead });
                for pol in ["lru", "w-tinylfu"] {
                    let mut p = mk(pol, cap, avg);
                    let s = replay(&t, p.as_mut(), &net, 0, t.len());
                    out.row(Row::new().s("exp", "prefetch").s("policy", pol).i("ahead_commits", ahead as u64).n("cap_fraction", 0.10).n("demand_hit_rate", s.hits as f64 / s.accesses as f64)
                        .n("demand_net_s_batched", s.t_batched).i("prefetch_objects", s.prefetch_fetches).i("prefetch_bytes", s.prefetch_bytes)
                        .n("prefetch_useful_fraction", if s.prefetch_fetches == 0 { f64::NAN } else { s.prefetch_useful as f64 / s.prefetch_fetches as f64 })
                        .i("total_bytes_transferred", s.fetch_bytes + s.prefetch_bytes));
                }
            }
        }

        // ---- two-level: memory L1 over disk L2 (inclusive fill), L2 = LRU 30% of unique bytes
        if kind == "checkout_sync" {
            let l2cap = (uniq_bytes as f64 * 0.30) as u64;
            for l1frac in [0.0, 0.01, 0.03] {
                for l1p in ["lru", "w-tinylfu"] {
                    let mut l1 = mk(l1p, ((uniq_bytes as f64 * l1frac) as u64).max(1), avg);
                    let mut l2 = Lru::new(l2cap);
                    let (mut h1, mut h2, mut miss) = (0u64, 0u64, 0u64);
                    for a in &tr {
                        let in1 = l1.access(a.key, a.size);
                        if in1 {
                            h1 += 1;
                            l2.access(a.key, a.size); // keep L2 recency in sync (inclusive)
                        } else if l2.access(a.key, a.size) {
                            h2 += 1;
                        } else {
                            miss += 1;
                        }
                    }
                    out.row(Row::new().s("exp", "two_level").s("l1_policy", l1p).n("l1_fraction", l1frac).n("l2_fraction", 0.30).n("l1_hit", h1 as f64 / tr.len() as f64)
                        .n("l2_hit", h2 as f64 / tr.len() as f64).n("remote", miss as f64 / tr.len() as f64));
                }
            }
        }
    }
}
