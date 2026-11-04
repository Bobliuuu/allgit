//! Line diff (LCS on trimmed middle) and unified output.
//!
//! MVP algorithm: strip common prefix/suffix, then O(n*m) LCS for the middle. If the middle
//! is huge (n*m > LIMIT) fall back to "delete all, insert all" (correct, not minimal).
//! Myers/patience/histogram are Milestone 4 and will be benchmarked against this.

const LIMIT: usize = 25_000_000;
const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Eq,
    Del,
    Ins,
}

fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split_inclusive(|&c| c == b'\n').collect()
}

/// FxHash-style hasher: lines are interned by the thousand per diff, SipHash dominated the cost.
#[derive(Default, Clone, Copy)]
struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let mut h = self.0;
        let mut rest = bytes;
        while rest.len() >= 8 {
            let (w, r) = rest.split_at(8);
            h = (h.rotate_left(5) ^ u64::from_le_bytes(w.try_into().unwrap())).wrapping_mul(K);
            rest = r;
        }
        for &b in rest {
            h = (h.rotate_left(5) ^ u64::from(b)).wrapping_mul(K);
        }
        self.0 = h;
    }
}

type FxMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

fn intern<'a>(ids: &mut FxMap<&'a [u8], u32>, ls: &[&'a [u8]]) -> Vec<u32> {
    ls.iter()
        .map(|l| {
            let next = ids.len() as u32;
            *ids.entry(*l).or_insert(next)
        })
        .collect()
}

