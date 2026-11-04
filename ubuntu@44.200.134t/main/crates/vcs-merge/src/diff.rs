//! Line diff producing monotone match lists. Deterministic: no hashing order, no randomness.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Algorithm {
    /// Classic O(ND) shortest edit script.
    #[default]
    Myers,
    /// Anchors on lines unique to both sides (LIS), recursing between anchors; falls back to Myers.
    Patience,
    /// Git-style histogram: anchors on the common region whose lines occur least often, recursing
    /// on both sides; falls back to Myers when every common line is too frequent.
    Histogram,
}

/// Matching line pairs `(index in a, index in b)`, strictly increasing in both coordinates,
/// with `a[i] == b[j]` for each pair.
pub fn matches(alg: Algorithm, a: &[u32], b: &[u32]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    solve(alg, a, b, 0, 0, &mut out);
    out
}

fn solve(alg: Algorithm, a: &[u32], b: &[u32], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    out.extend((0..pre).map(|i| (ao + i, bo + i)));
    let (a, b) = (&a[pre..], &b[pre..]);
    let (ao, bo) = (ao + pre, bo + pre);
    let suf = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count();
    let (ae, be) = (a.len() - suf, b.len() - suf);
    if ae > 0 && be > 0 {
        match alg {
            Algorithm::Myers => myers(&a[..ae], &b[..be], ao, bo, out),
            Algorithm::Patience => patience(&a[..ae], &b[..be], ao, bo, out),
            Algorithm::Histogram => histogram(&a[..ae], &b[..be], ao, bo, out),
        }
    }
    out.extend((0..suf).map(|i| (ao + ae + i, bo + be + i)));
}

