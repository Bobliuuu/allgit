//! Portability checks: symlink targets that stay inside the worktree, and paths that would
//! collide on case-insensitive or Unicode-normalizing filesystems (macOS, Windows).

use crate::error::{msg, Result};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;
use vcs_objects::names::is_meta_alias;

/// Why a symlink target is refused, or `Ok` if it provably stays inside the worktree.
///
/// `link_path` is the zone-independent worktree-relative path of the link itself. Rules:
/// relative only, valid UTF-8, no NUL or backslash, no empty components, never names the
/// metadata directory, and `..` may only appear as a leading run that does not climb above
/// the worktree root. A `..` after a normal component is refused because that component
/// could itself be a symlink, which would make a lexical resolution unsound.
pub fn check_symlink_target(link_path: &str, target: &[u8]) -> std::result::Result<(), String> {
    let t = std::str::from_utf8(target).map_err(|_| "target is not valid UTF-8".to_owned())?;
    if t.is_empty() {
        return Err("empty target".into());
    }
    if t.contains('\0') || t.contains('\\') {
        return Err("target contains NUL or backslash".into());
    }
    if t.starts_with('/') || t.as_bytes().get(1) == Some(&b':') {
        return Err("target is absolute".into());
    }
    let depth = link_path.split('/').count() - 1; // directories above the link inside the worktree
    let mut ups = 0usize;
    let mut seen_normal = false;
    for c in t.split('/') {
        match c {
            "" => return Err("target has an empty component".into()),
            "." => {}
            ".." => {
                if seen_normal {
                    return Err("target has '..' after a normal component".into());
                }
                ups += 1;
                if ups > depth {
                    return Err("target escapes the worktree".into());
                }
            }
            n => {
                if is_meta_alias(n) {
                    return Err("target points into repository metadata".into());
                }
                seen_normal = true;
            }
        }
    }
    Ok(())
}

/// The key under which two names are the same file on a folding filesystem.
pub fn fold_component(c: &str) -> String {
    let mut s = String::with_capacity(c.len());
    push_folded(&mut s, c);
    s
}

fn push_folded(out: &mut String, c: &str) {
    if c.is_ascii() {
        out.extend(c.chars().map(|ch| ch.to_ascii_lowercase()));
    } else {
        // Rust's lowercase is not the filesystems' case folding: APFS/NTFS also fold ß to ss, the
        // long s to s and final sigma to sigma (probed on APFS: `straße` and `STRASSE` are one file).
        for ch in c.nfd().collect::<String>().to_lowercase().chars() {
            match ch {
                'ß' => out.push_str("ss"),
                'ſ' => out.push('s'),
                'ς' => out.push('σ'),
                ch => out.push(ch),
            }
        }
    }
}

/// Find two distinct paths (or directory prefixes of paths) that fold to the same name.
/// Returns the pair `(first, second)` of the colliding spellings.
///
/// Consecutive paths usually share their leading directories (sorted input): those prefixes were
/// just examined, so they are skipped instead of folded and looked up again.
pub fn find_collision<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<(String, String)> {
    let mut seen: HashMap<String, &'a str> = HashMap::new();
    let mut prev: &str = "";
    // For the previous path: end offset of each component in the original and in the folded key.
    let mut orig_ends: Vec<usize> = Vec::new();
    let mut key_ends: Vec<usize> = Vec::new();
    let mut key = String::new();
    for p in paths {
        // number of leading components identical to the previous path's
        let mut shared = 0;
        for c in p.split('/') {
            if shared < orig_ends.len() && prev.get(if shared == 0 { 0 } else { orig_ends[shared - 1] + 1 }..orig_ends[shared]) == Some(c) {
                shared += 1;
            } else {
                break;
            }
        }
        orig_ends.truncate(shared);
        key_ends.truncate(shared);
        key.truncate(key_ends.last().copied().unwrap_or(0));
        let mut pos = if shared == 0 { 0 } else { orig_ends[shared - 1] + 1 };
        while pos <= p.len() {
            let end = p[pos..].find('/').map_or(p.len(), |i| pos + i);
            if !key.is_empty() {
                key.push('/');
            }
            push_folded(&mut key, &p[pos..end]);
            let orig = &p[..end];
            match seen.get(key.as_str()) {
                Some(o) if *o != orig => return Some(((*o).to_owned(), orig.to_owned())),
                Some(_) => {}
                None => {
                    seen.insert(key.clone(), orig);
                }
            }
            orig_ends.push(end);
            key_ends.push(key.len());
            pos = end + 1;
        }
        prev = p;
    }
    None
}

