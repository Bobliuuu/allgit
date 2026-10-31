//! The HTTP service. Routes (all but `/healthz` need `Authorization: Bearer <token>`):
//!
//! * `GET /healthz`
//! * `GET /v1/whoami`
//! * `GET|HEAD /v1/o/<key>[?offset=N&len=M]` read (ETag header = version tag)
//! * `PUT /v1/o/<key>` with `If-None-Match: *` (create-only) or `If-Match: "<tag>"` (CAS)
//! * `GET /v1/list?prefix=`
//!
//! Writes to the manifest key are validated by [`crate::validate`] before the CAS.

use crate::auth::{Principal, Role, TokenTable};
use crate::http::{pct_decode, read_request, write_response, ParseError, Request, Response};
use crate::validate::{validate_manifest_write, Reject};
use std::io::BufReader;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vcs_remote::backend::validate_key;
use vcs_remote::{manifest, Backend, BackendError, Manifest};

pub struct Config {
    pub tokens: TokenTable,
    pub max_body: usize,
    pub io_timeout: Duration,
    /// Validator clock in unix ms (injectable for tests).
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Config {
    pub fn new(tokens: TokenTable) -> Self {
        Self {
            tokens,
            max_body: 256 * 1024 * 1024,
            io_timeout: Duration::from_secs(30),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            }),
        }
    }
}

struct Shared {
    backend: Arc<dyn Backend>,
    cfg: Config,
    stop: AtomicBool,
}

