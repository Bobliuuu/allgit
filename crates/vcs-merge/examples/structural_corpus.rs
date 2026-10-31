//! Measures structural vs text merge on the generated corpus. Run:
//! `cargo run -p vcs-merge --features structural --example structural_corpus --release`
use std::collections::BTreeMap;
use vcs_merge::structural::{corpus, merge_rust, parses, Outcome};
use vcs_merge::{merge3, Algorithm, Hunk, TextMerge};

fn conflict_hunks(m: &TextMerge) -> usize {
    match m {
        TextMerge::Clean(_) => 0,
        TextMerge::Conflicted(h) => h.iter().filter(|h| matches!(h, Hunk::Conflict { .. })).count(),
    }
}

fn main() {
    let per_kind: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let scenarios = corpus::generate(per_kind);
    // kind -> (n, text conflicted files, structural conflicted files, text hunks, structural hunks, structural clean, parsed-bad)
    let mut rows: BTreeMap<&str, [usize; 7]> = BTreeMap::new();
    let mut tot = [0usize; 7];
    for s in &scenarios {
        let t = merge3(Algorithm::Myers, &s.base, &s.left, &s.right);
        let m = merge_rust(Algorithm::Myers, &s.base, &s.left, &s.right);
        let inputs_parse = parses(&s.base) && parses(&s.left) && parses(&s.right);
        let bad = matches!(&m.merge, TextMerge::Clean(b) if inputs_parse && !parses(b));
        let v = [
            1,
            usize::from(!t.is_clean()),
            usize::from(!m.merge.is_clean()),
            conflict_hunks(&t),
            conflict_hunks(&m.merge),
            usize::from(m.outcome == Outcome::Structural),
            usize::from(bad),
        ];
        for (i, x) in v.iter().enumerate() {
            rows.entry(s.kind).or_default()[i] += x;
            tot[i] += x;
        }
    }
    println!("per_kind={per_kind}");
    println!("kind | scenarios | text conflicted files | structural conflicted files | text hunks | structural hunks | resolved by structural | unparsable clean outputs");
    for (k, v) in &rows {
        println!("{k} | {} | {} | {} | {} | {} | {} | {}", v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
    }
    println!(
        "TOTAL | {} | {} | {} | {} | {} | {} | {}",
        tot[0], tot[1], tot[2], tot[3], tot[4], tot[5], tot[6]
    );
}
