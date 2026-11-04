#![cfg(feature = "structural")]
use vcs_merge::structural::{corpus, merge_rust, parses, Outcome};
use vcs_merge::{merge3, Algorithm, TextMerge};

const BASE: &str = "fn a() -> i32 {\n    1\n}\n\nfn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n";

fn run(base: &str, l: &str, r: &str) -> vcs_merge::structural::StructuralMerge {
    merge_rust(Algorithm::Myers, base.as_bytes(), l.as_bytes(), r.as_bytes())
}

fn clean(m: &vcs_merge::structural::StructuralMerge) -> String {
    match &m.merge {
        TextMerge::Clean(b) => String::from_utf8(b.clone()).unwrap(),
        TextMerge::Conflicted(_) => panic!("expected clean"),
    }
}

#[test]
fn move_and_edit_merges_structurally() {
    // Left moves `a` to the end; right edits `a`. Line diff3 conflicts here.
    let left = "fn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n\nfn a() -> i32 {\n    1\n}\n";
    let right = BASE.replace("    1\n", "    10\n");
    assert!(!merge3(Algorithm::Myers, BASE.as_bytes(), left.as_bytes(), right.as_bytes()).is_clean());
    let m = run(BASE, left, &right);
    assert_eq!(m.outcome, Outcome::Structural);
    assert_eq!(
        clean(&m),
        "fn b() -> i32 {\n    2\n}\n\nfn c() -> i32 {\n    3\n}\n\nfn a() -> i32 {\n    10\n}\n"
    );
}

#[test]
fn methods_of_one_impl_merge_independently() {
    let base = "impl W {\n    fn x(&self) -> i32 {\n        1\n    }\n    fn y(&self) -> i32 {\n        2\n    }\n}\n";
    let l = base.replace("        1\n", "        11\n");
    let r = base.replace("        2\n", "        22\n");
    let m = run(base, &l, &r);
    assert_eq!(m.outcome, Outcome::Structural);
    assert_eq!(clean(&m), base.replace("1\n", "11\n").replace("2\n", "22\n"));
}

#[test]
fn same_line_edit_declines_to_text_conflict() {
    let l = BASE.replace("    1\n", "    10\n");
    let r = BASE.replace("    1\n", "    11\n");
    let m = run(BASE, &l, &r);
    assert_eq!(m.outcome, Outcome::Declined);
    assert_eq!(m.merge, merge3(Algorithm::Myers, BASE.as_bytes(), l.as_bytes(), r.as_bytes()));
    assert!(!m.merge.is_clean());
}

#[test]
fn delete_versus_modify_is_not_silently_resolved() {
    let l = BASE.replace("fn b() -> i32 {\n    2\n}\n\n", "");
    let r = BASE.replace("    2\n", "    20\n");
    let m = run(BASE, &l, &r);
    assert_ne!(m.outcome, Outcome::Structural);
    if let TextMerge::Clean(b) = &m.merge {
        // Whatever the fallback decided, the edit must not be lost without a conflict.
        assert!(String::from_utf8_lossy(b).contains("20"));
    }
}

#[test]
fn unparsable_input_falls_back_to_text_merge() {
    let l = BASE.replace("    1\n", "    let = ;\n");
    let r = BASE.replace("    3\n", "    30\n");
    let m = run(BASE, &l, &r);
    assert_eq!(m.outcome, Outcome::ParseFailed);
    assert_eq!(m.merge, merge3(Algorithm::Myers, BASE.as_bytes(), l.as_bytes(), r.as_bytes()));
}

#[test]
fn identical_sides_and_unchanged_side() {
    assert_eq!(clean(&run(BASE, BASE, BASE)), BASE);
    let r = BASE.replace("    3\n", "    30\n");
    assert_eq!(clean(&run(BASE, BASE, &r)), r);
}

#[test]
fn corpus_never_regresses_and_never_emits_unparsable_output() {
    let mut text_conflicts = 0;
    let mut structural_conflicts = 0;
    for s in corpus::generate(40) {
        let t = merge3(Algorithm::Myers, &s.base, &s.left, &s.right);
        let m = merge_rust(Algorithm::Myers, &s.base, &s.left, &s.right);
        let inputs_parse = parses(&s.base) && parses(&s.left) && parses(&s.right);
        if let TextMerge::Clean(b) = &m.merge {
            assert!(!inputs_parse || parses(b), "unparsable clean output for {}", s.kind);
        }
        if m.outcome == Outcome::Structural {
            assert!(inputs_parse);
        }
        text_conflicts += usize::from(!t.is_clean());
        structural_conflicts += usize::from(!m.merge.is_clean());
        // Structural never conflicts where text merge was clean and parsed.
        if t.is_clean() {
            assert!(m.merge.is_clean(), "regression on {}", s.kind);
        }
    }
    assert!(structural_conflicts < text_conflicts);
}
