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
mod creds;
mod gcops;
mod stats;
pub use admin::ManifestWriteProbe;
pub use creds::{CredentialProvider, Creds};
pub use gcops::parse_iso_ms;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
pub(crate) use stats::STATS;
pub use stats::{credentials_rejected, stats, S3Stats, METHODS};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vcs_remote::backend::validate_key;
use vcs_remote::{Backend, BackendError, Conditional, ObjectStream, Versioned};

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
    /// Extra attempts after the first for a throttled (503 SlowDown), 5xx, timed-out or dropped request (0 = none).
    /// Writes are retried only on 503, which S3 answers before it applies anything; an ambiguous failure of a
    /// conditional write (500, timeout) surfaces so the caller can read back instead of replaying blind.
    pub retries: u32,
    /// Backoff base: attempt `n` waits a random time in `[d/2, d]` with `d = min(retry_max, retry_base * 2^n)`;
    /// SlowDown waits four times longer.
    pub retry_base: Duration,
    pub retry_max: Duration,
    /// Request timeout grows with the body so a large pack still fits: `timeout + body / min_throughput`.
    /// Bytes per second; 0 = fixed `timeout`.
    pub min_throughput: u64,
    /// Refreshing credential source (container or instance role). When set it replaces `access_key`,
    /// `secret_key` and `session_token`.
    pub credentials: Option<std::sync::Arc<CredentialProvider>>,
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
            .field("retries", &self.retries)
            .field("credentials", &self.credentials)
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
            retries: 4,
            retry_base: Duration::from_millis(100),
            retry_max: Duration::from_secs(5),
            min_throughput: 4 << 20,
            credentials: None,
        }
    }

    /// `VCS_S3_TIMEOUT_SECS` (request timeout), `VCS_S3_RETRIES`, `VCS_S3_RETRY_BASE_MS`, `VCS_S3_RETRY_MAX_MS`.
    pub fn apply_tuning_env(&mut self, env: &dyn Fn(&str) -> Option<String>) {
        let num = |k: &str| env(k).and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(n) = num("VCS_S3_TIMEOUT_SECS").filter(|n| *n > 0) {
            self.timeout = Duration::from_secs(n);
        }
        if let Some(n) = num("VCS_S3_RETRIES") {
            self.retries = n.min(20) as u32;
        }
        if let Some(n) = num("VCS_S3_RETRY_BASE_MS").filter(|n| *n > 0) {
            self.retry_base = Duration::from_millis(n);
        }
        if let Some(n) = num("VCS_S3_RETRY_MAX_MS").filter(|n| *n > 0) {
            self.retry_max = Duration::from_millis(n);
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
        cfg.apply_tuning_env(&|k| std::env::var(k).ok());
        Some(cfg)
    }
}

/// Idle keep-alive connections kept per endpoint (reads, writes and listings share the pool; parallel
/// range fetches and uploads use several at once).
const POOL: usize = 16;

/// Objects at least this large go up as a multipart upload (see `put_multipart`). OFF by default: the upload logic is
/// covered by no test against a real service yet (the measurement run lost its credentials before it could be
/// validated). `VCS_S3_MULTIPART_MB=16` enables it from 16 MiB up, `https://` endpoints only; 0 or unset disables.
fn multipart_threshold() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(
        || match std::env::var("VCS_S3_MULTIPART_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()) {
            Some(0) | None => u64::MAX,
            Some(mb) => mb << 20,
        },
    )
}

/// Part size: 8 MiB (S3 minimum 5 MiB), larger only when the object would need more than 10,000 parts.
fn multipart_part_size(len: u64) -> u64 {
    (8u64 << 20).max(len.div_ceil(10_000))
}

/// How long a passed provider probe is trusted (see [`S3Backend::connect`]).
const PROBE_TTL: Duration = Duration::from_secs(24 * 3600);

fn probe_marker_fresh(m: &std::path::Path) -> bool {
    std::fs::metadata(m)
        .and_then(|md| md.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < PROBE_TTL)
}

