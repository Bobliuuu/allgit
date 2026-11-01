//! Seeded dataset generators for the storage benchmark harness (issue #182).
//!
//! A [`Plan`] is computed deterministically from `(Workload, scale)`; file bytes are a pure function of
//! `(seed, file content id, edit revision)`. No clock, no OS randomness, no external data. The same
//! `(workload, scale, seed)` always yields identical bytes, identical edit sequences, identical commit
//! histories. Every workload is documented in docs/research/storage/workloads.md.

use crate::Rng;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workload {
    SmallSource,
    MediumApp,
    LargeMonorepo,
    BinaryHeavy,
    GameAssets,
    MlCheckpoints,
    ManySimilar,
    HighlyCompressible,
    Incompressible,
    RepeatedAcrossZones,
    FrequentSmallEdits,
    FrequentLargeRewrites,
    DeepHistory,
    ManyBranches,
    ConcurrentPublishing,
    ZoneSubscription,
}

pub const ALL: [Workload; 16] = [
    Workload::SmallSource,
    Workload::MediumApp,
    Workload::LargeMonorepo,
    Workload::BinaryHeavy,
    Workload::GameAssets,
    Workload::MlCheckpoints,
    Workload::ManySimilar,
    Workload::HighlyCompressible,
    Workload::Incompressible,
    Workload::RepeatedAcrossZones,
    Workload::FrequentSmallEdits,
    Workload::FrequentLargeRewrites,
    Workload::DeepHistory,
    Workload::ManyBranches,
    Workload::ConcurrentPublishing,
    Workload::ZoneSubscription,
];

impl Workload {
    pub fn name(self) -> &'static str {
        match self {
            Workload::SmallSource => "small_source",
            Workload::MediumApp => "medium_app",
            Workload::LargeMonorepo => "large_monorepo",
            Workload::BinaryHeavy => "binary_heavy",
            Workload::GameAssets => "game_assets",
            Workload::MlCheckpoints => "ml_checkpoints",
            Workload::ManySimilar => "many_similar",
            Workload::HighlyCompressible => "highly_compressible",
            Workload::Incompressible => "incompressible",
            Workload::RepeatedAcrossZones => "repeated_across_zones",
            Workload::FrequentSmallEdits => "frequent_small_edits",
            Workload::FrequentLargeRewrites => "frequent_large_rewrites",
            Workload::DeepHistory => "deep_history",
            Workload::ManyBranches => "many_branches",
            Workload::ConcurrentPublishing => "concurrent_publishing",
            Workload::ZoneSubscription => "zone_subscription",
        }
    }
    pub fn parse(s: &str) -> Option<Workload> {
        ALL.iter().copied().find(|w| w.name() == s)
    }
}

/// Content family of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Source-code-like lines (compressible, line-oriented).
    Source,
    /// Prose-like words (compressible, line-oriented).
    Text,
    /// Uniform random bytes (incompressible).
    Random,
    /// A short repeating pattern with zero runs (extremely compressible).
    Compressible,
    /// Smooth gradients plus low noise (texture-like, partly compressible).
    Image,
    /// float32 tensor bytes: random mantissa, narrow exponent range (barely compressible).
    Tensor,
    /// A 1 KiB template shared by all files with a few varying bytes.
    Similar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// Replace `n` whole lines with new lines (line-oriented kinds).
    Lines(usize),
    /// Overwrite `count` regions of `len` bytes in place with fresh bytes of the file's kind.
    Region { len: usize, count: usize },
    /// Regenerate the whole file (new seed).
    Rewrite,
    /// Flip the low byte of every `1/denom`-th float (tensor-style training step).
    Perturb { denom: usize },
}

#[derive(Clone, Debug)]
pub struct FileSpec {
    pub path: String,
    pub kind: Kind,
    pub size: usize,
    /// Content id: files with equal (kind,size,cid) have identical bytes (used for cross-zone repetition).
    pub cid: u64,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub workload: Workload,
    pub files: Vec<FileSpec>,
    /// (zone name, path prefix) in vcs terms; empty = only the root zone.
    pub zones: Vec<(String, String)>,
    /// Commits after the base commit (history length - 1).
    pub revisions: usize,
    /// Files touched per revision.
    pub edits_per_rev: usize,
    pub edit: Edit,
    /// Extra branches, each with one single-file commit.
    pub branches: usize,
    /// Zone whose files history/edits touch (`None` = whole repo, root zone).
    pub hist_zone: Option<String>,
    pub scale: usize,
}