fn lcs_ops<T>(am: &[u32], bm: &[u32], p: usize, ops: &mut Vec<(Tag, usize, usize)>)
where
    T: Copy + Ord + Default + From<u8> + std::ops::Add<Output = T>,
{
    let (n, m) = (am.len(), bm.len());
    let w = m + 1;
    let mut t = vec![T::default(); (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if am[i] == bm[j] {
                t[(i + 1) * w + j + 1] + T::from(1u8)
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            ops.push((Tag::Eq, p + i, p + j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || t[(i + 1) * w + j] >= t[i * w + j + 1]) {
            ops.push((Tag::Del, p + i, 0));
            i += 1;
        } else {
            ops.push((Tag::Ins, 0, p + j));
            j += 1;
        }
    }
}

/// Same result as [`lcs_ops`], computing the LCS table only for diagonals `|j - i| <= k`.
/// Returns `None` unless the result is provably identical to the full table's: cells outside the
/// band read as 0 (a lower bound), and every cell the traceback consults has an optimal suffix
/// alignment inside the band whenever `2 * D + 1 <= k`, `D` being the edit distance found.
fn lcs_ops_banded(am: &[u32], bm: &[u32], p: usize, k: usize, ops: &mut Vec<(Tag, usize, usize)>) -> Option<()> {
    let (n, m) = (am.len(), bm.len());
    let w = 2 * k + 1;
    let mut t = vec![0u32; (n + 1) * w];
    let at = |t: &[u32], i: usize, j: usize| -> u32 {
        // d = j - i + k, outside the band reads as 0
        match (j + k).checked_sub(i) {
            Some(d) if d < w => t[i * w + d],
            _ => 0,
        }
    };
    for i in (0..n).rev() {
        if m == 0 {
            break;
        }
        let lo = i.saturating_sub(k);
        let hi = (i + k).min(m.saturating_sub(1));
        if lo > hi {
            continue;
        }
        for j in (lo..=hi).rev() {
            let v = if am[i] == bm[j] {
                at(&t, i + 1, j + 1) + 1
            } else {
                at(&t, i + 1, j).max(at(&t, i, j + 1))
            };
            t[i * w + j + k - i] = v;
        }
    }
    let lcs = at(&t, 0, 0) as usize;
    let dist = n + m - 2 * lcs;
    if 2 * dist + 1 > k {
        return None;
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            ops.push((Tag::Eq, p + i, p + j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || at(&t, i + 1, j) >= at(&t, i, j + 1)) {
            ops.push((Tag::Del, p + i, 0));
            i += 1;
        } else {
            ops.push((Tag::Ins, 0, p + j));
            j += 1;
        }
    }
    Some(())
}

/// LCS script over interned lines; banded when the sequences are close (the common case: a few
/// edited lines), the full table otherwise. Both give the same script.
fn lcs_script(ai: &[u32], bi: &[u32], p: usize, ops: &mut Vec<(Tag, usize, usize)>) {
    let (n, m) = (ai.len(), bi.len());
    let mut k = 2 * n.abs_diff(m) + 17;
    while k < n.max(m) / 2 {
        let mark = ops.len();
        if lcs_ops_banded(ai, bi, p, k, ops).is_some() {
            return;
        }
        ops.truncate(mark);
        k *= 4;
    }
    if n.min(m) < u16::MAX as usize {
        lcs_ops::<u16>(ai, bi, p, ops);
    } else {
        lcs_ops::<u32>(ai, bi, p, ops);
    }
}

fn script(a: &[&[u8]], b: &[&[u8]]) -> Vec<(Tag, usize, usize)> {
    // (tag, index into a, index into b); unused index is 0
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (n, m) = (a.len() - p - s, b.len() - p - s);
    let mut ops = Vec::with_capacity(a.len().max(b.len()));
    for i in 0..p {
        ops.push((Tag::Eq, i, i));
    }
    let (am, bm) = (&a[p..p + n], &b[p..p + m]);
    if n * m > LIMIT {
        for i in 0..n {
            ops.push((Tag::Del, p + i, 0));
        }
        for j in 0..m {
            ops.push((Tag::Ins, 0, p + j));
        }
    } else {
        // the table holds LCS lengths, at most min(n, m): 16 bits halve the memory traffic when they fit
        // lines become small integers first: the inner loop compares n*m pairs
        let mut ids: FxMap<&[u8], u32> = FxMap::default();
        let (ai, bi) = (intern(&mut ids, am), intern(&mut ids, bm));
        lcs_script(&ai, &bi, p, &mut ops);
    }
    for k in 0..s {
        ops.push((Tag::Eq, p + n + k, p + m + k));
    }
    ops
}

/// For each line of `new`, the index of the `old` line it is unchanged from (`None` = added or
/// changed). Same alignment as [`unified`]; used by blame to pass attribution to the parent.
pub fn line_origins(old: &[u8], new: &[u8]) -> Vec<Option<usize>> {
    let (a, b) = (split_lines(old), split_lines(new));
    let mut out = vec![None; b.len()];
    for (t, ai, bi) in script(&a, &b) {
        if t == Tag::Eq {
            out[bi] = Some(ai);
        }
    }
    out
}

/// Lines of `data` as the diff engine splits them (each keeps its trailing newline).
pub fn lines_of(data: &[u8]) -> Vec<&[u8]> {
    split_lines(data)
}

pub fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

fn push_line(out: &mut String, prefix: char, line: &[u8]) {
    out.push(prefix);
    out.push_str(&String::from_utf8_lossy(line));
    if !line.ends_with(b"\n") {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

/// Unified diff of two byte strings with `a/` `b/` headers. Empty if identical.
/// `None` content means the file does not exist on that side.
pub fn unified(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> String {
    if old == new {
        return String::new();
    }
    let mut out = format!("diff --vcs a/{path} b/{path}\n");
    match (old, new) {
        (None, _) => out.push_str("new file\n"),
        (_, None) => out.push_str("deleted file\n"),
        _ => {}
    }
    let (ob, nb) = (old.unwrap_or(&[]), new.unwrap_or(&[]));
    if is_binary(ob) || is_binary(nb) {
        out.push_str(&format!("Binary files a/{path} and b/{path} differ\n"));
        return out;
    }
    out.push_str(&format!(
        "--- {}\n",
        if old.is_some() { format!("a/{path}") } else { "/dev/null".into() }
    ));
    out.push_str(&format!(
        "+++ {}\n",
        if new.is_some() { format!("b/{path}") } else { "/dev/null".into() }
    ));
    let (a, b) = (split_lines(ob), split_lines(nb));
    let ops = script(&a, &b);
    // positions: lines of a/b consumed before op k
    let mut apos = Vec::with_capacity(ops.len() + 1);
    let mut bpos = Vec::with_capacity(ops.len() + 1);
    let (mut ac, mut bc) = (0, 0);
    for (t, _, _) in &ops {
        apos.push(ac);
        bpos.push(bc);
        if *t != Tag::Ins {
            ac += 1;
        }
        if *t != Tag::Del {
            bc += 1;
        }
    }
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, o)| o.0 != Tag::Eq).map(|(i, _)| i).collect();
    let mut k = 0;
    while k < changed.len() {
        let mut lo = changed[k].saturating_sub(CONTEXT);
        let mut hi = changed[k];
        while k + 1 < changed.len() && changed[k + 1] - hi <= 2 * CONTEXT + 1 {
            k += 1;
            hi = changed[k];
        }
        k += 1;
        let hi = (hi + CONTEXT + 1).min(ops.len());
        lo = lo.min(hi);
        let (mut al, mut bl) = (0, 0);
        for (t, _, _) in &ops[lo..hi] {
            if *t != Tag::Ins {
                al += 1;
            }
            if *t != Tag::Del {
                bl += 1;
            }
        }
        let st = |pos: usize, len: usize| if len == 0 { pos } else { pos + 1 };
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", st(apos[lo], al), al, st(bpos[lo], bl), bl));
        for (t, ai, bi) in &ops[lo..hi] {
            match t {
                Tag::Eq => push_line(&mut out, ' ', a[*ai]),
                Tag::Del => push_line(&mut out, '-', a[*ai]),
                Tag::Ins => push_line(&mut out, '+', b[*bi]),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    /// The original byte-slice DP, kept as the reference for the interned, 16-bit version.
    fn script_ref(a: &[&[u8]], b: &[&[u8]]) -> Vec<(Tag, usize, usize)> {
        let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
        let (n, m) = (a.len() - p - s, b.len() - p - s);
        let mut ops: Vec<_> = (0..p).map(|i| (Tag::Eq, i, i)).collect();
        let (am, bm) = (&a[p..p + n], &b[p..p + m]);
        let w = m + 1;
        let mut t = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                t[i * w + j] = if am[i] == bm[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] {
                ops.push((Tag::Eq, p + i, p + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || t[(i + 1) * w + j] >= t[i * w + j + 1]) {
                ops.push((Tag::Del, p + i, 0));
                i += 1;
            } else {
                ops.push((Tag::Ins, 0, p + j));
                j += 1;
            }
        }
        for k in 0..s {
            ops.push((Tag::Eq, p + n + k, p + m + k));
        }
        ops
    }

    #[test]
    fn interned_script_equals_the_reference() {
        let mut x = 12345u64;
        let mut next = |n: u64| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) % n
        };
        for _ in 0..300 {
            let len = 1 + next(60) as usize;
            let alphabet = 1 + next(12);
            let a: Vec<String> = (0..len).map(|_| format!("l{}\n", next(alphabet))).collect();
            let mut b = a.clone();
            for _ in 0..next(6) {
                match next(3) {
                    0 if !b.is_empty() => {
                        let k = next(b.len() as u64) as usize;
                        b.remove(k);
                    }
                    1 => {
                        let k = next(b.len() as u64 + 1) as usize;
                        b.insert(k, format!("l{}\n", next(alphabet + 3)));
                    }
                    _ if !b.is_empty() => {
                        let k = next(b.len() as u64) as usize;
                        b[k] = format!("l{}\n", next(alphabet));
                    }
                    _ => {}
                }
            }
            let ar: Vec<&[u8]> = a.iter().map(|s| s.as_bytes()).collect();
            let br: Vec<&[u8]> = b.iter().map(|s| s.as_bytes()).collect();
            assert_eq!(script(&ar, &br), script_ref(&ar, &br));
        }
    }

    use super::*;

    /// Apply the edit script and check it reproduces `new` (correctness, not minimality).
    fn replay(old: &str, new: &str) {
        let (a, b) = (split_lines(old.as_bytes()), split_lines(new.as_bytes()));
        let mut got = Vec::new();
        for (t, ai, bi) in script(&a, &b) {
            match t {
                Tag::Eq | Tag::Ins => got.extend_from_slice(b[bi]),
                Tag::Del => {
                    let _ = ai;
                }
            }
        }
        assert_eq!(got, new.as_bytes());
        let mut base = Vec::new();
        for (t, ai, _) in script(&a, &b) {
            if t != Tag::Ins {
                base.extend_from_slice(a[ai]);
            }
        }
        assert_eq!(base, old.as_bytes());
    }

    #[test]
    fn scripts_reproduce_both_sides() {
        for (o, n) in [
            ("", "a\n"),
            ("a\n", ""),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nb\n", "b\na\n"),
            ("x", "x\n"),
            ("1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\n2\nX\n4\n5\n6\n7\nY\n9\n"),
        ] {
            replay(o, n);
        }
    }

    #[test]
    fn unified_simple() {
        let d = unified("hello.txt", Some(b"hello\n"), Some(b"hello\nworld\n"));
        assert!(d.contains("@@ -1,1 +1,2 @@"), "{d}");
        assert!(d.contains("+world\n"));
        assert!(d.contains(" hello\n"));
    }

    #[test]
    fn unified_new_deleted_binary_nonl() {
        assert!(unified("f", None, Some(b"a\n")).contains("new file"));
        assert!(unified("f", Some(b"a\n"), None).contains("deleted file"));
        assert!(unified("f", Some(b"a\0"), Some(b"b\0")).contains("Binary files"));
        assert!(unified("f", Some(b"a"), Some(b"a\n")).contains("No newline"));
        assert_eq!(unified("f", Some(b"a"), Some(b"a")), "");
    }

    #[test]
    fn separate_hunks() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new = old.replace("\n3\n", "\nthree\n").replace("\n28\n", "\ntwenty-eight\n");
        let d = unified("f", Some(old.as_bytes()), Some(new.as_bytes()));
        assert_eq!(d.matches("@@ ").count(), 2, "{d}");
    }
}

#[cfg(test)]
mod band_tests {
    use super::*;

    fn full(a: &[u32], b: &[u32]) -> Vec<(Tag, usize, usize)> {
        let mut o = Vec::new();
        lcs_ops::<u32>(a, b, 3, &mut o);
        o
    }

    #[test]
    fn banded_script_equals_full_table_script() {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut rnd = move |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for case in 0..3000 {
            let len = 1 + rnd(120) as usize;
            let alpha = if case % 3 == 0 { 3 } else { 1000 }; // small alphabets: many ties
            let a: Vec<u32> = (0..len).map(|_| rnd(alpha) as u32).collect();
            let mut b = a.clone();
            for _ in 0..rnd(if case % 5 == 0 { 40 } else { 6 }) {
                let pos = rnd(b.len() as u64 + 1) as usize;
                match rnd(3) {
                    0 if pos < b.len() => {
                        b.remove(pos);
                    }
                    1 => b.insert(pos, rnd(alpha) as u32),
                    _ if pos < b.len() => b[pos] = rnd(alpha) as u32,
                    _ => {}
                }
            }
            let mut got = Vec::new();
            lcs_script(&a, &b, 3, &mut got);
            assert_eq!(got, full(&a, &b), "case {case}: {a:?} vs {b:?}");
            // also force small bands directly: whenever they accept, they must agree
            for k in [1, 2, 3, 5, 9] {
                let mut g = Vec::new();
                if lcs_ops_banded(&a, &b, 3, k, &mut g).is_some() {
                    assert_eq!(g, full(&a, &b), "case {case} k {k}");
                }
            }
        }
    }
}
