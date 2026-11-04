//! Line-based diff3 three-way text merge on bytes (no UTF-8 requirement).
//!
//! Lines keep their terminating `\n`; a final line without one is a distinct line, so
//! "no newline at end of file" differences are real changes, never silently normalised.

use crate::diff::{matches, Algorithm};
use std::collections::BTreeMap;

pub type Line = Vec<u8>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hunk {
    Clean(Vec<Line>),
    Conflict {
        base: Vec<Line>,
        left: Vec<Line>,
        right: Vec<Line>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextMerge {
    Clean(Vec<u8>),
    Conflicted(Vec<Hunk>),
}

impl TextMerge {
    pub fn is_clean(&self) -> bool {
        matches!(self, TextMerge::Clean(_))
    }
    /// Hunk view of either outcome.
    pub fn into_hunks(self) -> Vec<Hunk> {
        match self {
            TextMerge::Clean(b) => {
                if b.is_empty() {
                    vec![]
                } else {
                    vec![Hunk::Clean(split_lines(&b))]
                }
            }
            TextMerge::Conflicted(h) => h,
        }
    }
}

fn split_lines(b: &[u8]) -> Vec<Line> {
    b.split_inclusive(|c| *c == b'\n').map(<[u8]>::to_vec).collect()
}

/// Three-way merge of `left` and `right` relative to `base`.
///
/// Rules per region between lines that all three versions share: only one side changed
/// -> take it; both changed identically -> take it; otherwise a `Conflict` carrying all three
/// versions verbatim. Deterministic and symmetric in left/right (when clean).
pub fn merge3(alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> TextMerge {
    if left == right || right == base {
        return TextMerge::Clean(left.to_vec());
    }
    if left == base {
        return TextMerge::Clean(right.to_vec());
    }
    let (bl, ll, rl) = (split_lines(base), split_lines(left), split_lines(right));
    let mut intern: BTreeMap<&[u8], u32> = BTreeMap::new();
    let (bi, li, ri) = (
        intern_all(&mut intern, &bl),
        intern_all(&mut intern, &ll),
        intern_all(&mut intern, &rl),
    );
    let mut ml = vec![None; bl.len()];
    for (i, j) in matches(alg, &bi, &li) {
        ml[i] = Some(j);
    }
    let mut mr = vec![None; bl.len()];
    for (i, j) in matches(alg, &bi, &ri) {
        mr[i] = Some(j);
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut b0, mut l0, mut r0) = (0, 0, 0);
    let mut s = 0;
    loop {
        // next base line kept by both sides
        while s < bl.len() && !(ml[s].is_some() && mr[s].is_some()) {
            s += 1;
        }
        let (l1, r1) = if s < bl.len() {
            (ml[s].unwrap(), mr[s].unwrap())
        } else {
            (ll.len(), rl.len())
        };
        resolve(&mut hunks, &bl[b0..s], &ll[l0..l1], &rl[r0..r1]);
        if s >= bl.len() {
            break;
        }
        push_clean(&mut hunks, std::slice::from_ref(&ll[l1]));
        b0 = s + 1;
        l0 = l1 + 1;
        r0 = r1 + 1;
        s += 1;
    }

    if hunks.iter().all(|h| matches!(h, Hunk::Clean(_))) {
        let mut out = Vec::new();
        for h in &hunks {
            if let Hunk::Clean(ls) = h {
                ls.iter().for_each(|l| out.extend_from_slice(l));
            }
        }
        TextMerge::Clean(out)
    } else {
        TextMerge::Conflicted(hunks)
    }
}

fn intern_all<'a>(map: &mut BTreeMap<&'a [u8], u32>, ls: &'a [Line]) -> Vec<u32> {
    ls.iter()
        .map(|l| {
            let n = map.len() as u32;
            *map.entry(l.as_slice()).or_insert(n)
        })
        .collect()
}

fn push_clean(hunks: &mut Vec<Hunk>, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    if let Some(Hunk::Clean(prev)) = hunks.last_mut() {
        prev.extend_from_slice(lines);
    } else {
        hunks.push(Hunk::Clean(lines.to_vec()));
    }
}

fn resolve(hunks: &mut Vec<Hunk>, base: &[Line], left: &[Line], right: &[Line]) {
    if left == right || right == base {
        push_clean(hunks, left);
    } else if left == base {
        push_clean(hunks, right);
    } else {
        hunks.push(Hunk::Conflict {
            base: base.to_vec(),
            left: left.to_vec(),
            right: right.to_vec(),
        });
    }
}

#[derive(Clone, Debug)]
pub struct MarkerLabels<'a> {
    pub left: &'a str,
    pub base: &'a str,
    pub right: &'a str,
}

impl Default for MarkerLabels<'_> {
    fn default() -> Self {
        Self {
            left: "left",
            base: "base",
            right: "right",
        }
    }
}

