//! Synthetic multi-zone monorepo history emitted as a `git fast-import` stream, so the very same git-based loader
//! (and the real `git repack` reference) apply to it. Skewed zone/file popularity, text edits, binary assets.

use crate::layout::Rng;
use std::io::Write;

fn word(rng: &mut Rng, vocab: &[String]) -> String {
    let i = ((rng.f().powf(2.0)) * vocab.len() as f64) as usize;
    vocab[i.min(vocab.len() - 1)].clone()
}

fn line(rng: &mut Rng, vocab: &[String]) -> String {
    let n = 3 + rng.below(9);
    let mut s = String::from("    ");
    for i in 0..n {
        if i > 0 {
            s.push(if rng.f() < 0.2 { '.' } else { ' ' });
        }
        s.push_str(&word(rng, vocab));
    }
    s.push_str(";\n");
    s
}

fn zipf_pick(rng: &mut Rng, n: usize, s: f64) -> usize {
    // inverse-transform on a continuous approximation
    let u = rng.f();
    let x = ((n as f64 + 1.0).powf(1.0 - s) - 1.0) * u + 1.0;
    ((x.powf(1.0 / (1.0 - s)) - 1.0) as usize).min(n - 1)
}

pub fn synth(zones: usize, commits: usize, seed: u64) {
    let mut rng = Rng(seed);
    let vocab: Vec<String> = (0..4000)
        .map(|i| {
            let l = 3 + rng.below(8);
            let mut w = String::new();
            for j in 0..l {
                w.push((b'a' + ((rng.below(26) + i + j) % 26) as u8) as char);
            }
            w
        })
        .collect();
    struct F {
        path: String,
        data: Vec<u8>,
        asset: bool,
    }
    let mut files: Vec<Vec<F>> = vec![];
    for z in 0..zones {
        let nf = 30 + rng.below(250);
        let mut v = vec![];
        for j in 0..nf {
            let asset = rng.f() < 0.03;
            let data = if asset {
                let n = 200_000 + rng.below(1_800_000);
                (0..n).map(|_| (rng.next() >> 20) as u8).collect()
            } else {
                let target = 2000 + (rng.f().powf(3.0) * 40_000.0) as usize;
                let mut s = String::new();
                while s.len() < target {
                    s.push_str(&line(&mut rng, &vocab));
                }
                s.into_bytes()
            };
            let path = if asset { format!("zone{z:02}/assets/img_{j}.bin") } else { format!("zone{z:02}/src/mod{}/file_{j}.rs", j % 12) };
            v.push(F { path, data, asset });
        }
        files.push(v);
    }
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::with_capacity(1 << 20, out.lock());
    let mut mark = 0;
    let mut write_file = |w: &mut dyn Write, f: &F| {
        writeln!(w, "M 100644 inline {}", f.path).unwrap();
        writeln!(w, "data {}", f.data.len()).unwrap();
        w.write_all(&f.data).unwrap();
        writeln!(w).unwrap();
    };
    // initial import: all files
    mark += 1;
    let msg = "initial import";
    writeln!(w, "commit refs/heads/main\nmark :{mark}\ncommitter Lab <lab@example.com> 1600000000 +0000\ndata {}\n{msg}", msg.len()).unwrap();
    for z in &files {
        for f in z {
            write_file(&mut w, f);
        }
    }
    writeln!(w).unwrap();
    for c in 0..commits {
        let nz = if rng.f() < 0.05 { 2 } else { 1 };
        let mut touched: Vec<(usize, usize)> = vec![];
        for _ in 0..nz {
            let z = zipf_pick(&mut rng, zones, 0.9);
            let nfile = 1 + rng.below(6);
            for _ in 0..nfile {
                let fi = zipf_pick(&mut rng, files[z].len(), 0.8);
                touched.push((z, fi));
            }
        }
        touched.sort();
        touched.dedup();
        for &(z, fi) in &touched {
            let f = &mut files[z][fi];
            if f.asset {
                let at = rng.below(f.data.len().saturating_sub(4096).max(1));
                for k in 0..4096.min(f.data.len() - at) {
                    f.data[at + k] = (rng.next() >> 20) as u8;
                }
                if rng.f() < 0.2 {
                    let extra: Vec<u8> = (0..2000).map(|_| (rng.next() >> 20) as u8).collect();
                    f.data.extend(extra);
                }
            } else {
                let s = String::from_utf8_lossy(&f.data).to_string();
                let mut lines: Vec<String> = s.split_inclusive('\n').map(|x| x.to_string()).collect();
                for _ in 0..1 + rng.below(3) {
                    let at = rng.below(lines.len().max(1));
                    let del = rng.below(4).min(lines.len().saturating_sub(at));
                    let ins: Vec<String> = (0..rng.below(5)).map(|_| line(&mut rng, &vocab)).collect();
                    lines.splice(at..at + del, ins);
                }
                f.data = lines.concat().into_bytes();
            }
        }
        mark += 1;
        let msg = format!("change {c}");
        writeln!(w, "commit refs/heads/main\nmark :{mark}\ncommitter Lab <lab@example.com> {} +0000\ndata {}\n{msg}\nfrom :{}", 1600000000 + c * 600, msg.len(), mark - 1).unwrap();
        for &(z, fi) in &touched {
            write_file(&mut w, &files[z][fi]);
        }
        writeln!(w).unwrap();
    }
}
