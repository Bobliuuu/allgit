//! S3-compatible implementation of [`vcs_remote::Backend`].
//!
//! # Dependency choice
//!
//! The `Backend` trait is synchronous and needs only six S3 operations (GET, ranged GET,
//! HEAD, conditional PUT, ListObjectsV2, DELETE). `aws-sdk-s3` would add hundreds of
//! transitive crates plus Tokio for that. Instead this crate hand-rolls AWS Signature V4
//! (about 60 lines on `sha2` + `hmac`) over blocking `ureq`, so there is no async runtime
//! at all and nothing Tokio-shaped can leak into the sync trait. `If-Match` /
//! `If-None-Match` are plain request headers and need no SDK support. The endpoint is
//! `http://` by default (MinIO, local gateways). With the `tls` cargo feature, `https://`
//! endpoints work through rustls with certificate verification always on; a failed
//! verification is a hard error. The default build has no TLS dependencies.
//!
//! # Semantics
//!
//! * create-only: `PUT` with `If-None-Match: *`; 412/409 means "already exists".
//! * CAS: `PUT` with `If-Match: <etag>`; 412/409/404 means precondition failed.
//! * version tag: the response `ETag` header, verbatim (quotes included).
//! * 5xx, 408, 429, timeouts and connection errors map to `BackendError::Unavailable`.
//! * [`S3Backend::connect`] probes the endpoint and refuses providers that ignore
//!   conditional writes.

mod admin;
pub use admin::ManifestWriteProbe;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vcs_remote::backend::validate_key;
use vcs_remote::{Backend, BackendError, Conditional, Versioned};

#[derive(Clone)]
pub struct S3Config {
    /// e.g. `http://127.0.0.1:9000` (path-style addressing).
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    /// Key prefix inside the bucket (no leading slash); empty for none.
    pub prefix: String,
    pub timeout: Duration,
    /// Temporary-credential session token (`AWS_SESSION_TOKEN`), sent as the signed
    /// `x-amz-security-token` header.
    pub session_token: Option<String>,
    /// PEM bundle of trusted root CAs for `https://` endpoints (feature `tls`). When set it
    /// replaces the built-in web roots, e.g. for MinIO with a private CA. Verification is
    /// never disabled.
    pub ca_pem: Option<Vec<u8>>,
}

/// Never prints the secret key, access key id or session token.
impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field("session_token", &self.session_token.as_ref().map(|_| "<redacted>"))
            .field("prefix", &self.prefix)
            .field("timeout", &self.timeout)
            .field("ca_pem", &self.ca_pem.as_ref().map(|p| p.len()))
            .finish()
    }
}

impl S3Config {
    pub fn new(endpoint: &str, bucket: &str, access_key: &str, secret_key: &str) -> Self {
        S3Config {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            prefix: String::new(),
            timeout: Duration::from_secs(30),
            session_token: None,
            ca_pem: None,
        }
    }

    /// Credentials from `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and the optional
    /// `AWS_SESSION_TOKEN` (empty counts as unset). `None` if the key pair is missing.
    pub fn from_env(endpoint: &str, bucket: &str) -> Option<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let mut cfg = Self::new(endpoint, bucket, &get("AWS_ACCESS_KEY_ID")?, &get("AWS_SECRET_ACCESS_KEY")?);
        cfg.session_token = get("AWS_SESSION_TOKEN");
        if let Some(r) = get("AWS_REGION").or_else(|| get("AWS_DEFAULT_REGION")) {
            cfg.region = r;
        }
        Some(cfg)
    }
}

/// Idle keep-alive connections kept per endpoint (reads, writes and listings share the pool; parallel
/// range fetches and uploads use several at once).
const POOL: usize = 16;

pub struct S3Backend {
    cfg: S3Config,
    host: String,
    /// Pooled agent shared by every request, writes included. ureq re-sends a request on a fresh
    /// connection only when a pooled one failed before the body went out, or when the request is
    /// idempotent with no body (GET/HEAD/DELETE); a conditional PUT whose answer is lost
    /// surfaces as `Unavailable` and is resolved by `RetryBackend`'s read-back, never replayed blind.
    agent: ureq::Agent,
    requests: AtomicU64,
    bytes_read: AtomicU64,
}

