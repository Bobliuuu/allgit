//! `--backend` URL for `vcs-server`: where the authoritative objects and manifest live.
//!
//! * `s3://bucket[/prefix]?endpoint=http(s)://host:port[&region=us-east-1][&timeout=SECS][&retries=N]`
//!   (S3-compatible store). Credentials never come from the URL: `AWS_ACCESS_KEY_ID` /
//!   `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` first, else the container credentials endpoint
//!   (`AWS_CONTAINER_CREDENTIALS_*`), else EC2 instance metadata (IMDSv2, off with
//!   `AWS_EC2_METADATA_DISABLED=true`); role credentials are refreshed before they expire.
//!   `VCS_S3_TIMEOUT_SECS`, `VCS_S3_RETRIES`, `VCS_S3_RETRY_BASE_MS`, `VCS_S3_RETRY_MAX_MS` tune the client.
//! * `file:///abs/dir` or a plain directory path (same as `--root`)

use std::sync::Arc;
use vcs_remote::{Backend, FsBackend};
use vcs_store_s3::{CredentialProvider, S3Backend, S3Config};

fn pct_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            out.push(h.ok_or_else(|| "bad percent-escape in backend URL".to_string())?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "invalid UTF-8 in backend URL".to_string())
}

/// Parse `s3://...` into a config; `env` supplies the credentials.
pub fn s3_config(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<S3Config, String> {
    s3_config_with(url, env, true)
}

/// `chain`: fall back to the container / instance-role providers when no static keys are set.
fn s3_config_with(url: &str, env: &dyn Fn(&str) -> Option<String>, chain: bool) -> Result<S3Config, String> {
    let usage = "expected s3://bucket[/prefix]?endpoint=http(s)://host:port[&region=us-east-1]";
    // never echo the query string: it may hold a secret pasted by mistake
    let shown = url.split('?').next().unwrap_or(url);
    let bad = |m: String| format!("invalid backend {shown}: {m} ({usage})");
    let rest = url.strip_prefix("s3://").ok_or_else(|| bad("not an s3:// URL".into()))?;
    let (loc, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (bucket, prefix) = loc.split_once('/').unwrap_or((loc, ""));
    if bucket.is_empty() {
        return Err(bad("missing bucket name".into()));
    }
    let (mut endpoint, mut region, mut timeout, mut retries) = (None, None, None, None);
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = pct_decode(v).map_err(bad)?;
        match k {
            "endpoint" => endpoint = Some(v),
            "region" => region = Some(v),
            "timeout" => {
                timeout = Some(
                    v.parse::<u64>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| bad("timeout must be seconds > 0".into()))?,
                )
            }
            "retries" => {
                retries = Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| *n <= 20)
                        .ok_or_else(|| bad("retries must be 0..=20".into()))?,
                )
            }
            _ => {
                return Err(bad(format!(
                    "unknown option {k:?} (credentials come from AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, never the URL)"
                )))
            }
        }
    }
    let endpoint = endpoint
        .filter(|e| !e.is_empty())
        .ok_or_else(|| bad("missing endpoint option".into()))?;
    let get = |name: &str| env(name).filter(|v| !v.is_empty());
    let mut cfg = match (get("AWS_ACCESS_KEY_ID"), get("AWS_SECRET_ACCESS_KEY")) {
        (Some(ak), Some(sk)) => S3Config::new(&endpoint, bucket, &ak, &sk),
        (ak, sk) => {
            let provider = if chain {
                CredentialProvider::container_from_env(env).or_else(|| CredentialProvider::imds_from_env(env))
            } else {
                None
            };
            let Some(p) = provider else {
                let name = if ak.is_none() {
                    "AWS_ACCESS_KEY_ID"
                } else {
                    "AWS_SECRET_ACCESS_KEY"
                };
                let _ = sk;
                return Err(format!(
                    "missing S3 credentials: environment variable {name} is not set{}",
                    if chain {
                        " (and no container credentials URI is set; instance metadata is disabled)"
                    } else {
                        ""
                    }
                ));
            };
            let mut c = S3Config::new(&endpoint, bucket, "", "");
            c.credentials = Some(Arc::new(p));
            c
        }
    };
    cfg.apply_tuning_env(env);
    if let Some(t) = timeout {
        cfg.timeout = std::time::Duration::from_secs(t);
    }
    if let Some(r) = retries {
        cfg.retries = r;
    }
    if let Some(r) = region
        .or_else(|| env("AWS_REGION"))
        .or_else(|| env("AWS_DEFAULT_REGION"))
        .filter(|r| !r.is_empty())
    {
        cfg.region = r;
    }
    cfg.session_token = env("AWS_SESSION_TOKEN").filter(|v| !v.is_empty());
    let prefix = prefix.trim_matches('/');
    if !prefix.is_empty() {
        cfg.prefix = format!("{prefix}/");
    }
    Ok(cfg)
}

/// Open the backend named by `--backend`. S3 mode refuses providers that ignore conditional
/// writes (the CAS the whole design rests on) at startup.
pub fn open(url: &str) -> Result<Arc<dyn Backend>, String> {
    // sole-writer server: reads of a sharded manifest come from the snapshot this process keeps current
    let b = vcs_remote::sharded::ShardedBackend::from_arc(open_raw(url)?).with_read_ttl(std::time::Duration::from_secs(2));
    Ok(Arc::new(b))
}

