//! Binary delta (copy/insert opcodes) used by pack v4 entries.
//!
//! The wire form is Git's delta encoding (gitformat-pack, "Deltified representation"): two
//! varints (base length, target length) then opcodes. Copy: high bit set, low 4 bits select
//! offset bytes, next 3 bits select size bytes (size 0 means 0x10000). Insert: 1..=127 literal
//! bytes. Opcode 0 is reserved and rejected. Written from the format description; no Git code.
//!
//! [`apply`] treats its input as hostile: every offset and length is bounds-checked, the output
//! size is capped by the caller and must equal the declared target length exactly.

const W: usize = 16;
const P: u64 = 0x9E37_79B1_85EB_CA87;
const MAX_CHAIN: usize = 64;
/// Bases larger than this are never deltified (copy offsets are 32-bit).
pub const MAX_BASE: usize = 1 << 30;

/// Hash index over a base object, built once and reused for several targets.
pub struct Index<'a> {
    base: &'a [u8],
    shift: u32,
    heads: Vec<u32>,
    next: Vec<u32>,
}

fn pow_w1() -> u64 {
    (0..W - 1).fold(1u64, |r, _| r.wrapping_mul(P))
}

fn hash_block(b: &[u8]) -> u64 {
    b[..W].iter().fold(0u64, |h, &c| h.wrapping_mul(P).wrapping_add(c as u64 + 1))
}