enum Resp {
    Ok {
        body: Vec<u8>,
        etag: Option<String>,
        /// HTTP status (200, 204, 206, 304 ...).
        status: u16,
    },
    /// Non-2xx, non-transient status that the caller interprets.
    Status(u16, String),
}

impl S3Backend {
    /// Build a client without contacting the server. Prefer [`S3Backend::connect`].
    pub fn new_unchecked(cfg: S3Config) -> Result<Self, BackendError> {
        let bad = || {
            BackendError::Other(format!(
                "endpoint must look like http://host:port or https://host:port, got {:?}",
                cfg.endpoint
            ))
        };
        let (https, rest) = match (cfg.endpoint.strip_prefix("http://"), cfg.endpoint.strip_prefix("https://")) {
            (Some(h), _) => (false, h),
            (_, Some(h)) => (true, h),
            _ => return Err(bad()),
        };
        if rest.is_empty() || rest.contains('/') {
            return Err(bad());
        }
        if https && !cfg!(feature = "tls") {
            return Err(BackendError::Other(
                "https:// endpoint requires building vcs-store-s3 with the `tls` feature".into(),
            ));
        }
        // The Host header omits the scheme's default port; sign exactly what is sent.
        let host = rest.strip_suffix(if https { ":443" } else { ":80" }).unwrap_or(rest).to_owned();
        // Every request reuses pooled connections (a new TCP/TLS handshake per request was the
        // dominant fixed cost).
        let mk = |idle: usize| -> Result<ureq::Agent, BackendError> {
            #[allow(unused_mut)]
            let mut builder = ureq::AgentBuilder::new()
                .timeout(cfg.timeout)
                // signed requests are bound to this host: never replay them (and the session token
                // header) at a Location chosen by the server
                .redirects(0)
                .max_idle_connections(idle)
                .max_idle_connections_per_host(idle);
            #[cfg(feature = "tls")]
            if https {
                if let Some(pem) = &cfg.ca_pem {
                    builder = builder.tls_config(std::sync::Arc::new(tls_config(pem)?));
                }
            }
            Ok(builder.build())
        };
        let agent = mk(POOL)?;
        Ok(S3Backend {
            cfg,
            host,
            agent,
            requests: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
        })
    }

    /// Build a client and verify the provider honours conditional writes; otherwise refuse.
    pub fn connect(cfg: S3Config) -> Result<Self, BackendError> {
        let b = Self::new_unchecked(cfg)?;
        b.probe()?;
        Ok(b)
    }

    /// Number of HTTP requests issued so far.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }
    /// Total response body bytes received so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// Create the bucket if it does not exist (convenience for tests and local setups).
    pub fn ensure_bucket(&self) -> Result<(), BackendError> {
        match self.send("PUT", "", &[], &[], b"")? {
            Resp::Ok { .. } => Ok(()),
            Resp::Status(409, _) => Ok(()), // already owned by us
            Resp::Status(c, m) => Err(BackendError::Other(format!("create bucket: {c} {m}"))),
        }
    }

    fn probe(&self) -> Result<(), BackendError> {
        let key = format!("{}.vcs-probe/{}", self.cfg.prefix, nonce());
        let refuse = |why: &str| {
            BackendError::Other(format!(
                "S3 provider does not honour conditional writes ({why}); refusing as authoritative backend"
            ))
        };
        let put = |extra: &[(&str, &str)]| self.send("PUT", &key, &[], extra, b"probe");
        let fail = |c: u16, m: String| BackendError::Other(format!("probe PUT failed: {c} {m}"));
        let result = (|| {
            let etag = match put(&[("if-none-match", "*")])? {
                Resp::Ok { etag: Some(e), .. } => e,
                Resp::Ok { .. } => return Err(refuse("no ETag returned")),
                Resp::Status(c, m) => return Err(fail(c, m)),
            };
            match put(&[("if-none-match", "*")])? {
                Resp::Status(412 | 409, _) => {}
                Resp::Ok { .. } => return Err(refuse("If-None-Match: * overwrote an existing key")),
                Resp::Status(c, m) => return Err(fail(c, m)),
            }
            match put(&[("if-match", "\"00000000000000000000000000000000\"")])? {
                Resp::Status(412 | 409, _) => {}
                Resp::Ok { .. } => return Err(refuse("If-Match with a stale ETag succeeded")),
                Resp::Status(c, m) => return Err(fail(c, m)),
            }
            match put(&[("if-match", etag.as_str())])? {
                Resp::Ok { .. } => Ok(()),
                Resp::Status(c, m) => Err(fail(c, m)),
            }
        })();
        // best-effort cleanup regardless of outcome
        let _ = self.send("DELETE", &key, &[], &[], b"");
        result
    }

