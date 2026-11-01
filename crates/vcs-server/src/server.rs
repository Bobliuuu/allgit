//! The HTTP service. Routes (all but `/healthz` and `/readyz` need `Authorization: Bearer <token>`):
//!
//! * `GET /healthz` liveness, `GET /readyz` backend reachable (both unauthenticated, constant bodies)
//! * `GET /metrics` Prometheus text (admin token or the metrics token)
//! * `GET /v1/whoami`
//! * `GET|HEAD /v1/o/<key>[?offset=N&len=M]` read (ETag header = version tag)
//! * `PUT /v1/o/<key>` with `If-None-Match: *` (create-only) or `If-Match: "<tag>"` (CAS)
//! * `DELETE /v1/o/<key>` admin only, never the manifest (garbage collection)
//! * `GET /v1/list?prefix=`
//!
//! Writes to the manifest key are validated by [`crate::validate`] before the CAS.

use crate::audit::AuditLog;
use crate::auth::{hash_token, AuthError, Principal, Role, TokenTable};
use crate::http::{pct_decode, read_request, write_response, write_response_ka, Limits, ParseError, Request, Response};
use crate::log::{Format, Level, Logger, V};
use crate::metrics::Metrics;
use crate::ratelimit::{Limiter, RateLimit};
use crate::resilient::{set_request_deadline, Resilience, ResilientBackend};
use crate::validate::{validate_and_normalize, Reject, ValidatorConfig};
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// How long an idle persistent (keep-alive) connection waits for its next request before it is closed.
    pub keepalive_idle: Duration,
    /// Requests served on one connection before the server asks the client to reconnect.
    pub max_requests_per_conn: usize,
    /// Per-principal token bucket; `None` = unlimited (no built-in default).
    pub rate_limit: Option<RateLimit>,
    /// Audit log of every authenticated PUT; `None` = no audit.
    pub audit: Option<Arc<AuditLog>>,
    /// Tokens file to poll for changes (new contents replace the table; unparsable or empty ones are ignored).
    pub tokens_file: Option<PathBuf>,
    pub token_reload_interval: Duration,
    /// Structured log sink (stderr, text, info by default).
    pub log: Arc<Logger>,
    /// Hash (`vcs-server hash-token`) of a token that may read `/metrics` only.
    pub metrics_token_hash: Option<String>,
    /// Retries, backoff, request deadline and circuit breaker for backend calls.
    pub resilience: Resilience,
    /// How long a `/readyz` backend probe result is reused.
    pub ready_cache: Duration,
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
            keepalive_idle: Duration::from_secs(5),
            max_requests_per_conn: 1000,
            rate_limit: None,
            audit: None,
            tokens_file: None,
            token_reload_interval: Duration::from_secs(1),
            log: Arc::new(Logger::new(Format::Text, Level::Info)),
            metrics_token_hash: None,
            resilience: Resilience::default(),
            ready_cache: Duration::from_secs(2),
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
    resilient: Arc<ResilientBackend>,
    metrics: Arc<Metrics>,
    log: Arc<Logger>,
    ready: Mutex<Option<(Instant, bool)>>,
    req_seq: AtomicU64,
    nonce: [u8; 8],
    cfg: Config,
    tokens: RwLock<Arc<TokenTable>>,
    /// Current validator settings (the policy is replaceable at runtime).
    validator: RwLock<Arc<ValidatorConfig>>,
    #[cfg(feature = "tls")]
    tls: RwLock<Option<Arc<rustls::ServerConfig>>>,
    read_index: Mutex<crate::readguard::ReadIndex>,
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

/// Reload access to a running server; see [`Server::handle`].
#[derive(Clone)]
pub struct Handle(Arc<Shared>);