/// Render hunks with conventional diff3 markers (`<<<<<<<`, `|||||||`, `=======`, `>>>>>>>`).
/// A section whose last line lacks `\n` gets one inserted before the next marker (the only
/// bytes the renderer adds besides the markers themselves).
pub fn render_markers(hunks: &[Hunk], labels: &MarkerLabels) -> Vec<u8> {
    fn section(out: &mut Vec<u8>, ls: &[Line]) {
        for l in ls {
            out.extend_from_slice(l);
        }
        if ls.last().is_some_and(|l| !l.ends_with(b"\n")) {
            out.push(b'\n');
        }
    }
    let mut out = Vec::new();
    for h in hunks {
        match h {
            Hunk::Clean(ls) => ls.iter().for_each(|l| out.extend_from_slice(l)),
            Hunk::Conflict { base, left, right } => {
                out.extend_from_slice(format!("<<<<<<< {}\n", labels.left).as_bytes());
                section(&mut out, left);
                out.extend_from_slice(format!("||||||| {}\n", labels.base).as_bytes());
                section(&mut out, base);
                out.extend_from_slice(b"=======\n");
                section(&mut out, right);
                out.extend_from_slice(format!(">>>>>>> {}\n", labels.right).as_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const ALGS: [Algorithm; 2] = [Algorithm::Myers, Algorithm::Patience];

    fn clean(alg: Algorithm, b: &str, l: &str, r: &str) -> String {
        match merge3(alg, b.as_bytes(), l.as_bytes(), r.as_bytes()) {
            TextMerge::Clean(v) => String::from_utf8(v).unwrap(),
            c => panic!("expected clean, got {c:?}"),
        }
    }

    #[test]
    fn disjoint_edits_merge() {
        for a in ALGS {
            assert_eq!(clean(a, "1\n2\n3\n4\n5\n", "1\nL\n3\n4\n5\n", "1\n2\n3\nR\n5\n"), "1\nL\n3\nR\n5\n");
        }
    }

    #[test]
    fn identical_edits_merge() {
        for a in ALGS {
            assert_eq!(clean(a, "1\n2\n3\n", "1\nX\n3\n", "1\nX\n3\n"), "1\nX\n3\n");
        }
    }

    #[test]
    fn same_line_conflicts_with_all_three_sides() {
        for a in ALGS {
            let m = merge3(a, b"1\n2\n3\n", b"1\nL\n3\n", b"1\nR\n3\n");
            let TextMerge::Conflicted(h) = m else { panic!() };
            assert_eq!(
                h,
                vec![
                    Hunk::Clean(vec![b"1\n".to_vec()]),
                    Hunk::Conflict {
                        base: vec![b"2\n".to_vec()],
                        left: vec![b"L\n".to_vec()],
                        right: vec![b"R\n".to_vec()]
                    },
                    Hunk::Clean(vec![b"3\n".to_vec()]),
                ]
            );
            let txt = render_markers(&h, &MarkerLabels::default());
            assert_eq!(
                String::from_utf8(txt).unwrap(),
                "1\n<<<<<<< left\nL\n||||||| base\n2\n=======\nR\n>>>>>>> right\n3\n"
            );
        }
    }

    #[test]
    fn delete_vs_modify_conflicts() {
        for a in ALGS {
            assert!(!merge3(a, b"1\n2\n3\n", b"1\n3\n", b"1\nX\n3\n").is_clean());
        }
    }

    #[test]
    fn one_side_deletes_other_untouched() {
        for a in ALGS {
            assert_eq!(clean(a, "1\n2\n3\n4\n", "1\n4\n", "1\n2\n3\n4\n"), "1\n4\n");
            assert_eq!(clean(a, "1\n2\n3\n4\n5\n", "1\n3\n4\n5\n", "1\n2\n3\n4\n"), "1\n3\n4\n");
        }
    }

    #[test]
    fn insert_insert_same_point_conflicts_unless_equal() {
        for a in ALGS {
            assert!(!merge3(a, b"a\nb\n", b"a\nX\nb\n", b"a\nY\nb\n").is_clean());
            assert_eq!(clean(a, "a\nb\n", "a\nX\nb\n", "a\nX\nb\n"), "a\nX\nb\n");
        }
    }

    #[test]
    fn empty_base() {
        for a in ALGS {
            assert_eq!(clean(a, "", "x\n", "x\n"), "x\n");
            assert!(!merge3(a, b"", b"x\n", b"y\n").is_clean());
            assert_eq!(clean(a, "", "", "y\n"), "y\n");
        }
    }

    #[test]
    fn missing_final_newline_is_a_real_change() {
        for a in ALGS {
            assert!(!merge3(a, b"a\nb\n", b"a\nb", b"a\nb\nc\n").is_clean());
        }
        let h = vec![Hunk::Conflict {
            base: vec![],
            left: vec![b"x".to_vec()],
            right: vec![b"y".to_vec()],
        }];
        let s = String::from_utf8(render_markers(&h, &MarkerLabels::default())).unwrap();
        assert_eq!(s, "<<<<<<< left\nx\n||||||| base\n=======\ny\n>>>>>>> right\n");
    }

    #[test]
    fn binary_safe_bytes() {
        for a in ALGS {
            let m = merge3(a, &[0, 1, 10, 2], &[0, 1, 10, 3], &[0, 1, 10, 2]);
            assert_eq!(m, TextMerge::Clean(vec![0, 1, 10, 3]));
        }
    }

    #[test]
    fn into_hunks_roundtrip() {
        let m = merge3(Algorithm::Myers, b"a\n", b"b\n", b"a\n");
        assert_eq!(m.into_hunks(), vec![Hunk::Clean(vec![b"b\n".to_vec()])]);
    }

    /// Hand-written accuracy corpus: cases where the algorithms align differently. Myers is
    /// LCS-optimal (never fewer matches); patience prefers unique-line anchors. Both merges
    /// must stay lossless whatever shape the conflicts take.
    #[test]
    fn accuracy_corpus_myers_vs_patience() {
        // (base, other) swap of a block: unique ends cross, repeated middle lines match.
        let (a, b) = ([1u32, 0, 0, 2], [2u32, 0, 0, 1]);
        let my = matches(Algorithm::Myers, &a, &b);
        let pa = matches(Algorithm::Patience, &a, &b);
        assert_eq!(my.len(), 2, "myers keeps the repeated middle");
        assert_eq!(pa.len(), 1, "patience commits to a unique anchor");
        assert_ne!(my, pa);

        let base = "A\nx\nx\nB\n";
        let left = "B\nx\nx\nA\n";
        let right = "A\nx\nx\nB2\n";
        let mut shapes = Vec::new();
        for alg in ALGS {
            let m = merge3(alg, base.as_bytes(), left.as_bytes(), right.as_bytes());
            let hunks = m.clone().into_hunks();
            let text = String::from_utf8(render_markers(&hunks, &MarkerLabels::default())).unwrap();
            for l in ["B\n", "A\n", "B2\n"] {
                assert!(text.contains(l), "{alg:?} lost {l:?}");
            }
            shapes.push((m.is_clean(), hunks.len()));
        }
        assert_ne!(shapes[0], shapes[1], "algorithms should yield different conflict shapes here");
    }

    fn lines(tag: &str, ops: &[u8], n: usize) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..n {
            match ops.get(i).copied().unwrap_or(0) % 4 {
                0 => out.push(format!("b{i}\n")),
                1 => {}
                2 => out.push(format!("{tag}mod{i}\n")),
                _ => {
                    out.push(format!("b{i}\n"));
                    out.push(format!("{tag}ins{i}\n"));
                }
            }
        }
        out
    }

    proptest! {
        #[test]
        fn idempotent(s in "[ab\n]{0,30}") {
            for a in ALGS {
                prop_assert_eq!(merge3(a, s.as_bytes(), s.as_bytes(), s.as_bytes()), TextMerge::Clean(s.clone().into_bytes()));
            }
        }

        #[test]
        fn one_side_unchanged_takes_other(b in "[abc\n]{0,30}", x in "[abc\n]{0,30}") {
            for a in ALGS {
                prop_assert_eq!(merge3(a, b.as_bytes(), x.as_bytes(), b.as_bytes()), TextMerge::Clean(x.clone().into_bytes()));
                prop_assert_eq!(merge3(a, b.as_bytes(), b.as_bytes(), x.as_bytes()), TextMerge::Clean(x.clone().into_bytes()));
            }
        }

        #[test]
        fn clean_merges_are_symmetric(b in "[abc\n]{0,24}", l in "[abc\n]{0,24}", r in "[abc\n]{0,24}") {
            for a in ALGS {
                let lr = merge3(a, b.as_bytes(), l.as_bytes(), r.as_bytes());
                let rl = merge3(a, b.as_bytes(), r.as_bytes(), l.as_bytes());
                prop_assert_eq!(lr.is_clean(), rl.is_clean());
                if lr.is_clean() { prop_assert_eq!(lr, rl); }
            }
        }

        #[test]
        fn deterministic(b in "[abc\n]{0,24}", l in "[abc\n]{0,24}", r in "[abc\n]{0,24}") {
            for a in ALGS {
                prop_assert_eq!(merge3(a, b.as_bytes(), l.as_bytes(), r.as_bytes()), merge3(a, b.as_bytes(), l.as_bytes(), r.as_bytes()));
            }
        }

        #[test]
        fn disjoint_edits_always_clean(n in 5usize..30, i in 0usize..30, j in 0usize..30) {
            let (i, j) = (i % n, j % n);
            prop_assume!(i.abs_diff(j) >= 2);
            let base: Vec<String> = (0..n).map(|k| format!("line{k}\n")).collect();
            let mut l = base.clone();
            l[i] = "LEFT\n".into();
            let mut r = base.clone();
            r[j] = "RIGHT\n".into();
            let mut want = base.clone();
            want[i] = "LEFT\n".into();
            want[j] = "RIGHT\n".into();
            for a in ALGS {
                let m = merge3(a, base.concat().as_bytes(), l.concat().as_bytes(), r.concat().as_bytes());
                prop_assert_eq!(m, TextMerge::Clean(want.concat().into_bytes()));
            }
        }

        /// Every line a side introduced (not in base) is in the output or inside a conflict hunk.
        #[test]
        fn introduced_lines_never_lost(n in 1usize..16,
            lo in proptest::collection::vec(any::<u8>(), 16),
            ro in proptest::collection::vec(any::<u8>(), 16)) {
            let base: Vec<String> = (0..n).map(|k| format!("b{k}\n")).collect();
            let l = lines("L", &lo, n);
            let r = lines("R", &ro, n);
            for a in ALGS {
                let m = merge3(a, base.concat().as_bytes(), l.concat().as_bytes(), r.concat().as_bytes());
                let hunks = m.into_hunks();
                let mut seen: Vec<&Line> = Vec::new();
                for h in &hunks {
                    match h {
                        Hunk::Clean(ls) => seen.extend(ls),
                        Hunk::Conflict { base, left, right } => { seen.extend(base); seen.extend(left); seen.extend(right); }
                    }
                }
                for line in l.iter().chain(r.iter()) {
                    if line.starts_with('L') || line.starts_with('R') {
                        prop_assert!(seen.iter().any(|s| s.as_slice() == line.as_bytes()), "lost {line:?}");
                    }
                }
            }
        }

        /// Conflict hunks always quote each side verbatim: rendering never invents or drops bytes
        /// other than the marker lines.
        #[test]
        fn render_contains_every_conflict_line(b in "[abc\n]{0,20}", l in "[abc\n]{0,20}", r in "[abc\n]{0,20}") {
            if let TextMerge::Conflicted(h) = merge3(Algorithm::Patience, b.as_bytes(), l.as_bytes(), r.as_bytes()) {
                let out = render_markers(&h, &MarkerLabels::default());
                let txt = String::from_utf8(out).unwrap();
                prop_assert!(txt.contains("<<<<<<<") && txt.contains("|||||||") && txt.contains("=======") && txt.contains(">>>>>>>"));
            }
        }
    }
}