    fn send(&self, method: &str, key: &str, query: &[(&str, String)], headers: &[(&str, &str)], body: &[u8]) -> Result<Resp, BackendError> {
        let mut path = format!("/{}", uri_encode(&self.cfg.bucket, true));
        if !key.is_empty() {
            path.push('/');
            path.push_str(&uri_encode(key, false));
        }
        let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, true), uri_encode(v, true))).collect();
        q.sort();
        let qs = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

        let amz_date = amz_timestamp();
        let date = &amz_date[..8];
        let payload_hash = hex(&Sha256::digest(body));
        let signed = signed_header_set(&self.host, &amz_date, &payload_hash, self.cfg.session_token.as_deref(), headers);
        let canon_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_names = signed.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let canon = format!("{method}\n{path}\n{qs}\n{canon_headers}\n{signed_names}\n{payload_hash}");
        let scope = format!("{date}/{}/s3/aws4_request", self.cfg.region);
        let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex(&Sha256::digest(canon.as_bytes())));
        let k = hmac(format!("AWS4{}", self.cfg.secret_key).as_bytes(), date.as_bytes());
        let k = hmac(&k, self.cfg.region.as_bytes());
        let k = hmac(&k, b"s3");
        let k = hmac(&k, b"aws4_request");
        let sig = hex(&hmac(&k, sts.as_bytes()));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_names}, Signature={sig}",
            self.cfg.access_key
        );

        let url = format!("{}{}{}{}", self.cfg.endpoint, path, if qs.is_empty() { "" } else { "?" }, qs);
        // Idempotent requests are repeated once if a pooled connection turns out to be dead.
        let safe = matches!(method, "GET" | "HEAD" | "DELETE");
        let agent = &self.agent;
        #[allow(clippy::result_large_err)] // ureq's own error type
        let attempt = || {
            let mut req = agent
                .request(method, &url)
                .set("x-amz-date", &amz_date)
                .set("x-amz-content-sha256", &payload_hash)
                .set("authorization", &auth);
            if let Some(t) = &self.cfg.session_token {
                req = req.set("x-amz-security-token", t);
            }
            for (k, v) in headers {
                req = req.set(k, v);
            }
            self.requests.fetch_add(1, Ordering::Relaxed);
            if method == "PUT" {
                req.send_bytes(body)
            } else {
                req.call()
            }
        };
        let mut res = attempt();
        if safe && matches!(res, Err(ureq::Error::Transport(_))) {
            // A pooled connection the server closed while idle fails like any transport error;
            // the request is safe to repeat, and the signature is still valid (same date).
            res = attempt();
        }
        match res {
            Ok(resp) => {
                let status = resp.status();
                let etag = resp.header("etag").map(str::to_owned);
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)
                    .map_err(|e| BackendError::Unavailable(format!("read body: {e}")))?;
                self.bytes_read.fetch_add(buf.len() as u64, Ordering::Relaxed);
                Ok(Resp::Ok { body: buf, etag, status })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                if code >= 500 || code == 408 || code == 429 {
                    Err(BackendError::Unavailable(format!("{method} {key}: HTTP {code}")))
                } else {
                    Ok(Resp::Status(code, text.chars().take(300).collect()))
                }
            }
            Err(ureq::Error::Transport(t)) => Err(BackendError::Unavailable(format!("{method} {key}: {t}"))),
        }
    }

    fn full_key(&self, key: &str) -> Result<String, BackendError> {
        validate_key(key)?;
        Ok(format!("{}{}", self.cfg.prefix, key))
    }

    fn unexpected(op: &str, c: u16, m: String) -> BackendError {
        BackendError::Other(format!("{op}: HTTP {c} {m}"))
    }
}

