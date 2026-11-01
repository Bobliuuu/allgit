//! A big `add` publishes its blobs as one pack, a small one as loose files. Both must give the
//! same content ids, trees and index semantics, and the repository must verify and reopen.
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::Repo;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn populate(root: &Path, seed: u64, n: usize) -> Vec<String> {
    let mut r = Rng(seed);
    let mut paths = Vec::new();
    for i in 0..n {
        let depth = r.below(4);
        let mut p = String::new();
        for _ in 0..depth {
            p.push_str(&format!("d{}/", r.below(5)));
        }
        p.push_str(&format!("f{i}"));
        let body: Vec<u8> = match r.below(5) {
            0 => Vec::new(),
            1 => b"shared content\n".to_vec(), // duplicates across files
            2 => (0..r.below(3000)).map(|_| r.below(256) as u8).collect(),
            3 if i % 97 == 0 => (0..(5 << 20)).map(|k| (k * 7 % 251) as u8).collect(), // chunked
            _ => format!("file {i}\n").into_bytes(),
        };
        let abs = root.join(&p);
        fs::create_dir_all(abs.parent().unwrap()).unwrap();
        fs::write(abs, body).unwrap();
        paths.push(p);
    }
    paths
}

#[test]
fn bulk_add_equals_small_batches() {
    for seed in 1..=3u64 {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let paths = populate(a.path(), seed, 700);
        populate(b.path(), seed, 700);
        let (ra, rb) = (Repo::init(a.path()).unwrap(), Repo::init(b.path()).unwrap());
        ra.add(&[String::new()]).unwrap(); // 700 pending objects: one pack
        for chunk in paths.chunks(40) {
            rb.add(chunk).unwrap(); // <256 pending per barrier: loose files
        }
        ra.commit("c", sig()).unwrap();
        rb.commit("c", sig()).unwrap();
        let (la, lb) = (ra.log(vcs_repo::ROOT_ZONE, None, 5).unwrap(), rb.log(vcs_repo::ROOT_ZONE, None, 5).unwrap());
        assert_eq!(la[0].1.root, lb[0].1.root, "same tree id, seed {seed}");
        assert!(ra.fsck().unwrap().problems.is_empty(), "{:?}", ra.fsck().unwrap().problems);
        assert!(rb.fsck().unwrap().problems.is_empty());
        let packs = fs::read_dir(a.path().join(".vcs/objects/pack")).map(|d| d.count()).unwrap_or(0);
        assert!(packs >= 2, "bulk add should have produced a pack");
        // reopen: everything reads back, status is clean
        let ra2 = Repo::discover(a.path()).unwrap();
        let (sa, sb) = (ra2.status().unwrap(), rb.status().unwrap());
        assert!(sa.untracked.is_empty(), "seed {seed}");
        assert!(sa.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "seed {seed}");
        assert!(sb.zones.iter().all(|z| z.staged.is_empty() && z.unstaged.is_empty()), "seed {seed}");
        assert!(ra2.fsck().unwrap().problems.is_empty());
    }
}
