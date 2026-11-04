//! Measures structural vs text merge on the generated corpus, one table per enabled language. Run:
//! `cargo run -p vcs-merge --features structural,structural-python,structural-typescript,structural-go --example structural_corpus --release -- 50`
#[cfg(any(
    feature = "structural",
    feature = "structural-python",
    feature = "structural-typescript",
    feature = "structural-go"
))]
mod run {
    use std::collections::BTreeMap;
    use vcs_merge::structural::corpus::{self, Style};
    use vcs_merge::structural::{merge_with, parses_as, Lang, Outcome};
    use vcs_merge::{merge3, Algorithm, Hunk, TextMerge};

    fn conflict_hunks(m: &TextMerge) -> usize {
        match m {
            TextMerge::Clean(_) => 0,
            TextMerge::Conflicted(h) => h.iter().filter(|h| matches!(h, Hunk::Conflict { .. })).count(),
        }
    }

    fn lang_of(st: Style) -> Option<&'static Lang> {
        vcs_merge::structural::lang_for_path(match st {
            Style::Rust => "x.rs",
            Style::Python => "x.py",
            Style::TypeScript => "x.ts",
            Style::Go => "x.go",
        })
    }

    pub fn main() {
        let per_kind: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(50);
        for st in Style::ALL {
            let Some(lang) = lang_of(st) else { continue };
            let scenarios = corpus::generate_for(st, per_kind);
            // kind -> (n, text conflicted files, structural conflicted files, text hunks, structural hunks, structural clean, parsed-bad)
            let mut rows: BTreeMap<&str, [usize; 7]> = BTreeMap::new();
            let mut tot = [0usize; 7];
            for s in &scenarios {
                let t = merge3(Algorithm::Myers, &s.base, &s.left, &s.right);
                let m = merge_with(lang, Algorithm::Myers, &s.base, &s.left, &s.right);
                let inputs_parse = parses_as(lang, &s.base) && parses_as(lang, &s.left) && parses_as(lang, &s.right);
                let bad = matches!(&m.merge, TextMerge::Clean(b) if inputs_parse && !parses_as(lang, b));
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
            println!("language={} per_kind={per_kind}", st.name());
            println!("kind | scenarios | text conflicted files | structural conflicted files | text hunks | structural hunks | resolved by structural | unparsable clean outputs");
            for (k, v) in &rows {
                println!("{k} | {} | {} | {} | {} | {} | {} | {}", v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
            }
            println!(
                "TOTAL | {} | {} | {} | {} | {} | {} | {}\n",
                tot[0], tot[1], tot[2], tot[3], tot[4], tot[5], tot[6]
            );
        }
    }
}

fn main() {
    #[cfg(any(
        feature = "structural",
        feature = "structural-python",
        feature = "structural-typescript",
        feature = "structural-go"
    ))]
    run::main();
    #[cfg(not(any(
        feature = "structural",
        feature = "structural-python",
        feature = "structural-typescript",
        feature = "structural-go"
    )))]
    eprintln!("enable at least one of the structural* features");
}