impl Backend for S3Backend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        let k = self.full_key(key)?;
        match self.send("GET", &k, &[], &[], b"")? {
            Resp::Ok { body, etag, .. } => {
                let tag = etag.ok_or_else(|| BackendError::Other("GET without ETag".into()))?;
                Ok(Some(Versioned { data: body, tag }))
            }
            Resp::Status(404, _) => Ok(None),
            Resp::Status(c, m) => Err(Self::unexpected("get", c, m)),
        }
    }

    /// `GET` with `If-None-Match: <etag>`: `304 Not Modified` carries no body.
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        let k = self.full_key(key)?;
        if have.contains(['\r', '\n']) {
            return Err(BackendError::Other("invalid tag".into()));
        }
        match self.send("GET", &k, &[], &[("if-none-match", have)], b"")? {
            Resp::Ok { status: 304, .. } => Ok(Conditional::NotModified),
            Resp::Ok { body, etag, .. } => {
                let tag = etag.ok_or_else(|| BackendError::Other("GET without ETag".into()))?;
                if tag == have {
                    // a server that ignores If-None-Match still sent the body; the tag says unchanged
                    return Ok(Conditional::NotModified);
                }
                Ok(Conditional::Modified(Versioned { data: body, tag }))
            }
            Resp::Status(404, _) => Ok(Conditional::Missing),
            Resp::Status(c, m) => Err(Self::unexpected("get_conditional", c, m)),
        }
    }

    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        let k = self.full_key(key)?;
        if len == 0 {
            return Ok(if self.exists(key)? { Some(Vec::new()) } else { None });
        }
        let range = format!("bytes={}-{}", offset, offset.saturating_add(len) - 1);
        match self.send("GET", &k, &[], &[("range", range.as_str())], b"")? {
            Resp::Ok { body, .. } => Ok(Some(body)),
            Resp::Status(404, _) => Ok(None),
            Resp::Status(416, _) => Ok(Some(Vec::new())), // offset past end
            Resp::Status(c, m) => Err(Self::unexpected("get_range", c, m)),
        }
    }

    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        let k = self.full_key(key)?;
        match self.send("HEAD", &k, &[], &[], b"")? {
            Resp::Ok { .. } => Ok(true),
            Resp::Status(404, _) => Ok(false),
            Resp::Status(c, m) => Err(Self::unexpected("exists", c, m)),
        }
    }

    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let k = self.full_key(key)?;
        match self.send("PUT", &k, &[], &[("if-none-match", "*")], data)? {
            Resp::Ok { .. } => Ok(true),
            Resp::Status(412 | 409, _) => Ok(false),
            Resp::Status(c, m) => Err(Self::unexpected("put_if_absent", c, m)),
        }
    }

    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let k = self.full_key(key)?;
        if tag.contains(['\r', '\n']) {
            return Err(BackendError::Other("invalid tag".into()));
        }
        match self.send("PUT", &k, &[], &[("if-match", tag)], data)? {
            Resp::Ok { etag, .. } => etag.map(Some).ok_or_else(|| BackendError::Other("PUT without ETag".into())),
            Resp::Status(412 | 409 | 404, _) => Ok(None),
            Resp::Status(c, m) => Err(Self::unexpected("put_if_match", c, m)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let full_prefix = format!("{}{}", self.cfg.prefix, prefix);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            pages += 1;
            if pages > MAX_LIST_PAGES {
                return Err(BackendError::Other("list: too many pages".into()));
            }
            let mut q = vec![("list-type", "2".to_owned()), ("prefix", full_prefix.clone())];
            if let Some(t) = &token {
                q.push(("continuation-token", t.clone()));
            }
            let body = match self.send("GET", "", &q, &[], b"")? {
                Resp::Ok { body, .. } => {
                    if body.len() > MAX_LIST_BODY {
                        return Err(BackendError::Other("list: response too large".into()));
                    }
                    String::from_utf8(body).map_err(|e| BackendError::Other(format!("list: {e}")))?
                }
                Resp::Status(c, m) => return Err(Self::unexpected("list", c, m)),
            };
            let page = parse_list_page(&body, &self.cfg.prefix)?;
            out.extend(page.keys);
            if out.len() > MAX_LIST_KEYS {
                return Err(BackendError::Other("list: too many keys".into()));
            }
            match page.next_token {
                None => break,
                // a server that repeats a token would loop us forever
                Some(t) if token.as_ref() == Some(&t) => {
                    return Err(BackendError::Other("list: continuation token did not advance".into()));
                }
                Some(t) => token = Some(t),
            }
        }
        out.sort();
        Ok(out)
    }

    /// `DeleteObject`. Unconditional: S3 has no `If-Match` on DELETE, so the GC handshake
    /// (docs/gc.md, G1) is what makes this safe, not the request. S3 answers 204 whether or
    /// not the key existed.
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        let k = self.full_key(key)?;
        match self.send("DELETE", &k, &[], &[], b"")? {
            Resp::Ok { .. } | Resp::Status(404, _) => Ok(()),
            Resp::Status(c, m) => Err(Self::unexpected("delete", c, m)),
        }
    }
    // `supports_gc` stays false: the S3 touch (server-side copy-to-self with
    // `x-amz-metadata-directive: REPLACE`), HEAD `Last-Modified` and a server clock (`Date`
    // header) are designed in docs/gc.md but not implemented, so remote GC refuses S3.
}

