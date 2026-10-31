//! URL -> backend opening.
use std::path::{Component, Path, PathBuf};
use vcs_remote::{Backend, FsBackend};
use vcs_repo::Error;

fn url_scheme(url: &str) -> Option<&str> {
    let (s, _) = url.split_once("://")?;
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))).then_some(s)
}

/// Make a user-supplied remote location absolute so it keeps working from any cwd.
/// Plain paths and `file://` URLs become absolute paths; other schemes are kept verbatim.
pub fn normalize_url(url: &str, cwd: &Path) -> String {
    match url_scheme(url) {
        None => absolutize(&cwd.join(url)),
        Some("file") => absolutize(&cwd.join(&url["file://".len()..])),
        Some(_) => url.to_owned(),
    }
}

/// Resolve `.`/`..` and symlinks in the longest existing prefix; the rest (a target that
/// will be created) is cleaned lexically and appended.
fn absolutize(p: &Path) -> String {
    let mut clean = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            c => clean.push(c),
        }
    }
    let mut tail = Vec::new();
    let mut base = clean.as_path();
    loop {
        if let Ok(real) = base.canonicalize() {
            let mut out = real;
            out.extend(tail.iter().rev());
            return out.to_string_lossy().into_owned();
        }
        match (base.parent(), base.file_name()) {
            (Some(par), Some(name)) => {
                tail.push(name);
                base = par;
            }
            _ => return clean.to_string_lossy().into_owned(),
        }
    }
}

/// Open the backend for a remote URL (plain path or `file://`).
pub fn open_backend(url: &str) -> Result<Box<dyn Backend>, Error> {
    let path: PathBuf = match url_scheme(url) {
        None => url.into(),
        Some("file") => url["file://".len()..].into(),
        Some(s) => {
            return Err(Error::Msg(format!(
                "remote scheme {s}:// is not supported in this build (url: {url})\n\nSupported:\n  a local directory path\n  file:///absolute/path"
            )))
        }
    };
    let b = FsBackend::open(path).map_err(|e| Error::Msg(format!("cannot open remote {url}: {e}")))?;
    Ok(Box::new(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemes() {
        let e = open_backend("s3://bucket/x").err().unwrap();
        assert!(e.to_string().contains("not supported in this build"));
        assert_eq!(normalize_url("file:///a/b", Path::new("/c")), "/a/b");
        assert_eq!(normalize_url("r", Path::new("/c")), "/c/r");
        assert_eq!(normalize_url("../x/./y", Path::new("/c/d")), "/c/x/y");
        assert_eq!(normalize_url("s3://b", Path::new("/c")), "s3://b");
    }
}
