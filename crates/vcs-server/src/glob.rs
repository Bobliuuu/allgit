//! Path globs for the zone policy. Paths are zone-relative with `/` separators.
//!
//! * `*` matches any run of characters inside one path segment (never `/`)
//! * `?` matches one character inside a segment
//! * a segment that is exactly `**` matches zero or more whole segments
//!
//! Matching is anchored: `art/*.psd` does not match `art/x/y.psd`; `art/**` matches `art/x/y.psd`
//! but not `art` itself unless you also write `art`. There are no character classes or escapes.

fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| seg_match(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && seg_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && seg_match(rest, &s[1..]),
    }
}

fn segs_match(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| segs_match(rest, &s[i..])),
        Some((seg, rest)) => !s.is_empty() && seg_match(seg.as_bytes(), s[0].as_bytes()) && segs_match(rest, &s[1..]),
    }
}

/// True if the zone-relative `path` matches `glob`.
pub fn glob_match(glob: &str, path: &str) -> bool {
    let p: Vec<&str> = glob.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs_match(&p, &s)
}

#[cfg(test)]
mod tests {
    use super::glob_match as g;

    #[test]
    fn matching() {
        assert!(g("a.bin", "a.bin"));
        assert!(!g("a.bin", "x/a.bin"));
        assert!(g("*.psd", "hero.psd"));
        assert!(!g("*.psd", "art/hero.psd"));
        assert!(g("art/**", "art/x/y.psd"));
        assert!(g("art/**", "art/y"));
        assert!(!g("art/**", "arty/y"));
        assert!(g("**/*.psd", "hero.psd"));
        assert!(g("**/*.psd", "a/b/hero.psd"));
        assert!(!g("**/*.psd", "a/b/hero.png"));
        assert!(g("a/**/z", "a/z"));
        assert!(g("a/**/z", "a/b/c/z"));
        assert!(g("h?ro.*", "hero.bin"));
        assert!(!g("h?ro.*", "hro.bin"));
        assert!(g("**", "any/thing/at/all"));
        assert!(!g("", "x"));
    }
}