/// Largest ListObjectsV2 page body we parse (S3 pages hold at most 1000 keys of <= 1024 bytes).
const MAX_LIST_BODY: usize = 16 << 20;
const MAX_LIST_PAGES: usize = 100_000;
const MAX_LIST_KEYS: usize = 10_000_000;

/// One parsed ListObjectsV2 page; keys are relative to the configured prefix.
#[derive(Debug, PartialEq, Eq)]
pub struct ListPage {
    pub keys: Vec<String>,
    pub next_token: Option<String>,
}

/// Parse an untrusted ListObjectsV2 response. Never panics: a key that is not under `prefix`
/// (a lying or buggy server) is an error, not a slice out of bounds.
pub fn parse_list_page(xml: &str, prefix: &str) -> Result<ListPage, BackendError> {
    let mut keys = Vec::new();
    for k in tag_values(xml, "Key") {
        let Some(rel) = k.strip_prefix(prefix) else {
            return Err(BackendError::Other("list: key outside the requested prefix".into()));
        };
        keys.push(rel.to_owned());
    }
    let mut next_token = None;
    if tag_values(xml, "IsTruncated").first().map(String::as_str) == Some("true") {
        next_token = tag_values(xml, "NextContinuationToken").into_iter().next();
        if next_token.is_none() {
            return Err(BackendError::Other("list: truncated without continuation token".into()));
        }
    }
    Ok(ListPage { keys, next_token })
}

/// The headers that enter the SigV4 signature, sorted by name.
fn signed_header_set(
    host: &str,
    amz_date: &str,
    payload_hash: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut signed: Vec<(String, String)> = vec![
        ("host".into(), host.to_owned()),
        ("x-amz-content-sha256".into(), payload_hash.to_owned()),
        ("x-amz-date".into(), amz_date.to_owned()),
    ];
    if let Some(t) = token {
        signed.push(("x-amz-security-token".into(), t.to_owned()));
    }
    for (k, v) in headers {
        if *k == "range" {
            signed.push((k.to_string(), v.to_string()));
        }
    }
    signed.sort();
    signed
}

