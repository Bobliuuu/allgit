//! Experiment J3: negative and positive "does the remote have X" caching under concurrent publish and GC (Monte Carlo).
//! Unguarded TTL caches versus caches guarded by a remote generation counter. See docs/research/storage/caching-experiments.md.
//! usage: negcache [--samples 2000000] [--out f.jsonl]
use vcs_storage_lab::util::*;

fn unif(r: &mut Rng) -> f64 {
    (r.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}
fn exp(r: &mut Rng, mean: f64) -> f64 {
    -mean * (1.0 - unif(r)).ln()
}

fn main() {
    let n: usize = arg_or("--samples", 2_000_000);
    let mut out = Out::new(arg("--out"));
    out.row(Row::new().s("exp", "env").s("env", &env_json()).i("samples", n as u64));
    let mut r = Rng::new(1234);

    // ---------- negative cache ----------
    // An object id x is referenced locally but absent on the remote at time 0 (e.g. an unsubscribed zone's tree, or an object a
    // peer has not published yet). With probability `p_pub` it gets published at time Exp(mean_pub). A client probes x at
    // t0, caches "absent", and probes again at t0 + Exp(mean_gap). Stale answer = cached absent while x is already published.
    let p_pub = 0.5;
    for mean_pub in [60.0, 600.0, 3600.0] {
        for mean_gap in [5.0, 60.0, 600.0] {
            for ttl in [1.0, 10.0, 60.0, 600.0, 3600.0] {
                let (mut reprobes, mut stale, mut served_from_cache) = (0u64, 0u64, 0u64);
                let mut lag_sum = 0.0;
                let mut lag_n = 0u64;
                for _ in 0..n {
                    let published_at = if unif(&mut r) < p_pub { exp(&mut r, mean_pub) } else { f64::INFINITY };
                    let t1 = exp(&mut r, mean_gap); // second probe
                    reprobes += 1;
                    if t1 <= ttl {
                        served_from_cache += 1;
                        if published_at <= t1 {
                            stale += 1;
                            lag_sum += t1 - published_at;
                            lag_n += 1;
                        }
                    }
                }
                out.row(Row::new().s("exp", "negative_cache_unguarded").n("mean_publish_delay_s", mean_pub).n("mean_gap_between_probes_s", mean_gap).n("ttl_s", ttl)
                    .n("fraction_answered_from_cache", served_from_cache as f64 / reprobes as f64)
                    .n("stale_negative_rate_per_reprobe", stale as f64 / reprobes as f64)
                    .n("stale_given_served_from_cache", if served_from_cache == 0 { f64::NAN } else { stale as f64 / served_from_cache as f64 })
                    .n("mean_visibility_lag_s_when_stale", if lag_n == 0 { f64::NAN } else { lag_sum / lag_n as f64 }));
            }
        }
    }
    // Guarded: the negative entry stores the remote manifest generation g. A later probe first reads the current generation
    // (one small request per batch of `batch` probes) and trusts the entry only if g is unchanged. Publishing x changes g
    // (publish = CAS on the manifest), so a stale answer is impossible by construction; the cost is 1 request per batch and
    // lost reuse when other publishes bump g. Reuse = P(no publish during the gap) = exp(-gap * publish_rate).
    for pub_rate_per_min in [0.1, 1.0, 10.0] {
        for batch in [1.0, 20.0] {
            for mean_gap in [5.0, 60.0, 600.0] {
                let mut reused = 0u64;
                let mut stale = 0u64;
                for _ in 0..n {
                    let gap = exp(&mut r, mean_gap);
                    let gen_changes = unif(&mut r) >= (-gap * pub_rate_per_min / 60.0).exp();
                    // x published during the gap implies generation changed; the guard then refuses the entry
                    if !gen_changes {
                        reused += 1;
                    }
                    stale += 0;
                }
                out.row(Row::new().s("exp", "negative_cache_guarded").n("remote_publish_rate_per_min", pub_rate_per_min).n("probes_per_generation_check", batch).n("mean_gap_s", mean_gap)
                    .n("entry_reuse_rate", reused as f64 / n as f64).n("generation_checks_per_probe", 1.0 / batch).i("stale_answers", stale)
                    .s("note", "stale = 0 by construction (publish changes the generation); reuse rate and check cost are the price"));
            }
        }
    }

    // ---------- positive cache of remote existence vs GC ----------
    // Orphaned objects (unreferenced) are deleted by a daily sweep once untouched for `grace`. A publisher about to re-adopt an
    // orphan consults a cache entry "remote has x" made at t0 (age a <= ttl) and, on a hit, skips the upload (and the touch).
    // Corruption = x deleted between t0 and the manifest CAS. Orphan age at t0 is U(0, 60 d).
    let day = 86400.0;
    for grace_days in [1.0, 14.0] {
        for ttl_days in [0.0, 1.0 / 24.0, 1.0, 3.5, 7.0, 14.0] {
            let (mut bad, mut cached_hits) = (0u64, 0u64);
            for _ in 0..n {
                let age0 = unif(&mut r) * 60.0 * day; // orphan age at t0, untouched since
                if age0 > grace_days * day + day {
                    continue; // already deleted by t0: cache entry cannot have been created from a live remote
                }
                let entry_age = unif(&mut r) * ttl_days * day; // time since the cache entry was made
                if ttl_days == 0.0 {
                    continue; // cache disabled
                }
                cached_hits += 1;
                // deleted in (t0, t0+entry_age] iff the orphan crosses grace + sweep rounding in that window
                let next_sweep = unif(&mut r) * day; // next daily sweep after t0
                let delete_at = (grace_days * day - age0).max(0.0) + next_sweep; // time after t0 when it is swept
                if delete_at <= entry_age {
                    bad += 1;
                }
            }
            out.row(Row::new().s("exp", "positive_cache_vs_gc_unguarded").n("grace_days", grace_days).n("ttl_days", ttl_days).i("cache_hits_considered", cached_hits)
                .i("dangling_references_created", bad).n("rate_per_hit", if cached_hits == 0 { 0.0 } else { bad as f64 / cached_hits as f64 })
                .s("note", "orphan = unreferenced object that a later publish re-adopts; model assumes no touch on cache hit"));
        }
    }
    out.row(Row::new().s("exp", "positive_cache_vs_gc_guarded").s("rule", "entry valid only while gc_epoch unchanged AND entry age < grace/4 AND a hit still performs the touch").i("dangling_references_created", 0)
        .s("note", "argument, not Monte Carlo: see the model check (modelcheck bin) for the exhaustive interleaving proof on a small instance"));
}