/// Parse "smoke" (1), "small" (8), "medium" (64) or a positive integer.
pub fn parse_scale(s: &str) -> Option<usize> {
    match s {
        "smoke" => Some(1),
        "small" => Some(8),
        "medium" => Some(64),
        n => n.parse().ok().filter(|v| *v > 0),
    }
}

const KB: usize = 1024;

fn sz(r: &mut Rng, lo: usize, hi: usize) -> usize {
    lo + (r.next_u64() as usize) % (hi - lo + 1)
}

/// Build the plan. `scale` multiplies file counts only (sizes, zones, history length are fixed per workload,
/// except `many_branches` whose branch count is fixed at 50).
pub fn plan(w: Workload, scale: usize) -> Plan {
    let f = scale.max(1);
    let mut r = Rng::new(0x5EED_0000 ^ (w as u64));
    let mut files: Vec<FileSpec> = Vec::new();
    let mut zones: Vec<(String, String)> = Vec::new();
    let mut p = Plan {
        workload: w,
        files: vec![],
        zones: vec![],
        revisions: 10,
        edits_per_rev: 10,
        edit: Edit::Lines(3),
        branches: 0,
        hist_zone: None,
        scale: f,
    };
    let mut cid = 0u64;
    let mut add = |files: &mut Vec<FileSpec>, path: String, kind: Kind, size: usize| {
        files.push(FileSpec { path, kind, size, cid });
        cid += 1;
    };
    match w {
        Workload::SmallSource => {
            for i in 0..1000 * f {
                let s = sz(&mut r, 200, 4 * KB);
                add(
                    &mut files,
                    format!("src/m{:03}/s{:02}/f{:06}.rs", i / 400, (i / 20) % 20, i),
                    Kind::Source,
                    s,
                );
            }
        }
        Workload::MediumApp => {
            for i in 0..200 * f {
                let s = sz(&mut r, KB, 16 * KB);
                add(&mut files, format!("src/d{:03}/f{:05}.rs", i / 50, i), Kind::Source, s);
            }
            for i in 0..40 * f {
                let s = sz(&mut r, 16 * KB, 128 * KB);
                add(&mut files, format!("assets/img/a{:05}.bin", i), Kind::Image, s);
            }
            zones = vec![("src".into(), "src".into()), ("assets".into(), "assets".into())];
            p.edit = Edit::Region { len: 512, count: 2 };
            p.edits_per_rev = 4;
            p.hist_zone = Some("src".into());
        }
        Workload::LargeMonorepo => {
            for s in 0..8 {
                for i in 0..500 * f {
                    let z = sz(&mut r, 200, 2 * KB);
                    add(
                        &mut files,
                        format!("svc{s}/pkg{:02}/mod{:02}/f{:06}.rs", i / 250, (i / 10) % 25, i),
                        Kind::Source,
                        z,
                    );
                }
                zones.push((format!("svc{s}"), format!("svc{s}")));
            }
            p.edits_per_rev = 20;
            p.hist_zone = Some("svc0".into());
        }
        Workload::BinaryHeavy => {
            for i in 0..24 * f {
                add(&mut files, format!("bin/f{:05}.bin", i), Kind::Random, 256 * KB);
            }
            p.edit = Edit::Region { len: 4 * KB, count: 4 };
            p.revisions = 5;
            p.edits_per_rev = 6;
        }
        Workload::GameAssets => {
            for i in 0..16 * f {
                let s = sz(&mut r, 512 * KB, 2 * 1024 * KB);
                add(&mut files, format!("assets/tex/t{:04}.img", i), Kind::Image, s);
            }
            for i in 0..8 * f {
                let s = sz(&mut r, 256 * KB, 1024 * KB);
                add(&mut files, format!("audio/a{:04}.snd", i), Kind::Random, s);
            }
            for i in 0..50 * f {
                let s = sz(&mut r, KB, 8 * KB);
                add(&mut files, format!("code/s{:04}.rs", i), Kind::Source, s);
            }
            zones = vec![
                ("assets".into(), "assets".into()),
                ("audio".into(), "audio".into()),
                ("code".into(), "code".into()),
            ];
            p.edit = Edit::Rewrite;
            p.revisions = 5;
            p.edits_per_rev = 2;
            p.hist_zone = Some("assets".into());
        }
        Workload::MlCheckpoints => {
            for i in 0..3 * f {
                add(&mut files, format!("ckpt/layer{:02}.tensor", i), Kind::Tensor, 4 * 1024 * KB);
            }
            add(&mut files, "ckpt/optimizer.state".into(), Kind::Tensor, 8 * 1024 * KB);
            zones = vec![("ckpt".into(), "ckpt".into())];
            p.edit = Edit::Perturb { denom: 20 };
            p.revisions = 5;
            p.edits_per_rev = 3;
            p.hist_zone = Some("ckpt".into());
        }
        Workload::ManySimilar => {
            for i in 0..2000 * f {
                add(&mut files, format!("tpl/d{:03}/f{:06}.cfg", i / 100, i), Kind::Similar, KB);
            }
            p.edit = Edit::Region { len: 16, count: 2 };
            p.revisions = 5;
            p.edits_per_rev = 20;
        }
        Workload::HighlyCompressible => {
            for i in 0..16 * f {
                add(&mut files, format!("zeros/f{:05}.dat", i), Kind::Compressible, 256 * KB);
            }
            p.edit = Edit::Region { len: 4 * KB, count: 2 };
            p.revisions = 5;
            p.edits_per_rev = 4;
        }
        Workload::Incompressible => {
            for i in 0..16 * f {
                add(&mut files, format!("rand/f{:05}.dat", i), Kind::Random, 256 * KB);
            }
            p.edit = Edit::Rewrite;
            p.revisions = 5;
            p.edits_per_rev = 2;
        }
        Workload::RepeatedAcrossZones => {
            for z in 0..4 {
                for i in 0..200 * f {
                    // identical content across zones: cid depends only on i
                    let mut s = FileSpec {
                        path: format!("z{z}/shared/f{:05}.txt", i),
                        kind: Kind::Text,
                        size: 2 * KB,
                        cid: 1_000_000 + i as u64,
                    };
                    s.size = 2 * KB;
                    files.push(s);
                }
                for i in 0..20 * f {
                    files.push(FileSpec {
                        path: format!("z{z}/own/u{:05}.txt", i),
                        kind: Kind::Text,
                        size: 2 * KB,
                        cid: 2_000_000 + (z * 100_000 + i) as u64,
                    });
                }
                zones.push((format!("z{z}"), format!("z{z}")));
            }
            p.edits_per_rev = 8;
            p.hist_zone = Some("z0".into());
        }
        Workload::FrequentSmallEdits => {
            for i in 0..300 * f {
                let s = sz(&mut r, KB, 4 * KB);
                add(&mut files, format!("src/d{:02}/f{:05}.rs", i / 30, i), Kind::Source, s);
            }
            p.edit = Edit::Lines(1);
            p.revisions = 100;
            p.edits_per_rev = 3;
        }
        Workload::FrequentLargeRewrites => {
            for i in 0..20 * f {
                add(&mut files, format!("docs/f{:04}.txt", i), Kind::Text, 256 * KB);
            }
            p.edit = Edit::Rewrite;
            p.revisions = 20;
            p.edits_per_rev = 5;
        }
        Workload::DeepHistory => {
            for i in 0..200 * f {
                let s = sz(&mut r, 500, 3 * KB);
                add(&mut files, format!("src/d{:02}/f{:05}.rs", i / 20, i), Kind::Source, s);
            }
            p.edit = Edit::Lines(2);
            p.revisions = 300;
            p.edits_per_rev = 2;
        }
        Workload::ManyBranches => {
            for i in 0..300 * f {
                let s = sz(&mut r, KB, 4 * KB);
                add(&mut files, format!("src/d{:02}/f{:05}.rs", i / 30, i), Kind::Source, s);
            }
            p.revisions = 3;
            p.branches = 50;
        }
        Workload::ConcurrentPublishing => {
            for z in 0..4 {
                for i in 0..200 * f {
                    let s = sz(&mut r, KB, 4 * KB);
                    add(&mut files, format!("p{z}/f{:05}.rs", i), Kind::Source, s);
                }
                zones.push((format!("p{z}"), format!("p{z}")));
            }
            p.revisions = 5;
            p.hist_zone = Some("p0".into());
        }
        Workload::ZoneSubscription => {
            for z in 0..100 {
                for i in 0..20 * f {
                    let s = sz(&mut r, KB, 3 * KB);
                    add(&mut files, format!("z{z:03}/f{:04}.rs", i), Kind::Source, s);
                }
                zones.push((format!("z{z:03}"), format!("z{z:03}")));
            }
            p.revisions = 3;
            p.hist_zone = Some("z000".into());
        }
    }
    p.files = files;
    p.zones = zones;
    p
}

