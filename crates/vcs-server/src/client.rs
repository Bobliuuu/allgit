//! `HttpBackend`: a [`Backend`] that talks to `vcs-server`. Token comes from the caller
//! (the CLI reads `VCS_TOKEN`); it is never part of the URL. `https://` needs the `tls` cargo
//! feature (certificate verification is always on; [`HttpBackend::with_ca`] trusts a given CA bundle
//! instead of the built-in web roots). Without it, only `http://`: use loopback, a private network
//! or a TLS proxy.

use crate::http::pct_encode;
use std::time::Duration;
use vcs_remote::backend::validate_key;
use vcs_remote::{Backend, BackendError, Conditional, Versioned};

/// Idle keep-alive connections kept per server (parallel range fetches use several at once).
const POOL: usize = 16;

/// Never prints the token.
impl std::fmt::Debug for HttpBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpBackend")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub struct HttpBackend {
    base: String,
    token: String,
    agent: ureq::Agent,
    gc: std::sync::Mutex<GcClock>,
}

/// What this client knows about the server's clock endpoint.
#[derive(Clone, Copy)]
enum GcClock {
    Unprobed,
    Unsupported,
    Anchored(u64, std::time::Instant),
}

const REANCHOR: Duration = Duration::from_secs(600);

impl HttpBackend {
    /// `base` is `http://host:port` or (feature `tls`) `https://host:port`; an optional trailing slash is ignored.
    pub fn new(base: &str, token: &str) -> Result<Self, BackendError> {
        Self::with_ca(base, token, None)
    }

    /// Like [`new`](Self::new); for `https://`, `ca_pem` (if given) is the only trust root.
    pub fn with_ca(base: &str, token: &str, ca_pem: Option<&[u8]>) -> Result<Self, BackendError> {
        Self::with_tls(base, token, ca_pem, None)
    }

    /// Like [`with_ca`](Self::with_ca), additionally presenting `(cert_pem, key_pem)` as the client
    /// certificate (mutual TLS). A client certificate requires `ca_pem`.
    pub fn with_tls(base: &str, token: &str, ca_pem: Option<&[u8]>, identity: Option<(&[u8], &[u8])>) -> Result<Self, BackendError> {
        let base = base.trim_end_matches('/');
        #[allow(unused_mut)]
        let mut b = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .max_idle_connections(POOL)
            .max_idle_connections_per_host(POOL);
        if base.starts_with("https://") {
            #[cfg(feature = "tls")]
            match (ca_pem, identity) {
                (Some(pem), Some((c, k))) => {
                    b = b.tls_config(
                        crate::tls::client_config_with_identity(pem, c, k)
                            .map_err(|e| BackendError::Other(format!("invalid ca bundle or client certificate: {e}")))?,
                    );
                }
                (None, Some(_)) => {
                    return Err(BackendError::Other(
                        "a client certificate needs a CA bundle too (remote.cainfo): the server certificate is always verified".into(),
                    ))
                }
                (Some(pem), None) => {
                    b = b.tls_config(crate::tls::client_config(pem).map_err(|e| BackendError::Other(format!("invalid ca bundle: {e}")))?);
                }
                (None, None) => {}
            }
            #[cfg(not(feature = "tls"))]
            {
                let _ = (ca_pem, identity);
                return Err(BackendError::Other(
                    "https:// needs vcs-server built with the `tls` feature (or use a TLS proxy and http://)".into(),
                ));
            }
        } else if base.starts_with("http://") {
            if ca_pem.is_some() || identity.is_some() {
                return Err(BackendError::Other(
                    "a CA bundle or client certificate only applies to https:// URLs".into(),
                ));
            }
        } else {
            return Err(BackendError::Other(
                "HttpBackend supports http:// and (with the tls feature) https:// only".into(),
            ));
        }
        Ok(Self {
            base: base.to_owned(),
            token: token.to_owned(),
            agent: b.build(),
            gc: std::sync::Mutex::new(GcClock::Unprobed),
        })
    }

    /// Replace every occurrence of the token (and its header form) in `s`.
    fn redact(&self, s: &str) -> String {
        if self.token.is_empty() {
            return s.to_owned();
        }
        s.replace(&self.token, "<redacted>")
    }

