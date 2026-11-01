//! The HTTP service. Routes (all but `/healthz` need `Authorization: Bearer <token>`):
//!
//! * `GET /healthz`
//! * `GET /v1/whoami`
//! * `GET|HEAD /v1/o/<key>[?offset=N&len=M]` read (ETag header = version tag)
//! * `PUT /v1/o/<key>` with `If-None-Match: *` (create-only) or `If-Match: "<tag>"` (CAS)
//! * `DELETE /v1/o/<key>` admin only, never the manifest (garbage collection)
//! * `GET /v1/list?prefix=`
//!
//! Writes to the manifest key are validated by [`crate::validate`] before the CAS.

use crate::audit::AuditLog;
use crate::auth::{AuthError, Principal, Role, TokenTable};
use crate::http::{pct_decode, read_request, write_response, Limits, ParseError, Request, Response};
use crate::ratelimit::{Limiter, RateLimit};
use crate::validate::{validate_and_normalize, Reject, ValidatorConfig};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_remote::backend::validate_key;
use vcs_remote::{manifest, Backend, BackendError, Manifest};

pub struct Config {
    pub tokens: TokenTable,
    /// Largest accepted body of an `objects/` or other non-manifest PUT.
    pub max_body: usize,
    /// Largest accepted body of a manifest PUT.
    pub max_manifest_body: usize,
    /// Request line plus headers, in bytes (431 beyond).
    pub max_header_bytes: usize,
    /// Per read/write timeout on a connection.
    /// Path policy, lease cap and clock-skew allowance for manifest writes.
    pub validator: ValidatorConfig,
    pub io_timeout: Duration,
    /// Total time allowed to deliver the request line and headers (also covers the TLS handshake).
    pub header_timeout: Duration,
    /// Total time allowed to deliver the body, and again to write the response.
    pub body_timeout: Duration,
    /// Simultaneous connections served; further ones are refused with 503 (plain) or dropped (TLS).
    pub max_connections: usize,
    /// Per-principal token bucket; `None` = unlimited (no built-in default).
    pub rate_limit: Option<RateLimit>,
    /// Audit log of every authenticated PUT; `None` = no audit.
    pub audit: Option<Arc<AuditLog>>,
    /// Tokens file to poll for changes (new contents replace the table; unparsable or empty ones are ignored).
    pub tokens_file: Option<PathBuf>,
    pub token_reload_interval: Duration,
    /// Serve HTTPS with this configuration (see [`crate::tls`]).
    #[cfg(feature = "tls")]
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Validator clock in unix ms (injectable for tests). Also drives token expiry and rate limiting.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Config {
    pub fn new(tokens: TokenTable) -> Self {
        Self {
            tokens,
            max_body: 256 * 1024 * 1024,
            max_manifest_body: 32 * 1024 * 1024,
            max_header_bytes: 32 * 1024,
            validator: ValidatorConfig::default(),
            io_timeout: Duration::from_secs(30),
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(600),
            max_connections: 256,
            rate_limit: None,
            audit: None,
            tokens_file: None,
            token_reload_interval: Duration::from_secs(1),
            #[cfg(feature = "tls")]
            tls: None,
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
    tokens: RwLock<Arc<TokenTable>>,
    limiter: Option<Limiter>,
    active: AtomicUsize,
    stop: AtomicBool,
}

struct ConnGuard(Arc<Shared>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Server {
    shared: Arc<Shared>,
    addr: SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
    watcher: Option<std::thread::JoinHandle<()>>,
}

fn file_sig(p: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl Server {
    /// Bind and serve on a background thread. Use port 0 for an ephemeral port.
    pub fn start(backend: Arc<dyn Backend>, mut cfg: Config, addr: &str) -> std::io::Result<Server> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let tokens = std::mem::take(&mut cfg.tokens);
        let shared = Arc::new(Shared {
            backend,
            limiter: cfg.rate_limit.map(Limiter::new),
            tokens: RwLock::new(Arc::new(tokens)),
            cfg,
            active: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let s2 = shared.clone();
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s2.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                if s2.active.fetch_add(1, Ordering::SeqCst) >= s2.cfg.max_connections {
                    s2.active.fetch_sub(1, Ordering::SeqCst);
                    refuse(&s2, conn);
                    continue;
                }
                let guard = ConnGuard(s2.clone());
                let s3 = s2.clone();
                // If the spawn fails the closure (and its guard) is dropped, releasing the slot.
                let _ = std::thread::Builder::new().spawn(move || {
                    let _guard = guard;
                    handle_conn(&s3, conn)
                });
            }
        });
        let watcher = shared.cfg.tokens_file.clone().map(|path| {
            let s = shared.clone();
            std::thread::spawn(move || {
                let mut last = file_sig(&path);
                let mut waited = Duration::ZERO;
                let slice = Duration::from_millis(20).min(s.cfg.token_reload_interval);
                while !s.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(slice);
                    waited += slice;
                    if waited < s.cfg.token_reload_interval {
                        continue;
                    }
                    waited = Duration::ZERO;
                    let sig = file_sig(&path);
                    if sig == last {
                        continue;
                    }
                    last = sig;
                    match std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| TokenTable::parse(&t))
                    {
                        Ok(t) if !t.is_empty() => {
                            *s.tokens.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(t);
                            eprintln!("vcs-server: reloaded tokens from {}", path.display());
                        }
                        Ok(_) => eprintln!("vcs-server: ignoring empty tokens file {}", path.display()),
                        Err(e) => eprintln!("vcs-server: keeping old tokens, cannot reload {}: {e}", path.display()),
                    }
                }
            })
        });
        Ok(Server {
            shared,
            addr,
            thread: Some(thread),
            watcher,
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
    /// Replace the token table now (revocation and expiry changes take effect for the next request).
    pub fn reload_tokens(&self, t: TokenTable) {
        *self.shared.tokens.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(t);
    }
    /// Connections currently being served.
    pub fn active_connections(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
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
        if let Some(t) = self.watcher.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Over the connection cap: answer 503 on plain connections (best effort, 1 s write budget), drop TLS ones.
fn refuse(#[allow(unused_variables)] sh: &Shared, mut conn: TcpStream) {
    #[cfg(feature = "tls")]
    if sh.cfg.tls.is_some() {
        return;
    }
    // Consume what the client already sent (briefly) so that closing does not reset the connection
    // and destroy the reply in flight.
    let _ = conn.set_read_timeout(Some(Duration::from_millis(5)));
    let _ = conn.read(&mut [0u8; 4096]);
    let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
    let r = text(503, "too many connections").with("Retry-After", "1");
    let _ = write_response(&mut conn, &r, false);
    let _ = conn.shutdown(std::net::Shutdown::Write);
}

trait Io: Read + Write + Send {}
impl<T: Read + Write + Send> Io for T {}

/// A TCP stream whose every read and write is bounded by both the per-operation timeout and a
/// shared overall deadline, so a client that drips one byte at a time cannot hold a connection open.
struct DeadlineTcp {
    tcp: TcpStream,
    deadline: Arc<Mutex<Instant>>,
    io_timeout: Duration,
}

impl DeadlineTcp {
    fn budget(&self) -> std::io::Result<Duration> {
        let dl = *self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        let left = dl.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        Ok(left.min(self.io_timeout))
    }
}

impl Read for DeadlineTcp {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let t = self.budget()?;
        self.tcp.set_read_timeout(Some(t))?;
        self.tcp.read(buf)
    }
}

impl Write for DeadlineTcp {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let t = self.budget()?;
        self.tcp.set_write_timeout(Some(t))?;
        self.tcp.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.tcp.flush()
    }
}

fn handle_conn(sh: &Shared, conn: TcpStream) {
    let deadline = Arc::new(Mutex::new(Instant::now() + sh.cfg.header_timeout));
    let ds = DeadlineTcp {
        tcp: conn,
        deadline: deadline.clone(),
        io_timeout: sh.cfg.io_timeout,
    };
    let io: Box<dyn Io> = {
        #[cfg(feature = "tls")]
        {
            match &sh.cfg.tls {
                Some(c) => match rustls::ServerConnection::new(c.clone()) {
                    Ok(sc) => Box::new(rustls::StreamOwned::new(sc, ds)),
                    Err(_) => return,
                },
                None => Box::new(ds),
            }
        }
        #[cfg(not(feature = "tls"))]
        {
            Box::new(ds)
        }
    };
    let mut rd = BufReader::new(io);
    let limits = Limits {
        max_header_bytes: sh.cfg.max_header_bytes,
        ..Limits::default()
    };
    let body_limit = |r: &Request| -> usize {
        if r.method != "PUT" {
            64 * 1024
        } else if pct_decode(r.path.strip_prefix("/v1/o/").unwrap_or("")).as_deref() == Some(manifest::KEY) {
            sh.cfg.max_manifest_body
        } else {
            sh.cfg.max_body
        }
    };
    let body_dl = sh.cfg.body_timeout;
    let dl2 = deadline.clone();
    let mut on_headers = move || *dl2.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + body_dl;
    let mut error_reply = false;
    let (resp, head) = match read_request(&mut rd, &limits, &body_limit, &mut on_headers) {
        Ok(req) => {
            let head = req.method == "HEAD";
            (route(sh, &req), head)
        }
        Err(ParseError::Io(e)) => {
            error_reply = true;
            if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) {
                (text(408, "request timeout"), false)
            } else {
                return;
            }
        }
        Err(ParseError::Bad(m)) => {
            error_reply = true;
            (text(400, m), false)
        }
        Err(ParseError::TooLarge) => {
            error_reply = true;
            (text(413, "body too large"), false)
        }
        Err(ParseError::HeadersTooLarge) => {
            error_reply = true;
            (text(431, "request headers too large"), false)
        }
        Err(ParseError::Unsupported(m)) => {
            error_reply = true;
            (text(501, m), false)
        }
    };
    // A client that sent garbage or stalled gets a short, best-effort error reply (on TLS this may
    // be cut short if the handshake never completed); real responses get the full write budget.
    let reply_budget = if error_reply {
        Duration::from_secs(2).min(body_dl)
    } else {
        body_dl
    };
    *deadline.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + reply_budget;
    let _ = write_response(rd.get_mut(), &resp, head);
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
    let now = (sh.cfg.clock)();
    let table = sh.tokens.read().unwrap_or_else(|e| e.into_inner()).clone();
    let who = match tok.map(|t| table.authenticate_at(t, now)) {
        Some(Ok(p)) => p,
        Some(Err(AuthError::Expired)) => return text(401, "token expired").with("WWW-Authenticate", "Bearer"),
        _ => return text(401, "unauthorized").with("WWW-Authenticate", "Bearer"),
    };
    let mutating = req.method == "PUT" && req.path.starts_with("/v1/o/");
    let resp = match &sh.limiter {
        Some(l) => match l.check(&who.name, now) {
            Ok(()) => dispatch(sh, req, who),
            Err(ms) => {
                let secs = ms.div_ceil(1000).max(1);
                text(429, "rate limit exceeded").with("Retry-After", secs.to_string())
            }
        },
        None => dispatch(sh, req, who),
    };
    if let (true, Some(a)) = (mutating, &sh.cfg.audit) {
        let key = pct_decode(&req.path["/v1/o/".len()..]).unwrap_or_else(|| req.path.clone());
        if let Err(e) = a.record(now, &who.name, &req.method, &key, &resp.status.to_string()) {
            eprintln!("vcs-server: audit write failed: {e}");
            return text(503, "audit log unavailable; the request may have been applied");
        }
    }
    resp
}

fn dispatch(sh: &Shared, req: &Request, who: &Principal) -> Response {
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
            if shadows_manifest(&key) {
                return text(400, "key is a variant spelling of the manifest key");
            }
            match m {
                "GET" | "HEAD" => object_read(sh, req, &key, m == "HEAD"),
                "PUT" => object_put(sh, req, who, &key),
                "DELETE" => object_delete(sh, who, &key),
                _ => text(405, "method not allowed"),
            }
        }
        _ => text(404, "no such route"),
    }
}

/// Another spelling of the manifest key (case, trailing dots/spaces, `:stream`) that a case-folding or
/// Windows filesystem backend would resolve to the manifest file, skipping manifest validation.
fn shadows_manifest(key: &str) -> bool {
    if key == manifest::KEY {
        return false;
    }
    let base = key.split(':').next().unwrap_or(key).trim_end_matches(['.', ' ']);
    base.eq_ignore_ascii_case(manifest::KEY)
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
        Reject::LockRequired { zone, path } => text(
            428,
            &format!("LockRequired: {path} in zone {zone} may only be changed while you hold a lock on it (vcs lock)"),
        ),
        Reject::Invalid(m) => text(422, &format!("Invalid: {m}")),
        Reject::Unavailable(m) => text(503, &format!("backend unavailable: {m}")),
    }
}

