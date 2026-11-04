//! Git-format binary delta (copy/insert opcodes), written from the format description in
//! Documentation/gitformat-pack.txt ("Deltified representation"). Greedy 16-byte block matcher.
//! Lab code only: it is not the product's wire or storage format.

const W: usize = 16;
const P: u64 = 0x9E37_79B1_85EB_CA87;
const MAX_CHAIN: usize = 64;

pub struct Index<'a> {
    base: &'a [u8],
    shift: u32,
    heads: Vec<u32>,
    next: Vec<u32>,
}

fn pow_w1() -> u64 {
    let mut r = 1u64;
    for _ in 0..W - 1 {
        r = r.wrapping_mul(P);
    }
    r
}

fn hash_block(b: &[u8]) -> u64 {
    let mut h = 0u64;
    for &c in &b[..W] {
        h = h.wrapping_mul(P).wrapping_add(c as u64 + 1);
    }
    h
}

impl<'a> Index<'a> {
    pub fn new(base: &'a [u8]) -> Index<'a> {
        let nb = base.len() / W;
        let bits = (usize::BITS - (nb.max(1) * 2).leading_zeros()).clamp(4, 24);
        let shift = 64 - bits;
        let mut heads = vec![u32::MAX; 1 << bits];
        let mut next = vec![u32::MAX; nb];
        for i in (0..nb).rev() {
            let h = hash_block(&base[i * W..]);
            let s = (h.wrapping_mul(0xff51afd7ed558ccd) >> shift) as usize;
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

fn get_varint(d: &[u8], pos: &mut usize) -> usize {
    let mut v = 0usize;
    let mut sh = 0;
    loop {
        let b = d[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as usize) << sh;
        if b & 0x80 == 0 {
            return v;
        }
        sh += 7;
    }
}

fn emit_insert(out: &mut Vec<u8>, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(127);
        out.push(n as u8);
        out.extend_from_slice(&data[..n]);
        data = &data[n..];
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

/// Create a delta turning `idx.base` into `target`. Returns None as soon as the delta would exceed `max`.
pub fn create(idx: &Index, target: &[u8], max: usize) -> Option<Vec<u8>> {
    let base = idx.base;
    let n = target.len();
    let mut out = Vec::with_capacity(64);
    put_varint(&mut out, base.len());
    put_varint(&mut out, n);
    let pw = pow_w1();
    let mut i = 0usize;
    let mut ins = 0usize;
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
    if out.len() > max {
        return None;
    }
    Some(out)
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

pub fn apply(base: &[u8], d: &[u8]) -> Option<Vec<u8>> {
    let mut p = 0;
    let bl = get_varint(d, &mut p);
    if bl != base.len() {
        return None;
    }
    let tl = get_varint(d, &mut p);
    let mut out = Vec::with_capacity(tl);
    while p < d.len() {
        let c = d[p];
        p += 1;
        if c & 0x80 != 0 {
            let mut off = 0usize;
            let mut sz = 0usize;
            for i in 0..4 {
                if c & (1 << i) != 0 {
                    off |= (d[p] as usize) << (8 * i);
                    p += 1;
                }
            }
            for i in 0..3 {
                if c & (0x10 << i) != 0 {
                    sz |= (d[p] as usize) << (8 * i);
                    p += 1;
                }
            }
            if sz == 0 {
                sz = 0x10000;
            }
            out.extend_from_slice(base.get(off..off + sz)?);
        } else if c != 0 {
            let n = c as usize;
            out.extend_from_slice(d.get(p..p + n)?);
            p += n;
        } else {
            return None;
        }
    }
    (out.len() == tl).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let mut s = 12345u64;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        };
        let base: Vec<u8> = (0..50_000).map(|_| rnd()).collect();
        let mut t = base.clone();
        t.splice(1000..1010, [1u8, 2, 3]);
        t.extend_from_slice(b"tail");
        t.splice(40000..40000, (0..300).map(|_| rnd()));
        let idx = Index::new(&base);
        let d = create(&idx, &t, usize::MAX).unwrap();
        assert!(d.len() < 700, "delta {}", d.len());
        assert_eq!(apply(&base, &d).unwrap(), t);
        let u: Vec<u8> = (0..1000).map(|_| rnd()).collect();
        let d = create(&idx, &u, usize::MAX).unwrap();
        assert_eq!(apply(&base, &d).unwrap(), u);
        assert_eq!(apply(&[], &create(&Index::new(&[]), &u, usize::MAX).unwrap()).unwrap(), u);
    }
}