impl<'a> Index<'a> {
    pub fn new(base: &'a [u8]) -> Index<'a> {
        let nb = base.len().min(MAX_BASE) / W;
        let bits = (usize::BITS - (nb.max(1) * 2).leading_zeros()).clamp(4, 24);
        let shift = 64 - bits;
        let mut heads = vec![u32::MAX; 1 << bits];
        let mut next = vec![u32::MAX; nb];
        for i in (0..nb).rev() {
            let s = (hash_block(&base[i * W..]).wrapping_mul(0xff51afd7ed558ccd) >> shift) as usize;
            next[i] = heads[s];
            heads[s] = i as u32;
        }
        Index { base, shift, heads, next }
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: usize) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(d: &[u8], pos: &mut usize) -> Option<usize> {
    let (mut v, mut sh) = (0usize, 0u32);
    loop {
        let b = *d.get(*pos)?;
        *pos += 1;
        if sh >= 63 {
            return None;
        }
        v |= ((b & 0x7f) as usize) << sh;
        if b & 0x80 == 0 {
            return Some(v);
        }
        sh += 7;
    }
}

fn emit_insert(out: &mut Vec<u8>, data: &[u8]) {
    for c in data.chunks(127) {
        out.push(c.len() as u8);
        out.extend_from_slice(c);
    }
}

fn emit_copy(out: &mut Vec<u8>, mut off: usize, mut len: usize) {
    while len > 0 {
        let n = len.min(0x10000);
        let mut cmd = 0x80u8;
        let mut args = [0u8; 7];
        let mut k = 0;
        for i in 0..4 {
            let b = ((off >> (8 * i)) & 0xff) as u8;
            if b != 0 {
                cmd |= 1 << i;
                args[k] = b;
                k += 1;
            }
        }
        if n != 0x10000 {
            for i in 0..3 {
                let b = ((n >> (8 * i)) & 0xff) as u8;
                if b != 0 {
                    cmd |= 0x10 << i;
                    args[k] = b;
                    k += 1;
                }
            }
        }
        out.push(cmd);
        out.extend_from_slice(&args[..k]);
        off += n;
        len -= n;
    }
}

fn common(a: &[u8], b: &[u8]) -> usize {
    let m = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= m {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap()) ^ u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if x != 0 {
            return i + (x.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < m && a[i] == b[i] {
        i += 1;
    }
    i
}

/// A delta turning the indexed base into `target`, or `None` as soon as it would exceed `max`
/// bytes (or the base is too large to address).
pub fn create(idx: &Index, target: &[u8], max: usize) -> Option<Vec<u8>> {
    let base = idx.base;
    if base.len() > MAX_BASE {
        return None;
    }
    let n = target.len();
    let mut out = Vec::with_capacity(64);
    put_varint(&mut out, base.len());
    put_varint(&mut out, n);
    let pw = pow_w1();
    let (mut i, mut ins) = (0usize, 0usize);
    let mut h = if n >= W { hash_block(target) } else { 0 };
    while i + W <= n {
        if out.len() > max {
            return None;
        }
        let mut best = (0usize, 0usize);
        if !idx.heads.is_empty() {
            let s = (h.wrapping_mul(0xff51afd7ed558ccd) >> idx.shift) as usize;
            let mut c = idx.heads[s];
            let mut steps = 0;
            while c != u32::MAX && steps < MAX_CHAIN {
                let off = c as usize * W;
                let l = common(&base[off..], &target[i..]);
                if l > best.0 {
                    best = (l, off);
                }
                c = idx.next[c as usize];
                steps += 1;
            }
        }
        if best.0 >= W {
            let (mut len, mut off) = best;
            let mut start = i;
            while start > ins && off > 0 && base[off - 1] == target[start - 1] {
                start -= 1;
                off -= 1;
                len += 1;
            }
            emit_insert(&mut out, &target[ins..start]);
            emit_copy(&mut out, off, len);
            i = start + len;
            ins = i;
            if i + W <= n {
                h = hash_block(&target[i..]);
            }
        } else {
            if i + W < n {
                h = h
                    .wrapping_sub((target[i] as u64 + 1).wrapping_mul(pw))
                    .wrapping_mul(P)
                    .wrapping_add(target[i + W] as u64 + 1);
            }
            i += 1;
        }
    }
    emit_insert(&mut out, &target[ins..]);
    (out.len() <= max).then_some(out)
}

/// The declared `(base length, target length)` of a delta, if its header parses.
pub fn header(d: &[u8]) -> Option<(usize, usize)> {
    let mut p = 0;
    let bl = get_varint(d, &mut p)?;
    let tl = get_varint(d, &mut p)?;
    Some((bl, tl))
}

/// Apply `d` to `base`. Fails (never panics) on a malformed delta, a base of the wrong length,
/// a copy outside the base, or a result that is not exactly the declared length (which must
/// not exceed `max_out`).
pub fn apply(base: &[u8], d: &[u8], max_out: usize) -> Result<Vec<u8>, &'static str> {
    let mut p = 0;
    let bl = get_varint(d, &mut p).ok_or("bad delta header")?;
    if bl != base.len() {
        return Err("delta base length mismatch");
    }
    let tl = get_varint(d, &mut p).ok_or("bad delta header")?;
    if tl > max_out {
        return Err("delta target too large");
    }
    let mut out = Vec::with_capacity(tl);
    while p < d.len() {
        let c = d[p];
        p += 1;
        if c & 0x80 != 0 {
            let (mut off, mut sz) = (0usize, 0usize);
            for i in 0..4 {
                if c & (1 << i) != 0 {
                    off |= (*d.get(p).ok_or("truncated delta")? as usize) << (8 * i);
                    p += 1;
                }
            }
            for i in 0..3 {
                if c & (0x10 << i) != 0 {
                    sz |= (*d.get(p).ok_or("truncated delta")? as usize) << (8 * i);
                    p += 1;
                }
            }
            if sz == 0 {
                sz = 0x10000;
            }
            let end = off.checked_add(sz).ok_or("delta copy overflows")?;
            if out.len() + sz > tl {
                return Err("delta output exceeds declared length");
            }
            out.extend_from_slice(base.get(off..end).ok_or("delta copy outside base")?);
        } else if c != 0 {
            let n = c as usize;
            if out.len() + n > tl {
                return Err("delta output exceeds declared length");
            }
            out.extend_from_slice(d.get(p..p + n).ok_or("truncated delta")?);
            p += n;
        } else {
            return Err("reserved delta opcode");
        }
    }
    if out.len() != tl {
        return Err("delta output shorter than declared length");
    }
    Ok(out)
}

/// Path-free content sketch `(min, max)` of a rolling hash over every 16-byte window. Similar
/// objects (versions of one file or directory) share the minimum, so sorting by sketch puts them
/// next to each other without knowing any path.
pub fn sketch(enc: &[u8]) -> (u64, u64) {
    if enc.len() < W + 2 {
        return (u64::MAX, 0);
    }
    let pw = pow_w1();
    let mut h = hash_block(&enc[2..]);
    let mix = |x: u64| (x ^ (x >> 29)).wrapping_mul(0xff51afd7ed558ccd) ^ (x >> 32);
    let (mut mn, mut mx) = (mix(h), mix(h));
    for i in 2 + W..enc.len() {
        h = h
            .wrapping_sub((enc[i - W] as u64 + 1).wrapping_mul(pw))
            .wrapping_mul(P)
            .wrapping_add(enc[i] as u64 + 1);
        let m = mix(h);
        mn = mn.min(m);
        mx = mx.max(m);
    }
    (mn, mx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: u64) -> impl FnMut() -> u8 {
        let mut s = seed;
        move || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        }
    }

    #[test]
    fn roundtrip_and_small_delta() {
        let mut rnd = rng(12345);
        let base: Vec<u8> = (0..50_000).map(|_| rnd()).collect();
        let mut t = base.clone();
        t.splice(1000..1010, [1u8, 2, 3]);
        t.extend_from_slice(b"tail");
        let idx = Index::new(&base);
        let d = create(&idx, &t, usize::MAX).unwrap();
        assert!(d.len() < 100, "delta {}", d.len());
        assert_eq!(apply(&base, &d, 1 << 20).unwrap(), t);
        let u: Vec<u8> = (0..1000).map(|_| rnd()).collect();
        let d = create(&idx, &u, usize::MAX).unwrap();
        assert_eq!(apply(&base, &d, 1 << 20).unwrap(), u);
        assert_eq!(apply(&[], &create(&Index::new(&[]), &u, usize::MAX).unwrap(), 1 << 20).unwrap(), u);
    }

    #[test]
    fn hostile_deltas_are_errors_not_panics() {
        let base = vec![7u8; 100];
        // wrong base length, truncated copy, copy outside base, reserved opcode, overlong output, short output
        for d in [
            &[5u8, 3, 1, 2, 3][..],
            &[100, 4, 0x91][..],
            &[100, 4, 0x91, 200, 4][..],
            &[100, 1, 0][..],
            &[100, 1, 3, 1, 2, 3][..],
            &[100, 9, 3, 1, 2, 3][..],
            &[100, 0x80][..],
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f][..],
        ] {
            assert!(apply(&base, d, 1 << 20).is_err(), "{d:?}");
        }
        // declared target above the caller cap
        assert!(apply(&base, &[100, 0xff, 0xff, 0x7f], 1 << 10).is_err());
    }

    #[test]
    fn random_garbage_never_panics() {
        let mut rnd = rng(99);
        let base: Vec<u8> = (0..300).map(|_| rnd()).collect();
        for _ in 0..20_000 {
            let n = (rnd() as usize) % 40;
            let mut d: Vec<u8> = vec![0xAC, 0x02, 1 + rnd() % 60];
            d.extend((0..n).map(|_| rnd()));
            let _ = apply(&base, &d, 1 << 16);
        }
    }

    #[test]
    fn sketch_groups_similar_objects() {
        let mut rnd = rng(5);
        let a: Vec<u8> = (0..20_000).map(|_| rnd()).collect();
        let mut b = a.clone();
        b[10_000] ^= 1;
        let c: Vec<u8> = (0..20_000).map(|_| rnd()).collect();
        assert_eq!(sketch(&a).0, sketch(&b).0);
        assert_ne!(sketch(&a).0, sketch(&c).0);
    }
}