/// The store itself, without the sharded-manifest layer (`migrate-sharded` needs the marker as it is).
pub fn open_raw(url: &str) -> Result<Arc<dyn Backend>, String> {
    if url.starts_with("s3://") {
        let cfg = s3_config(url, &|k| std::env::var(k).ok())?;
        let b = S3Backend::connect(cfg).map_err(|e| format!("cannot open {}: {e}", url.split('?').next().unwrap_or(url)))?;
        return Ok(Arc::new(b));
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    if path.contains("://") {
        return Err(format!("unsupported backend {url} (use s3://... or a directory)"));
    }
    let b = FsBackend::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    Ok(Arc::new(b))
}

/// Startup self-check for the "sole manifest writer" deployment (docs/deploy.md): with the
/// *client* credential (`VCS_CLIENT_ACCESS_KEY_ID` / `VCS_CLIENT_SECRET_ACCESS_KEY`, optional
/// `VCS_CLIENT_SESSION_TOKEN`) try to write the manifest of the `--backend` bucket. `Ok` only if
/// every write route is refused with 401/403; the manifest is never modified by the probe.
pub fn check_client_cannot_write_manifest(url: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    if !url.starts_with("s3://") {
        return Err("--check-client-cannot-write-manifest only applies to an s3:// backend".into());
    }
    let mapped = |k: &str| match k {
        "AWS_ACCESS_KEY_ID" => env("VCS_CLIENT_ACCESS_KEY_ID"),
        "AWS_SECRET_ACCESS_KEY" => env("VCS_CLIENT_SECRET_ACCESS_KEY"),
        "AWS_SESSION_TOKEN" => env("VCS_CLIENT_SESSION_TOKEN"),
        other => env(other),
    };
    let cfg = s3_config_with(url, &mapped, false).map_err(|e| {
        format!("{e} (the self-check reads the CLIENT credential from VCS_CLIENT_ACCESS_KEY_ID / VCS_CLIENT_SECRET_ACCESS_KEY)")
    })?;
    let c = S3Backend::new_unchecked(cfg).map_err(|e| e.to_string())?;
    match c.probe_manifest_write().map_err(|e| e.to_string())? {
        vcs_store_s3::ManifestWriteProbe::Denied => Ok(()),
        vcs_store_s3::ManifestWriteProbe::Allowed(how) => Err(format!(
            "the client credential can write the manifest ({how}); refusing to start. Attach a bucket policy that denies s3:PutObject on <prefix>manifest to every principal except the server's"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env(k: &str) -> Option<String> {
        match k {
            "AWS_ACCESS_KEY_ID" => Some("AK".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("SK".into()),
            _ => None,
        }
    }
    #[test]
    fn parses_s3_urls() {
        let c = s3_config("s3://bkt/a/b/?endpoint=http%3A%2F%2F127.0.0.1%3A9000&region=eu-1", &env).unwrap();
        assert_eq!((c.bucket.as_str(), c.prefix.as_str(), c.region.as_str()), ("bkt", "a/b/", "eu-1"));
        assert_eq!(c.endpoint, "http://127.0.0.1:9000");
    }
    #[test]
    fn credential_chain_and_client_options() {
        // static keys win over a configured container endpoint
        let both = |k: &str| match k {
            "AWS_CONTAINER_CREDENTIALS_FULL_URI" => Some("http://127.0.0.1:1/c".to_string()),
            other => env(other),
        };
        assert!(s3_config("s3://b?endpoint=http://h:1", &both).unwrap().credentials.is_none());
        // no keys: container endpoint, then instance metadata
        let container = |k: &str| (k == "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").then(|| "/v2/credentials/x".to_string());
        let c = s3_config("s3://b?endpoint=http://h:1&timeout=90&retries=7", &container).unwrap();
        assert!(
            format!("{:?}", c.credentials).contains("169.254.170.2/v2/credentials/x"),
            "{:?}",
            c.credentials
        );
        assert_eq!((c.timeout.as_secs(), c.retries), (90, 7));
        assert!(format!("{:?}", s3_config("s3://b?endpoint=http://h:1", &|_| None).unwrap().credentials).contains("169.254.169.254"));
        assert!(s3_config("s3://b?endpoint=http://h:1&timeout=0", &env).is_err());
        // the client-credential self-check never uses the chain
        assert!(s3_config_with("s3://b?endpoint=http://h:1", &container, false).is_err());
    }
    #[test]
    fn errors_never_echo_secrets() {
        let e = s3_config("s3://bkt?endpoint=http://h:1&secret=hunter2", &env).unwrap_err();
        assert!(e.contains("unknown option") && !e.contains("hunter2"), "{e}");
        let no_imds = |k: &str| (k == "AWS_EC2_METADATA_DISABLED").then(|| "true".to_string());
        assert!(s3_config("s3://bkt?endpoint=http://h:1", &no_imds)
            .unwrap_err()
            .contains("AWS_ACCESS_KEY_ID"));
        assert!(s3_config("s3://bkt", &env).unwrap_err().contains("missing endpoint"));
        assert!(s3_config("s3://?endpoint=http://h:1", &env).unwrap_err().contains("missing bucket"));
    }
}