    /// Map a non-success client-error status to a message; 401/403 say what to do.
    fn status_err(&self, code: u16, msg: &str) -> BackendError {
        let detail = self.redact(msg.trim());
        let text = match code {
            401 => format!(
                "authentication failed (401): the server rejected the token ({detail}); it may be missing, expired or revoked. Run `vcs login {}` to store a new one",
                self.base
            ),
            403 => format!(
                "permission denied (403): this token's role or zone scope does not allow the operation ({detail}). Ask the server admin for a token with the needed role/zones, then `vcs login {}`",
                self.base
            ),
            _ => format!("server returned {code}: {detail}"),
        };
        BackendError::Other(text)
    }

    fn req(&self, method: &str, path_q: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}{path_q}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
    }

    /// `GET /v1/whoami` text: `<principal> <role> <zones>`.
    pub fn whoami(&self) -> Result<String, BackendError> {
        let r = self.send(self.req("GET", "/v1/whoami"), None)?;
        Ok(r.0.trim().to_owned())
    }

    /// Returns (body text lossy, bytes, etag) or maps statuses: 404/412 are returned as Err(status).
    fn send(&self, r: ureq::Request, body: Option<&[u8]>) -> Result<(String, Vec<u8>, Option<String>), BackendError> {
        match self.send_raw(r, body)? {
            Ok(x) => Ok(x),
            Err((code, msg)) => Err(self.status_err(code, &msg)),
        }
    }

    #[allow(clippy::type_complexity)]
    fn send_raw(
        &self,
        r: ureq::Request,
        body: Option<&[u8]>,
    ) -> Result<Result<(String, Vec<u8>, Option<String>), (u16, String)>, BackendError> {
        let res = match body {
            Some(b) => r.send_bytes(b),
            None => r.call(),
        };
        match res {
            Ok(resp) => {
                let etag = resp
                    .header("ETag")
                    .map(|e| e.strip_prefix('"').and_then(|e| e.strip_suffix('"')).unwrap_or(e).to_owned());
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut data).map_err(|e| BackendError::Unavailable(e.to_string()))?;
                Ok(Ok((String::from_utf8_lossy(&data).into_owned(), data, etag)))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let msg = resp.into_string().unwrap_or_default();
                if code >= 500 || code == 429 {
                    Err(BackendError::Unavailable(
                        self.redact(&format!("server returned {code}: {}", msg.trim())),
                    ))
                } else {
                    Ok(Err((code, msg)))
                }
            }
            Err(ureq::Error::Transport(t)) => Err(BackendError::Unavailable(self.redact(&t.to_string()))),
        }
    }

    fn mtime_path(key: &str) -> Result<String, BackendError> {
        validate_key(key)?;
        Ok(format!("/v1/mtime/{}", pct_encode(key, true)))
    }

    /// The anchor is the instant the request was SENT, so the estimate can only run ahead of the server by
    /// about one round trip, never behind a time the server already stamped.
    /// The server clock now (anchored read, see [`Backend::now_ms`]), or `None` if unsupported.
    fn gc_anchor(&self) -> Option<u64> {
        let cur = *self.gc.lock().unwrap_or_else(|e| e.into_inner());
        match cur {
            GcClock::Unsupported => return None,
            GcClock::Anchored(ms, at) if at.elapsed() < REANCHOR => return Some(ms + at.elapsed().as_millis() as u64),
            _ => {}
        }
        let before = std::time::Instant::now();
        let read = self.send_raw(self.req("GET", "/v1/clock"), None);
        let next = match read {
            Ok(Ok((t, _, _))) => match t.trim().parse::<u64>() {
                Ok(ms) => GcClock::Anchored(ms, before),
                Err(_) => GcClock::Unsupported,
            },
            // old server (404), unsupported backend (501), or no permission: not a GC remote
            Ok(Err(_)) => GcClock::Unsupported,
            // a transient failure must not be cached as "unsupported"
            Err(_) => return None,
        };
        *self.gc.lock().unwrap_or_else(|e| e.into_inner()) = next;
        match next {
            GcClock::Anchored(ms, at) => Some(ms + at.elapsed().as_millis() as u64),
            _ => None,
        }
    }

    fn key_path(key: &str) -> Result<String, BackendError> {
        validate_key(key)?;
        Ok(format!("/v1/o/{}", pct_encode(key, true)))
    }
}

