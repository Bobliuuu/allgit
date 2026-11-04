//! Security regression (docs/security-review.md): names that a case-/normalization-folding or
//! Windows filesystem resolves to the `.vcs` metadata directory must be refused like `.vcs`.
//! Verified on macOS APFS: `.vcſ/config` (U+017F) opens `.vcs/config`.
use vcs_objects::names::{validate_component, validate_path};
use vcs_objects::{EntryMode, Object, ObjectId, Tree, TreeEntry};

const ALIASES: &[&str] = &[
    ".vcſ", // LATIN SMALL LETTER LONG S folds to s
    ".VCſ",
    ".ｖｃｓ", // fullwidth
    ".vcs.",   // NTFS strips trailing dots and spaces
    ".vcs ",
    ".vcs...",
    ".vcs::$INDEX_ALLOCATION", // NTFS alternate stream
    ".vcs:stream",
    "VCS~1", // NTFS 8.3 short name of `.vcs`
    "vcs~2",
    ".v\u{200c}cs", // HFS+ ignores zero-width code points
    ".vc\u{200d}s",
    ".\u{feff}vcs",
    ".v\u{202e}cs",
    ".vcs\u{206a}",
];

#[test]
fn metadata_dir_aliases_are_refused() {
    for a in ALIASES {
        assert!(validate_component(a).is_err(), "component {a:?} accepted");
        assert!(validate_path(&format!("sub/{a}/config")).is_err(), "path through {a:?} accepted");
    }
    // ordinary names that merely look similar stay valid
    for ok in [
        ".vcsignore",
        ".vcs-notes",
        "vcs",
        "my.vcs.txt",
        "vcs~",
        "vcs~x",
        ".vc",
        "résumé.txt",
        "a:b.txt",
    ] {
        let r = validate_component(ok);
        // `a:b.txt` is a legal name on unix; only a `.vcs` base is refused
        assert!(r.is_ok(), "{ok:?} wrongly refused");
    }
}

#[test]
fn tree_decode_rejects_alias_names() {
    for a in ALIASES {
        // a valid tree whose single name has the alias's byte length, then patch the name bytes in
        let plain = "q".repeat(a.len());
        let t = Tree::new(vec![TreeEntry {
            name: plain.clone(),
            mode: EntryMode::File,
            id: ObjectId::from_bytes([7; 32]),
        }])
        .unwrap();
        let mut bytes = Object::Tree(t).encode();
        let at = bytes.windows(a.len()).position(|w| w == plain.as_bytes()).unwrap();
        bytes[at..at + a.len()].copy_from_slice(a.as_bytes());
        assert!(Object::decode(&bytes).is_err(), "tree entry {a:?} decoded");
        // and the constructor refuses it too
        let id = ObjectId::from_bytes([7; 32]);
        assert!(Tree::new(vec![TreeEntry {
            name: (*a).to_owned(),
            mode: EntryMode::File,
            id
        }])
        .is_err());
    }
}
