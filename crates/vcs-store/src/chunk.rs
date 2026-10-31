//! Content-defined chunking prototype (gear hash, FastCDC-style cut with normalized masks).
//! Not wired into the object model: it only finds boundaries so dedup can be measured.

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
pub fn chunk_ends(data: &[u8], p: ChunkParams) -> Vec<usize> {
    let bits = p.avg.trailing_zeros();
    // Stricter mask before the average size, looser after (normalized chunking).
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
                // gear hash depends on the last 64 bytes only; mask tests the high bits
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
