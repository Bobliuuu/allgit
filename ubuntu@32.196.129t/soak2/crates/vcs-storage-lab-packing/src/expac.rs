//! Drivers for experiments A (git-style baseline) and C (bounded depth, checkpoints, blocks).

use crate::dataset::*;
use crate::json::*;
use crate::packlab::*;
use std::process::Command;
use std::time::Instant;

fn lcg(s: &mut u64) -> u64 {
    *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *s >> 33
}

pub fn sample_targets(order: &[u32], ds: &Dataset, n: usize, seed: u64) -> Vec<usize> {
    let cand: Vec<usize> = (0..order.len()).filter(|&p| ds.objs[order[p] as usize].kind != COMMIT).collect();
    let mut s = seed;
    (0..n.min(cand.len())).map(|_| cand[lcg(&mut s) as usize % cand.len()]).collect()
}

pub fn head_targets(order: &[u32], ds: &Dataset) -> Vec<usize> {
    let mut inv = vec![0usize; order.len()];
    for (p, &o) in order.iter().enumerate() {
        inv[o as usize] = p;
    }
    ds.head.iter().map(|&o| inv[o as usize]).collect()
}

fn hist_json(h: &[(u32, usize)]) -> J {
    // bucketed so the record stays small
    let mut b = vec![0usize; 8];
    let edges = [0, 1, 2, 4, 8, 16, 32, u32::MAX];
    for (d, c) in h {
        let i = edges.iter().position(|e| *d <= *e).unwrap();
        b[i] += c;
    }
    J::A(b.into_iter().map(|x| J::U(x as u64)).collect())
}

fn recon_fields(prefix: &'static str, r: &ReconStats, f: &mut Vec<(&'static str, J)>) {
    let k = |s: &str| -> &'static str { Box::leak(format!("{prefix}_{s}").into_boxed_str()) };
    f.push((k("reads_mean"), J::F(r.mean_reads)));
    f.push((k("reads_max"), J::U(r.max_reads as u64)));
    f.push((k("blocks_mean"), J::F(r.mean_blocks)));
    f.push((k("bytes_mean"), J::F(r.mean_bytes)));
    f.push((k("ns_median"), J::F(r.ns_per_obj_median)));
    f.push((k("ns_min"), J::F(r.ns_spread.0)));
    f.push((k("ns_max"), J::F(r.ns_spread.1)));
}

