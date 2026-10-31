//! Deterministic synthetic binary / ML-like datasets, written as dirrevs (one subdirectory per revision).
use std::path::Path;

pub struct Rng(u64);
impl Rng {
    pub fn new(s: u64) -> Rng {
        Rng(s)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    pub fn f64(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn gauss(&mut self) -> f64 {
        let (u, v) = (self.f64().max(1e-12), self.f64());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
    pub fn fill(&mut self, b: &mut [u8]) {
        for c in b.chunks_mut(8) {
            let x = self.next().to_le_bytes();
            c.copy_from_slice(&x[..c.len()]);
        }
    }
}

fn tensor_f32(r: &mut Rng, n: usize, sigma: f64) -> Vec<f32> {
    (0..n).map(|_| (r.gauss() * sigma) as f32).collect()
}
fn to_bytes_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn to_bytes_bf16(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| ((x.to_bits() >> 16) as u16).to_le_bytes()).collect()
}
fn to_bytes_i8(v: &[f32]) -> Vec<u8> {
    let m = v.iter().fold(0f32, |a, b| a.max(b.abs())).max(1e-9);
    v.iter().map(|x| ((x / m * 127.0).round() as i8) as u8).collect()
}
fn safetensors(tensors: &[(String, Vec<u8>)], dtype: &str) -> Vec<u8> {
    let mut off = 0;
    let mut hdr = String::from("{");
    for (i, (n, d)) in tensors.iter().enumerate() {
        if i > 0 {
            hdr.push(',');
        }
        hdr += &format!("\"{n}\":{{\"dtype\":\"{dtype}\",\"shape\":[{}],\"data_offsets\":[{off},{}]}}", d.len(), off + d.len());
        off += d.len();
    }
    hdr.push('}');
    while hdr.len() % 8 != 0 {
        hdr.push(' ');
    }
    let mut out = (hdr.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(hdr.as_bytes());
    for (_, d) in tensors {
        out.extend_from_slice(d);
    }
    out
}

/// kinds: ml (fp32/bf16/int8 checkpoints over 3 revisions: base, partial finetune (20% layers change fully),
/// full finetune (every weight perturbed by small noise)), assets (incompressible container with insertions), sparse (embedding table, 90% zero rows)
pub fn generate(kind: &str, out: &Path, mib: usize, seed: u64) {
    let mut r = Rng::new(seed);
    match kind {
        "ml" => {
            let ntensors = 24usize;
            let per = mib * (1 << 20) / 4 / ntensors; // f32 elements per tensor
            let base: Vec<Vec<f32>> = (0..ntensors).map(|_| tensor_f32(&mut r, per, 0.02)).collect();
            let mut partial = base.clone();
            for t in partial.iter_mut().enumerate().filter(|(i, _)| i % 5 == 4).map(|(_, t)| t) {
                *t = tensor_f32(&mut r, per, 0.02);
            }
            let mut full = base.clone();
            for t in full.iter_mut() {
                for x in t.iter_mut() {
                    *x += (r.gauss() * 1e-4) as f32;
                }
            }
            for (ri, ck) in [("r1_base", &base), ("r2_partial_finetune", &partial), ("r3_full_finetune", &full)] {
                let d = out.join(ri);
                std::fs::create_dir_all(&d).unwrap();
                let named = |f: &dyn Fn(&[f32]) -> Vec<u8>| ck.iter().enumerate().map(|(i, t)| (format!("layer{i}.weight"), f(t))).collect::<Vec<_>>();
                std::fs::write(d.join("model-fp32.safetensors"), safetensors(&named(&|t| to_bytes_f32(t)), "F32")).unwrap();
                std::fs::write(d.join("model-bf16.safetensors"), safetensors(&named(&|t| to_bytes_bf16(t)), "BF16")).unwrap();
                std::fs::write(d.join("model-q8.gguf"), safetensors(&named(&|t| to_bytes_i8(t)), "I8")).unwrap();
            }
        }
        "sparse" => {
            let rows = mib * (1 << 20) / (256 * 4);
            let mut base: Vec<f32> = Vec::new();
            for _ in 0..rows {
                if r.f64() < 0.1 {
                    base.extend(tensor_f32(&mut r, 256, 0.05));
                } else {
                    base.extend(std::iter::repeat(0f32).take(256));
                }
            }
            for (ri, upd) in [("r1", 0usize), ("r2", rows / 50)] {
                let mut t = base.clone();
                for _ in 0..upd {
                    let row = (r.next() as usize) % rows;
                    for j in 0..256 {
                        t[row * 256 + j] = (r.gauss() * 0.05) as f32;
                    }
                }
                let d = out.join(ri);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("embeddings.npy"), to_bytes_f32(&t)).unwrap();
            }
            let _ = base;
        }
        "assets" => {
            // container of ~mib MiB made of random-size "already compressed" assets plus a compressible header table
            let mut assets: Vec<Vec<u8>> = Vec::new();
            let mut total = 0;
            while total < mib << 20 {
                let n = 4096 + (r.next() as usize % (1 << 20));
                let mut a = vec![0u8; n];
                r.fill(&mut a);
                total += n;
                assets.push(a);
            }
            let build = |a: &[Vec<u8>]| {
                let mut o = Vec::new();
                for (i, x) in a.iter().enumerate() {
                    o.extend_from_slice(format!("ASSET{:08}\0\0\0\0\0\0\0\0\0\0\0\0{:016}\n", i, x.len()).as_bytes());
                    o.extend_from_slice(x);
                }
                o
            };
            let mut v2 = assets.clone();
            let mut ins = vec![0u8; 100_000];
            r.fill(&mut ins);
            v2.insert(v2.len() / 2, ins);
            let mut v3 = v2.clone();
            let k = v3.len() / 3;
            r.fill(&mut v3[k]);
            for (ri, a) in [("r1", &assets), ("r2_insert_mid", &v2), ("r3_modify_one", &v3)] {
                let d = out.join(ri);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("game.pak"), build(a)).unwrap();
            }
        }
        _ => panic!("unknown synth kind {kind}: ml|sparse|assets"),
    }
}