impl Backend for HttpBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        match self.send_raw(self.req("GET", &Self::key_path(key)?), None)? {
            Ok((_, data, etag)) => Ok(Some(Versioned {
                data,
                tag: etag.ok_or_else(|| BackendError::Other("server sent no ETag".into()))?,
            })),
            Err((404, _)) => Ok(None),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }

    /// `GET` with `If-None-Match: "<tag>"`; the server answers `304` with no body when equal.
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        let r = self
            .req("GET", &Self::key_path(key)?)
            .set("If-None-Match", &format!("\"{}\"", have.replace('"', "")));
        let res = r.call();
        match res {
            Ok(resp) if resp.status() == 304 => Ok(Conditional::NotModified),
            Ok(resp) => {
                let etag = resp
                    .header("ETag")
                    .map(|e| e.strip_prefix('"').and_then(|e| e.strip_suffix('"')).unwrap_or(e).to_owned());
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut data).map_err(|e| BackendError::Unavailable(e.to_string()))?;
                Ok(Conditional::Modified(Versioned {
                    data,
                    tag: etag.ok_or_else(|| BackendError::Other("server sent no ETag".into()))?,
                }))
            }
            Err(ureq::Error::Status(404, _)) => Ok(Conditional::Missing),
            Err(ureq::Error::Status(code, resp)) => {
                let msg = resp.into_string().unwrap_or_default();
                if code >= 500 || code == 429 {
                    Err(BackendError::Unavailable(
                        self.redact(&format!("server returned {code}: {}", msg.trim())),
                    ))
                } else {
                    Err(self.status_err(code, &msg))
                }
            }
            Err(ureq::Error::Transport(t)) => Err(BackendError::Unavailable(self.redact(&t.to_string()))),
        }
    }

    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        let p = format!("{}?offset={offset}&len={len}", Self::key_path(key)?);
        match self.send_raw(self.req("GET", &p), None)? {
            Ok((_, data, _)) => Ok(Some(data)),
            Err((404, _)) => Ok(None),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }

    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        match self.send_raw(self.req("HEAD", &Self::key_path(key)?), None)? {
            Ok(_) => Ok(true),
            Err((404, _)) => Ok(false),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }

    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let r = self.req("PUT", &Self::key_path(key)?).set("If-None-Match", "*");
        match self.send_raw(r, Some(data))? {
            Ok(_) => Ok(true),
            Err((412, _)) => Ok(false),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }

    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let r = self.req("PUT", &Self::key_path(key)?).set("If-Match", &format!("\"{tag}\""));
        match self.send_raw(r, Some(data))? {
            Ok((_, _, etag)) => Ok(Some(etag.ok_or_else(|| BackendError::Other("server sent no ETag".into()))?)),
            Err((412, _)) => Ok(None),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let r = self.send(self.req("GET", &format!("/v1/list?prefix={}", pct_encode(prefix, false))), None)?;
        Ok(r.0.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
    }

    /// `DELETE /v1/o/<key>`: needs an admin token. Idempotent.
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        match self.send_raw(self.req("DELETE", &Self::key_path(key)?), None)? {
            Ok(_) | Err((404, _)) => Ok(()),
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    /// True iff the server exposes a clock (`GET /v1/clock` answered 200), probed once per client; an
    /// older server (404) or a backend that cannot be collected (501) means false, as before.
    fn supports_gc(&self) -> bool {
        self.gc_anchor().is_some()
    }
    /// The server backend's clock: read once, then advanced with the local monotonic clock and re-read
    /// every ten minutes (publishers only need elapsed time between two readings).
    fn now_ms(&self) -> Result<u64, BackendError> {
        match self.gc_anchor() {
            Some(a) => Ok(a),
            None => Err(BackendError::Other("this server has no trusted clock".into())),
        }
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        match self.send_raw(self.req("GET", &Self::mtime_path(key)?), None)? {
            Ok((t, _, _)) => t.trim().parse().map(Some).map_err(|_| BackendError::Other("bad mtime from server".into())),
            Err((404, _)) => Ok(None),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        let p = format!("/v1/touch/{}", pct_encode(key, true));
        validate_key(key)?;
        match self.send_raw(self.req("POST", &p), None)? {
            Ok(_) => Ok(true),
            Err((404, _)) => Ok(false),
            Err((c, m)) => Err(self.status_err(c, &m)),
        }
    }
}