impl Handle {
    pub fn reload_tokens(&self, t: TokenTable) {
        *self.0.tokens.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(t);
    }
    pub fn reload_policy(&self, p: crate::Policy) {
        let mut v = (**self.0.validator.read().unwrap_or_else(|e| e.into_inner())).clone();
        v.policy = p;
        *self.0.validator.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(v);
    }
    #[cfg(feature = "tls")]
    pub fn reload_tls(&self, c: Arc<rustls::ServerConfig>) {
        *self.0.tls.write().unwrap_or_else(|e| e.into_inner()) = Some(c);
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
        let metrics = Arc::new(Metrics::new());
        let resilient = Arc::new(ResilientBackend::new(backend, cfg.resilience.clone(), metrics.clone()));
        let nonce = {
            let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
            let h = blake3::hash(format!("{t}:{}", std::process::id()).as_bytes());
            h.as_bytes()[..8].try_into().unwrap_or([0; 8])
        };
        let shared = Arc::new(Shared {
            backend: resilient.clone(),
            resilient,
            log: cfg.log.clone(),
            metrics,
            ready: Mutex::new(None),
            req_seq: AtomicU64::new(0),
            nonce,
            limiter: cfg.rate_limit.map(Limiter::new),
            tokens: RwLock::new(Arc::new(tokens)),
            validator: RwLock::new(Arc::new(cfg.validator.clone())),
            #[cfg(feature = "tls")]
            tls: RwLock::new(cfg.tls.clone()),
            read_index: Mutex::new(Default::default()),
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
                            s.log.info("reloaded tokens", &[("file", V::S(&path.display().to_string()))]);
                        }
                        Ok(_) => s
                            .log
                            .warn("ignoring empty tokens file", &[("file", V::S(&path.display().to_string()))]),
                        Err(e) => s.log.warn(
                            "keeping old tokens, cannot reload",
                            &[("file", V::S(&path.display().to_string())), ("error", V::S(&e))],
                        ),
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
        self.handle().reload_tokens(t)
    }
    /// Replace the path/read policy now (applies to the next request).
    pub fn reload_policy(&self, p: crate::Policy) {
        self.handle().reload_policy(p)
    }
    /// Replace the TLS configuration (certificate and key) for connections accepted from now on;
    /// connections already established keep the one they were accepted with.
    #[cfg(feature = "tls")]
    pub fn reload_tls(&self, c: Arc<rustls::ServerConfig>) {
        self.handle().reload_tls(c)
    }
    /// A cloneable handle for reloading from another thread (a signal handler thread) after
    /// [`join`](Self::join) has taken the server.
    pub fn handle(&self) -> Handle {
        Handle(self.shared.clone())
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
    /// Stop accepting (the listening socket closes), let in-flight requests finish for up to
    /// `grace`, then fsync the audit log. Returns whether every request finished in time.
    pub fn shutdown_graceful(&mut self, grace: Duration) -> bool {
        self.shutdown();
        let t0 = Instant::now();
        while self.shared.active.load(Ordering::SeqCst) > 0 && t0.elapsed() < grace {
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(a) = &self.shared.cfg.audit {
            if let Err(e) = a.sync() {
                self.shared.log.error("audit log flush failed", &[("error", V::S(&e.to_string()))]);
            }
        }
        self.shared.active.load(Ordering::SeqCst) == 0
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
    if sh.tls.read().unwrap_or_else(|e| e.into_inner()).is_some() {
        return;
    }
    // Consume what the client already sent (briefly) so that closing does not reset the connection
    // and destroy the reply in flight.
    let _ = conn.set_read_timeout(Some(Duration::from_millis(5)));
    let _ = conn.read(&mut [0u8; 4096]);
    let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
    sh.metrics.observe("OTHER", 503, "none", Duration::ZERO, 0, 0);
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
    let _ = conn.set_nodelay(true);
    let deadline = Arc::new(Mutex::new(Instant::now() + sh.cfg.header_timeout));
    let ds = DeadlineTcp {
        tcp: conn,
        deadline: deadline.clone(),
        io_timeout: sh.cfg.io_timeout,
    };
    let io: Box<dyn Io> = {
        #[cfg(feature = "tls")]
        {
            let cur = sh.tls.read().unwrap_or_else(|e| e.into_inner()).clone();
            match &cur {
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
    let mut served = 0usize;
    loop {
        let mut error_reply = false;
        let t0 = Instant::now();
        let mut ctx = Ctx {
            principal: "-".into(),
            role: "none",
            request_id: String::new(),
        };
        let (mut method, mut path, mut bytes_in) = (String::from("-"), String::from("-"), 0u64);
        let (mut resp, head, mut want_ka) = match read_request(&mut rd, &limits, &body_limit, &mut on_headers) {
            Ok(req) => {
                let head = req.method == "HEAD";
                ctx.request_id = request_id(sh, &req);
                method = req.method.clone();
                path = req.path.clone();
                bytes_in = req.body.len() as u64;
                set_request_deadline(sh.cfg.resilience.request_deadline.map(|d| Instant::now() + d));
                let r = route(sh, &req, &mut ctx);
                set_request_deadline(None);
                (r, head, req.keep_alive)
            }
            Err(ParseError::Io(e)) => {
                error_reply = true;
                if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) && served == 0 {
                    (text(408, "request timeout"), false, false)
                } else {
                    return;
                }
            }
            Err(ParseError::Bad(m)) => {
                error_reply = true;
                (text(400, m), false, false)
            }
            Err(ParseError::TooLarge) => {
                error_reply = true;
                (text(413, "body too large"), false, false)
            }
            Err(ParseError::HeadersTooLarge) => {
                error_reply = true;
                (text(431, "request headers too large"), false, false)
            }
            Err(ParseError::Unsupported(m)) => {
                error_reply = true;
                (text(501, m), false, false)
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
        if ctx.request_id.is_empty() {
            ctx.request_id = new_request_id(sh);
        }
        resp.headers.push(("X-Request-Id".into(), ctx.request_id.clone()));
        served += 1;
        want_ka = want_ka && !error_reply && served < sh.cfg.max_requests_per_conn && !sh.stop.load(Ordering::SeqCst);
        let written = write_response_ka(rd.get_mut(), &resp, head, want_ka).is_ok();
        let dur = t0.elapsed();
        let bytes_out = if head { 0 } else { resp.body.len() as u64 };
        sh.metrics.observe(&method, resp.status, ctx.role, dur, bytes_in, bytes_out);
        let lvl = if resp.status >= 500 { Level::Warn } else { Level::Info };
        sh.log.log(
            lvl,
            "request",
            &[
                ("request_id", V::S(&ctx.request_id)),
                ("method", V::S(&method)),
                ("path", V::S(&path.chars().take(256).collect::<String>())),
                ("status", V::N(u64::from(resp.status))),
                ("principal", V::S(&ctx.principal)),
                ("role", V::S(ctx.role)),
                ("dur_ms", V::F(dur.as_secs_f64() * 1000.0)),
                ("bytes_in", V::N(bytes_in)),
                ("bytes_out", V::N(bytes_out)),
                ("write_ok", V::N(u64::from(written))),
            ],
        );
        if !(want_ka && written) {
            return;
        }
        // Idle wait for the next request: bounded by keepalive_idle, not the (longer) header timeout.
        *deadline.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + sh.cfg.keepalive_idle;
        if std::io::BufRead::fill_buf(&mut rd).map(<[u8]>::is_empty).unwrap_or(true) {
            return;
        }
        *deadline.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + sh.cfg.header_timeout;

    }
}

/// Per-request facts the access log and metrics need; filled in by [`route`].
struct Ctx {
    principal: String,
    role: &'static str,
    request_id: String,
}

fn new_request_id(sh: &Shared) -> String {
    let n = sh.req_seq.fetch_add(1, Ordering::Relaxed);
    let mut b = sh.nonce.to_vec();
    b.extend_from_slice(&n.to_le_bytes());
    blake3::hash(&b).to_hex()[..16].to_string()
}

/// Reuse a client-supplied `X-Request-Id` when it is short and plain, else mint one.
fn request_id(sh: &Shared, req: &Request) -> String {
    match req.header("x-request-id") {
        Some(v) if !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) => v.to_owned(),
        _ => new_request_id(sh),
    }
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

fn route(sh: &Shared, req: &Request, ctx: &mut Ctx) -> Response {
    let mut resp = route_inner(sh, req, ctx);
    if resp.status == 503 && !resp.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("retry-after")) {
        let secs = sh.resilient.retry_after_secs().unwrap_or(1);
        resp = resp.with("Retry-After", secs.to_string());
    }
    resp
}

fn role_label(r: Role) -> &'static str {
    match r {
        Role::Admin => "admin",
        Role::Write => "write",
        Role::Read => "read",
    }
}

/// Liveness: the process answers. Constant body, no state.
fn health(req: &Request, ok: bool, body: &'static str) -> Response {
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        return text(405, "method not allowed");
    }
    text(if ok { 200 } else { 503 }, body)
}

/// Readiness: the backend answers a HEAD of the manifest key (present or absent both count). The
/// result is reused for `ready_cache`, so probes cannot load the backend.
fn ready(sh: &Shared) -> bool {
    let mut c = sh.ready.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, ok)) = *c {
        if t.elapsed() < sh.cfg.ready_cache {
            return ok;
        }
    }
    let ok = sh.resilient.probe(manifest::KEY);
    *c = Some((Instant::now(), ok));
    ok
}

fn route_inner(sh: &Shared, req: &Request, ctx: &mut Ctx) -> Response {
    match req.path.as_str() {
        "/healthz" => return health(req, true, "ok"),
        "/readyz" => {
            let ok = ready(sh);
            return health(req, ok, if ok { "ready" } else { "not ready" });
        }
        _ => {}
    }
    let tok = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
        .map(str::trim);
    let now = (sh.cfg.clock)();
    let table = sh.tokens.read().unwrap_or_else(|e| e.into_inner()).clone();
    let unauthorized = |msg: &str| {
        sh.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
        text(401, msg).with("WWW-Authenticate", "Bearer")
    };
    let auth = tok.map(|t| table.authenticate_at(t, now));
    if req.path == "/metrics" {
        let is_metrics_tok = match (&sh.cfg.metrics_token_hash, tok) {
            (Some(h), Some(t)) => crate::auth::ct_eq(h.as_bytes(), hash_token(t).as_bytes()),
            _ => false,
        };
        let admin = match &auth {
            Some(Ok(p)) => {
                ctx.principal = p.name.clone();
                ctx.role = role_label(p.role);
                Some(p.role >= Role::Admin)
            }
            _ => None,
        };
        if is_metrics_tok {
            ctx.principal = "metrics".into();
        } else if admin.is_none() {
            return unauthorized("unauthorized");
        } else if admin != Some(true) {
            return text(403, "Forbidden: /metrics needs an admin or metrics token");
        }
        if !matches!(req.method.as_str(), "GET" | "HEAD") {
            return text(405, "method not allowed");
        }
        return Response::new(200, sh.metrics.render(sh.active.load(Ordering::SeqCst)))
            .with("Content-Type", "text/plain; version=0.0.4; charset=utf-8");
    }
    let who = match auth {
        Some(Ok(p)) => p,
        Some(Err(AuthError::Expired)) => return unauthorized("token expired"),
        _ => return unauthorized("unauthorized"),
    };
    ctx.principal = who.name.clone();
    ctx.role = role_label(who.role);
    let mutating = matches!(req.method.as_str(), "PUT" | "DELETE") && req.path.starts_with("/v1/o/");
    let resp = match &sh.limiter {
        Some(l) => match l.check(&who.name, now) {
            Ok(()) => dispatch(sh, req, who),
            Err(ms) => {
                sh.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                let secs = ms.div_ceil(1000).max(1);
                text(429, "rate limit exceeded").with("Retry-After", secs.to_string())
            }
        },
        None => dispatch(sh, req, who),
    };
    if let (true, Some(a)) = (mutating, &sh.cfg.audit) {
        let key = pct_decode(&req.path["/v1/o/".len()..]).unwrap_or_else(|| req.path.clone());
        match a.record(now, &who.name, &req.method, &key, &resp.status.to_string()) {
            Ok(_) => {
                sh.metrics.audit_appends.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                sh.metrics.audit_errors.fetch_add(1, Ordering::Relaxed);
                sh.log.error("audit write failed", &[("error", V::S(&e.to_string()))]);
                return text(503, "audit log unavailable; the request may have been applied");
            }
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
            if restricted(sh, who) && !prefix.starts_with(vcs_remote::shelf::SHELF_PREFIX) {
                // Listing would enumerate object ids of every zone. Packs are hidden (they mix zones);
                // tx/, tomb/ and the rest name ids too.
                if prefix.starts_with("packs/") {
                    return Response::new(200, "");
                }
                return text(403, "Forbidden: listing is not available to a read-restricted token");
            }
            match sh.backend.list(&prefix) {
                // other principals' shelves are invisible unless a `shelve-read` rule grants them
                Ok(keys) => Response::new(
                    200,
                    keys.into_iter()
                        .filter(|k| shelf_read_ok(sh, who, k))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
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
                "GET" | "HEAD" => object_read(sh, req, who, &key, m == "HEAD"),
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

/// Shelves are private (docs/shelves.md): for a key under `shelves/`, only its owner, an admin, or a
/// principal granted by a `shelve-read` rule may read it. Every other key is not a shelf key: true.
fn shelf_read_ok(sh: &Shared, who: &Principal, key: &str) -> bool {
    let Some(owner) = key
        .strip_prefix(vcs_remote::shelf::SHELF_PREFIX)
        .map(|r| r.split('/').next().unwrap_or(""))
    else {
        return true;
    };
    who.role >= Role::Admin
        || owner == who.name
        || sh
            .validator
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .policy
            .shelve_read_allowed(&who.name, owner)
}

/// May `who` create or delete this shelf key? Owner (with a write token) or admin; the key must be
/// exactly `shelves/<principal>/<name>/{meta,pack,idx}`.
fn shelf_write_ok(who: &Principal, key: &str) -> Result<(), &'static str> {
    let Some((owner, _, _)) = vcs_remote::shelf::parse_key(key) else {
        return Err("a shelf key must be shelves/<principal>/<name>/{meta,pack,idx}");
    };
    if who.role >= Role::Admin || (who.role >= Role::Write && owner == who.name) {
        Ok(())
    } else {
        Err("only the owner of a shelf (or an admin) may change it")
    }
}

/// Does the read policy limit this principal? (Admins never.)
fn restricted(sh: &Shared, who: &Principal) -> bool {
    who.role < Role::Admin
        && sh
            .validator
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .policy
            .read_restricted(&who.name)
}

/// For a read-restricted principal: may it read `key`? Whitelist: the manifest, and canonical object
/// keys that are not reachable only from zones it may not read. Everything else (packs, tx, tomb,
/// non-canonical spellings) is reported as absent, the same answer as for a key that does not exist.
fn read_allowed(sh: &Shared, who: &Principal, key: &str) -> Result<bool, BackendError> {
    if key == manifest::KEY {
        return Ok(true);
    }
    let Some(id) = crate::readguard::canonical_object_id(key) else {
        return Ok(false);
    };
    let policy = sh.validator.read().unwrap_or_else(|e| e.into_inner()).policy.clone();
    let mut idx = sh.read_index.lock().unwrap_or_else(|e| e.into_inner());
    idx.refresh(sh.backend.as_ref())?;
    Ok(!idx.denied_only(&id, &|z| policy.read_zone_allowed(&who.name, z)))
}

fn object_read(sh: &Shared, req: &Request, who: &Principal, key: &str, head: bool) -> Response {
    let is_shelf = key.starts_with(vcs_remote::shelf::SHELF_PREFIX);
    if is_shelf && !shelf_read_ok(sh, who, key) {
        return if head { Response::new(404, "") } else { text(404, "not found") };
    }
    if !is_shelf && restricted(sh, who) {
        match read_allowed(sh, who, key) {
            Ok(true) => {}
            Ok(false) => return if head { Response::new(404, "") } else { text(404, "not found") },
            Err(e) => return backend_err(e),
        }
    }
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
        // Conditional GET: the caller already holds this version, send no body.
        Ok(Some(v)) if req.header("if-none-match").is_some_and(|t| unquote(t.trim()) == v.tag) => {
            Response::new(304, "").with("ETag", etag(&v.tag))
        }
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
    if key.starts_with(vcs_remote::shelf::SHELF_PREFIX) {
        return match shelf_write_ok(who, key) {
            Ok(()) => match sh.backend.delete(key) {
                Ok(()) => Response::new(204, ""),
                Err(e) => backend_err(e),
            },
            Err(m) => text(403, &format!("Forbidden: {m}")),
        };
    }
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
    if key.starts_with(vcs_remote::shelf::SHELF_PREFIX) {
        if let Err(m) = shelf_write_ok(who, key) {
            return text(403, &format!("Forbidden: {m}"));
        }
        if if_match.is_some() {
            return text(428, "shelf keys are create-only: send If-None-Match: *");
        }
        if let Some((p, n, "meta")) = vcs_remote::shelf::parse_key(key) {
            match vcs_remote::shelf::ShelfMeta::parse(&req.body) {
                Ok(m) if m.principal == p && m.name == n => {}
                Ok(_) => return text(422, "Invalid: meta names a different principal or shelf than its key"),
                Err(e) => return text(422, &format!("Invalid: {e}")),
            }
        }
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
        let vcfg = sh.validator.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Err(r) = validate_and_normalize(sh.backend.as_ref(), &cur, &mut new, who, (sh.cfg.clock)(), &vcfg) {
            if matches!(r, Reject::LockedBy { .. } | Reject::LockRequired { .. }) {
                sh.metrics.lock_conflicts.fetch_add(1, Ordering::Relaxed);
            }
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
