//! Experimental structural three-way merge (features `structural` for Rust, `structural-python`,
//! `structural-typescript`, `structural-go`; all off by default).
//!
//! The item-matching logic below is shared; each language is a small [`Lang`] table (grammar, trivia
//! kinds, container bodies, item keys) in `langs.rs`.
//!
//! The file is parsed with tree-sitter into a sequence of top-level items; `impl`, `trait` and
//! inline `mod` bodies are parsed one level at a time, recursively. Items are keyed by kind and
//! name, so two sides that edit different functions, or move them, merge without a text
//! conflict. Anything the item-level merge cannot decide (same item edited on both sides with
//! overlapping text, delete/modify, add/add at one spot, ...) makes the driver decline and the
//! line-based diff3 result is used instead.
//!
//! Safety rule: when base, left and right all parse without errors, a clean result is returned
//! only if it parses too. Otherwise the whole file becomes one conflict. See `docs/merge.md`.

pub mod corpus;
mod langs;

pub use langs::*;

use crate::{merge3, Algorithm, Hunk, TextMerge};
use tree_sitter::{Node, Parser, Tree};

/// Per-language description used by the shared item-matching code.
pub struct Lang {
    pub name: &'static str,
    language: fn() -> tree_sitter::Language,
    /// Node kinds that are trivia (comments, attributes): they travel with the item that follows.
    trivia: &'static [&'static str],
    /// The container body of an item whose members are merged one level down (`impl`, `class`, ...).
    body: fn(Node) -> Option<Node>,
    /// Identity of an item (kind and name); two sides edit "the same item" when keys are equal.
    key: fn(&[u8], Node) -> String,
    /// Bytes of opening and closing delimiter of a body (`{` and `}` are 1 and 1, Python blocks 0 and 0).
    delims: (usize, usize),
}

/// Which path produced a [`StructuralMerge`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Item-level merge decided every item; the output parses.
    Structural,
    /// Declined (conflicting items or item order); the text merge result is returned.
    Declined,
    /// An input did not parse (or is not valid for the grammar); the text merge result is returned.
    ParseFailed,
    /// Text merge was clean but its output does not parse; downgraded to a whole-file conflict.
    Downgraded,
}

#[derive(Clone, Debug)]
pub struct StructuralMerge {
    pub merge: TextMerge,
    pub outcome: Outcome,
}

fn parse(lang: &Lang, src: &[u8]) -> Option<Tree> {
    let mut p = Parser::new();
    p.set_language(&(lang.language)()).ok()?;
    let t = p.parse(src, None)?;
    if t.root_node().has_error() {
        None
    } else {
        Some(t)
    }
}

/// Whether `src` parses in the language without any error node.
pub fn parses_as(lang: &Lang, src: &[u8]) -> bool {
    parse(lang, src).is_some()
}

/// Three-way merge, structural first, line-based diff3 as the fallback.
pub fn merge_with(lang: &Lang, alg: Algorithm, base: &[u8], left: &[u8], right: &[u8]) -> StructuralMerge {
    let text = || merge3(alg, base, left, right);
    let (Some(tb), Some(tl), Some(tr)) = (parse(lang, base), parse(lang, left), parse(lang, right)) else {
        return StructuralMerge {
            merge: text(),
            outcome: Outcome::ParseFailed,
        };
    };
    let b = seq_of(lang, base, tb.root_node(), 0, base.len());
    let l = seq_of(lang, left, tl.root_node(), 0, left.len());
    let r = seq_of(lang, right, tr.root_node(), 0, right.len());
    if let Some(out) = merge_seq(alg, &b, &l, &r) {
        let mut bytes = Vec::new();
        flatten(&out, &mut bytes);
        if parses_as(lang, &bytes) {
            return StructuralMerge {
                merge: TextMerge::Clean(bytes),
                outcome: Outcome::Structural,
            };
        }
    }
    match text() {
        TextMerge::Clean(bytes) if !parses_as(lang, &bytes) => StructuralMerge {
            merge: whole_file_conflict(base, left, right),
            outcome: Outcome::Downgraded,
        },
        m => StructuralMerge {
            merge: m,
            outcome: Outcome::Declined,
        },
    }
}

fn whole_file_conflict(base: &[u8], left: &[u8], right: &[u8]) -> TextMerge {
    let lines = |b: &[u8]| -> Vec<Vec<u8>> { b.split_inclusive(|c| *c == b'\n').map(<[u8]>::to_vec).collect() };
    TextMerge::Conflicted(vec![Hunk::Conflict {
        base: lines(base),
        left: lines(left),
        right: lines(right),
    }])
}