impl Plan {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size as u64).sum()
    }
    /// Path prefix (with trailing '/') of the history zone, or "" for the whole repo.
    pub fn hist_prefix(&self) -> String {
        match &self.hist_zone {
            Some(z) => self
                .zones
                .iter()
                .find(|(n, _)| n == z)
                .map(|(_, p)| format!("{p}/"))
                .unwrap_or_default(),
            None => String::new(),
        }
    }
    /// Indices of files inside the history zone.
    pub fn hist_files(&self) -> Vec<usize> {
        self.files_in(&self.hist_prefix())
    }
    /// Indices of files whose path starts with `prefix` ("" = all).
    pub fn files_in(&self, prefix: &str) -> Vec<usize> {
        (0..self.files.len()).filter(|i| self.files[*i].path.starts_with(prefix)).collect()
    }
    /// A deterministic "hot" file (many edits land here): first file of the history zone.
    pub fn hot_file(&self) -> &str {
        &self.files[self.hist_files()[0]].path
    }
    /// Paths (relative) belonging to the zones/dirs chosen by `names`.
    pub fn zone_prefix(&self, name: &str) -> Option<String> {
        self.zones.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone())
    }
}

const WORDS: [&str; 32] = [
    "alpha", "buffer", "commit", "delta", "engine", "fetch", "graph", "hash", "index", "journal", "kernel", "layer", "merge", "node",
    "object", "pack", "query", "ref", "store", "tree", "unit", "view", "write", "zone", "chunk", "blob", "cache", "diff", "fsck", "log",
    "sync", "tag",
];