pub fn require_no_collision<'a>(paths: impl IntoIterator<Item = &'a str>, what: &str) -> Result<()> {
    match find_collision(paths) {
        None => Ok(()),
        Some((a, b)) => msg(format!(
            "{what}: {a:?} and {b:?} differ only by case or Unicode normalization and would collide on case-insensitive filesystems; rename one"
        )),
    }
}

/// True for a symlink, and on Windows also for any other reparse point (directory junctions,
/// mount points), which `FileType::is_symlink` does not report. Used where checkout must refuse
/// to write through a link planted in the worktree.
pub(crate) fn is_link_like(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert!(check_symlink_target("a/l", b"b").is_ok());
        assert!(check_symlink_target("a/l", b"../x/y").is_ok());
        assert!(check_symlink_target("a/l", b"../..").is_err());
        assert!(check_symlink_target("l", b"..").is_err());
        assert!(check_symlink_target("a/l", b"/etc/passwd").is_err());
        assert!(check_symlink_target("a/l", b"C:/x").is_err());
        assert!(check_symlink_target("a/b/l", b"x/../../..").is_err());
        assert!(check_symlink_target("l", b".VCS/config").is_err());
        assert!(check_symlink_target("l", b"").is_err());
        assert!(check_symlink_target("l", b"a//b").is_err());
        assert!(check_symlink_target("l", &[0xff]).is_err());
    }

    #[test]
    fn folds_like_apfs_for_sharp_s_and_long_s() {
        assert!(find_collision(["dir/straße", "dir/STRASSE"]).is_some());
        assert!(find_collision(["xs", "xſ"]).is_some());
        assert!(find_collision(["a/.vcs-x", "a/.vcs-y"]).is_none());
    }

    #[test]
    fn symlink_targets_into_metadata_aliases_are_refused() {
        assert!(check_symlink_target("l", ".vcſ/config".as_bytes()).is_err());
        assert!(check_symlink_target("l", b"x/.vcs./config").is_err());
    }

    /// The straightforward algorithm `find_collision` replaced (no prefix skipping).
    fn reference(paths: &[String]) -> Option<(String, String)> {
        let mut seen: std::collections::BTreeMap<String, String> = Default::default();
        for p in paths {
            let (mut key, mut orig) = (String::new(), String::new());
            for c in p.split('/') {
                if !key.is_empty() {
                    key.push('/');
                    orig.push('/');
                }
                key.push_str(&c.nfd().collect::<String>().to_lowercase());
                orig.push_str(c);
                match seen.get(&key) {
                    Some(o) if *o != orig => return Some((o.clone(), orig)),
                    Some(_) => {}
                    None => {
                        seen.insert(key.clone(), orig.clone());
                    }
                }
            }
        }
        None
    }

    #[test]
    fn collision_search_matches_reference_on_random_paths() {
        let names = ["a", "A", "b", "B", "ab", "aB", "\u{e9}", "e\u{301}", "x"];
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        for round in 0..3000 {
            let n = 1 + rnd(8);
            let mut paths: Vec<String> = (0..n)
                .map(|_| (0..1 + rnd(4)).map(|_| names[rnd(names.len())]).collect::<Vec<_>>().join("/"))
                .collect();
            if round % 2 == 0 {
                paths.sort();
            }
            let got = find_collision(paths.iter().map(String::as_str));
            assert_eq!(got, reference(&paths), "{paths:?}");
        }
    }

    #[test]
    fn collisions() {
        assert!(find_collision(["a/x", "b/x", "a/y"]).is_none());
        assert!(find_collision(["a", "a"]).is_none());
        assert!(find_collision(["README", "readme"]).is_some());
        assert!(find_collision(["Dir/a", "dir/b"]).is_some());
        assert!(find_collision(["caf\u{e9}", "cafe\u{301}"]).is_some());
        // shared-prefix skipping must not hide a collision between non-adjacent paths
        assert!(find_collision(["a/b/x", "a/b/y", "a/B/z"]).is_some());
        assert!(find_collision(["a/b/x", "a/c/y", "a/B/x"]).is_some());
        assert!(find_collision(["a/b/x", "a/b/y", "a/c/y", "a/b/z"]).is_none());
        assert_eq!(find_collision(["Dir/a", "dir/b"]), Some(("Dir".into(), "dir".into())));
    }
}