pub struct S3Backend {
    cfg: S3Config,
    host: String,
    /// Pooled agent shared by every request, writes included. ureq re-sends a request on a fresh
    /// connection only when a pooled one failed before the body went out, or when the request is
    /// idempotent with no body (GET/HEAD/DELETE); a conditional PUT whose answer is lost
    /// surfaces as `Unavailable` and is resolved by `RetryBackend`'s read-back, never replayed blind.
    agent: ureq::Agent,
    requests: AtomicU64,
    bytes_read: Arc<AtomicU64>,
    /// Server-clock anchor for `now_ms` (see gcops.rs).
    anchor: gcops::Anchor,
    /// S3 Express One Zone directory bucket (name ends in `--x-s3`): virtual-hosted addressing, service
    /// `s3express`, and per-session credentials from CreateSession (see `session`).
    express: bool,
    session: std::sync::Mutex<Option<Session>>,
    /// Set by the probe when the provider creates a missing key on `PUT If-Match` instead of answering 404/412 (seen on a
    /// MinIO 2025-05 build; AWS S3 answers 404). `put_if_match` then looks the key up first, so a CAS on a missing key still
    /// fails. Not atomic against a concurrent create of that key, which only matters for keys that are deleted and recreated
    /// (the manifest never is). Unknown (false) when a passed probe is remembered for an https endpoint.
    cas_creates_missing: std::sync::atomic::AtomicBool,
}

/// CreateSession credentials of a directory bucket (valid 5 minutes; refreshed after [`SESSION_REFRESH`]).
struct Session {
    access_key: String,
    secret_key: String,
    token: String,
    fetched: std::time::Instant,
}
const SESSION_REFRESH: Duration = Duration::from_secs(240);

enum Resp {
    Ok {
        body: Vec<u8>,
        etag: Option<String>,
        /// HTTP status (200, 204, 206, 304 ...).
        status: u16,
        /// Declared `Content-Length` (what a HEAD reports for the object).
        len: Option<u64>,
        /// Unread body, only for a [`Mode::Stream`] request (then `body` is empty).
        reader: Option<Box<dyn std::io::Read + Send>>,
    },
    /// Non-2xx, non-transient status that the caller interprets.
    Status(u16, String),
}

/// How [`S3Backend::send_once`] treats a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Normal,
    /// CreateSession for an Express One Zone directory bucket (signed with the main credentials).
    CreateSession,
    /// Hand back the response body as a reader instead of buffering it. Carries the expected length, which scales
    /// the time allowance exactly as a request body of that size does.
    Stream(u64),
}

/// Body reader of a streamed GET: exactly `left` bytes or an error. A connection cut short must not look like a
/// complete object, and a server that sends more than it declared must not overrun the declared length.
struct ExactReader {
    inner: Box<dyn std::io::Read + Send>,
    left: u64,
    counter: Arc<AtomicU64>,
}
impl std::io::Read for ExactReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(self.left as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("S3 body ended {} bytes short", self.left),
            ));
        }
        self.left -= n as u64;
        self.counter.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailKind {
    /// Do not retry (a local error, or one the caller must interpret).
    Permanent,
    /// 503 / 429 / SlowDown: nothing was applied; back off harder.
    Slow,
    /// Other 5xx and 408.
    Server,
    /// Timeout, reset or short body.
    Transport,
}

/// A failed attempt: the error to surface plus how it may be retried.
struct Fail {
    err: BackendError,
    kind: FailKind,
    after: Option<Duration>,
}
impl Fail {
    fn new(kind: FailKind, err: BackendError) -> Self {
        Fail { err, kind, after: None }
    }
}
impl From<BackendError> for Fail {
    fn from(err: BackendError) -> Self {
        Fail::new(FailKind::Permanent, err)
    }
}

/// S3's answer to a signature made with expired temporary credentials.
fn expired_creds(body: &str) -> bool {
    [
        "ExpiredToken",
        "RequestExpired",
        "InvalidToken",
        "TokenRefreshRequired",
        "ExpiredTokenException",
    ]
    .iter()
    .any(|c| body.contains(c))
}

