//! Path globs for the zone policy. Paths are zone-relative with `/` separators.
//!
//! * `*` matches any run of characters inside one path segment (never `/`)
//! * `?` matches one character inside a segment
//! * a segment that is exactly `**` matches zero or more whole segments
//!
//! Matching is anchored: `art/*.psd` does not match `art/x/y.psd`; `art/**` matches `art/x/y.psd`
//! but not `art` itself unless you also write `art`. There are no character classes or escapes.

/// Wildcard match of one segment: `*` any run, `?` one byte. Iterative with one backtrack point, so
/// the work is at most `p.len() * s.len()` (a recursive version is exponential in the number of `*`).
fn seg_match(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

/// Segment-level match where a `**` segment matches zero or more segments. Dynamic programming over
/// (pattern index, path index): `O(p * s)` segment matches.
fn segs_match(p: &[&str], s: &[&str]) -> bool {
    // ok[j] = p[i..] matches s[j..]
    let mut ok = vec![false; s.len() + 1];
    ok[s.len()] = true;
    for seg in p.iter().rev() {
        if *seg == "**" {
            // ok'[j] = any ok[k], k >= j
            for j in (0..s.len()).rev() {
                ok[j] = ok[j] || ok[j + 1];
            }
        } else {
            for j in 0..s.len() {
                ok[j] = ok[j + 1] && seg_match(seg.as_bytes(), s[j].as_bytes());
            }
            ok[s.len()] = false;
        }
    }
    ok[0]
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

    /// Found by the policy_tokens fuzz target: the recursive matcher took exponential time.
    #[test]
    fn many_wildcards_do_not_blow_up() {
        let t = std::time::Instant::now();
        let stars = "*".repeat(60);
        assert!(!g(&format!("{stars}b"), &"a".repeat(60)));
        assert!(g(&stars, &"a".repeat(60)));
        let segs = vec!["**"; 40].join("/");
        let path = vec!["x"; 40].join("/");
        assert!(g(&segs, &path));
        assert!(!g(&format!("{segs}/q"), &path));
        assert!(t.elapsed().as_secs() < 1);
    }
}