fn gen_lines(r: &mut Rng, size: usize, source: bool) -> Vec<u8> {
    let mut s = String::with_capacity(size + 80);
    let mut i = 0u64;
    while s.len() < size {
        let a = WORDS[(r.next_u64() % 32) as usize];
        let b = WORDS[(r.next_u64() % 32) as usize];
        let n = r.next_u64() % 10_000;
        if source {
            s.push_str(&format!("    let {a}_{i} = {b}::get({n}, {});\n", r.next_u64() % 97));
        } else {
            s.push_str(&format!(
                "{a} {b} {n} {} {}\n",
                WORDS[(r.next_u64() % 32) as usize],
                WORDS[(r.next_u64() % 32) as usize]
            ));
        }
        i += 1;
    }
    s.truncate(size);
    s.into_bytes()
}

/// Bytes of kind `kind` and length `size` for stream `seed`.
fn gen_kind(kind: Kind, size: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    match kind {
        Kind::Source => gen_lines(&mut r, size, true),
        Kind::Text => gen_lines(&mut r, size, false),
        Kind::Random => r.bytes(size),
        Kind::Compressible => {
            let pat = r.bytes(64);
            let mut v = Vec::with_capacity(size);
            while v.len() < size {
                if r.next_u64().is_multiple_of(8) {
                    v.extend(std::iter::repeat_n(0u8, 256));
                } else {
                    v.extend_from_slice(&pat);
                }
            }
            v.truncate(size);
            v
        }
        Kind::Image => {
            let w = 512usize;
            let mut v = Vec::with_capacity(size);
            let mut i = 0usize;
            while v.len() < size {
                let (x, y) = (i % w, i / w);
                v.push(((x * 3 + y * 5) as u64 + (r.next_u64() & 7)) as u8);
                i += 1;
            }
            v
        }
        Kind::Tensor => {
            let mut v = Vec::with_capacity(size + 4);
            while v.len() < size {
                let m = r.next_u64();
                // sign random, exponent in 0x76..=0x7d (values ~ 2^-9..2^-2), 23-bit random mantissa
                let bits: u32 = (((m >> 40) as u32 & 1) << 31) | ((0x76 + ((m >> 32) as u32 % 8)) << 23) | (m as u32 & 0x7F_FFFF);
                v.extend_from_slice(&bits.to_le_bytes());
            }
            v.truncate(size);
            v
        }
        Kind::Similar => {
            let mut t = Rng::new(0x51_71_14_AA).bytes(size);
            for b in &mut t {
                *b = b'a' + (*b % 26);
            }
            for _ in 0..if size > 32 { 4 } else { 0 } {
                let at = (r.next_u64() as usize) % (size - 16);
                for k in 0..16 {
                    t[at + k] = b'0' + (r.next_u64() % 10) as u8;
                }
            }
            t
        }
    }
}

