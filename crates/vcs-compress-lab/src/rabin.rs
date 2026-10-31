//! Rabin fingerprint content-defined chunker (LBFS/restic style: GF(2) polynomial, 64-byte window).
const POLY: u64 = 0x3DA3358B4DC173;
const WIN: usize = 64;
fn deg(p: u64) -> i32 {
    63 - p.leading_zeros() as i32
}
fn pmod(mut x: u64, p: u64) -> u64 {
    let dp = deg(p);
    while x != 0 && deg(x) >= dp {
        x ^= p << (deg(x) - dp);
    }
    x
}
pub struct Rabin {
    out: [u64; 256],
    modt: [u64; 256],
    shift: u32,
}
impl Rabin {
    pub fn new() -> Rabin {
        let k = deg(POLY) as u32;
        let mut out = [0u64; 256];
        let mut modt = [0u64; 256];
        for b in 0..256u64 {
            let mut h = pmod(b, POLY);
            for _ in 0..WIN - 1 {
                h = pmod(h << 8, POLY);
            }
            out[b as usize] = h;
            modt[b as usize] = pmod(b << k, POLY) | (b << k);
        }
        Rabin { out, modt, shift: k - 8 }
    }
    /// Returns cut lengths. avg must be a power of two.
    pub fn cuts(&self, data: &[u8], min: usize, avg: usize, max: usize) -> Vec<usize> {
        let mask = (avg - 1) as u64;
        let mut res = Vec::new();
        let mut start = 0;
        while start < data.len() {
            let end_max = (start + max).min(data.len());
            let mut win = [0u8; WIN];
            let mut wpos = 0;
            let mut d: u64 = 0;
            let mut cut = end_max;
            let mut i = start;
            while i < end_max {
                let b = data[i];
                let o = win[wpos];
                win[wpos] = b;
                d ^= self.out[o as usize];
                wpos = (wpos + 1) % WIN;
                let idx = (d >> self.shift) as usize & 0xff;
                d = (d << 8) | b as u64;
                d ^= self.modt[idx];
                i += 1;
                if i - start >= min && (d & mask) == 0 {
                    cut = i;
                    break;
                }
            }
            res.push(cut - start);
            start = cut;
        }
        res
    }
}
