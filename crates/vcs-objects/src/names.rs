//! Validation of untrusted names and paths. Everything that reaches the filesystem
//! or an object goes through here.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("invalid zone name {0:?} (allowed: [A-Za-z0-9_.-], 1..=64 chars, not starting with '.' or '-')")]
    Zone(String),
    #[error("invalid branch name {0:?}")]
    Branch(String),
    #[error("invalid path component {0:?}")]
    Component(String),
    #[error("invalid path {0:?}: {1}")]
    Path(String, &'static str),
}

pub const MAX_NAME: usize = 255;
pub const MAX_PATH: usize = 4096;
/// Reserved metadata directory name; never tracked, compared case-insensitively
/// (case-insensitive filesystems would otherwise alias `.VCS`).
pub const META_DIR: &str = ".vcs";

pub fn validate_zone_name(s: &str) -> Result<(), NameError> {
    let ok = !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && !s.starts_with('-')
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(NameError::Zone(s.to_owned()))
    }
}

pub fn validate_branch_name(s: &str) -> Result<(), NameError> {
    let ok = !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('/')
        && !s.ends_with('/')
        && !s.contains("//")
        && !s.contains("..")
        && !s.starts_with('-')
        && s.split('/')
            .all(|c| !c.is_empty() && c != "." && !c.starts_with('.') && !c.ends_with(".lock"))
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-' | b'/'));
    if ok {
        Ok(())
    } else {
        Err(NameError::Branch(s.to_owned()))
    }
}

/// True if `s` names the metadata directory on *some* filesystem: ASCII case variants, but also
/// what case-/normalization-folding volumes (APFS, HFS+: `.vcſ` with U+017F, fullwidth forms,
/// zero-width code points that HFS+ ignores) and NTFS (trailing dots/spaces, `:stream` suffixes,
/// the `VCS~1` short name) resolve to it. Writing `.vcſ/config` on such a volume would overwrite
/// `.vcs/config`, so the name is refused wherever `.vcs` is.
pub fn is_meta_alias(s: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    if s.eq_ignore_ascii_case(META_DIR) {
        return true;
    }
    if s.is_ascii() && !s.contains([':', '~', '.', ' ']) {
        return false;
    }
    let base = s.split(':').next().unwrap_or(s).trim_end_matches(['.', ' ']);
    if base.is_ascii() {
        // NFKC, the zero-width filter and lowercasing are the identity / ASCII lowercase on ASCII,
        // so skip the Unicode machinery (every `file.txt` lands here).
        let short =
            base.len() > 4 && base.as_bytes()[..4].eq_ignore_ascii_case(b"vcs~") && base.as_bytes()[4..].iter().all(u8::is_ascii_digit);
        return base.eq_ignore_ascii_case(META_DIR) || short;
    }
    let folded: String = base
        .nfkc()
        .filter(|c| !matches!(*c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'))
        .flat_map(char::to_lowercase)
        .collect();
    let short = folded
        .strip_prefix("vcs~")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    folded == META_DIR || short
}

/// One tree-entry name / path component.
pub fn validate_component(s: &str) -> Result<(), NameError> {
    let bad = s.is_empty()
        || s.len() > MAX_NAME
        || s == "."
        || s == ".."
        || s.bytes().any(|c| c == b'/' || c == b'\\' || c == 0)
        || is_meta_alias(s);
    if bad {
        Err(NameError::Component(s.to_owned()))
    } else {
        Ok(())
    }
}

/// Validate a repo-relative, `/`-separated path (no leading `/`, no `.`/`..`, no empty parts).
pub fn validate_path(s: &str) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Path(s.to_owned(), "empty"));
    }
    if s.len() > MAX_PATH {
        return Err(NameError::Path(s.to_owned(), "too long"));
    }
    for c in s.split('/') {
        validate_component(c).map_err(|_| NameError::Path(s.to_owned(), "bad component"))?;
    }
    Ok(())
}

/// True if `a` and `b` are equal or one is a component-wise prefix of the other.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let pre = |x: &str, y: &str| x == y || (y.starts_with(x) && y.as_bytes()[x.len()] == b'/');
    pre(a, b) || pre(b, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full Unicode rule, with no ASCII shortcut: the reference for the fast path.
    fn meta_alias_reference(s: &str) -> bool {
        use unicode_normalization::UnicodeNormalization;
        if s.eq_ignore_ascii_case(META_DIR) {
            return true;
        }
        let base = s.split(':').next().unwrap_or(s).trim_end_matches(['.', ' ']);
        let folded: String = base.nfkc().flat_map(char::to_lowercase).collect();
        let short = folded
            .strip_prefix("vcs~")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        folded == META_DIR || short
    }

    #[test]
    fn meta_alias_ascii_fast_path_matches_reference() {
        let alphabet = [".", "v", "c", "s", "V", "~", "1", "9", ":", " ", "a", "x"];
        let mut cases = vec![String::new()];
        for _ in 0..5 {
            let next: Vec<String> = cases.iter().flat_map(|c| alphabet.iter().map(move |a| format!("{c}{a}"))).collect();
            cases.extend(next);
            cases.truncate(60_000);
        }
        for extra in [
            "file.txt",
            ".vcs",
            ".VCS.",
            ".vcs:stream",
            "vcs~1",
            "VCS~12",
            "vcs~",
            "vcs~1a",
            ".vcs ",
            "a.vcs",
        ] {
            cases.push(extra.to_owned());
        }
        for c in &cases {
            assert_eq!(is_meta_alias(c), meta_alias_reference(c), "{c:?}");
        }
    }
    #[test]
    fn paths() {
        for ok in ["a", "a/b/c.txt", "dir/.hidden"] {
            validate_path(ok).unwrap();
        }
        for bad in [
            "", "/a", "a/", "a//b", "../x", "a/../b", "./a", ".vcs/x", "a/.VCS/x", "a\\b", "a\0b",
        ] {
            assert!(validate_path(bad).is_err(), "{bad:?}");
        }
    }
    #[test]
    fn names() {
        validate_zone_name("engine").unwrap();
        assert!(validate_zone_name("../x").is_err());
        assert!(validate_zone_name("").is_err());
        validate_branch_name("feature/rendering").unwrap();
        for bad in ["", "a..b", "/a", "a/", "a//b", "-x", "a/.b", "x.lock", "a b"] {
            assert!(validate_branch_name(bad).is_err(), "{bad:?}");
        }
    }
}