fn pack_row(ds: &Dataset, order: &[u32], pk: &Packed, blk: &[u32], codec: Codec, label: &str, extra: Vec<(&'static str, J)>, out: &Out, exp: &str, secs: f64) {
    let rnd = sample_targets(order, ds, 2000, 7);
    let hd = head_targets(order, ds);
    let r1 = recon(ds, order, pk, blk, &rnd, codec, false);
    let r2 = recon(ds, order, pk, blk, &hd, codec, false);
    let r3 = recon(ds, order, pk, blk, &hd, codec, true);
    let mut f: Vec<(&'static str, J)> = vec![
        ("variant", J::s(label)),
        ("codec", J::s(codec.name())),
        ("raw_bytes", J::U(pk.raw)),
        ("stored_bytes", J::U(pk.total())),
        ("blob_stored_bytes", J::U((0..order.len()).filter(|&p| ds.objs[order[p] as usize].kind == BLOB).map(|p| pk.stored[p] as u64).sum())),
        ("ratio", J::F(pk.total() as f64 / pk.raw as f64)),
        ("objects", J::U(order.len() as u64)),
        ("deltas", J::U(pk.ndelta() as u64)),
        ("depth_mean", J::F(pk.mean_depth())),
        ("depth_max", J::U(pk.max_depth() as u64)),
        ("depth_hist_le0_1_2_4_8_16_32_inf", hist_json(&pk.depth_hist())),
        ("build_secs", J::F(secs)),
    ];
    f.extend(extra);
    recon_fields("rand", &r1, &mut f);
    recon_fields("head", &r2, &mut f);
    recon_fields("headcache", &r3, &mut f);
    out.emit(exp, f);
}

fn scratch() -> String {
    std::env::var("LAB_SCRATCH").unwrap_or_else(|_| "/tmp".into())
}

fn dir_size(p: &str) -> u64 {
    let o = Command::new("sh").args(["-c", &format!("cat {p}/objects/pack/*.pack 2>/dev/null | wc -c")]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0)
}

/// Reference: the real git binary repacking a copy of the same repository.
pub fn git_reference(repo: &str, name: &str, out: &Out) {
    for (w, d) in [(0u32, 0u32), (10, 50), (250, 250)] {
        if w == 250 && std::env::var("LAB_SKIP250").is_ok() {
            continue;
        }
        let tmp = format!("{}/gitref-{}-{}-{}", scratch(), name, w, d);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(Command::new("cp").args(["-r", repo, &tmp]).status().unwrap().success());
        let t0 = Instant::now();
        let st = Command::new("git").args(["-C", &tmp, "repack", "-adfq", &format!("--window={w}"), &format!("--depth={d}")]).status().unwrap();
        let secs = t0.elapsed().as_secs_f64();
        assert!(st.success());
        let _ = Command::new("git").args(["-C", &tmp, "prune-packed"]).status();
        let sz = dir_size(&tmp);
        let vp = Command::new("sh").args(["-c", &format!("git -C {tmp} verify-pack -v {tmp}/objects/pack/*.idx | grep -E '^chain length|^non delta' ")]).output().unwrap();
        let txt = String::from_utf8_lossy(&vp.stdout).to_string();
        let mut hist = vec![];
        for l in txt.lines() {
            if let Some(r) = l.strip_prefix("chain length = ") {
                if let Some((a, b)) = r.split_once(':') {
                    let n: usize = b.trim().split(' ').next().unwrap().parse().unwrap_or(0);
                    hist.push((a.trim().parse::<u32>().unwrap_or(0), n));
                }
            } else if let Some(r) = l.strip_prefix("non delta: ") {
                hist.push((0, r.split(' ').next().unwrap().parse().unwrap_or(0)));
            }
        }
        let nobj: usize = hist.iter().map(|x| x.1).sum();
        let ndelta: usize = hist.iter().filter(|x| x.0 > 0).map(|x| x.1).sum();
        let maxd = hist.iter().map(|x| x.0).max().unwrap_or(0);
        let mean = hist.iter().map(|x| x.0 as f64 * x.1 as f64).sum::<f64>() / nobj.max(1) as f64;
        out.emit(
            "A",
            vec![
                ("variant", J::s(format!("git repack -adf --window={w} --depth={d}"))),
                ("tool", J::s("git 2.34.1 (zlib)")),
                ("stored_bytes", J::U(sz)),
                ("objects", J::U(nobj as u64)),
                ("deltas", J::U(ndelta as u64)),
                ("depth_mean", J::F(mean)),
                ("depth_max", J::U(maxd as u64)),
                ("wall_secs", J::F(secs)),
            ],
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

pub fn run_a(ds: &Dataset, repo: &str, out: &Out) {
    let raw = ds.total_bytes();
    let (mut nc, mut nt, mut nb) = (0, 0, 0);
    for o in &ds.objs {
        match o.kind {
            COMMIT => nc += 1,
            TREE => nt += 1,
            _ => nb += 1,
        }
    }
    out.emit(
        "A",
        vec![
            ("variant", J::s("dataset")),
            ("raw_bytes", J::U(raw)),
            ("commits", J::U(nc)),
            ("trees", J::U(nt)),
            ("blobs", J::U(nb)),
            ("history_commits", J::U(ds.ncommits as u64)),
            ("head_objects", J::U(ds.head.len() as u64)),
            ("zones", J::U(ds.zones.len() as u64)),
            ("paths", J::U(ds.paths.len() as u64)),
        ],
    );
    git_reference(repo, &ds.name, out);
    // per-object compression only (what a pack with compression but no deltas would be)
    for codec in [Codec::Zlib6, Codec::Zstd3] {
        let t0 = Instant::now();
        let z = full_sizes(ds, codec);
        let secs = t0.elapsed().as_secs_f64();
        let order = ordering(ds, Ord_::Git);
        let sel = vec![None; order.len()];
        let pk = finalize(ds, &order, &sel, &z, codec, true);
        let blk = blocks_by_size(&pk.full_z, 1 << 20);
        pack_row(ds, &order, &pk, &blk, codec, "no deltas (per-object compression)", vec![], out, "A", secs);
    }
    let zz = full_sizes(ds, Codec::Zstd3);
    let zl = full_sizes(ds, Codec::Zlib6);
    // Git order, W=50 candidates give the window sweep, zlib row compares engine with git
    let order = ordering(ds, Ord_::Git);
    let t0 = Instant::now();
    let c50 = candidates(ds, &order, 50);
    let cand_secs = t0.elapsed().as_secs_f64();
    for w in [4usize, 10, 25, 50] {
        for (codec, z) in [(Codec::Zlib6, &zl), (Codec::Zstd3, &zz)] {
            if codec == Codec::Zlib6 && w != 10 {
                continue;
            }
            let c: Cands = c50.iter().enumerate().map(|(p, v)| v.iter().filter(|(q, _)| p - *q as usize <= w).cloned().collect()).collect();
            let t0 = Instant::now();
            let sel = select(&c, 50, &|_, _| true);
            let pk = finalize(ds, &order, &sel, z, codec, true);
            let secs = t0.elapsed().as_secs_f64() + cand_secs * (w as f64 / 50.0);
            let blk = blocks_by_size(&pk.full_z, 1 << 20);
            pack_row(ds, &order, &pk, &blk, codec, &format!("delta git-like order window={w} depth<=50"), vec![("window", J::U(w as u64)), ("order", J::s("git-like"))], out, "A", secs);
        }
    }
    // ordering study at window 10 depth 50, zstd
    for o in [Ord_::PathCommitAsc, Ord_::PathCommitDesc, Ord_::Commit, Ord_::Size, Ord_::Id] {
        let order = ordering(ds, o);
        let t0 = Instant::now();
        let c = candidates(ds, &order, 10);
        let sel = select(&c, 50, &|_, _| true);
        let pk = finalize(ds, &order, &sel, &zz, Codec::Zstd3, true);
        let secs = t0.elapsed().as_secs_f64();
        let blk = blocks_by_size(&pk.full_z, 1 << 20);
        pack_row(ds, &order, &pk, &blk, Codec::Zstd3, &format!("delta window=10 depth<=50 order={}", o.name()), vec![("window", J::U(10)), ("order", J::s(o.name()))], out, "A", secs);
    }
    // whole-pack compression contrast (no random access possible without decompressing from the start)
    for o in [Ord_::Git, Ord_::Id] {
        let order = ordering(ds, o);
        let t0 = Instant::now();
        let mut enc = zstd::stream::Encoder::new(CountSink(0), 3).unwrap();
        enc.long_distance_matching(true).unwrap();
        enc.window_log(27).unwrap();
        use std::io::Write;
        for &i in &order {
            enc.write_all(&ds.objs[i as usize].data).unwrap();
        }
        let sink = enc.finish().unwrap();
        out.emit(
            "A",
            vec![
                ("variant", J::s(format!("whole-pack zstd-3 long-window=128MiB order={}", o.name()))),
                ("stored_bytes", J::U(sink.0)),
                ("raw_bytes", J::U(raw)),
                ("ratio", J::F(sink.0 as f64 / raw as f64)),
                ("build_secs", J::F(t0.elapsed().as_secs_f64())),
                ("note", J::s("one stream: reading any object means decompressing from a stream start; no per-object range reads")),
            ],
        );
    }
}

struct CountSink(u64);
impl std::io::Write for CountSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0 += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn run_c(ds: &Dataset, out: &Out) {
    let codec = Codec::Zstd3;
    let zz = full_sizes(ds, codec);
    let order = ordering(ds, Ord_::Git);
    let t0 = Instant::now();
    let c = candidates(ds, &order, 50);
    let cand_secs = t0.elapsed().as_secs_f64();
    let zpos: Vec<u32> = order.iter().map(|&o| zz[o as usize]).collect();
    // C1: depth bound sweep
    for d in [0u32, 1, 2, 3, 4, 8, 16, 32, 50] {
        let t0 = Instant::now();
        let sel = select(&c, d, &|_, _| true);
        let pk = finalize(ds, &order, &sel, &zz, codec, true);
        let blk = blocks_by_size(&zpos, 1 << 20);
        pack_row(ds, &order, &pk, &blk, codec, &format!("depth bound {d} (window 50, git-like order)"), vec![("max_depth", J::U(d as u64))], out, "C1", t0.elapsed().as_secs_f64() + cand_secs);
    }
    // C3: in-block vs cross-block deltas
    for bsz in [256u64 << 10, 1 << 20, 4 << 20, 16 << 20] {
        let blk = blocks_by_size(&zpos, bsz);
        for inblock in [false, true] {
            let t0 = Instant::now();
            let sel = if inblock { select(&c, 50, &|q, p| blk[q] == blk[p]) } else { select(&c, 50, &|_, _| true) };
            let pk = finalize(ds, &order, &sel, &zz, codec, true);
            let nblk = *blk.last().unwrap_or(&0) as u64 + 1;
            let cross = (0..order.len()).filter(|&p| pk.base[p].map_or(false, |q| blk[q as usize] != blk[p])).count();
            let mut bytes_per_blk = vec![0u64; nblk as usize];
            for p in 0..order.len() {
                bytes_per_blk[blk[p] as usize] += pk.stored[p] as u64;
            }
            let mean_phys = bytes_per_blk.iter().sum::<u64>() as f64 / nblk as f64;
            pack_row(
                ds,
                &order,
                &pk,
                &blk,
                codec,
                &format!("block {} KiB {}", bsz >> 10, if inblock { "in-block deltas only" } else { "cross-block deltas allowed" }),
                vec![("block_logical_bytes", J::U(bsz)), ("in_block_only", J::U(inblock as u64)), ("blocks", J::U(nblk)), ("cross_block_deltas", J::U(cross as u64)), ("mean_physical_block_bytes", J::F(mean_phys))],
                out,
                "C3",
                t0.elapsed().as_secs_f64(),
            );
        }
    }
    drop(c);
    // C2: file-history chain strategies on blobs
    for (oname, o) in [("oldest-first (forward deltas)", Ord_::PathCommitAsc), ("newest-first (reverse deltas)", Ord_::PathCommitDesc)] {
        let order = ordering(ds, o);
        let n = order.len();
        let zpos: Vec<u32> = order.iter().map(|&x| zz[x as usize]).collect();
        let blk = blocks_by_size(&zpos, 1 << 20);
        // group index within path run
        let mut gi = vec![0u32; n];
        let mut gstart = vec![0usize; n];
        for p in 0..n {
            let ob = &ds.objs[order[p] as usize];
            if p > 0 && ds.objs[order[p - 1] as usize].path == ob.path && ds.objs[order[p - 1] as usize].kind == ob.kind {
                gi[p] = gi[p - 1] + 1;
                gstart[p] = gstart[p - 1];
            } else {
                gstart[p] = p;
            }
        }
        let strategies: Vec<(String, Box<dyn Fn(usize) -> Option<u32>>)> = {
            let gi = gi.clone();
            let gs = gstart.clone();
            let mut v: Vec<(String, Box<dyn Fn(usize) -> Option<u32>>)> = vec![];
            for k in [1u32, 2, 4, 8, 16, 64, u32::MAX] {
                let (gi, gs) = (gi.clone(), gs.clone());
                let name = if k == u32::MAX { "single chain (checkpoint only at first version)".to_string() } else { format!("chain with full checkpoint every {k} versions") };
                v.push((
                    name,
                    Box::new(move |p| {
                        let i = gi[p];
                        let kind_blob = true;
                        if !kind_blob || i % k == 0 {
                            None
                        } else {
                            Some(p as u32 - 1)
                        }
                    }),
                ));
            }
            let (gi2, gs2) = (gi.clone(), gs.clone());
            v.push((
                "skip-delta (base = version with lowest set bit cleared)".into(),
                Box::new(move |p| {
                    let i = gi2[p] as usize;
                    if i == 0 {
                        None
                    } else {
                        Some((gs2[p] + (i & (i - 1))) as u32)
                    }
                }),
            ));
            v
        };
        for (sname, f) in strategies {
            let t0 = Instant::now();
            let sel: Vec<Option<u32>> = (0..n).map(|p| if ds.objs[order[p] as usize].kind == BLOB { f(p) } else { None }).collect();
            let pk = finalize(ds, &order, &sel, &zz, codec, true);
            // restrict recon targets to blobs
            pack_row(ds, &order, &pk, &blk, codec, &format!("{sname}; {oname}"), vec![("strategy", J::s(sname.clone())), ("direction", J::s(oname))], out, "C2", t0.elapsed().as_secs_f64());
        }
    }
}

/// C4: independent compressed blocks (no deltas): a block is the unit of random access, so a single-object read
/// decompresses the whole block. Shows how much of the delta/long-window gain survives a bounded block.
pub fn run_c4(ds: &Dataset, out: &Out) {
    use std::io::Write;
    let order = ordering(ds, Ord_::Git);
    let raw = ds.total_bytes();
    for bsz in [256u64 << 10, 1 << 20, 4 << 20, 16 << 20, 64 << 20] {
        for (level, ldm) in [(3i32, false), (3, true), (19, true)] {
            if level == 19 && bsz > (16 << 20) {
                continue;
            }
            let mut groups: Vec<Vec<u32>> = vec![vec![]];
            let mut acc = 0u64;
            for &o in &order {
                let l = ds.objs[o as usize].data.len() as u64;
                if acc >= bsz && !groups.last().unwrap().is_empty() {
                    groups.push(vec![]);
                    acc = 0;
                }
                groups.last_mut().unwrap().push(o);
                acc += l;
            }
            let t0 = Instant::now();
            let sizes: Vec<(u64, u64)> = par_map(groups.len(), |g| {
                let mut enc = zstd::stream::Encoder::new(CountSink(0), level).unwrap();
                if ldm {
                    enc.long_distance_matching(true).unwrap();
                }
                enc.window_log(27).unwrap();
                let mut r = 0u64;
                for &o in &groups[g] {
                    enc.write_all(&ds.objs[o as usize].data).unwrap();
                    r += ds.objs[o as usize].data.len() as u64;
                }
                (enc.finish().unwrap().0, r)
            });
            let secs = t0.elapsed().as_secs_f64();
            let stored: u64 = sizes.iter().map(|x| x.0).sum();
            // expected raw bytes decompressed to read one uniformly random object = sum(raw_g^2 / n_g)/N approx per object
            let nobj = order.len() as f64;
            let exp_dec = groups.iter().zip(&sizes).map(|(g, s)| g.len() as f64 * s.1 as f64).sum::<f64>() / nobj;
            out.emit(
                "C4",
                vec![
                    ("variant", J::s(format!("independent zstd-{level}{} blocks of {} KiB raw", if ldm { " +ldm" } else { "" }, bsz >> 10))),
                    ("block_raw_bytes", J::U(bsz)),
                    ("zstd_level", J::U(level as u64)),
                    ("blocks", J::U(groups.len() as u64)),
                    ("raw_bytes", J::U(raw)),
                    ("stored_bytes", J::U(stored)),
                    ("ratio", J::F(stored as f64 / raw as f64)),
                    ("expected_raw_bytes_decompressed_per_random_read", J::F(exp_dec)),
                    ("compress_secs_wall", J::F(secs)),
                ],
            );
        }
    }
}
