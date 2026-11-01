//! Experiment B: codecs, dictionaries, per-type adaptive choice.
use crate::codec::{Codec, Engine};
use crate::corpus::{Class, Corpus};
use crate::sys::*;
use serde_json::{json, Value};
use std::time::Instant;

pub struct Opts {
    pub reps: usize,
    pub cap_bytes: usize,
    pub max_file_window: usize,
}

/// Deterministic sample of a class: stride-selected files up to cap_bytes; files above the window are
/// represented by a contiguous window from their middle.
fn sample<'a>(c: &'a Corpus, class: Class, o: &Opts) -> Vec<(&'a [u8], bool)> {
    // If big files alone would exceed half of the cap, shrink the per-file window (floor 1 MiB) so that every big file is represented.
    let nbig = c
        .blobs
        .iter()
        .filter(|b| b.class == class && b.data.len() > o.max_file_window)
        .count();
    let window = if nbig * o.max_file_window > o.cap_bytes / 2 && nbig > 0 {
        (o.cap_bytes / 2 / nbig).max(1 << 20)
    } else {
        o.max_file_window
    };
    let files: Vec<(&[u8], bool)> = c
        .blobs
        .iter()
        .filter(|b| b.class == class && !b.data.is_empty())
        .map(|b| {
            let d = &b.data[..];
            // split by PATH hash so that versions of one file never land on both sides of the dictionary train/test split
            let side = blake3::hash(b.path.as_bytes()).as_bytes()[0] & 1 == 0;
            if d.len() > window {
                let s = (d.len() - window) / 2;
                (&d[s..s + window], side)
            } else {
                (d, side)
            }
        })
        .collect();
    let total: usize = files.iter().map(|f| f.0.len()).sum();
    if total <= o.cap_bytes {
        return files;
    }
    let stride = (total as f64 / o.cap_bytes as f64).ceil() as usize;
    files
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % stride == 0)
        .map(|(_, f)| f)
        .collect()
}

fn mbps(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / 1e6 / secs.max(1e-9)
}

pub fn run(c: &Corpus, o: &Opts) -> Value {
    let base: Vec<&str> = vec!["none", "lz4", "zstd1", "zstd3", "zstd9", "zstd19", "brotli5", "brotli9", "brotli11"];
    let mut classes = Vec::new();
    for class in Class::ALL {
        let mut names = base.clone();
        if matches!(class, Class::ModelCkpt | Class::LargeBinary) {
            names.extend(["shuf2+zstd3", "shuf4+zstd3", "shuf4+zstd9"]);
        }
        let codecs: Vec<Codec> = names.iter().map(|s| Codec::parse(s)).collect();
        let sampled = sample(c, class, o);
        if sampled.is_empty() {
            continue;
        }
        let side: Vec<bool> = sampled.iter().map(|x| x.1).collect();
        let files: Vec<&[u8]> = sampled.iter().map(|x| x.0).collect();
        let total: usize = files.iter().map(|f| f.len()).sum();
        eprintln!("[codecs] class {} files {} bytes {}", class.name(), files.len(), total);
        let mut rows = Vec::new();
        let mut sizes: Vec<(Codec, Vec<usize>)> = Vec::new();
        for &cd in &codecs {
            let slow = matches!(cd, Codec::Zstd(19) | Codec::Brotli(9) | Codec::Brotli(11));
            let reps = if slow { 1 } else { o.reps };
            let mut ctimes = Vec::new();
            let mut per: Vec<usize> = Vec::new();
            let mut cpeak = 0;
            for r in 0..reps {
                let mut e = Engine::new(cd);
                let mut p = Vec::with_capacity(files.len());
                let t = Instant::now();
                let ((), pk) = with_peak(|| {
                    for f in &files {
                        p.push(e.compress(f).len());
                    }
                });
                ctimes.push(t.elapsed().as_secs_f64());
                if r == 0 {
                    per = p;
                    cpeak = pk;
                }
            }
            // keep compressed copies for decompress timing
            let mut e = Engine::new(cd);
            let comps: Vec<Vec<u8>> = files.iter().map(|f| e.compress(f)).collect();
            let mut dtimes = Vec::new();
            let mut dpeak = 0;
            for r in 0..reps {
                let mut e = Engine::new(cd);
                let t = Instant::now();
                let ((), pk) = with_peak(|| {
                    for (cb, f) in comps.iter().zip(&files) {
                        let d = e.decompress(cb, f.len());
                        std::hint::black_box(&d);
                    }
                });
                dtimes.push(t.elapsed().as_secs_f64());
                if r == 0 {
                    dpeak = pk;
                }
            }
            let phys: usize = per.iter().sum();
            let cmin = ctimes.iter().cloned().fold(f64::MAX, f64::min);
            let cmax = ctimes.iter().cloned().fold(0.0, f64::max);
            let mut ct = ctimes.clone();
            let mut dt = dtimes.clone();
            rows.push(json!({
                "codec": cd.name(), "physical_bytes": phys, "ratio": total as f64 / phys as f64,
                "compress_MBps_median": mbps(total, median(&mut ct)), "compress_MBps_min_max": [mbps(total, cmax), mbps(total, cmin)],
                "decompress_MBps_median": mbps(total, median(&mut dt)), "reps": reps,
                "compress_peak_rss_delta_KiB": cpeak, "decompress_peak_rss_delta_KiB": dpeak,
            }));
            sizes.push((cd, per));
        }
        // adaptive policies (computed from per-file sizes)
        let nf = files.len();
        let mut adaptive = Vec::new();
        let oracle: usize = (0..nf).map(|i| sizes.iter().map(|(_, s)| s[i]).min().unwrap()).sum();
        adaptive.push(json!({"policy": "oracle_min_per_file_all_codecs(ignores time)", "physical_bytes": oracle}));
        let fast: Vec<&(Codec, Vec<usize>)> = sizes
            .iter()
            .filter(|(c, _)| matches!(c, Codec::None | Codec::Lz4 | Codec::Zstd(1) | Codec::Zstd(3)))
            .collect();
        let o2: usize = (0..nf).map(|i| fast.iter().map(|(_, s)| s[i]).min().unwrap()).sum();
        adaptive.push(json!({"policy": "oracle_min_per_file_among_none,lz4,zstd1,zstd3", "physical_bytes": o2}));
        // probe: lz4 on first 16KiB; incompressible (>=0.95) -> store, else zstd3
        let mut l = Engine::new(Codec::Lz4);
        let z3 = &sizes.iter().find(|(c, _)| *c == Codec::Zstd(3)).unwrap().1;
        let mut probe_total = 0usize;
        let mut stored = 0usize;
        let t = Instant::now();
        let mut decisions = Vec::new();
        for f in &files {
            let h = &f[..f.len().min(16384)];
            let pc = l.compress(h).len();
            decisions.push(pc as f64 / h.len() as f64 >= 0.95);
        }
        let probe_secs = t.elapsed().as_secs_f64();
        for (i, f) in files.iter().enumerate() {
            if decisions[i] {
                probe_total += f.len();
                stored += 1;
            } else {
                probe_total += z3[i];
            }
        }
        adaptive.push(json!({"policy": "probe16K_lz4>=0.95?store:zstd3", "physical_bytes": probe_total, "files_stored_raw": stored, "probe_MBps": mbps(files.iter().map(|f| f.len().min(16384)).sum(), probe_secs)}));
        // dictionaries
        let dict = if matches!(class, Class::SourceText | Class::SmallBinary) {
            dict_experiment(&files, &side)
        } else {
            Value::Null
        };
        classes.push(json!({"class": class.name(), "sample_files": nf, "sample_bytes": total, "codecs": rows, "adaptive": adaptive, "dictionary": dict}));
    }
    json!({"experiment": "codecs", "dataset": c.summary(), "sample_cap_bytes": o.cap_bytes, "max_file_window": o.max_file_window, "classes": classes})
}

