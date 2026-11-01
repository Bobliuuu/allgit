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

// ---------------------------------------------------------------------------------------------
// Large-file chunker (format: Kind::ChunkedLarge)
//
// Same gear hash and normalized masks as above, with two differences that make boundary search
// parallel and streamable: the hash of position `p` is defined over the 64 bytes ending at `p`
// only (`x = (x << 1) + GEAR[b]` forgets anything older), and it is evaluated at every position,
// not restarted `min` bytes after the previous cut. A position is therefore a candidate or not
// independently of where the previous chunk ended, so threads can scan disjoint segments and a
// cheap sequential pass picks the cuts. Parameters min/avg/max = 512 KiB / 2 MiB / 8 MiB are part
// of the object format.

/// Large-file chunk bounds (format constants).
pub const LARGE_MIN: usize = 512 << 10;
pub const LARGE_AVG: usize = 2 << 20;
pub const LARGE_MAX: usize = 8 << 20;
const LARGE_BITS: u32 = LARGE_AVG.trailing_zeros(); // 21
const LARGE_MASK_S: u64 = ((1u64 << (LARGE_BITS + 1)) - 1) << 16;
const LARGE_MASK_L: u64 = ((1u64 << (LARGE_BITS - 1)) - 1) << 16;
/// Bytes of history that influence the hash of a position.
pub const LARGE_WARM: usize = 64;

/// A position whose window hash clears the loose mask. `strict` also clears the strict mask.
/// The chunk ends right after byte `pos`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cand {
    pub pos: u64,
    pub strict: bool,
}

/// Append the candidates among `seg` (whose first byte is at absolute offset `seg_start`) to `out`.
/// `warm` is the (up to) 64 bytes immediately before `seg` (all that exist when `seg_start < 64`).
pub fn large_candidates(warm: &[u8], seg: &[u8], seg_start: u64, out: &mut Vec<Cand>) {
    let mut x = 0u64;
    for &b in &warm[warm.len().saturating_sub(LARGE_WARM)..] {
        x = (x << 1).wrapping_add(GEAR[b as usize]);
    }
    let mut i = 0;
    let n = seg.len();
    while i < n {
        x = (x << 1).wrapping_add(GEAR[seg[i] as usize]);
        if x & LARGE_MASK_L == 0 {
            out.push(Cand {
                pos: seg_start + i as u64,
                strict: x & LARGE_MASK_S == 0,
            });
        }
        i += 1;
    }
}

/// End offset (exclusive) of the chunk that starts at `start`, given every candidate below
/// `known_end` (`cands` sorted by position, may contain earlier ones). `total` is the input length
/// when `eof` (then `known_end == total`). `None` means more input is needed to decide.
pub fn large_next_cut(start: u64, cands: &[Cand], known_end: u64, eof: bool, total: u64) -> Option<u64> {
    let (lo, mid, max_end) = (start + LARGE_MIN as u64, start + LARGE_AVG as u64, start + LARGE_MAX as u64);
    let i = cands.partition_point(|c| c.pos < lo);
    for c in &cands[i..] {
        if c.pos >= max_end || c.pos >= known_end {
            break;
        }
        if c.pos >= mid || c.strict {
            return Some(c.pos + 1);
        }
    }
    if known_end >= max_end {
        return Some(max_end);
    }
    if eof {
        return Some(total.min(max_end));
    }
    None
}

/// Number of worker threads for data-parallel work (bounded; `VCS_THREADS` overrides).
pub fn workers() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("VCS_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n: &usize| *n >= 1)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()).min(32))
    })
}

