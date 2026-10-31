#![cfg(any(feature = "structural-python", feature = "structural-typescript", feature = "structural-go"))]
use vcs_merge::structural::corpus::{self, Style};
use vcs_merge::structural::{lang_for_path, merge_with, parses_as, Lang, Outcome, StructuralMerge};
use vcs_merge::{merge3, Algorithm, TextMerge};

fn run(lang: &Lang, b: &str, l: &str, r: &str) -> StructuralMerge {
    merge_with(lang, Algorithm::Myers, b.as_bytes(), l.as_bytes(), r.as_bytes())
}

fn clean(m: &StructuralMerge) -> String {
    match &m.merge {
        TextMerge::Clean(b) => String::from_utf8(b.clone()).unwrap(),
        TextMerge::Conflicted(_) => panic!("expected clean"),
    }
}

/// Shared checks: the generated corpus never yields an unparsable clean output, never regresses
/// against text merge, and structural merge strictly reduces conflicted files.
fn check_corpus(lang: &Lang, st: Style) {
    let mut text_conflicts = 0;
    let mut structural_conflicts = 0;
    let mut structural_wins = 0;
    for s in corpus::generate_for(st, 40) {
        let t = merge3(Algorithm::Myers, &s.base, &s.left, &s.right);
        let m = merge_with(lang, Algorithm::Myers, &s.base, &s.left, &s.right);
        let inputs_parse = parses_as(lang, &s.base) && parses_as(lang, &s.left) && parses_as(lang, &s.right);
        if s.kind == "left_syntax_error" {
            assert!(!parses_as(lang, &s.left), "{}: syntax error scenario must not parse", lang.name);
            assert_eq!(m.outcome, Outcome::ParseFailed);
            assert_eq!(m.merge, t);
            continue;
        }
        assert!(inputs_parse, "{}: generated input does not parse ({})", lang.name, s.kind);
        if let TextMerge::Clean(b) = &m.merge {
            assert!(parses_as(lang, b), "{}: unparsable clean output for {}", lang.name, s.kind);
        }
        text_conflicts += usize::from(!t.is_clean());
        structural_conflicts += usize::from(!m.merge.is_clean());
        structural_wins += usize::from(m.outcome == Outcome::Structural);
        if t.is_clean() {
            assert!(m.merge.is_clean(), "{}: regression on {}", lang.name, s.kind);
        }
    }
    assert!(structural_wins > 0, "{}: structural never fired", lang.name);
    assert!(
        structural_conflicts < text_conflicts,
        "{}: {structural_conflicts} !< {text_conflicts}",
        lang.name
    );
}

#[cfg(feature = "structural-python")]
mod python {
    use super::*;
    use vcs_merge::structural::{merge_python, PYTHON};

    const BASE: &str = "import os\n\ndef a():\n    return 1\n\ndef b():\n    return 2\n\ndef c():\n    return 3\n";

    #[test]
    fn move_and_edit_merges_structurally() {
        let left = "import os\n\ndef b():\n    return 2\n\ndef c():\n    return 3\n\ndef a():\n    return 1\n";
        let right = BASE.replace("return 1", "return 10");
        assert!(!merge3(Algorithm::Myers, BASE.as_bytes(), left.as_bytes(), right.as_bytes()).is_clean());
        let m = run(&PYTHON, BASE, left, &right);
        assert_eq!(m.outcome, Outcome::Structural);
        assert_eq!(
            clean(&m),
            "import os\n\ndef b():\n    return 2\n\ndef c():\n    return 3\n\ndef a():\n    return 10\n"
        );
    }

    #[test]
    fn methods_of_one_class_merge_independently() {
        let base = "class W:\n    def x(self):\n        return 1\n    def y(self):\n        return 2\n";
        let l = base.replace("return 1", "return 11");
        let r = base.replace("return 2", "return 22");
        let m = run(&PYTHON, base, &l, &r);
        assert_eq!(m.outcome, Outcome::Structural);
        assert_eq!(clean(&m), base.replace("1\n", "11\n").replace("2\n", "22\n"));
    }

    #[test]
    fn moving_the_first_method_keeps_indentation() {
        let base = "class W:\n    def x(self):\n        return 1\n\n    def y(self):\n        return 2\n";
        let l = "class W:\n    def y(self):\n        return 2\n\n    def x(self):\n        return 1\n";
        let r = base.replace("return 1", "return 10");
        let m = run(&PYTHON, base, l, &r);
        assert_eq!(m.outcome, Outcome::Structural);
        assert_eq!(
            clean(&m),
            "class W:\n    def y(self):\n        return 2\n\n    def x(self):\n        return 10\n"
        );
    }

    #[test]
    fn decorated_class_and_same_line_conflict() {
        let base = "@dec\nclass W:\n    x = 1\n    def y(self):\n        return 2\n";
        let l = base.replace("x = 1", "x = 2");
        let r = base.replace("return 2", "return 3");
        let m = run(&PYTHON, base, &l, &r);
        assert_eq!(m.outcome, Outcome::Structural);
        assert!(clean(&m).contains("x = 2") && clean(&m).contains("return 3") && clean(&m).starts_with("@dec\nclass W:"));
        let l2 = BASE.replace("return 1", "return 10");
        let r2 = BASE.replace("return 1", "return 11");
        let m = run(&PYTHON, BASE, &l2, &r2);
        assert_eq!(m.outcome, Outcome::Declined);
        assert_eq!(m.merge, merge3(Algorithm::Myers, BASE.as_bytes(), l2.as_bytes(), r2.as_bytes()));
    }