#[cfg(feature = "tls")]
fn tls_config(pem: &[u8]) -> Result<rustls::ClientConfig, BackendError> {
    use rustls::pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    let mut n = 0;
    for cert in rustls::pki_types::CertificateDer::pem_reader_iter(pem) {
        let cert = cert.map_err(|e| BackendError::Other(format!("invalid ca_pem: {e}")))?;
        roots
            .add(cert)
            .map_err(|e| BackendError::Other(format!("invalid CA certificate: {e}")))?;
        n += 1;
    }
    if n == 0 {
        return Err(BackendError::Other("ca_pem contains no certificates".into()));
    }
    Ok(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
}

/// All text values of `<tag>..</tag>` in a (flat, well-formed) S3 XML response.
fn tag_values(xml: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(unescape(&after[..j]));
        rest = &after[j + close.len()..];
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b'/' if !encode_slash => o.push('/'),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

fn nonce() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{t:x}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// `YYYYMMDDTHHMMSSZ` for now (UTC).
fn amz_timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_shape() {
        let t = amz_timestamp();
        assert_eq!(t.len(), 16);
        assert!(t.starts_with("20") && t.ends_with('Z') && &t[8..9] == "T");
    }

    #[test]
    fn signed_headers_include_token_only_when_present() {
        let names = |t| {
            signed_header_set("h", "d", "p", t, &[("range", "bytes=0-1")])
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>()
        };
        assert!(!names(None).contains(&"x-amz-security-token".to_owned()));
        let with = signed_header_set("h", "d", "p", Some("tok"), &[]);
        assert!(with.contains(&("x-amz-security-token".to_owned(), "tok".to_owned())));
        assert!(names(Some("tok")).windows(2).all(|w| w[0] <= w[1]), "sorted");
    }

    #[cfg(not(feature = "tls"))]
    #[test]
    fn https_without_feature_is_error() {
        let e = S3Backend::new_unchecked(S3Config::new("https://x:9000", "b", "a", "s"))
            .err()
            .unwrap();
        assert!(e.to_string().contains("tls"));
    }

    #[test]
    fn list_key_outside_prefix_is_an_error_not_a_panic() {
        // Regression: `k[prefix.len()..]` panicked on a key shorter than the prefix or one that
        // split a UTF-8 character; a lying server could crash the client.
        for xml in ["<Key>a</Key>", "<Key></Key>", "<Key>é</Key>", "<Key>other/x</Key>"] {
            assert!(parse_list_page(xml, "pre/fix/").is_err(), "{xml}");
        }
        assert!(parse_list_page("<Key>é</Key>", "\u{e9}x").is_err());
        let p = parse_list_page("<Key>p/a</Key><Key>p/b&amp;c</Key>", "p/").unwrap();
        assert_eq!(p.keys, vec!["a", "b&c"]);
        assert_eq!(p.next_token, None);
    }

    #[test]
    fn list_truncation_rules() {
        assert!(parse_list_page("<IsTruncated>true</IsTruncated>", "").is_err());
        let p = parse_list_page(
            "<IsTruncated>true</IsTruncated><NextContinuationToken>t&amp;1</NextContinuationToken>",
            "",
        )
        .unwrap();
        assert_eq!(p.next_token.as_deref(), Some("t&1"));
        assert_eq!(parse_list_page("<IsTruncated>false</IsTruncated>", "").unwrap().next_token, None);
    }

    #[test]
    fn list_parser_mutation_loop_never_panics() {
        let seed = "<ListBucketResult><IsTruncated>true</IsTruncated><Contents><Key>p/a&amp;b</Key></Contents><Contents><Key>p/c</Key></Contents><NextContinuationToken>tok</NextContinuationToken></ListBucketResult>";
        let tokens = [
            "<Key>",
            "</Key>",
            "<",
            ">",
            "&",
            "&amp;",
            "&lt;",
            "é",
            "\u{10000}",
            "<IsTruncated>true</IsTruncated>",
            "</",
            "\0",
        ];
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..200_000 {
            let mut s = seed.to_owned();
            for _ in 0..1 + next() % 4 {
                let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
                let at = cuts[(next() as usize) % cuts.len()];
                match next() % 3 {
                    0 => s.insert_str(at, tokens[(next() as usize) % tokens.len()]),
                    1 => s.truncate(at),
                    _ => {
                        if at < s.len() {
                            s.remove(at);
                        }
                    }
                }
            }
            let _ = parse_list_page(&s, "p/");
        }
    }

    proptest::proptest! {
        #[test]
        fn list_parser_never_panics_on_any_text(s in "\\PC{0,300}", prefix in "\\PC{0,6}") {
            let _ = parse_list_page(&s, &prefix);
            let _ = parse_list_page(&format!("<Key>{s}</Key>"), &prefix);
        }
    }

    #[test]
    fn encodes_and_parses() {
        assert_eq!(uri_encode("a b/c+d", false), "a%20b/c%2Bd");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(tag_values("<R><Key>a&amp;b</Key><Key>c</Key></R>", "Key"), vec!["a&b", "c"]);
    }
}