/// `f(0..n)` evaluated on up to [`workers`] threads, results in index order.
pub fn par_map<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let t = workers().min(n);
    if t <= 1 {
        return (0..n).map(&f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut parts: Vec<Vec<(usize, T)>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..t)
            .map(|_| {
                sc.spawn(|| {
                    let mut v = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n {
                            break v;
                        }
                        v.push((i, f(i)));
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("worker panicked")).collect()
    });
    let mut all: Vec<(usize, T)> = parts.drain(..).flatten().collect();
    all.sort_by_key(|(i, _)| *i);
    all.into_iter().map(|(_, t)| t).collect()
}

/// All candidates of `data`, scanning segments in parallel.
pub fn large_all_candidates(data: &[u8]) -> Vec<Cand> {
    const SEG: usize = 4 << 20;
    let nseg = data.len().div_ceil(SEG).max(1);
    let parts = par_map(nseg, |k| {
        let (s, e) = (k * SEG, ((k + 1) * SEG).min(data.len()));
        let mut v = Vec::new();
        large_candidates(&data[s.saturating_sub(LARGE_WARM)..s], &data[s..e], s as u64, &mut v);
        v
    });
    parts.concat()
}

/// End offsets (exclusive) of the large-file chunks of `data`; the last equals `data.len()`.
pub fn large_chunk_ends(data: &[u8]) -> Vec<usize> {
    let cands = large_all_candidates(data);
    let total = data.len() as u64;
    let mut ends = Vec::with_capacity(data.len() / LARGE_AVG + 2);
    let mut start = 0u64;
    while start < total {
        let cut = large_next_cut(start, &cands, total, true, total).expect("eof always decides");
        ends.push(cut as usize);
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

    /// Straight-line definition of the large chunker: hash every position from scratch over its own 64-byte window.
    fn large_reference(data: &[u8]) -> Vec<usize> {
        let h = |p: usize| -> u64 {
            let mut x = 0u64;
            for &b in &data[(p + 1).saturating_sub(64)..=p] {
                x = (x << 1).wrapping_add(GEAR[b as usize]);
            }
            x
        };
        let mut ends = Vec::new();
        let mut start = 0usize;
        while start < data.len() {
            let end_max = (start + LARGE_MAX).min(data.len());
            let mut cut = end_max;
            for p in (start + LARGE_MIN)..end_max {
                let x = h(p);
                let ok = if p < start + LARGE_AVG {
                    x & LARGE_MASK_S == 0
                } else {
                    x & LARGE_MASK_L == 0
                };
                if ok {
                    cut = p + 1;
                    break;
                }
            }
            ends.push(cut);
            start = cut;
        }
        ends
    }

    #[test]
    fn large_chunker_matches_straight_line_definition() {
        let mut d = rnd(40 << 20, 11);
        d[5 << 20..(5 << 20) + (300 << 10)].fill(0); // zero runs force max-size chunks
        let got = large_chunk_ends(&d);
        let want = large_reference(&d[..14 << 20]);
        // the straight-line version is slow: compare on a prefix (cuts below its end are identical)
        let n = want.len() - 1;
        assert_eq!(&got[..n], &want[..n]);
        let mut prev = 0;
        for (i, &e) in got.iter().enumerate() {
            let l = e - prev;
            assert!(l <= LARGE_MAX && (l >= LARGE_MIN || i == got.len() - 1), "chunk {i} len {l}");
            prev = e;
        }
        assert_eq!(prev, d.len());
        let avg = d.len() / got.len();
        assert!((1 << 20..=5 << 20).contains(&avg), "average {avg}");
    }

    #[test]
    fn large_cuts_are_independent_of_segmenting_and_window_boundaries() {
        let d = rnd(30 << 20, 21);
        let whole = {
            let mut v = Vec::new();
            large_candidates(&[], &d, 0, &mut v);
            v
        };
        assert_eq!(whole, large_all_candidates(&d));
        // streaming: feed windows of odd sizes, select as soon as decidable
        let total = d.len() as u64;
        let mut cands: Vec<Cand> = Vec::new();
        let (mut known, mut start, mut ends) = (0usize, 0u64, Vec::new());
        while start < total {
            match large_next_cut(start, &cands, known as u64, known as u64 == total, total) {
                Some(c) => {
                    ends.push(c as usize);
                    start = c;
                }
                None => {
                    let to = (known + 3_333_333).min(d.len());
                    large_candidates(&d[known.saturating_sub(64)..known], &d[known..to], known as u64, &mut cands);
                    known = to;
                }
            }
        }
        assert_eq!(ends, large_chunk_ends(&d));
    }
}
