//! Three-way application of patch hunks (`apply -3`, `am -3`).
//!
//! A patch carries no blob ids, so there is no stored base file to merge against. The base of a
//! hunk is its own pre-image (context plus removed lines), "theirs" is its post-image (context
//! plus added lines) and "ours" is the stretch of the file that the pre-image matches best
//! (a line-level alignment inside a window around the recorded position). The three are merged
//! with the ordinary text merge: clean merges are applied silently, overlapping edits leave
//! conflict markers. A hunk whose post-image is already present is skipped.

use crate::diff::{line_origins, lines_of};
use crate::patch::{Hunk, HunkLine};
use vcs_merge::{merge3, render_markers, Algorithm, MarkerLabels, TextMerge};

const WINDOW: usize = 200;

fn fits(src: &[&[u8]], pos: usize, taken: usize, want: &[Vec<u8>]) -> bool {
    pos >= taken && pos + want.len() <= src.len() && want.iter().enumerate().all(|(k, w)| src[pos + k] == w.as_slice())
}

/// Search outward from `guess` for an exact fit of `want`.
fn find_exact(src: &[&[u8]], taken: usize, want: &[Vec<u8>], guess: usize) -> Option<usize> {
    if fits(src, guess, taken, want) {
        return Some(guess);
    }
    for d in 1..=src.len() {
        if guess + d <= src.len() && fits(src, guess + d, taken, want) {
            return Some(guess + d);
        }
        if d <= guess && fits(src, guess - d, taken, want) {
            return Some(guess - d);
        }
    }
    None
}

/// Apply `hunks` to `old`, merging three ways where a hunk does not fit as written. Returns the
/// new content and whether conflict markers were written. Errors name the hunk that could not be
/// placed at all.
pub fn apply_hunks_3way(old: &[u8], hunks: &[Hunk]) -> Result<(Vec<u8>, bool), String> {
    let src = lines_of(old);
    let mut out: Vec<u8> = Vec::with_capacity(old.len());
    let mut taken = 0usize;
    let mut offset: isize = 0;
    let mut conflicted = false;
    for (hi, h) in hunks.iter().enumerate() {
        let pre: Vec<Vec<u8>> = h
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Ctx(x) | HunkLine::Del(x) => Some(x.clone()),
                HunkLine::Add(_) => None,
            })
            .collect();
        let post: Vec<Vec<u8>> = h
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Ctx(x) | HunkLine::Add(x) => Some(x.clone()),
                HunkLine::Del(_) => None,
            })
            .collect();
        let expected = if h.old_len == 0 {
            h.old_start
        } else {
            h.old_start.saturating_sub(1)
        };
        let guess = ((expected as isize + offset).max(taken as isize) as usize).min(src.len());
        // (start, end) of the stretch of `src` replaced, and what replaces it
        let (start, end, repl): (usize, usize, Vec<u8>) = if pre.is_empty() {
            (guess, guess, post.concat())
        } else if let Some(p) = find_exact(&src, taken, &pre, guess) {
            (p, p + pre.len(), post.concat())
        } else if let Some(p) = find_exact(&src, taken, &post, guess) {
            // already applied: keep the file as is
            (p, p + post.len(), post.concat())
        } else {
            let lo = guess.saturating_sub(WINDOW).max(taken);
            let hi_end = (guess + pre.len() + WINDOW).min(src.len());
            if lo >= hi_end {
                return Err(format!("hunk #{} cannot be placed: nothing left to match against", hi + 1));
            }
            let win: Vec<u8> = src[lo..hi_end].concat();
            let pre_b: Vec<u8> = pre.concat();
            let org = line_origins(&pre_b, &win);
            let matched: Vec<usize> = org.iter().enumerate().filter(|(_, o)| o.is_some()).map(|(i, _)| i).collect();
            let (Some(&first), Some(&last)) = (matched.first(), matched.last()) else {
                return Err(format!("hunk #{} cannot be placed: none of its lines are in the file", hi + 1));
            };
            if matched.len() * 2 < pre.len() || last - first + 1 > pre.len() * 2 + 8 {
                return Err(format!(
                    "hunk #{} cannot be placed: only {} of its {} lines are found near line {}",
                    hi + 1,
                    matched.len(),
                    pre.len(),
                    expected + 1
                ));
            }
            let ours: Vec<u8> = src[lo + first..=lo + last].concat();
            let labels = MarkerLabels {
                left: "ours",
                base: "base (patch pre-image)",
                right: "theirs (patch)",
            };
            let merged = match merge3(Algorithm::Myers, &pre_b, &ours, &post.concat()) {
                TextMerge::Clean(b) => b,
                TextMerge::Conflicted(hs) => {
                    conflicted = true;
                    render_markers(&hs, &labels)
                }
            };
            (lo + first, lo + last + 1, merged)
        };
        offset = start as isize - expected as isize;
        for s in &src[taken..start] {
            out.extend_from_slice(s);
        }
        out.extend_from_slice(&repl);
        taken = end;
    }
    for s in &src[taken..] {
        out.extend_from_slice(s);
    }
    Ok((out, conflicted))
}