/// Cheap thread-safe jitter in `0..=max` (splitmix64 over an atomic counter).
fn jitter(max: u64) -> u64 {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let seed = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut z = CTR.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed).wrapping_add(seed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) % (max + 1)
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
        let mut host = rest.strip_suffix(if https { ":443" } else { ":80" }).unwrap_or(rest).to_owned();
        let express = cfg.bucket.ends_with("--x-s3");
        if express {
            // `s3://BUCKET--azid--x-s3?endpoint=https://s3express-azid.REGION.amazonaws.com`: the bucket is a host label
            host = format!("{}.{host}", cfg.bucket);
        }
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
            bytes_read: Arc::new(AtomicU64::new(0)),
            anchor: Default::default(),
            express,
            session: std::sync::Mutex::new(None),
            cas_creates_missing: Default::default(),
        })
    }

    /// Build a client and verify the provider honours conditional writes; otherwise refuse.
    pub fn connect(cfg: S3Config) -> Result<Self, BackendError> {
        let b = Self::new_unchecked(cfg)?;
        let marker = b.probe_marker();
        if marker.as_ref().is_some_and(|m| probe_marker_fresh(m)) {
            return Ok(b);
        }
        b.probe()?;
        if let Some(m) = marker {
            if let Some(d) = m.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&m, b"ok");
        }
        Ok(b)
    }

    /// The probe is five sequential requests (about 120 ms to real S3) and what it learns is a property of the
    /// provider, not of the repository, so a pass is remembered per (https endpoint, region, bucket) for
    /// [`PROBE_TTL`] in the client cache directory. Only for `https://` endpoints (real services; a local
    /// test double can change behaviour under the same address). `VCS_S3_PROBE=always` probes every time. A failed probe is never remembered.
    fn probe_marker(&self) -> Option<std::path::PathBuf> {
        if !self.cfg.endpoint.starts_with("https://") || std::env::var("VCS_S3_PROBE").is_ok_and(|v| v == "always") {
            return None;
        }
        let root: std::path::PathBuf = match std::env::var_os("VCS_CACHE_DIR").filter(|v| !v.is_empty()) {
            Some(d) => d.into(),
            None => match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
                Some(x) => std::path::PathBuf::from(x).join("vcs"),
                None => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".cache/vcs"),
            },
        };
        let h = hex(&Sha256::digest(
            format!("{}\n{}\n{}", self.cfg.endpoint, self.cfg.region, self.cfg.bucket).as_bytes(),
        ));
        Some(root.join("s3-probe").join(&h[..32]))
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
                Resp::Ok { .. } => {}
                Resp::Status(c, m) => return Err(fail(c, m)),
            }
            // a CAS on a key that does not exist must fail; some providers create it instead
            let missing = format!("{}.vcs-probe/{}", self.cfg.prefix, nonce());
            match self.send(
                "PUT",
                &missing,
                &[],
                &[("if-match", "\"00000000000000000000000000000000\"")],
                b"probe",
            )? {
                Resp::Status(404 | 412 | 409, _) => {}
                Resp::Ok { .. } => {
                    self.cas_creates_missing.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = self.send("DELETE", &missing, &[], &[], b"");
                }
                Resp::Status(c, m) => return Err(fail(c, m)),
            }
            Ok(())
        })();
        // best-effort cleanup regardless of outcome
        let _ = self.send("DELETE", &key, &[], &[], b"");
        result
    }

    /// `VCS_S3_TRACE=1`: one stderr line per request (`t_ms method key range ms status/bytes`), for measuring
    /// round trips; off by default (one cached env lookup).
    fn send(&self, method: &str, key: &str, query: &[(&str, String)], headers: &[(&str, &str)], body: &[u8]) -> Result<Resp, BackendError> {
        static TRACE: std::sync::OnceLock<Option<std::time::Instant>> = std::sync::OnceLock::new();
        let t0 = *TRACE.get_or_init(|| {
            std::env::var_os("VCS_S3_TRACE")
                .filter(|v| v != "0")
                .map(|_| std::time::Instant::now())
        });
        let Some(t0) = t0 else {
            return self.send_inner(method, key, query, headers, body, Mode::Normal);
        };
        let start = t0.elapsed();
        let s = std::time::Instant::now();
        let r = self.send_inner(method, key, query, headers, body, Mode::Normal);
        let out = match &r {
            Ok(Resp::Ok { body, status, .. }) => format!("{status}/{}", body.len()),
            Ok(Resp::Status(c, _)) => c.to_string(),
            Err(_) => "err".into(),
        };
        let range = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("range"))
            .map(|(_, v)| *v)
            .unwrap_or("-");
        eprintln!(
            "s3trace {:>7.1} {method} {key} {range} {:.1}ms {out} {:?}",
            start.as_secs_f64() * 1e3,
            s.elapsed().as_secs_f64() * 1e3,
            std::thread::current().id()
        );
        r
    }

    /// A GET whose body is handed back as a reader (retries apply until the headers arrive).
    fn send_stream(&self, key: &str, headers: &[(&str, &str)], expect_len: u64) -> Result<Resp, BackendError> {
        self.send_inner("GET", key, &[], headers, b"", Mode::Stream(expect_len))
    }

    /// CreateSession for a directory bucket; the credentials are cached and renewed every [`SESSION_REFRESH`].
    fn session_creds(&self) -> Result<(String, String, String), BackendError> {
        let mut g = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_ref().filter(|s| s.fetched.elapsed() < SESSION_REFRESH) {
            return Ok((s.access_key.clone(), s.secret_key.clone(), s.token.clone()));
        }
        let body = match self.send_inner(
            "GET",
            "",
            &[("session", String::new())],
            &[("x-amz-create-session-mode", "ReadWrite")],
            b"",
            Mode::CreateSession,
        )? {
            Resp::Ok { body, .. } => String::from_utf8_lossy(&body).into_owned(),
            Resp::Status(c, m) => return Err(Self::unexpected("CreateSession", c, m)),
        };
        let one = |t: &str| tag_values(&body, t).into_iter().next();
        let (Some(ak), Some(sk), Some(tok)) = (one("AccessKeyId"), one("SecretAccessKey"), one("SessionToken")) else {
            return Err(BackendError::Other("CreateSession response without credentials".into()));
        };
        *g = Some(Session {
            access_key: ak.clone(),
            secret_key: sk.clone(),
            token: tok.clone(),
            fetched: std::time::Instant::now(),
        });
        Ok((ak, sk, tok))
    }

    /// Static keys, or the provider chain's current (refreshed before expiry) credentials.
    fn main_creds(&self) -> Result<(String, String, Option<String>), BackendError> {
        match &self.cfg.credentials {
            Some(p) => {
                let c = p.current()?;
                Ok((c.access_key, c.secret_key, c.token))
            }
            None => Ok((
                self.cfg.access_key.clone(),
                self.cfg.secret_key.clone(),
                self.cfg.session_token.clone(),
            )),
        }
    }

    /// Wait before retry `n` (0-based): random in `[d/2, d]`, `d` doubling from `retry_base` up to `retry_max`
    /// (a throttled request backs off four times harder, up to twice the cap), never less than `Retry-After`.
    fn backoff(&self, n: u32, slow: bool, after: Option<Duration>) -> Duration {
        let (base, cap) = if slow {
            (self.cfg.retry_base * 4, self.cfg.retry_max * 2)
        } else {
            (self.cfg.retry_base, self.cfg.retry_max)
        };
        let d = base.saturating_mul(1u32 << n.min(16)).min(cap).as_millis() as u64;
        let wait = Duration::from_millis(d / 2 + jitter(d / 2));
        after.map_or(wait, |a| wait.max(a.min(Duration::from_secs(30))))
    }

    /// One logical request: [`Self::send_once`] plus bounded, jittered retries. 503 SlowDown and 429 are retried
    /// for every method (S3 has applied nothing); 5xx, timeouts and dropped connections only for requests that
    /// are safe to repeat (GET, HEAD, DELETE). A 403 for expired credentials refreshes the provider once.
    fn send_inner(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, String)],
        headers: &[(&str, &str)],
        body: &[u8],
        mode: Mode,
    ) -> Result<Resp, BackendError> {
        let mut attempt = 0u32;
        let mut refreshed = false;
        loop {
            let f = match self.send_once(method, key, query, headers, body, mode) {
                Ok(Resp::Status(403, m)) if !refreshed && self.cfg.credentials.is_some() && expired_creds(&m) => {
                    refreshed = true;
                    if let Some(p) = &self.cfg.credentials {
                        p.invalidate();
                    }
                    continue;
                }
                Ok(r) => return Ok(r),
                Err(f) => f,
            };
            let retryable = match f.kind {
                FailKind::Permanent => false,
                FailKind::Slow => true,
                FailKind::Server | FailKind::Transport => matches!(method, "GET" | "HEAD" | "DELETE"),
            };
            if !retryable || attempt >= self.cfg.retries {
                return Err(f.err);
            }
            std::thread::sleep(self.backoff(attempt, f.kind == FailKind::Slow, f.after));
            attempt += 1;
            STATS.retries.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn send_once(
        &self,
        method: &str,
        key: &str,
        query: &[(&str, String)],
        headers: &[(&str, &str)],
        body: &[u8],
        mode: Mode,
    ) -> Result<Resp, Fail> {
        let mut path = if self.express {
            String::new()
        } else {
            format!("/{}", uri_encode(&self.cfg.bucket, true))
        };
        if !key.is_empty() {
            path.push('/');
            path.push_str(&uri_encode(key, false));
        }
        if path.is_empty() {
            path.push('/');
        }
        let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, true), uri_encode(v, true))).collect();
        q.sort();
        let qs = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

        // (access key, secret, token, token header, SigV4 service)
        let (access_key, secret_key, token, token_header, service): (String, String, Option<String>, &str, &str) =
            match (self.express, mode == Mode::CreateSession) {
                (false, _) => {
                    let (a, s, t) = self.main_creds()?;
                    (a, s, t, "x-amz-security-token", "s3")
                }
                (true, true) => {
                    let (a, s, t) = self.main_creds()?;
                    (a, s, t, "x-amz-security-token", "s3express")
                }
                (true, false) => {
                    let (a, s, t) = self.session_creds()?;
                    (a, s, Some(t), "x-amz-s3session-token", "s3express")
                }
            };
        let amz_date = amz_timestamp();
        let date = &amz_date[..8];
        let payload_hash = hex(&Sha256::digest(body));
        let signed = signed_header_set(&self.host, &amz_date, &payload_hash, token.as_deref(), token_header, headers);
        let canon_headers: String = signed.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let signed_names = signed.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let canon = format!("{method}\n{path}\n{qs}\n{canon_headers}\n{signed_names}\n{payload_hash}");
        let scope = format!("{date}/{}/{service}/aws4_request", self.cfg.region);
        let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", hex(&Sha256::digest(canon.as_bytes())));
        let k = hmac(format!("AWS4{secret_key}").as_bytes(), date.as_bytes());
        let k = hmac(&k, self.cfg.region.as_bytes());
        let k = hmac(&k, service.as_bytes());
        let k = hmac(&k, b"aws4_request");
        let sig = hex(&hmac(&k, sts.as_bytes()));
        let auth = format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_names}, Signature={sig}");

        let base = if self.express {
            format!(
                "{}://{}",
                if self.cfg.endpoint.starts_with("https://") {
                    "https"
                } else {
                    "http"
                },
                self.host
            )
        } else {
            self.cfg.endpoint.clone()
        };
        let url = format!("{base}{path}{}{qs}", if qs.is_empty() { "" } else { "?" });
        // Idempotent requests are repeated once if a pooled connection turns out to be dead.
        let safe = matches!(method, "GET" | "HEAD" | "DELETE");
        let agent = &self.agent;
        #[allow(clippy::result_large_err)] // ureq's own error type
        // a large body gets proportionally more time (a fixed 30 s cannot move a 256 MiB pack below 70 Mbit/s)
        let scaled = match mode {
            Mode::Stream(n) => n,
            _ => body.len() as u64,
        };
        let timeout = if self.cfg.min_throughput > 0 {
            self.cfg.timeout + Duration::from_secs(scaled / self.cfg.min_throughput)
        } else {
            self.cfg.timeout
        };
        let _inflight = stats::Inflight::new(body.len() as u64);
        let attempt = || {
            let mut req = agent
                .request(method, &url)
                .timeout(timeout)
                .set("x-amz-date", &amz_date)
                .set("x-amz-content-sha256", &payload_hash)
                .set("authorization", &auth);
            if let Some(t) = &token {
                req = req.set(token_header, t);
            }
            for (k, v) in headers {
                req = req.set(k, v);
            }
            self.requests.fetch_add(1, Ordering::Relaxed);
            STATS.requests[stats::method_index(method)].fetch_add(1, Ordering::Relaxed);
            if method == "PUT" || method == "POST" {
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
                let declared = resp.header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
                if matches!(mode, Mode::Stream(_)) {
                    STATS.creds_rejected.store(false, Ordering::Relaxed);
                    return Ok(Resp::Ok { body: Vec::new(), etag, status, len: declared, reader: Some(resp.into_reader()) });
                }
                // Presize from Content-Length: growing by doubling copies a pack range several times and
                // leaves up to 2x capacity resident. Capped so a lying server cannot force a huge
                // reservation (untouched capacity costs no RSS; the cap bounds address space).
                let want = resp
                    .header("content-length")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(256 << 20);
                let _resp_bytes = stats::Inflight::new(want as u64);
                let mut buf = Vec::with_capacity(want);
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)
                    .map_err(|e| Fail::new(FailKind::Transport, BackendError::Unavailable(format!("read body: {e}"))))?;
                self.bytes_read.fetch_add(buf.len() as u64, Ordering::Relaxed);
                STATS.creds_rejected.store(false, Ordering::Relaxed);
                Ok(Resp::Ok { body: buf, etag, status, len: declared, reader: None })
            }
            Err(ureq::Error::Status(code, resp)) => {
                let after = resp
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let text = resp.into_string().unwrap_or_default();
                let throttled = code == 429 || text.contains("SlowDown") || text.contains("Slow Down") || text.contains("Throttl");
                if throttled {
                    STATS.slowdown.fetch_add(1, Ordering::Relaxed);
                }
                if code == 409 {
                    STATS.conflict_409.fetch_add(1, Ordering::Relaxed);
                }
                if code >= 500 || code == 408 || code == 429 {
                    if code >= 500 {
                        STATS.status_5xx.fetch_add(1, Ordering::Relaxed);
                    }
                    let tag = if throttled { " SlowDown" } else { "" };
                    let kind = if throttled || code == 503 {
                        FailKind::Slow
                    } else {
                        FailKind::Server
                    };
                    let mut f = Fail::new(kind, BackendError::Unavailable(format!("{method} {key}: HTTP {code}{tag}")));
                    f.after = after;
                    Err(f)
                } else {
                    if code == 403 {
                        // HEAD answers carry no body, so a 403 there cannot name ExpiredToken; /readyz probes with HEAD
                        if method == "HEAD" || expired_creds(&text) {
                            STATS.creds_rejected.store(true, Ordering::Relaxed);
                        }
                    } else {
                        STATS.creds_rejected.store(false, Ordering::Relaxed);
                    }
                    Ok(Resp::Status(code, text.chars().take(300).collect()))
                }
            }
            Err(ureq::Error::Transport(t)) => {
                let msg = t.to_string();
                if msg.contains("timed out") || msg.contains("timeout") {
                    STATS.timeouts.fetch_add(1, Ordering::Relaxed);
                }
                Err(Fail::new(
                    FailKind::Transport,
                    BackendError::Unavailable(format!("{method} {key}: {msg}")),
                ))
            }
        }
    }

    /// Create-only upload of a large object as a multipart upload with parts on up to [`POOL`] connections.
    /// Measured from a c6i.4xlarge in the bucket's region: one stream moves about 95 MB/s in either direction,
    /// 16 parallel parts about 510 MB/s (docs/research/real-s3.md), so a 48 MiB pack drops from ~500 to ~120 ms.
    /// The create-only guarantee is `If-None-Match: *` on CompleteMultipartUpload (S3 honours it; a provider that
    /// does not is refused by the connect probe only for plain PUT, so multipart is skipped unless the endpoint
    /// is https, i.e. a real service). Any failure aborts the upload so no parts are left billed.
    fn put_multipart(&self, k: &str, data: &[u8]) -> Result<bool, BackendError> {
        let part = multipart_part_size(data.len() as u64) as usize;
        let parts: Vec<&[u8]> = data.chunks(part).collect();
        let created = match self.send("POST", k, &[("uploads", String::new())], &[], b"")? {
            Resp::Ok { body, .. } => String::from_utf8_lossy(&body).into_owned(),
            Resp::Status(c, m) => return Err(Self::unexpected("create multipart upload", c, m)),
        };
        let Some(upload_id) = tag_values(&created, "UploadId").into_iter().next() else {
            return Err(BackendError::Other("CreateMultipartUpload returned no UploadId".into()));
        };
        let abort = || {
            let _ = self.send("DELETE", k, &[("uploadId", upload_id.clone())], &[], b"");
        };
        let next = std::sync::atomic::AtomicUsize::new(0);
        let etags: std::sync::Mutex<Vec<Option<String>>> = std::sync::Mutex::new(vec![None; parts.len()]);
        let failed: std::sync::Mutex<Option<BackendError>> = std::sync::Mutex::new(None);
        std::thread::scope(|sc| {
            for _ in 0..POOL.min(parts.len()) {
                sc.spawn(|| loop {
                    if failed.lock().unwrap().is_some() {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(chunk) = parts.get(i) else { break };
                    let q = [("partNumber", (i + 1).to_string()), ("uploadId", upload_id.clone())];
                    let r = match self.send("PUT", k, &q, &[], chunk) {
                        Ok(Resp::Ok { etag: Some(e), .. }) => Ok(e),
                        Ok(Resp::Ok { .. }) => Err(BackendError::Other("UploadPart without ETag".into())),
                        Ok(Resp::Status(c, m)) => Err(Self::unexpected("upload part", c, m)),
                        Err(e) => Err(e),
                    };
                    match r {
                        Ok(e) => etags.lock().unwrap()[i] = Some(e),
                        Err(e) => {
                            failed.lock().unwrap().get_or_insert(e);
                            break;
                        }
                    }
                });
            }
        });
        if let Some(e) = failed.into_inner().unwrap() {
            abort();
            return Err(e);
        }
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (i, e) in etags.into_inner().unwrap().into_iter().enumerate() {
            let e = e.expect("every part uploaded");
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                i + 1,
                e.replace('"', "&quot;")
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let done = self.send(
            "POST",
            k,
            &[("uploadId", upload_id.clone())],
            &[("if-none-match", "*")],
            xml.as_bytes(),
        );
        match done {
            // S3 can answer 200 and put the failure in the body
            Ok(Resp::Ok { body, .. }) if String::from_utf8_lossy(&body).contains("<Error>") => {
                abort();
                Err(BackendError::Unavailable(format!("CompleteMultipartUpload {k}: error in 200 body")))
            }
            Ok(Resp::Ok { .. }) => Ok(true),
            Ok(Resp::Status(412 | 409, _)) => {
                abort();
                Ok(false)
            }
            Ok(Resp::Status(c, m)) => {
                abort();
                Err(Self::unexpected("complete multipart upload", c, m))
            }
            Err(e) => {
                abort();
                Err(e)
            }
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

    /// HEAD for length and ETag, then one un-ranged GET pinned to that ETag (`If-Match`) whose body is streamed.
    /// The tag is the ETag S3 returned, so there is no hash pass. If the object changed between the two requests
    /// (412 or a different ETag) or vanished, the answer is `Ok(None)` and the caller falls back to a whole `get`.
    fn get_stream(&self, key: &str, min_len: u64) -> Result<Option<ObjectStream>, BackendError> {
        let k = self.full_key(key)?;
        let (len, tag) = match self.send("HEAD", &k, &[], &[], b"")? {
            Resp::Ok { len: Some(len), etag: Some(tag), .. } => (len, tag),
            Resp::Ok { .. } => return Ok(None),
            Resp::Status(404, _) => return Ok(None),
            Resp::Status(c, m) => return Err(Self::unexpected("get_stream", c, m)),
        };
        if len < min_len || tag.contains(['\r', '\n']) {
            return Ok(None);
        }
        match self.send_stream(&k, &[("if-match", tag.as_str())], len)? {
            Resp::Ok { reader: Some(inner), etag, len: got, .. } if etag.as_deref() == Some(tag.as_str()) && got == Some(len) => {
                let reader = ExactReader { inner, left: len, counter: self.bytes_read.clone() };
                Ok(Some(ObjectStream { len, tag, reader: Box::new(reader) }))
            }
            // replaced between HEAD and GET, or deleted: let the caller read the current object whole
            Resp::Ok { .. } | Resp::Status(404 | 412, _) => Ok(None),
            Resp::Status(c, m) => Err(Self::unexpected("get_stream", c, m)),
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
        if data.len() as u64 >= multipart_threshold() && self.cfg.endpoint.starts_with("https://") {
            return self.put_multipart(&k, data);
        }
        match self.send("PUT", &k, &[], &[("if-none-match", "*")], data)? {
            Resp::Ok { .. } => Ok(true),
            Resp::Status(412 | 409, _) => {
                STATS.cas_losses.fetch_add(1, Ordering::Relaxed);
                Ok(false)
            }
            Resp::Status(c, m) => Err(Self::unexpected("put_if_absent", c, m)),
        }
    }

    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let k = self.full_key(key)?;
        if tag.contains(['\r', '\n']) {
            return Err(BackendError::Other("invalid tag".into()));
        }
        if self.cas_creates_missing.load(std::sync::atomic::Ordering::Relaxed) && !self.exists(key)? {
            return Ok(None);
        }
        match self.send("PUT", &k, &[], &[("if-match", tag)], data)? {
            Resp::Ok { etag, .. } => etag.map(Some).ok_or_else(|| BackendError::Other("PUT without ETag".into())),
            Resp::Status(412 | 409 | 404, _) => {
                STATS.cas_losses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
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
    /// Remote GC support (gcops.rs): touch = server-side copy onto itself, ages from
    /// `LastModified`, clock = the server's `LastModified` of `gc/clock`, anchored.
    /// Directory buckets reject the in-place CopyObject (`x-amz-metadata-directive: REPLACE`) that `touch` and the
    /// server clock use ("Request must be signed with AWS Credentials", measured), so remote GC is not offered there.
    fn supports_gc(&self) -> bool {
        !self.express
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.gc_now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.gc_modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.gc_touch(key)
    }
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
    token_header: &str,
    headers: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut signed: Vec<(String, String)> = vec![
        ("host".into(), host.to_owned()),
        ("x-amz-content-sha256".into(), payload_hash.to_owned()),
        ("x-amz-date".into(), amz_date.to_owned()),
    ];
    if let Some(t) = token {
        signed.push((token_header.into(), t.to_owned()));
    }
    for (k, v) in headers {
        if *k == "range" || k.starts_with("x-amz-") {
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
    fn directory_bucket_is_virtual_hosted_and_has_no_gc() {
        let c = S3Config::new("http://s3express-use1-az4.us-east-1.amazonaws.com", "b--use1-az4--x-s3", "k", "s");
        let b = S3Backend::new_unchecked(c).unwrap();
        assert!(b.express);
        assert_eq!(b.host, "b--use1-az4--x-s3.s3express-use1-az4.us-east-1.amazonaws.com");
        assert!(!b.supports_gc());
        let g = S3Backend::new_unchecked(S3Config::new("http://127.0.0.1:9000", "plain", "k", "s")).unwrap();
        assert!(!g.express && g.supports_gc() && g.host == "127.0.0.1:9000");
    }

    #[test]
    fn probe_marker_freshness() {
        let d = std::env::temp_dir().join(format!("vcs-probe-marker-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let m = d.join("m");
        assert!(!probe_marker_fresh(&m), "missing marker is not fresh");
        std::fs::write(&m, b"ok").unwrap();
        assert!(probe_marker_fresh(&m));
        let old = std::fs::File::options().write(true).open(&m).unwrap();
        old.set_modified(SystemTime::now() - PROBE_TTL - Duration::from_secs(5)).unwrap();
        assert!(!probe_marker_fresh(&m), "expired marker is not fresh");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timestamp_shape() {
        let t = amz_timestamp();
        assert_eq!(t.len(), 16);
        assert!(t.starts_with("20") && t.ends_with('Z') && &t[8..9] == "T");
    }

    #[test]
    fn signed_headers_include_token_only_when_present() {
        let names = |t| {
            signed_header_set("h", "d", "p", t, "x-amz-security-token", &[("range", "bytes=0-1")])
                .into_iter()
                .map(|(k, _)| k)
                .collect::<Vec<_>>()
        };
        assert!(!names(None).contains(&"x-amz-security-token".to_owned()));
        let with = signed_header_set("h", "d", "p", Some("tok"), "x-amz-security-token", &[]);
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
