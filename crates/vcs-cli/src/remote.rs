//! URL -> backend opening.
use std::path::{Component, Path, PathBuf};
use vcs_remote::{Backend, FsBackend};
use vcs_repo::Error;
use vcs_store_s3::{S3Backend, S3Config};

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
        Some("s3") => return open_s3(url),
        Some(s) => {
            return Err(Error::Msg(format!(
                "remote scheme {s}:// is not supported in this build (url: {url})\n\nSupported:\n  a local directory path\n  file:///absolute/path\n  s3://bucket/prefix?endpoint=http://host:port"
            )))
        }
    };
    let b = FsBackend::open(path).map_err(|e| Error::Msg(format!("cannot open remote {url}: {e}")))?;
    Ok(Box::new(b))
}

fn open_s3(url: &str) -> Result<Box<dyn Backend>, Error> {
    let cfg = s3_config(url, &|k| std::env::var(k).ok())?;
    let b = S3Backend::connect(cfg).map_err(|e| Error::Msg(format!("cannot open remote {url}: {e}")))?;
    Ok(Box::new(b))
}

fn pct_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            out.push(h.ok_or_else(|| format!("bad percent-escape in {s:?}"))?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("invalid UTF-8 in {s:?}"))
}

/// Build an [`S3Config`] from `s3://bucket/prefix?endpoint=...&region=...`.
/// Credentials come only from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (via `env`);
/// they are never part of the URL, so they are never stored in `.vcs/remote`.
fn s3_config(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<S3Config, Error> {
    let usage = "expected s3://bucket[/prefix]?endpoint=http(s)://host:port[&region=us-east-1]";
    // Never echo the query string: it may hold a secret pasted by mistake.
    let shown = url.split('?').next().unwrap_or(url);
    let bad = |m: String| Error::Msg(format!("invalid S3 remote {shown}: {m}\n\nExpected:\n  {usage}"));
    let rest = &url["s3://".len()..];
    let (loc, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (bucket, prefix) = loc.split_once('/').unwrap_or((loc, ""));
    if bucket.is_empty() {
        return Err(bad("missing bucket name".into()));
    }
    let prefix = prefix.trim_matches('/');
    let (mut endpoint, mut region) = (None, None);
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = pct_decode(v).map_err(bad)?;
        match k {
            "endpoint" => endpoint = Some(v),
            "region" => region = Some(v),
            // Do not echo the value: it may be a secret pasted by mistake.
            _ => {
                return Err(bad(format!(
                    "unknown option {k:?} (credentials must be set via AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, never in the URL)"
                )))
            }
        }
    }
    let endpoint = endpoint
        .filter(|e| !e.is_empty())
        .ok_or_else(|| bad("missing endpoint option (see docs/s3.md)".into()))?;
    let need = |name: &str| {
        env(name)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::Msg(format!("missing S3 credentials: environment variable {name} is not set\n\nOptions:\n  export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=...")))
    };
    let (ak, sk) = (need("AWS_ACCESS_KEY_ID")?, need("AWS_SECRET_ACCESS_KEY")?);
    let mut cfg = S3Config::new(&endpoint, bucket, &ak, &sk);
    if let Some(r) = region
        .or_else(|| env("AWS_REGION"))
        .or_else(|| env("AWS_DEFAULT_REGION"))
        .filter(|r| !r.is_empty())
    {
        cfg.region = r;
    }
    cfg.session_token = env("AWS_SESSION_TOKEN").filter(|v| !v.is_empty());
    if !prefix.is_empty() {
        cfg.prefix = format!("{prefix}/");
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(k: &str) -> Option<String> {
        match k {
            "AWS_ACCESS_KEY_ID" => Some("AK".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".into()),
            "AWS_SESSION_TOKEN" => Some("TOK".into()),
            _ => None,
        }
    }
    #[test]
    fn s3_url_parsing() {
        let c = s3_config("s3://bkt/a/b/?endpoint=http%3A%2F%2F127.0.0.1%3A9000&region=eu-1", &env).unwrap();
        assert_eq!((c.bucket.as_str(), c.prefix.as_str(), c.region.as_str()), ("bkt", "a/b/", "eu-1"));
        assert_eq!((c.endpoint.as_str(), c.access_key.as_str()), ("http://127.0.0.1:9000", "AK"));
        assert_eq!(c.session_token.as_deref(), Some("TOK"));
        let c = s3_config("s3://bkt?endpoint=https%3A%2F%2Fs3.example.com", &env).unwrap();
        assert_eq!(c.endpoint, "https://s3.example.com");
        let c = s3_config("s3://bkt?endpoint=http://h:1", &env).unwrap();
        assert_eq!((c.prefix.as_str(), c.region.as_str()), ("", "us-east-1"));
    }
    #[test]
    fn s3_url_errors_are_clear() {
        let none = |_: &str| None;
        let e = s3_config("s3://bkt?endpoint=http://h:1", &none).unwrap_err().to_string();
        assert!(e.contains("AWS_ACCESS_KEY_ID is not set"), "{e}");
        let e = s3_config("s3://?endpoint=http://h:1", &env).unwrap_err().to_string();
        assert!(e.contains("missing bucket"), "{e}");
        let e = s3_config("s3://bkt", &env).unwrap_err().to_string();
        assert!(e.contains("missing endpoint"), "{e}");
        let e = s3_config("s3://bkt?endpoint=http://h:1&secret=hunter2", &env)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown option") && !e.contains("hunter2"), "{e}");
    }
    #[test]
    fn schemes() {
        let e = open_backend("gs://bucket/x").err().unwrap();
        assert!(e.to_string().contains("not supported in this build"));
        assert_eq!(normalize_url("file:///a/b", Path::new("/c")), "/a/b");
        assert_eq!(normalize_url("r", Path::new("/c")), "/c/r");
        assert_eq!(normalize_url("../x/./y", Path::new("/c/d")), "/c/x/y");
        assert_eq!(normalize_url("s3://b", Path::new("/c")), "s3://b");
    }
}
