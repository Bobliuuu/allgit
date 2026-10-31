//! Portability checks: symlink targets that stay inside the worktree, and paths that would
//! collide on case-insensitive or Unicode-normalizing filesystems (macOS, Windows).

use crate::error::{msg, Result};
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;
use vcs_objects::names::META_DIR;

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
                if n.eq_ignore_ascii_case(META_DIR) {
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
    c.nfd().collect::<String>().to_lowercase()
}

/// Find two distinct paths (or directory prefixes of paths) that fold to the same name.
/// Returns the pair `(first, second)` of the colliding spellings.
pub fn find_collision<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<(String, String)> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for p in paths {
        let mut key = String::new();
        let mut orig = String::new();
        for c in p.split('/') {
            if !key.is_empty() {
                key.push('/');
                orig.push('/');
            }
            key.push_str(&fold_component(c));
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
    fn collisions() {
        assert!(find_collision(["a/x", "b/x", "a/y"]).is_none());
        assert!(find_collision(["a", "a"]).is_none());
        assert!(find_collision(["README", "readme"]).is_some());
        assert!(find_collision(["Dir/a", "dir/b"]).is_some());
        assert!(find_collision(["caf\u{e9}", "cafe\u{301}"]).is_some());
    }
}