/// **DESTRUCTIVE.** Admin-only, idempotent. The manifest can never be deleted this way.
fn object_delete(sh: &Shared, who: &Principal, key: &str) -> Response {
    if who.role < Role::Admin {
        return text(403, "Forbidden: delete needs an admin token");
    }
    if key == manifest::KEY {
        return text(403, "Forbidden: the manifest cannot be deleted");
    }
    match sh.backend.delete(key) {
        Ok(()) => Response::new(204, ""),
        Err(e) => backend_err(e),
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
    if key.starts_with("objects/") {
        if let Err(m) = verify_object_body(key, &req.body) {
            return text(422, &format!("Invalid: {m}"));
        }
    }
    let is_manifest = key == manifest::KEY;
    let mut body = std::borrow::Cow::Borrowed(req.body.as_slice());
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
        let mut new = match Manifest::decode(&req.body) {
            Ok(m) => m,
            Err(e) => return text(422, &format!("Invalid: {e}")),
        };
        if let Err(r) = validate_and_normalize(sh.backend.as_ref(), &cur, &mut new, who, (sh.cfg.clock)(), &sh.cfg.validator) {
            return reject_response(r);
        }
        // the server may have rewritten lock times: store its version, not the client's bytes
        body = std::borrow::Cow::Owned(new.encode());
    }
    if if_none {
        match sh.backend.put_if_absent(key, &body) {
            Ok(true) => Response::new(201, ""),
            Ok(false) => Response::new(412, ""),
            Err(e) => backend_err(e),
        }
    } else {
        match sh.backend.put_if_match(key, &body, &if_match.unwrap_or_default()) {
            Ok(Some(t)) => Response::new(200, "").with("ETag", etag(&t)),
            Ok(None) => Response::new(412, ""),
            Err(e) => backend_err(e),
        }
    }
}

/// An `objects/<2 hex>/<62 hex>` key encodes the object id; the body must hash to it.
fn verify_object_body(key: &str, body: &[u8]) -> Result<(), String> {
    let rest = key.strip_prefix("objects/").unwrap_or(key);
    let hex = match rest.split_once('/') {
        Some((a, b)) if a.len() == 2 && b.len() == 62 => format!("{a}{b}"),
        _ => return Err("objects/ key must be objects/<2 hex>/<62 hex> (the content id)".into()),
    };
    if hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err("object id must be lowercase hex".into());
    }
    let want = ObjectId::from_hex(&hex).map_err(|e| format!("bad object id in key: {e}"))?;
    let got = id_of_encoded(body);
    if got != want {
        return Err(format!("content hash mismatch: key says {want}, body hashes to {got}"));
    }
    Ok(())
}
