//! Content-defined chunking prototype (gear hash, FastCDC-style cut with normalized masks).
//! Finds boundaries for chunked blobs (`Object::Chunked`); parameters are fixed by the object format.

const fn gear_table() -> [u64; 256] {
    let mut t = [0u64; 256];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut i = 0;
    while i < 256 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
}
static GEAR: [u64; 256] = gear_table();

/// Chunk sizes: never below `min`, never above `max`, about `avg` on average (`avg` power of two).
#[derive(Clone, Copy, Debug)]
pub struct ChunkParams {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl ChunkParams {
    pub fn new(min: usize, avg: usize, max: usize) -> Self {
        assert!(min >= 1 && min <= avg && avg <= max && avg.is_power_of_two());
        Self { min, avg, max }
    }
}

/// End offsets (exclusive) of each chunk; the last equals `data.len()`. Empty input gives no chunks.
///
/// Boundaries are part of the object format (they determine chunk ids): this loop is only an
/// optimised form of the reference scan, checked against it in the tests.
pub fn chunk_ends(data: &[u8], p: ChunkParams) -> Vec<usize> {
    let bits = p.avg.trailing_zeros();
    // Stricter mask before the average size, looser after (normalized chunking). The `>> 16`
    // of the reference scan is folded into the masks (`m << 16`) so the hot loop is shift+add+test.
    let mask_s = ((1u64 << (bits + 1)) - 1) << 16;
    let mask_l = ((1u64 << (bits - 1).max(1)) - 1) << 16;
    let mut ends = Vec::with_capacity(data.len() / p.avg.max(1) + 2);
    let mut start = 0;
    while start < data.len() {
        let end_max = (start + p.max).min(data.len());
        let mut cut = end_max;
        if end_max - start > p.min {
            let mut h = 0u64;
            let from = start + p.min;
            let mid = (start + p.avg).clamp(from, end_max);
            match scan(&data[from..mid], &mut h, mask_s) {
                Some(i) => cut = from + i + 1,
                None => {
                    if let Some(i) = scan(&data[mid..end_max], &mut h, mask_l) {
                        cut = mid + i + 1;
                    }
                }
            }
        }
        ends.push(cut);
        start = cut;
    }
    ends
}

/// Roll the gear hash over `bytes`; index of the first byte whose hash has `mask` bits clear.
#[inline(always)]
fn scan(bytes: &[u8], h: &mut u64, mask: u64) -> Option<usize> {
    let mut x = *h;
    for (i, &b) in bytes.iter().enumerate() {
        x = (x << 1).wrapping_add(GEAR[b as usize]);
        if x & mask == 0 {
            *h = x;
            return Some(i);
        }
    }
    *h = x;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }
    const P: fn() -> ChunkParams = || ChunkParams::new(2048, 8192, 32768);

    #[test]
    fn covers_input_respects_bounds_and_is_deterministic() {
        let d = rnd(1 << 20, 7);
        let e = chunk_ends(&d, P());
        assert_eq!(*e.last().unwrap(), d.len());
        let mut prev = 0;
        for (i, &x) in e.iter().enumerate() {
            let len = x - prev;
            assert!(len <= 32768);
            assert!(len >= 2048 || i == e.len() - 1);
            prev = x;
        }
        assert_eq!(e, chunk_ends(&d, P()));
        assert!(chunk_ends(&[], P()).is_empty());
    }

    /// The original scan, kept to pin the boundaries (they are part of the object format).
    fn reference_ends(data: &[u8], p: ChunkParams) -> Vec<usize> {
        let bits = p.avg.trailing_zeros();
        let mask_s = (1u64 << (bits + 1)) - 1;
        let mask_l = (1u64 << (bits - 1).max(1)) - 1;
        let mut ends = Vec::new();
        let mut start = 0;
        while start < data.len() {
            let end_max = (start + p.max).min(data.len());
            let mut cut = end_max;
            if end_max - start > p.min {
                let mut h = 0u64;
                let mut i = start + p.min;
                let mid = (start + p.avg).min(end_max);
                while i < end_max {
                    h = (h << 1).wrapping_add(GEAR[data[i] as usize]);
                    let mask = if i < mid { mask_s } else { mask_l };
                    if (h >> 16) & mask == 0 {
                        cut = i + 1;
                        break;
                    }
                    i += 1;
                }
            }
            ends.push(cut);
            start = cut;
        }
        ends
    }

    #[test]
    fn matches_reference_scan_exactly() {
        for (seed, n) in [(1u64, 5_000_000usize), (2, 70_000), (3, 3), (4, 0), (5, 4097)] {
            let mut d = rnd(n, seed);
            if seed == 1 {
                d[1_000_000..1_300_000].fill(0); // long runs force max-size chunks
            }
            for p in [
                ChunkParams::new(4 << 10, 16 << 10, 64 << 10),
                ChunkParams::new(2048, 8192, 32768),
                ChunkParams::new(1, 2, 8),
            ] {
                assert_eq!(chunk_ends(&d, p), reference_ends(&d, p), "seed {seed} n {n} {p:?}");
            }
        }
    }

    #[test]
    fn insertion_resyncs_boundaries() {
        let d = rnd(1 << 20, 9);
        let mut d2 = d[..300_000].to_vec();
        d2.extend_from_slice(&[0xAB; 37]);
        d2.extend_from_slice(&d[300_000..]);
        let a: std::collections::HashSet<Vec<u8>> = {
            let mut s = 0;
            chunk_ends(&d, P())
                .into_iter()
                .map(|e| {
                    let c = d[s..e].to_vec();
                    s = e;
                    c
                })
                .collect()
        };
        let mut s = 0;
        let shared = chunk_ends(&d2, P())
            .into_iter()
            .filter(|&e| {
                let c = &d2[s..e];
                s = e;
                a.contains(c)
            })
            .count();
        let total = chunk_ends(&d2, P()).len();
        assert!(
            shared * 10 >= total * 8,
            "only {shared}/{total} chunks survived a 37-byte insertion"
        );
    }
}
