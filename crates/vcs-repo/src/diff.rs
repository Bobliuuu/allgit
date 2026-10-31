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
    }
    for k in 0..s {
        ops.push((Tag::Eq, p + n + k, p + m + k));
    }
    ops
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
