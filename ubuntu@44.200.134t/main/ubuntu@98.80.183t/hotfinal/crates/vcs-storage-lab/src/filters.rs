//! Approximate-membership filters over uniform 32-byte ids (hash words come straight from the id).
use crate::util::{u64_at, Id};

pub trait Filter {
    fn name(&self) -> String;
    fn contains(&self, id: &Id) -> bool;
    fn bytes(&self) -> usize;
}

/// Fast range reduction (Lemire): maps a 64-bit hash to [0, n) without requiring n to be a power of two.
fn red(h: u64, n: u64) -> u64 {
    ((h as u128 * n as u128) >> 64) as u64
}

/// Plain Bloom filter, `k` probes by double hashing.
pub struct Bloom {
    bits: Vec<u64>,
    mask: u64,
    k: u32,
    bpk: f64,
}
impl Bloom {
    pub fn build(ids: &[Id], bits_per_key: f64, k: u32) -> Bloom {
        let nbits = (((ids.len() as f64 * bits_per_key) as u64).max(64) + 63) / 64 * 64;
        let mut b = Bloom { bits: vec![0; (nbits / 64) as usize], mask: nbits, k, bpk: bits_per_key };
        for id in ids {
            let (h1, h2) = (u64_at(id, 0), u64_at(id, 1) | 1);
            for i in 0..k as u64 {
                let p = red(h1.wrapping_add(i.wrapping_mul(h2)), b.mask);
                b.bits[(p >> 6) as usize] |= 1 << (p & 63);
            }
        }
        b
    }
}
impl Filter for Bloom {
    fn name(&self) -> String {
        format!("bloom_{}bpk_k{}", self.bpk, self.k)
    }
    fn contains(&self, id: &Id) -> bool {
        let (h1, h2) = (u64_at(id, 0), u64_at(id, 1) | 1);
        (0..self.k as u64).all(|i| {
            let p = red(h1.wrapping_add(i.wrapping_mul(h2)), self.mask);
            self.bits[(p >> 6) as usize] >> (p & 63) & 1 == 1
        })
    }
    fn bytes(&self) -> usize {
        self.bits.len() * 8
    }
}

/// Cuckoo filter: 4-way buckets of 16-bit fingerprints (Fan et al. 2014, partial-key cuckoo hashing).
pub struct Cuckoo {
    b: Vec<[u16; 4]>,
    mask: u64,
}
fn fp_hash(fp: u16) -> u64 {
    (fp as u64).wrapping_mul(0x5bd1_e995_9e37_79b9) >> 7
}
impl Cuckoo {
    fn fp_and_idx(id: &Id, mask: u64) -> (u16, u64) {
        let mut fp = (u64_at(id, 2) & 0xffff) as u16;
        if fp == 0 {
            fp = 1
        }
        (fp, u64_at(id, 0) & mask)
    }
    /// Returns None when an insert fails (caller retries with a larger table).
    pub fn try_build(ids: &[Id], load: f64) -> Option<Cuckoo> {
        let nb = ((ids.len() as f64 / (4.0 * load)) as u64).max(2).next_power_of_two();
        let mut c = Cuckoo { b: vec![[0; 4]; nb as usize], mask: nb - 1 };
        let mut kick = 0x9e37_79b9u64;
        for id in ids {
            let (mut fp, i1) = Self::fp_and_idx(id, c.mask);
            let mut i = i1;
            if c.put(i, fp) || c.put(i ^ (fp_hash(fp) & c.mask), fp) {
                continue;
            }
            i ^= fp_hash(fp) & c.mask;
            let mut ok = false;
            for _ in 0..500 {
                kick = kick.wrapping_mul(6364136223846793005).wrapping_add(1);
                let s = (kick >> 60) as usize & 3;
                std::mem::swap(&mut fp, &mut c.b[i as usize][s]);
                i ^= fp_hash(fp) & c.mask;
                if c.put(i, fp) {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return None;
            }
        }
        Some(c)
    }
    pub fn build(ids: &[Id]) -> Cuckoo {
        // 0.95 is the textbook max load for 4-way buckets; back off when it fails
        for load in [0.95, 0.9, 0.85, 0.8, 0.7, 0.5] {
            if let Some(c) = Self::try_build(ids, load) {
                return c;
            }
        }
        panic!("cuckoo build failed")
    }
    fn put(&mut self, i: u64, fp: u16) -> bool {
        for s in self.b[i as usize].iter_mut() {
            if *s == 0 {
                *s = fp;
                return true;
            }
        }
        false
    }
}
impl Filter for Cuckoo {
    fn name(&self) -> String {
        "cuckoo_16bit_b4".into()
    }
    fn contains(&self, id: &Id) -> bool {
        let (fp, i1) = Self::fp_and_idx(id, self.mask);
        let i2 = i1 ^ (fp_hash(fp) & self.mask);
        self.b[i1 as usize].contains(&fp) || self.b[i2 as usize].contains(&fp)
    }
    fn bytes(&self) -> usize {
        self.b.len() * 8
    }
}

/// Binary fuse filter, 8-bit fingerprints (xorf crate; static, built from all keys at once).
pub struct Fuse8(pub xorf::BinaryFuse8);
impl Fuse8 {
    pub fn build(ids: &[Id]) -> Fuse8 {
        let keys: Vec<u64> = ids.iter().map(|i| u64_at(i, 0)).collect();
        Fuse8(xorf::BinaryFuse8::try_from(&keys).expect("fuse build"))
    }
}
impl Filter for Fuse8 {
    fn name(&self) -> String {
        "binary_fuse8".into()
    }
    fn contains(&self, id: &Id) -> bool {
        use xorf::Filter as _;
        self.0.contains(&u64_at(id, 0))
    }
    fn bytes(&self) -> usize {
        use xorf::Filter as _;
        self.0.len() // fingerprints: one byte each
    }
}
