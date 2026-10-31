//! `HttpBackend`: a [`Backend`] that talks to `vcs-server`. Token comes from the caller
//! (the CLI reads `VCS_TOKEN`); it is never part of the URL. Plain `http://` only: put a TLS
//! proxy in front of the server for anything beyond loopback or a private network.

use crate::http::pct_encode;
use std::time::Duration;
use vcs_remote::backend::validate_key;
use vcs_remote::{Backend, BackendError, Versioned};

pub struct HttpBackend {
    base: String,
    token: String,
    agent: ureq::Agent,
}

impl HttpBackend {
    /// `base` is `http://host:port` (an optional trailing slash is ignored).
    pub fn new(base: &str, token: &str) -> Result<Self, BackendError> {
        let base = base.trim_end_matches('/');
        if !base.starts_with("http://") {
            return Err(BackendError::Other(
                "HttpBackend supports http:// only (no TLS in this build; use a TLS proxy and a private network)".into(),
            ));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .max_idle_connections(0)
            .build();
        Ok(Self {
            base: base.to_owned(),
            token: token.to_owned(),
            agent,
        })
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
            Err((code, msg)) => Err(BackendError::Other(format!("server returned {code}: {}", msg.trim()))),
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
                if code >= 500 {
                    Err(BackendError::Unavailable(format!("server returned {code}: {}", msg.trim())))
                } else {
                    Ok(Err((code, msg)))
                }
            }
            Err(ureq::Error::Transport(t)) => Err(BackendError::Unavailable(t.to_string())),
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
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        let p = format!("{}?offset={offset}&len={len}", Self::key_path(key)?);
        match self.send_raw(self.req("GET", &p), None)? {
            Ok((_, data, _)) => Ok(Some(data)),
            Err((404, _)) => Ok(None),
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        match self.send_raw(self.req("HEAD", &Self::key_path(key)?), None)? {
            Ok(_) => Ok(true),
            Err((404, _)) => Ok(false),
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let r = self.req("PUT", &Self::key_path(key)?).set("If-None-Match", "*");
        match self.send_raw(r, Some(data))? {
            Ok(_) => Ok(true),
            Err((412, _)) => Ok(false),
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let r = self.req("PUT", &Self::key_path(key)?).set("If-Match", &format!("\"{tag}\""));
        match self.send_raw(r, Some(data))? {
            Ok((_, _, etag)) => Ok(Some(etag.ok_or_else(|| BackendError::Other("server sent no ETag".into()))?)),
            Err((412, _)) => Ok(None),
            Err((c, m)) => Err(BackendError::Other(format!("server returned {c}: {}", m.trim()))),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let r = self.send(self.req("GET", &format!("/v1/list?prefix={}", pct_encode(prefix, false))), None)?;
        Ok(r.0.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
    }
}