    #[test]
    fn unparsable_input_falls_back() {
        let l = BASE.replace("return 1", "return = ;");
        let r = BASE.replace("return 3", "return 30");
        let m = merge_python(Algorithm::Myers, BASE.as_bytes(), l.as_bytes(), r.as_bytes());
        assert_eq!(m.outcome, Outcome::ParseFailed);
    }

    #[test]
    fn corpus() {
        check_corpus(&PYTHON, Style::Python);
        assert_eq!(lang_for_path("a/b.py").map(|l| l.name), Some("python"));
    }
}

#[cfg(feature = "structural-typescript")]
mod typescript {
    use super::*;
    use vcs_merge::structural::{merge_typescript, TYPESCRIPT};

    const BASE: &str = "export function a(): number {\n  return 1;\n}\n\nexport function b(): number {\n  return 2;\n}\n\nfunction c(): number {\n  return 3;\n}\n";

    #[test]
    fn move_and_edit_merges_structurally() {
        let left = "export function b(): number {\n  return 2;\n}\n\nfunction c(): number {\n  return 3;\n}\n\nexport function a(): number {\n  return 1;\n}\n";
        let right = BASE.replace("return 1", "return 10");
        assert!(!merge3(Algorithm::Myers, BASE.as_bytes(), left.as_bytes(), right.as_bytes()).is_clean());
        let m = run(&TYPESCRIPT, BASE, left, &right);
        assert_eq!(m.outcome, Outcome::Structural);
        assert!(clean(&m).ends_with("export function a(): number {\n  return 10;\n}\n"));
    }

    #[test]
    fn exported_class_members_merge_independently() {
        let base = "export class W {\n  x(): number {\n    return 1;\n  }\n  y(): number {\n    return 2;\n  }\n}\n";
        let l = base.replace("return 1", "return 11");
        let r = base.replace("return 2", "return 22");
        let m = run(&TYPESCRIPT, base, &l, &r);
        assert_eq!(m.outcome, Outcome::Structural);
        assert_eq!(clean(&m), base.replace("1;", "11;").replace("2;", "22;"));
    }

    #[test]
    fn same_line_edit_declines() {
        let l = BASE.replace("return 1", "return 10");
        let r = BASE.replace("return 1", "return 11");
        let m = run(&TYPESCRIPT, BASE, &l, &r);
        assert_eq!(m.outcome, Outcome::Declined);
        assert!(!m.merge.is_clean());
    }

    #[test]
    fn unparsable_input_falls_back() {
        let l = BASE.replace("return 1;", "return = ;");
        let m = merge_typescript(Algorithm::Myers, BASE.as_bytes(), l.as_bytes(), BASE.as_bytes());
        assert_eq!(m.outcome, Outcome::ParseFailed);
    }

    #[test]
    fn corpus() {
        check_corpus(&TYPESCRIPT, Style::TypeScript);
        assert_eq!(lang_for_path("a/b.ts").map(|l| l.name), Some("typescript"));
    }
}

#[cfg(feature = "structural-go")]
mod go {
    use super::*;
    use vcs_merge::structural::{merge_go, GO};

    const BASE: &str = "package main\n\nfunc a() int {\n\treturn 1\n}\n\nfunc b() int {\n\treturn 2\n}\n\nfunc c() int {\n\treturn 3\n}\n";

    #[test]
    fn move_and_edit_merges_structurally() {
        let left = "package main\n\nfunc b() int {\n\treturn 2\n}\n\nfunc c() int {\n\treturn 3\n}\n\nfunc a() int {\n\treturn 1\n}\n";
        let right = BASE.replace("return 1", "return 10");
        assert!(!merge3(Algorithm::Myers, BASE.as_bytes(), left.as_bytes(), right.as_bytes()).is_clean());
        let m = run(&GO, BASE, left, &right);
        assert_eq!(m.outcome, Outcome::Structural);
        assert!(clean(&m).ends_with("func a() int {\n\treturn 10\n}\n"));
    }

    #[test]
    fn methods_are_keyed_by_receiver() {
        let base = "package main\n\nfunc (w *A) M() int {\n\treturn 1\n}\n\nfunc (w *B) M() int {\n\treturn 2\n}\n";
        let l = base.replace("return 1", "return 11");
        let r = base.replace("return 2", "return 22");
        let m = run(&GO, base, &l, &r);
        assert_eq!(m.outcome, Outcome::Structural);
        assert_eq!(clean(&m), base.replace("1\n", "11\n").replace("2\n", "22\n"));
    }

    #[test]
    fn delete_versus_modify_is_not_silently_resolved() {
        let l = BASE.replace("func b() int {\n\treturn 2\n}\n\n", "");
        let r = BASE.replace("return 2", "return 20");
        let m = run(&GO, BASE, &l, &r);
        assert_ne!(m.outcome, Outcome::Structural);
        if let TextMerge::Clean(b) = &m.merge {
            assert!(String::from_utf8_lossy(b).contains("20"));
        }
    }

    #[test]
    fn unparsable_input_falls_back() {
        let l = BASE.replace("return 1", "return = ;");
        let m = merge_go(Algorithm::Myers, BASE.as_bytes(), l.as_bytes(), BASE.as_bytes());
        assert_eq!(m.outcome, Outcome::ParseFailed);
    }

    #[test]
    fn corpus() {
        check_corpus(&GO, Style::Go);
        assert_eq!(lang_for_path("a/b.go").map(|l| l.name), Some("go"));
    }
}
