//! Experiment I: independently compressed blocks, ratio, throughput, random access, range reads.
use crate::codec::{Codec, Engine};
use crate::corpus::{Class, Corpus};
use crate::sys::*;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::time::Instant;

pub struct Opts {
    pub reps: usize,
    pub stream_cap: usize,
    pub tmp: String,
    pub order_by_path: bool,
}
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

fn stream(c: &Corpus, class: Class, cap: usize, by_path: bool) -> Vec<u8> {
    let mut s = Vec::new();
    let mut order: Vec<usize> = (0..c.blobs.len()).collect();
    if by_path {
        order.sort_by(|&a, &b| c.blobs[a].path.cmp(&c.blobs[b].path).then(a.cmp(&b)));
    }
    for b in order.iter().map(|&i| &c.blobs[i]) {
        if b.class == class {
            if s.len() + b.data.len() > cap {
                let room = cap - s.len();
                // large single files: take a leading window so the stream is contiguous real data
                if s.is_empty() {
                    s.extend_from_slice(&b.data[..room.min(b.data.len())]);
                }
                if s.len() >= cap || !s.is_empty() && room < 4096 {
                    break;
                }
                continue;
            }
            s.extend_from_slice(&b.data);
        }
    }
    s
}
fn mbps(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / 1e6 / secs.max(1e-9)
}

pub fn run(c: &Corpus, o: &Opts) -> Value {
    let mut classes = Vec::new();
    for class in Class::ALL {
        let data = stream(c, class, o.stream_cap, o.order_by_path);
        if data.len() < 1 << 20 {
            continue;
        }
        eprintln!("[blocks] class {} stream {} bytes", class.name(), data.len());
        let mut rows = Vec::new();
        for cd in [Codec::Lz4, Codec::Zstd(3), Codec::Zstd(9)] {
            let mut sizes: Vec<usize> = vec![64 << 10, 256 << 10, 1 << 20, 4 << 20, 16 << 20];
            sizes.push(data.len()); // solid
            for bs in sizes {
                let solid = bs == data.len();
                if bs > data.len() && !solid {
                    continue;
                }
                rows.push(one(&data, cd, bs, solid, o));
            }
        }
        classes.push(json!({"class": class.name(), "stream_bytes": data.len(), "rows": rows}));
    }
    json!({"experiment": "blocks", "dataset": c.summary(), "stream_order": if o.order_by_path { "by path (versions of a file adjacent)" } else { "first-seen (revision order)" }, "metadata_model": "12 bytes per block (u64 offset + u32 length)", "classes": classes})
}