/// Base content of a file (revision 0).
pub fn content(seed: u64, f: &FileSpec) -> Vec<u8> {
    gen_kind(f.kind, f.size, seed ^ f.cid.wrapping_mul(0x2545_F491_4F6C_DD1D))
}

fn edit_bytes(buf: &mut Vec<u8>, f: &FileSpec, e: Edit, seed: u64, tag: u64) {
    let mut r = Rng::new(seed ^ tag.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ f.cid);
    match e {
        Edit::Rewrite => *buf = gen_kind(f.kind, f.size, r.next_u64()),
        Edit::Region { len, count } => {
            for _ in 0..count {
                let len = len.min(buf.len() / 2).max(1);
                let at = (r.next_u64() as usize) % (buf.len() - len + 1);
                let fresh = gen_kind(f.kind, len, r.next_u64());
                buf[at..at + len].copy_from_slice(&fresh);
            }
        }
        Edit::Lines(n) => {
            // replace n lines (by index) with new same-kind lines; keeps file roughly the same size
            let mut lines: Vec<Vec<u8>> = buf.split_inclusive(|b| *b == b'\n').map(|l| l.to_vec()).collect();
            if lines.is_empty() {
                return;
            }
            for _ in 0..n {
                let i = (r.next_u64() as usize) % lines.len();
                let mut l = gen_kind(f.kind, 40, r.next_u64());
                l.push(b'\n');
                lines[i] = l;
            }
            *buf = lines.concat();
        }
        Edit::Perturb { denom } => {
            let mut i = 0;
            while i + 4 <= buf.len() {
                if r.next_u64().is_multiple_of(denom as u64) {
                    buf[i] = (r.next_u64() & 0xFF) as u8; // low mantissa byte
                }
                i += 4;
            }
        }
    }
}

/// Write the base files under `root`. Returns total bytes written.
pub fn generate(plan: &Plan, root: &Path, seed: u64) -> io::Result<u64> {
    let mut total = 0;
    for f in &plan.files {
        let full = root.join(&f.path);
        if let Some(p) = full.parent() {
            fs::create_dir_all(p)?;
        }
        let c = content(seed, f);
        total += c.len() as u64;
        fs::write(full, c)?;
    }
    Ok(total)
}

/// Apply revision `rev` (1-based; tag any u64) of the workload's edit pattern in place: `n` files of the history
/// zone chosen deterministically. Returns the changed relative paths.
pub fn evolve(plan: &Plan, root: &Path, seed: u64, tag: u64, n: usize) -> io::Result<Vec<String>> {
    evolve_in(plan, root, seed, tag, n, &plan.hist_prefix())
}