/// A run of items: each segment owns the trivia (comments, attributes, whitespace) before its item.
#[derive(Clone, Debug)]
struct Seq {
    segs: Vec<Seg>,
    tail: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Seg {
    key: String,
    /// Leading trivia plus the item text up to and including an opening `{` of a container body.
    head: Vec<u8>,
    inner: Option<Seq>,
    /// Closing `}` of a container (and anything after it); empty for plain items.
    close: Vec<u8>,
}

impl Seg {
    fn text(&self) -> Vec<u8> {
        let mut v = Vec::new();
        flatten_seg(self, &mut v);
        v
    }
}

fn flatten(s: &Seq, out: &mut Vec<u8>) {
    for g in &s.segs {
        flatten_seg(g, out);
    }
    out.extend_from_slice(&s.tail);
}

fn flatten_seg(g: &Seg, out: &mut Vec<u8>) {
    out.extend_from_slice(&g.head);
    if let Some(i) = &g.inner {
        flatten(i, out);
    }
    out.extend_from_slice(&g.close);
}

fn text_of(src: &[u8], n: Node) -> String {
    String::from_utf8_lossy(&src[n.start_byte()..n.end_byte()]).into_owned()
}

fn norm_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Build the segments for the children of `parent` that lie inside `start..end`.
fn seq_of(lang: &Lang, src: &[u8], parent: Node, start: usize, end: usize) -> Seq {
    let mut segs: Vec<Seg> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, usize> = Default::default();
    let mut pos = start;
    let mut cur = parent.walk();
    for n in parent.children(&mut cur) {
        if n.start_byte() < start || n.end_byte() > end || !n.is_named() || lang.trivia.contains(&n.kind()) {
            continue;
        }
        let mut key = (lang.key)(src, n);
        let c = seen.entry(key.clone()).or_insert(0);
        *c += 1;
        if *c > 1 {
            key = format!("{key}#{c}");
        }
        let (open, close) = lang.delims;
        let seg = match (lang.body)(n).filter(|b| b.start_byte() >= pos && b.end_byte() <= n.end_byte()) {
            Some(b) => {
                let mut inner_start = b.start_byte() + open;
                if open == 0 {
                    // Indentation-delimited body: let the first member own its indentation.
                    let col = b.start_position().column;
                    let line_start = b.start_byte().saturating_sub(col);
                    if line_start >= pos && src[line_start..b.start_byte()].iter().all(|c| *c == b' ' || *c == b'\t') {
                        inner_start = line_start;
                    }
                }
                let inner_end = b.end_byte() - close;
                Seg {
                    key,
                    head: src[pos..inner_start].to_vec(),
                    inner: Some(seq_of(lang, src, b, inner_start, inner_end)),
                    close: src[inner_end..n.end_byte()].to_vec(),
                }
            }
            None => Seg {
                key,
                head: src[pos..n.end_byte()].to_vec(),
                inner: None,
                close: Vec::new(),
            },
        };
        segs.push(seg);
        pos = n.end_byte();
    }
    Seq {
        segs,
        tail: src[pos..end].to_vec(),
    }
}

fn text_clean(alg: Algorithm, b: &[u8], l: &[u8], r: &[u8]) -> Option<Vec<u8>> {
    match merge3(alg, b, l, r) {
        TextMerge::Clean(v) => Some(v),
        TextMerge::Conflicted(_) => None,
    }
}

fn find<'a>(s: &'a Seq, k: &str) -> Option<&'a Seg> {
    s.segs.iter().find(|g| g.key == k)
}

fn merge_seq(alg: Algorithm, b: &Seq, l: &Seq, r: &Seq) -> Option<Seq> {
    let keylines = |s: &Seq| -> Vec<u8> { s.segs.iter().flat_map(|g| format!("{}\n", g.key).into_bytes()).collect() };
    let order = match merge3(alg, &keylines(b), &keylines(l), &keylines(r)) {
        TextMerge::Clean(v) => String::from_utf8(v).ok()?,
        TextMerge::Conflicted(_) => return None,
    };
    // A key dropped by the merged order must have been deleted by one side and left untouched by the other.
    for g in &b.segs {
        if order.lines().any(|k| k == g.key) {
            continue;
        }
        let (sl, sr) = (find(l, &g.key), find(r, &g.key));
        let unchanged = |o: Option<&Seg>| o.is_some_and(|o| o.text() == g.text());
        if !(unchanged(sl) || unchanged(sr)) && (sl.is_some() || sr.is_some()) {
            return None;
        }
    }
    let mut segs = Vec::new();
    for k in order.lines() {
        segs.push(merge_seg(alg, find(b, k), find(l, k), find(r, k))?);
    }
    let tail = text_clean(alg, &b.tail, &l.tail, &r.tail)?;
    Some(Seq { segs, tail })
}

fn merge_seg(alg: Algorithm, b: Option<&Seg>, l: Option<&Seg>, r: Option<&Seg>) -> Option<Seg> {
    let (l, r) = match (l, r) {
        (Some(l), Some(r)) => (l, r),
        // Added by exactly one side (a delete/modify mix is declined).
        (Some(x), None) | (None, Some(x)) => return b.is_none().then(|| x.clone()),
        (None, None) => return None,
    };
    let (lt, rt) = (l.text(), r.text());
    if lt == rt {
        return Some(l.clone());
    }
    let b = b?; // add/add with different text
    let bt = b.text();
    if lt == bt {
        return Some(r.clone());
    }
    if rt == bt {
        return Some(l.clone());
    }
    match (&b.inner, &l.inner, &r.inner) {
        (Some(bi), Some(li), Some(ri)) => Some(Seg {
            key: l.key.clone(),
            head: text_clean(alg, &b.head, &l.head, &r.head)?,
            inner: Some(merge_seq(alg, bi, li, ri)?),
            close: text_clean(alg, &b.close, &l.close, &r.close)?,
        }),
        (None, None, None) => Some(Seg {
            key: l.key.clone(),
            head: text_clean(alg, &bt, &lt, &rt)?,
            inner: None,
            close: Vec::new(),
        }),
        _ => None,
    }
}