fn one(data: &[u8], cd: Codec, bs: usize, solid: bool, o: &Opts) -> Value {
    let slow = matches!(cd, Codec::Zstd(9)) && data.len() > (128 << 20);
    let reps = if slow { 1 } else { o.reps };
    let blocks: Vec<&[u8]> = data.chunks(bs).collect();
    let mut ct = Vec::new();
    let mut comps: Vec<Vec<u8>> = Vec::new();
    let mut cpeak = 0;
    for r in 0..reps {
        let mut e = Engine::new(cd);
        let t = Instant::now();
        let (cs, pk) = with_peak(|| blocks.iter().map(|b| e.compress(b)).collect::<Vec<_>>());
        ct.push(t.elapsed().as_secs_f64());
        if r == 0 {
            comps = cs;
            cpeak = pk;
        }
    }
    let phys: usize = comps.iter().map(|c| c.len()).sum();
    let mut dt = Vec::new();
    let mut dpeak = 0;
    for r in 0..reps {
        let mut e = Engine::new(cd);
        let t = Instant::now();
        let ((), pk) = with_peak(|| {
            for (cb, b) in comps.iter().zip(&blocks) {
                std::hint::black_box(e.decompress(cb, b.len()));
            }
        });
        dt.push(t.elapsed().as_secs_f64());
        if r == 0 {
            dpeak = pk;
        }
    }
    let meta = 12 * comps.len();
    let mut row = json!({
        "codec": cd.name(), "block_bytes": if solid { Value::from("solid") } else { Value::from(bs) }, "blocks": comps.len(),
        "physical_bytes": phys, "ratio": data.len() as f64 / phys as f64, "block_index_bytes_model": meta,
        "compress_MBps_median": mbps(data.len(), median(&mut ct.clone())), "decompress_MBps_median": mbps(data.len(), median(&mut dt.clone())), "reps": reps,
        "compress_peak_rss_delta_KiB": cpeak, "decompress_peak_rss_delta_KiB": dpeak,
    });
    // random access: warm (memory) and cold-ish (file, fadvise DONTNEED)
    let nrd = if solid {
        3
    } else if bs >= 4 << 20 {
        300
    } else {
        2000
    };
    let mut rng = Rng(0xC0FFEE ^ bs as u64);
    let mut e = Engine::new(cd);
    for (label, len, count) in [("read4K", 4096usize, nrd), ("read1M", 1 << 20, (nrd / 10).max(3))] {
        if len > data.len() {
            continue;
        }
        let mut lat = Vec::new();
        let mut fetched = Vec::new();
        let mut sink = Vec::with_capacity(len);
        for _ in 0..count {
            let off = (rng.next() as usize) % (data.len() - len + 1);
            let b0 = off / bs;
            let b1 = (off + len - 1) / bs;
            let t = Instant::now();
            sink.clear();
            let mut f = 0;
            for bi in b0..=b1 {
                f += comps[bi].len();
                let d = e.decompress(&comps[bi], blocks[bi].len());
                let s = if bi == b0 { off - bi * bs } else { 0 };
                let en = if bi == b1 { off + len - bi * bs } else { d.len() };
                sink.extend_from_slice(&d[s..en]);
            }
            lat.push(t.elapsed().as_secs_f64() * 1e6);
            fetched.push(f as f64);
            std::hint::black_box(&sink);
        }
        let mean_f = fetched.iter().sum::<f64>() / fetched.len() as f64;
        row[label] = json!({"reads": count, "warm_us_median": median(&mut lat.clone()), "warm_us_p99": pct(&mut lat.clone(), 0.99), "compressed_bytes_fetched_mean": mean_f,
            "read_amplification_vs_requested": mean_f / len as f64});
    }
    // cold-ish file read of 4K ranges (page cache evicted per read via POSIX_FADV_DONTNEED)
    if !solid {
        let path = format!("{}/blocks_{}_{}.bin", o.tmp, cd.name(), bs);
        if let Ok(mut f) = std::fs::File::create(&path) {
            let mut offs = Vec::new();
            let mut p = 0u64;
            for cb in &comps {
                f.write_all(cb).unwrap();
                offs.push(p);
                p += cb.len() as u64;
            }
            f.sync_all().unwrap();
            drop(f);
            let f = std::fs::File::open(&path).unwrap();
            let fd = f.as_raw_fd();
            let mut lat = Vec::new();
            let mut buf = Vec::new();
            for _ in 0..(nrd / 4).max(50) {
                let off = (rng.next() as usize) % (data.len() - 4096 + 1);
                let bi = off / bs;
                unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED) };
                let t = Instant::now();
                buf.resize(comps[bi].len(), 0);
                f.read_exact_at(&mut buf, offs[bi]).unwrap();
                let d = e.decompress(&buf, blocks[bi].len());
                let s0 = off - bi * bs;
                std::hint::black_box(&d[s0..(s0 + 4096).min(d.len())]);
                lat.push(t.elapsed().as_secs_f64() * 1e6);
            }
            row["read4K_file_cold_us_median"] = json!(median(&mut lat.clone()));
            row["read4K_file_cold_us_p99"] = json!(pct(&mut lat, 0.99));
            let _ = std::fs::remove_file(&path);
        }
    }
    row
}