pub struct Server {
    shared: Arc<Shared>,
    addr: SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Bind and serve on a background thread. Use port 0 for an ephemeral port.
    pub fn start(backend: Arc<dyn Backend>, cfg: Config, addr: &str) -> std::io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            backend,
            cfg,
            stop: AtomicBool::new(false),
        });
        let s2 = shared.clone();
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s2.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let s3 = s2.clone();
                std::thread::spawn(move || handle_conn(&s3, conn));
            }
        });
        Ok(Server {
            shared,
            addr,
            thread: Some(thread),
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    /// Block until the accept loop ends (it only ends on `shutdown`).
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_conn(sh: &Shared, conn: TcpStream) {
    let _ = conn.set_read_timeout(Some(sh.cfg.io_timeout));
    let _ = conn.set_write_timeout(Some(sh.cfg.io_timeout));
    let Ok(rd) = conn.try_clone() else { return };
    let mut rd = BufReader::new(rd);
    let mut conn = conn;
    let (resp, head) = match read_request(&mut rd, sh.cfg.max_body) {
        Ok(req) => {
            let head = req.method == "HEAD";
            (route(sh, &req), head)
        }
        Err(ParseError::Io(_)) => return,
        Err(ParseError::Bad(m)) => (text(400, m), false),
        Err(ParseError::TooLarge) => (text(413, "body too large"), false),
        Err(ParseError::Unsupported(m)) => (text(501, m), false),
    };
    let _ = write_response(&mut conn, &resp, head);
}

fn text(status: u16, msg: &str) -> Response {
    Response::new(status, format!("{msg}\n"))
}

fn backend_err(e: BackendError) -> Response {
    match e {
        BackendError::Unavailable(m) => text(503, &format!("backend unavailable: {m}")),
        BackendError::Other(m) => text(400, &m),
    }
}

fn etag(tag: &str) -> String {
    format!("\"{tag}\"")
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

fn route(sh: &Shared, req: &Request) -> Response {
    if req.path == "/healthz" {
        return text(200, "ok");
    }
    let tok = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
        .map(str::trim);
    let who = match tok.and_then(|t| sh.cfg.tokens.authenticate(t)) {
        Some(p) => p,
        None => return text(401, "unauthorized").with("WWW-Authenticate", "Bearer"),
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/whoami") => {
            let zones = who
                .zones
                .as_ref()
                .map(|z| z.iter().cloned().collect::<Vec<_>>().join(","))
                .unwrap_or_else(|| "*".into());
            text(200, &format!("{} {} {zones}", who.name, who.role.name()))
        }
        ("GET", "/v1/list") => {
            let prefix = req.query_param("prefix").unwrap_or_default();
            match sh.backend.list(&prefix) {
                Ok(keys) => Response::new(200, keys.join("\n")),
                Err(e) => backend_err(e),
            }
        }
        (m, p) if p.starts_with("/v1/o/") => {
            let Some(key) = pct_decode(&p["/v1/o/".len()..]) else {
                return text(400, "bad key encoding");
            };
            if let Err(e) = validate_key(&key) {
                return backend_err(e);
            }
            match m {
                "GET" | "HEAD" => object_read(sh, req, &key, m == "HEAD"),
                "PUT" => object_put(sh, req, who, &key),
                _ => text(405, "method not allowed"),
            }
        }
        _ => text(404, "no such route"),
    }
}

fn object_read(sh: &Shared, req: &Request, key: &str, head: bool) -> Response {
    if head {
        return match sh.backend.exists(key) {
            Ok(true) => Response::new(200, ""),
            Ok(false) => Response::new(404, ""),
            Err(e) => backend_err(e),
        };
    }
    if let (Some(o), Some(l)) = (req.query_param("offset"), req.query_param("len")) {
        let (Ok(o), Ok(l)) = (o.parse::<u64>(), l.parse::<u64>()) else {
            return text(400, "bad offset/len");
        };
        return match sh.backend.get_range(key, o, l) {
            Ok(Some(d)) => Response::new(200, d),
            Ok(None) => text(404, "not found"),
            Err(e) => backend_err(e),
        };
    }
    match sh.backend.get(key) {
        Ok(Some(v)) => Response::new(200, v.data).with("ETag", etag(&v.tag)),
        Ok(None) => text(404, "not found"),
        Err(e) => backend_err(e),
    }
}

fn reject_response(r: Reject) -> Response {
    match r {
        Reject::Forbidden(m) => text(403, &format!("Forbidden: {m}")),
        Reject::LockedBy {
            zone,
            path,
            holder,
            expires_at,
        } => text(
            409,
            &format!("LockedBy: {path} in zone {zone} is locked by {holder} until {expires_at} (unix ms)"),
        ),
        Reject::Invalid(m) => text(422, &format!("Invalid: {m}")),
        Reject::Unavailable(m) => text(503, &format!("backend unavailable: {m}")),
    }
}

fn object_put(sh: &Shared, req: &Request, who: &Principal, key: &str) -> Response {
    if who.role < Role::Write {
        return text(403, "Forbidden: read-only principal");
    }
    let if_none = req.header("if-none-match").map(str::trim) == Some("*");
    let if_match = req.header("if-match").map(|s| unquote(s.trim()).to_owned());
    if !if_none && if_match.is_none() {
        return text(428, "unconditional PUT is not allowed; send If-None-Match: * or If-Match");
    }
    let is_manifest = key == manifest::KEY;
    // Read the manifest we are about to replace; validate against exactly that version.
    let mut cur = Manifest::default();
    if is_manifest {
        let v = match sh.backend.get(key) {
            Ok(v) => v,
            Err(e) => return backend_err(e),
        };
        match (&v, &if_match) {
            (Some(v), Some(t)) => {
                if &v.tag != t {
                    return Response::new(412, "");
                }
            }
            (Some(_), None) => return Response::new(412, ""),
            (None, Some(_)) => return Response::new(412, ""),
            (None, None) => {}
        }
        if let Some(v) = v {
            cur = match Manifest::decode(&v.data) {
                Ok(m) => m,
                Err(e) => return text(500, &format!("stored manifest invalid: {e}")),
            };
        }
        let new = match Manifest::decode(&req.body) {
            Ok(m) => m,
            Err(e) => return text(422, &format!("Invalid: {e}")),
        };
        if let Err(r) = validate_manifest_write(sh.backend.as_ref(), &cur, &new, who, (sh.cfg.clock)()) {
            return reject_response(r);
        }
    }
    if if_none {
        match sh.backend.put_if_absent(key, &req.body) {
            Ok(true) => Response::new(201, ""),
            Ok(false) => Response::new(412, ""),
            Err(e) => backend_err(e),
        }
    } else {
        match sh.backend.put_if_match(key, &req.body, &if_match.unwrap_or_default()) {
            Ok(Some(t)) => Response::new(200, "").with("ETag", etag(&t)),
            Ok(None) => Response::new(412, ""),
            Err(e) => backend_err(e),
        }
    }
}
