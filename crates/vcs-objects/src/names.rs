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

/// One tree-entry name / path component.
pub fn validate_component(s: &str) -> Result<(), NameError> {
    let bad = s.is_empty()
        || s.len() > MAX_NAME
        || s == "."
        || s == ".."
        || s.bytes().any(|c| c == b'/' || c == b'\\' || c == 0)
        || s.eq_ignore_ascii_case(META_DIR);
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

#[cfg(test)]
mod tests {
    use super::*;
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