fn myers(a: &[u32], b: &[u32], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = n + m;
    let off = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'outer: for d in 0..=max {
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) {
                v[(off + k + 1) as usize]
            } else {
                v[(off + k - 1) as usize] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
    }
    let dmax = found.expect("myers terminates by d = n + m");
    let mut rev = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=dmax).rev() {
        let vp = &trace[(d - 1) as usize];
        let get = |k: isize| vp[(k + d - 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && get(k - 1) < get(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = get(prev_k);
        let prev_y = prev_x - prev_k;
        let mid_x = if prev_k == k + 1 { prev_x } else { prev_x + 1 };
        let mid_y = mid_x - k;
        while x > mid_x && y > mid_y {
            x -= 1;
            y -= 1;
            rev.push((x as usize, y as usize));
        }
        x = prev_x;
        y = prev_y;
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        rev.push((x as usize, y as usize));
    }
    out.extend(rev.into_iter().rev().map(|(i, j)| (ao + i, bo + j)));
}

fn patience(a: &[u32], b: &[u32], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    // (count, last position) per line id on each side
    let mut ca: BTreeMap<u32, usize> = BTreeMap::new();
    let mut cb: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    for x in a {
        *ca.entry(*x).or_insert(0) += 1;
    }
    for (j, x) in b.iter().enumerate() {
        let e = cb.entry(*x).or_insert((0, j));
        e.0 += 1;
        e.1 = j;
    }
    // unique-in-both pairs, in a-order
    let pairs: Vec<(usize, usize)> = a
        .iter()
        .enumerate()
        .filter_map(|(i, x)| match (ca[x], cb.get(x)) {
            (1, Some(&(1, j))) => Some((i, j)),
            _ => None,
        })
        .collect();
    let anchors = lis(&pairs);
    if anchors.is_empty() {
        myers(a, b, ao, bo, out);
        return;
    }
    let (mut pa, mut pb) = (0, 0);
    for (i, j) in anchors {
        solve(Algorithm::Patience, &a[pa..i], &b[pb..j], ao + pa, bo + pb, out);
        out.push((ao + i, bo + j));
        pa = i + 1;
        pb = j + 1;
    }
    solve(Algorithm::Patience, &a[pa..], &b[pb..], ao + pa, bo + pb, out);
}

/// Lines occurring more often than this in the old side are not used as histogram anchors.
const HIST_MAX_OCC: usize = 64;

fn histogram(a: &[u32], b: &[u32], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    let mut occ: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, x) in a.iter().enumerate() {
        occ.entry(*x).or_default().push(i);
    }
    // best region: (lowest occurrence count, then longest), earliest wins ties
    let mut best: Option<(usize, usize, usize, usize)> = None; // (count, len, i, j)
    let mut j = 0;
    while j < b.len() {
        let mut next_j = j + 1;
        if let Some(ps) = occ.get(&b[j]) {
            if ps.len() <= HIST_MAX_OCC {
                for &i in ps {
                    let (mut i0, mut j0) = (i, j);
                    let mut cnt = ps.len();
                    while i0 > 0 && j0 > 0 && a[i0 - 1] == b[j0 - 1] {
                        i0 -= 1;
                        j0 -= 1;
                        cnt = cnt.min(occ.get(&a[i0]).map_or(usize::MAX, Vec::len));
                    }
                    let (mut i1, mut j1) = (i + 1, j + 1);
                    while i1 < a.len() && j1 < b.len() && a[i1] == b[j1] {
                        cnt = cnt.min(occ.get(&a[i1]).map_or(usize::MAX, Vec::len));
                        i1 += 1;
                        j1 += 1;
                    }
                    next_j = next_j.max(j1);
                    let len = i1 - i0;
                    if best.is_none_or(|(bc, bl, ..)| cnt < bc || (cnt == bc && len > bl)) {
                        best = Some((cnt, len, i0, j0));
                    }
                }
            }
        }
        j = next_j;
    }
    match best {
        Some((_, len, i, j)) => {
            solve(Algorithm::Histogram, &a[..i], &b[..j], ao, bo, out);
            out.extend((0..len).map(|k| (ao + i + k, bo + j + k)));
            solve(Algorithm::Histogram, &a[i + len..], &b[j + len..], ao + i + len, bo + j + len, out);
        }
        None => {
            // nothing common, or every common line is too frequent
            if a.iter()
                .any(|x| occ.get(x).is_some_and(|p| p.len() > HIST_MAX_OCC) && b.contains(x))
            {
                myers(a, b, ao, bo, out);
            }
        }
    }
}

/// Longest strictly increasing subsequence by `.1` of pairs already increasing in `.0`.
fn lis(p: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; p.len()];
    for i in 0..p.len() {
        let pos = tails.partition_point(|&t| p[t].1 < p[i].1);
        if pos > 0 {
            prev[i] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.push(p[i]);
        cur = (prev[i] != usize::MAX).then_some(prev[i]);
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(alg: Algorithm, a: &[u32], b: &[u32]) -> usize {
        let m = matches(alg, a, b);
        for w in m.windows(2) {
            assert!(w[0].0 < w[1].0 && w[0].1 < w[1].1);
        }
        for (i, j) in &m {
            assert_eq!(a[*i], b[*j]);
        }
        m.len()
    }
    #[test]
    fn myers_small_cases() {
        assert_eq!(check(Algorithm::Myers, &[1, 2, 3, 4], &[2, 4, 5]), 2);
        assert_eq!(check(Algorithm::Myers, &[], &[1]), 0);
        assert_eq!(check(Algorithm::Myers, &[1, 1, 1], &[1, 1]), 2);
        assert_eq!(check(Algorithm::Myers, &[1, 2, 3], &[4, 5, 6]), 0);
    }
    #[test]
    fn patience_valid() {
        check(Algorithm::Patience, &[1, 2, 3, 4, 2, 5], &[5, 2, 4, 3, 1]);
        check(Algorithm::Patience, &[7, 7, 7], &[7, 7]);
    }
    #[test]
    fn myers_matches_lcs_length() {
        fn lcs(a: &[u32], b: &[u32]) -> usize {
            let mut t = vec![vec![0; b.len() + 1]; a.len() + 1];
            for i in 0..a.len() {
                for j in 0..b.len() {
                    t[i + 1][j + 1] = if a[i] == b[j] { t[i][j] + 1 } else { t[i][j + 1].max(t[i + 1][j]) };
                }
            }
            t[a.len()][b.len()]
        }
        let mut s = 12345u64;
        let mut rnd = |m: u64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % m) as u32
        };
        for _ in 0..300 {
            let a: Vec<u32> = (0..rnd(12)).map(|_| rnd(4)).collect();
            let b: Vec<u32> = (0..rnd(12)).map(|_| rnd(4)).collect();
            assert_eq!(check(Algorithm::Myers, &a, &b), lcs(&a, &b));
            check(Algorithm::Patience, &a, &b);
            check(Algorithm::Histogram, &a, &b);
        }
    }

    /// Turn a match list into an edit script and replay it: the result must be exactly `b`.
    fn replay(alg: Algorithm, a: &[u32], b: &[u32]) {
        let m = matches(alg, a, b);
        let (mut i, mut j) = (0, 0);
        let mut got: Vec<u32> = Vec::new();
        for (mi, mj) in m.iter().copied().chain(std::iter::once((a.len(), b.len()))) {
            assert!(mi >= i && mj >= j && mi <= a.len() && mj <= b.len(), "non-monotone script");
            // delete a[i..mi], insert b[j..mj]
            got.extend_from_slice(&b[j..mj]);
            if mi < a.len() {
                assert_eq!(a[mi], b[mj], "match must be equal lines");
                got.push(a[mi]);
            }
            i = mi + 1;
            j = mj + 1;
        }
        assert_eq!(got, b, "{alg:?} script does not reconstruct new text: a={a:?} b={b:?}");
    }

    /// Randomized round trip: every algorithm yields a valid edit script reconstructing the new text.
    #[test]
    fn every_algorithm_reconstructs_new_text() {
        let mut s = 0x9e3779b97f4a7c15u64;
        let mut rnd = |m: u64| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % m) as u32
        };
        for round in 0..1500 {
            let alpha = 2 + rnd(8) as u64;
            let a: Vec<u32> = (0..rnd(40)).map(|_| rnd(alpha)).collect();
            let mut b = a.clone();
            for _ in 0..rnd(6) {
                match rnd(3) {
                    0 if !b.is_empty() => {
                        b.remove(rnd(b.len() as u64) as usize);
                    }
                    1 => b.insert(rnd(b.len() as u64 + 1) as usize, rnd(alpha)),
                    _ if b.len() > 1 => {
                        let l = b.len() - 1;
                        b.swap(0, l)
                    }
                    _ => {}
                }
            }
            if round % 5 == 0 {
                b = (0..rnd(40)).map(|_| rnd(alpha)).collect();
            }
            for alg in [Algorithm::Myers, Algorithm::Patience, Algorithm::Histogram] {
                replay(alg, &a, &b);
            }
        }
    }

    #[test]
    fn histogram_handles_frequent_lines() {
        let a: Vec<u32> = std::iter::repeat_n(1, 100).chain([2, 3]).collect();
        let b: Vec<u32> = std::iter::repeat_n(1, 90).chain([9, 2, 3]).collect();
        replay(Algorithm::Histogram, &a, &b);
        assert!(matches(Algorithm::Histogram, &a, &b).len() >= 92);
    }
}