/// Train on files whose path hashes to side A, test on side B (no path overlap => no near-duplicate leakage). Buckets by file size.
fn dict_experiment(files: &[&[u8]], side: &[bool]) -> Value {
    let train: Vec<&[u8]> = files
        .iter()
        .enumerate()
        .filter(|(i, _)| side[*i])
        .map(|(_, f)| &f[..f.len().min(65536)])
        .take(4000)
        .collect();
    let test: Vec<&[u8]> = files
        .iter()
        .enumerate()
        .filter(|(i, _)| !side[*i])
        .map(|(_, f)| *f)
        .filter(|f| f.len() <= 262144)
        .collect();
    if train.len() < 20 || test.len() < 20 {
        return json!({"skipped": "too few files", "train": train.len(), "test": test.len()});
    }
    let mut out = Vec::new();
    let buckets: [(usize, usize); 4] = [(0, 1024), (1024, 4096), (4096, 32768), (32768, 262145)];
    for dsz in [16 * 1024usize, 112 * 1024] {
        let t = Instant::now();
        let dict = match zstd::dict::from_samples(&train, dsz) {
            Ok(d) => d,
            Err(e) => {
                out.push(json!({"dict_bytes": dsz, "error": e.to_string()}));
                continue;
            }
        };
        let train_secs = t.elapsed().as_secs_f64();
        for level in [3, 9] {
            let mut plain = Engine::new(Codec::Zstd(level));
            let mut withd = Engine::zstd_dict(level, &dict);
            let mut brs = Vec::new();
            for (lo, hi) in buckets {
                let sel: Vec<&&[u8]> = test.iter().filter(|f| f.len() >= lo && f.len() < hi).collect();
                if sel.is_empty() {
                    continue;
                }
                let raw: usize = sel.iter().map(|f| f.len()).sum();
                let tp = Instant::now();
                let p: usize = sel.iter().map(|f| plain.compress(f).len()).sum();
                let tps = tp.elapsed().as_secs_f64();
                let td = Instant::now();
                let cds: Vec<Vec<u8>> = sel.iter().map(|f| withd.compress(f)).collect();
                let tds = td.elapsed().as_secs_f64();
                let d: usize = cds.iter().map(|c| c.len()).sum();
                let tdec = Instant::now();
                for (c, f) in cds.iter().zip(&sel) {
                    std::hint::black_box(withd.decompress(c, f.len()));
                }
                let tdecs = tdec.elapsed().as_secs_f64();
                brs.push(json!({"size_bucket": format!("[{lo},{hi})"), "files": sel.len(), "raw": raw, "zstd_nodict": p, "zstd_dict": d,
                    "gain_pct": 100.0 * (p as f64 - d as f64) / p as f64, "nodict_compress_MBps": mbps(raw, tps), "dict_compress_MBps": mbps(raw, tds), "dict_decompress_MBps": mbps(raw, tdecs)}));
            }
            out.push(json!({"dict_bytes": dict.len(), "requested": dsz, "train_files": train.len(), "train_secs": train_secs, "level": level, "test_files": test.len(), "buckets": brs}));
        }
    }
    json!(out)
}
