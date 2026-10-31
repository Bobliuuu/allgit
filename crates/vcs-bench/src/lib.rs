//! Deterministic dataset generation for the benchmark harness. No external data,
//! no clock, no OS randomness: the same (layout, n, seed) always yields identical bytes.

use std::fs;
use std::io;
use std::path::Path;

/// SplitMix64: tiny, fast, well-distributed, fully deterministic.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn fill(&mut self, buf: &mut [u8]) {
        for ch in buf.chunks_mut(8) {
            let b = self.next_u64().to_le_bytes();
            ch.copy_from_slice(&b[..ch.len()]);
        }
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0; n];
        self.fill(&mut v);
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 100 files per directory (n/100 dirs, one level).
    Flat,
    /// Directory depth 8, fan-out 4; files spread over the leaves.
    Deep,
    /// All files in a single directory.
    BigDir,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Layout> {
        match s {
            "flat" => Some(Layout::Flat),
            "deep" => Some(Layout::Deep),
            "bigdir" => Some(Layout::BigDir),
            _ => None,
        }
    }
}

/// Relative path of file `i` for a layout.
pub fn path_of(layout: Layout, i: usize) -> String {
    match layout {
        Layout::Flat => format!("d{:04}/f{:06}.txt", i / 100, i),
        Layout::BigDir => format!("big/f{:06}.txt", i),
        Layout::Deep => {
            let mut p = String::new();
            let mut x = i;
            for _ in 0..8 {
                p.push_str(&format!("l{}/", x % 4));
                x /= 4;
            }
            format!("{p}f{:06}.txt", i)
        }
    }
}

/// Deterministic small text-ish content (100..1100 bytes) for file `i`.
pub fn content_of(seed: u64, i: usize) -> Vec<u8> {
    let mut r = Rng::new(seed ^ (i as u64).wrapping_mul(0x2545_F491_4F6C_DD1D));
    let len = 100 + (r.next_u64() % 1000) as usize;
    let mut v = r.bytes(len);
    for b in &mut v {
        *b = b'a' + (*b % 26); // printable, non-binary
    }
    v
}

/// Generate `n` files under `root`. Returns the relative paths in index order.
pub fn generate(root: &Path, layout: Layout, n: usize, seed: u64) -> io::Result<Vec<String>> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let rel = path_of(layout, i);
        let full = root.join(&rel);
        if let Some(parent) = full.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&full, content_of(seed, i))?;
        out.push(rel);
    }
    Ok(out)
}

/// Rewrite every `every`-th file with different deterministic content. Returns the count.
pub fn modify(root: &Path, paths: &[String], every: usize, seed: u64) -> io::Result<usize> {
    let mut k = 0;
    for (i, rel) in paths.iter().enumerate().step_by(every) {
        fs::write(root.join(rel), content_of(seed ^ 0xDEAD_BEEF, i))?;
        k += 1;
    }
    Ok(k)
}

/// Two large text files (`lines` lines each) differing in ~1% of lines, for diff benchmarks.
pub fn large_text_pair(lines: usize, seed: u64) -> (Vec<u8>, Vec<u8>) {
    let mut r = Rng::new(seed);
    let (mut a, mut b) = (String::new(), String::new());
    for i in 0..lines {
        let l = format!("line {i} {:016x}\n", r.next_u64());
        a.push_str(&l);
        if r.next_u64() % 100 < 1 {
            b.push_str(&format!("changed {i} {:016x}\n", r.next_u64()));
        } else {
            b.push_str(&l);
        }
    }
    (a.into_bytes(), b.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(content_of(1, 5), content_of(1, 5));
        assert_ne!(content_of(1, 5), content_of(2, 5));
        let (a1, b1) = large_text_pair(2000, 3);
        let (a2, b2) = large_text_pair(2000, 3);
        assert!(a1 == a2 && b1 == b2 && a1 != b1);
    }

    #[test]
    fn layouts_unique_paths() {
        for l in [Layout::Flat, Layout::Deep, Layout::BigDir] {
            let s: std::collections::HashSet<_> = (0..2000).map(|i| path_of(l, i)).collect();
            assert_eq!(s.len(), 2000);
        }
    }

    #[test]
    fn generate_small() {
        let d = tempfile::tempdir().unwrap();
        let p = generate(d.path(), Layout::Deep, 50, 7).unwrap();
        assert_eq!(p.len(), 50);
        assert!(d.path().join(&p[49]).is_file());
    }
}

/// Binary-heavy dataset: `n` files of `size` random (incompressible) bytes at `bin/fNNNNNN.bin`.
pub fn generate_binary(dir: &Path, n: usize, size: usize, seed: u64) -> io::Result<()> {
    fs::create_dir_all(dir.join("bin"))?;
    let mut rng = Rng::new(seed);
    for i in 0..n {
        fs::write(dir.join(format!("bin/f{i:06}.bin")), rng.bytes(size))?;
    }
    Ok(())
}

/// Next version of a binary dataset: every file gets `edits` in-place overwrites of `edit_len` bytes.
pub fn mutate_binary(dir: &Path, n: usize, edits: usize, edit_len: usize, seed: u64) -> io::Result<()> {
    let mut rng = Rng::new(seed);
    for i in 0..n {
        let p = dir.join(format!("bin/f{i:06}.bin"));
        let mut b = fs::read(&p)?;
        for _ in 0..edits {
            let at = (rng.next_u64() as usize) % (b.len() - edit_len);
            let patch = rng.bytes(edit_len);
            b[at..at + edit_len].copy_from_slice(&patch);
        }
        fs::write(&p, b)?;
    }
    Ok(())
}