/// Like [`evolve`] but restricted to files under `prefix` ("" = whole repo).
pub fn evolve_in(plan: &Plan, root: &Path, seed: u64, tag: u64, n: usize, prefix: &str) -> io::Result<Vec<String>> {
    let hf = plan.files_in(prefix);
    let mut r = Rng::new(seed ^ 0x00E7_010E ^ tag.wrapping_mul(0xD6E8_FEB8_6659_FD93));
    let mut out: Vec<String> = Vec::new();
    let mut guard = 0;
    while out.len() < n.min(hf.len()) && guard < n * 20 + 20 {
        guard += 1;
        let f = &plan.files[hf[(r.next_u64() as usize) % hf.len()]];
        if out.contains(&f.path) {
            continue;
        }
        let p = root.join(&f.path);
        let mut buf = fs::read(&p)?;
        edit_bytes(&mut buf, f, plan.edit, seed, tag);
        fs::write(&p, buf)?;
        out.push(f.path.clone());
    }
    Ok(out)
}

/// Edit a percentage of the history zone's files (at least 1) with the workload's edit pattern.
pub fn evolve_pct(plan: &Plan, root: &Path, seed: u64, tag: u64, pct: usize) -> io::Result<Vec<String>> {
    let n = (plan.hist_files().len() * pct / 100).max(1);
    evolve(plan, root, seed, tag, n)
}

/// Edit one file for a branch commit.
pub fn evolve_one(plan: &Plan, root: &Path, seed: u64, tag: u64) -> io::Result<Vec<String>> {
    evolve(plan, root, seed, tag, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_are_sane_and_deterministic() {
        for w in ALL {
            let a = plan(w, 1);
            let b = plan(w, 1);
            assert!(!a.files.is_empty(), "{}", w.name());
            assert_eq!(a.files.len(), b.files.len());
            let set: std::collections::HashSet<_> = a.files.iter().map(|f| f.path.clone()).collect();
            assert_eq!(set.len(), a.files.len(), "{} duplicate paths", w.name());
            assert!(!a.hist_files().is_empty(), "{}", w.name());
            for (n, p) in &a.zones {
                assert!(
                    a.files.iter().any(|f| f.path.starts_with(&format!("{p}/"))),
                    "{} zone {n} empty",
                    w.name()
                );
            }
            assert_eq!(Workload::parse(w.name()), Some(w));
        }
    }

    #[test]
    fn generate_and_evolve_reproducible() {
        for w in [
            Workload::SmallSource,
            Workload::MlCheckpoints,
            Workload::ManySimilar,
            Workload::GameAssets,
        ] {
            let mut p = plan(w, 1);
            p.files.truncate(p.files.len().min(40));
            let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            generate(&p, d1.path(), 9).unwrap();
            generate(&p, d2.path(), 9).unwrap();
            let c1 = evolve(&p, d1.path(), 9, 1, 3).unwrap();
            let c2 = evolve(&p, d2.path(), 9, 1, 3).unwrap();
            assert_eq!(c1, c2);
            for f in &p.files {
                assert_eq!(
                    fs::read(d1.path().join(&f.path)).unwrap(),
                    fs::read(d2.path().join(&f.path)).unwrap(),
                    "{}",
                    f.path
                );
            }
            let base = content(9, &p.files[0].clone());
            assert_eq!(base.len(), p.files[0].size);
            for c in &c1 {
                let f = p.files.iter().find(|f| &f.path == c).unwrap();
                assert_ne!(fs::read(d1.path().join(c)).unwrap(), content(9, f), "{} unchanged", c);
            }
        }
    }

    #[test]
    fn repeated_across_zones_share_bytes() {
        let p = plan(Workload::RepeatedAcrossZones, 1);
        let a = p.files.iter().find(|f| f.path == "z0/shared/f00003.txt").unwrap();
        let b = p.files.iter().find(|f| f.path == "z3/shared/f00003.txt").unwrap();
        assert_eq!(content(1, a), content(1, b));
    }
}
