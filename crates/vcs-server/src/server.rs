//! The HTTP service. Routes (all but `/healthz` and `/readyz` need `Authorization: Bearer <token>`):
//!
//! * `GET /healthz` liveness, `GET /readyz` backend reachable (both unauthenticated, constant bodies)
//! * `GET /metrics` Prometheus text (admin token or the metrics token)
//! * `GET /v1/whoami`
//! * `GET|HEAD /v1/o/<key>[?offset=N&len=M]` read (ETag header = version tag)
//! * `PUT /v1/o/<key>` with `If-None-Match: *` (create-only) or `If-Match: "<tag>"` (CAS)
//! * `DELETE /v1/o/<key>` admin only, never the manifest (garbage collection)
//! * `GET /v1/clock` (any role): the backend's clock in unix ms; 501 if the backend cannot be garbage collected
//! * `GET /v1/mtime/<key>` (admin): last write or touch time in unix ms on the backend's clock; 404 if absent
//! * `POST /v1/touch/<key>` (write): refresh the object's time (204; 404 if absent); never the manifest or a shelf key
//! * `GET /v1/list?prefix=`
//!
//! Writes to the manifest key are validated by [`crate::validate`] before the CAS.

use crate::audit::{AuditLog, Extra};
use crate::auth::{hash_token, AuthError, Principal, Role, TokenTable};
use crate::http::{pct_decode, read_request, write_response, write_response_ka, Limits, ParseError, Request, Response};
use crate::log::{Format, Level, Logger, V};
use crate::metrics::Metrics;
use crate::ratelimit::{Limiter, RateLimit};
use crate::resilient::{set_request_deadline, Resilience, ResilientBackend};
use crate::validate::{validate_and_report, Reject, Rewrite, ValidatorConfig};
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
    /// Un-ranged GETs of objects at least this large are streamed from the backend in windows when it can supply
    /// a length (directory backend), instead of being loaded whole. 0 streams everything it can.
    pub stream_min_bytes: u64,
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
    /// Extra connections accepted beyond `max_connections` that may only fetch `/healthz` and `/readyz` (anything else
    /// on them gets 503 and a close), so a saturated node still answers its orchestrator's probes. 0 = none.
    pub probe_reserve: usize,
    /// Most bytes of request bodies buffered at once across all connections (announced Content-Length is
    /// reserved before the body is read); a request that does not fit gets 503 + Retry-After. 0 = unlimited.
    pub max_inflight_body: usize,
    /// Reconcile stale manifest writes in the server instead of answering 412 (see [`crate::groupcommit`]).
    pub group_commit: bool,
    /// How long a branch ticket may be held before it is considered abandoned.
    pub ticket_lease: Duration,
    /// How long a ticket request waits for its turn before answering 503 (the client asks again).
    pub ticket_wait: Duration,
    /// How long an idle persistent (keep-alive) connection waits for its next request before it is closed.
    pub keepalive_idle: Duration,
    /// Requests served on one connection before the server asks the client to reconnect.
    pub max_requests_per_conn: usize,
    /// Per-principal token bucket; `None` = unlimited (no built-in default).
    pub rate_limit: Option<RateLimit>,
    /// Per-address throttle on failed authentication (`--auth-fail-limit`); `None` = off.
    pub auth_fail_limit: Option<RateLimit>,
    /// Most simultaneous connections from one address (`--max-conns-per-ip`); 0 = unlimited. Behind a
    /// reverse proxy every client shares the proxy's address, so leave it off there.
    pub max_conns_per_ip: usize,
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
    /// Mutual-TLS subject binding (`--tls-client-map`): when set, a request must come over a client
    /// certificate whose common name maps to the principal its token authenticates as.
    pub client_cert_map: Option<Arc<crate::certmap::ClientCertMap>>,
    /// Validator clock in unix ms (injectable for tests). Also drives token expiry and rate limiting.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Config {
    pub fn new(tokens: TokenTable) -> Self {
        Self {
            tokens,
            max_body: 1024 * 1024 * 1024,
            max_manifest_body: 32 * 1024 * 1024,
            max_header_bytes: 32 * 1024,
            stream_min_bytes: 8 << 20,
            validator: ValidatorConfig::default(),
            io_timeout: Duration::from_secs(30),
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(600),
            max_connections: 256,
            probe_reserve: 32,
            max_inflight_body: 2 << 30,
            group_commit: true,
            ticket_lease: Duration::from_secs(15),
            ticket_wait: Duration::from_secs(50),
            keepalive_idle: Duration::from_secs(5),
            max_requests_per_conn: 1000,
            rate_limit: None,
            auth_fail_limit: None,
            max_conns_per_ip: 0,
            audit: None,
            tokens_file: None,
            token_reload_interval: Duration::from_secs(1),
            log: Arc::new(Logger::new(Format::Text, Level::Info)),
            metrics_token_hash: None,
            resilience: Resilience::default(),
            ready_cache: Duration::from_secs(2),
            client_cert_map: None,
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
    /// The manifest version this process last read or wrote: (version tag, bytes). A manifest write
    /// that names exactly this tag in `If-Match` is validated against these bytes instead of reading
    /// the store again; the store's own compare-and-swap on that tag still decides whether it
    /// commits, so a stale entry (another replica wrote meanwhile) costs a 412, never a wrong accept.
    manifest_cache: Mutex<Option<(String, Arc<Vec<u8>>)>>,
    /// Serializes manifest writes in this process (so they stop racing each other) and remembers recent
    /// head changes so stale writes can be reconciled (see [`crate::groupcommit`]).
    manifest_write: Mutex<crate::groupcommit::Chain>,
    /// The decoded form of the manifest version in `manifest_cache` when this process wrote it.
    manifest_parsed: Mutex<Option<(String, Arc<Manifest>)>>,
    merged_writes: AtomicU64,
    /// Manifest writes waiting for the next batch commit (flat combining, see `manifest_combine`).
    batch_q: Mutex<Vec<BatchItem>>,
    tickets: crate::tickets::Tickets,
    batches: AtomicU64,
    batched_writes: AtomicU64,
    limiter: Option<Limiter>,
    fail_throttle: Option<crate::ratelimit::FailThrottle>,
    per_ip: Mutex<std::collections::HashMap<std::net::IpAddr, usize>>,
    active: AtomicUsize,
    /// Connections in the probe-only lane (over the cap; see `Config::probe_reserve`).
    probe_active: AtomicUsize,
    inflight_body: AtomicUsize,
    stop: AtomicBool,
}

struct ProbeGuard(Arc<Shared>);
impl Drop for ProbeGuard {
    fn drop(&mut self) {
        self.0.probe_active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ConnGuard(Arc<Shared>, Option<std::net::IpAddr>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        if let Some(ip) = self.1 {
            let mut m = self.0.per_ip.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(n) = m.get_mut(&ip) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&ip);
                }
            }
        }
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
        // Immutable reads (indexes, pack ranges, objects) and this server's own recent pack writes
        // are remembered, so validating a publish does not re-read from the object store what the
        // same client uploaded a moment ago.
        let backend: Arc<dyn Backend> =
            Arc::new(vcs_remote::MemoBackend::new(Box::new(backend.clone()), false).with_write_through(128 << 20));
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
            fail_throttle: cfg.auth_fail_limit.map(crate::ratelimit::FailThrottle::new),
            per_ip: Mutex::new(Default::default()),
            tokens: RwLock::new(Arc::new(tokens)),
            validator: RwLock::new(Arc::new(cfg.validator.clone())),
            #[cfg(feature = "tls")]
            tls: RwLock::new(cfg.tls.clone()),
            read_index: Mutex::new(Default::default()),
            manifest_cache: Mutex::new(None),
            manifest_write: Mutex::new(Default::default()),
            manifest_parsed: Mutex::new(None),
            merged_writes: AtomicU64::new(0),
            batch_q: Mutex::new(Vec::new()),
            tickets: Default::default(),
            batches: AtomicU64::new(0),
            batched_writes: AtomicU64::new(0),
            cfg,
            active: AtomicUsize::new(0),
            probe_active: AtomicUsize::new(0),
            inflight_body: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let s2 = shared.clone();
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if s2.stop.load(Ordering::SeqCst) {
                    break;
                }
                let conn = match conn {
                    Ok(c) => c,
                    Err(e) => {
                        // EMFILE/ENFILE/ENOBUFS/ENOMEM leave the pending connection in the queue, so retrying
                        // at once spins a core at 100 percent; back off until descriptors are freed.
                        if !matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::Interrupted
                        ) {
                            s2.log.warn("accept failed, backing off", &[("error", V::S(&e.to_string()))]);
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        continue;
                    }
                };
                if s2.active.fetch_add(1, Ordering::SeqCst) >= s2.cfg.max_connections {
                    s2.active.fetch_sub(1, Ordering::SeqCst);
                    // Over the cap: the connection may still be a health probe. It gets a separate, small lane that
                    // answers only /healthz and /readyz, so saturation cannot make the node fail its own probes.
                    if s2.probe_active.fetch_add(1, Ordering::SeqCst) < s2.cfg.probe_reserve {
                        let s3 = s2.clone();
                        let spawned = std::thread::Builder::new().spawn(move || {
                            let _lane = ProbeGuard(s3.clone());
                            handle_conn(&s3, conn, true)
                        });
                        if spawned.is_ok() {
                            continue;
                        }
                        s2.probe_active.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    s2.probe_active.fetch_sub(1, Ordering::SeqCst);
                    refuse(&s2, conn);
                    continue;
                }
                let mut ip = None;
                if s2.cfg.max_conns_per_ip > 0 {
                    if let Ok(a) = conn.peer_addr() {
                        let mut m = s2.per_ip.lock().unwrap_or_else(|e| e.into_inner());
                        let n = m.entry(a.ip()).or_insert(0);
                        if *n >= s2.cfg.max_conns_per_ip {
                            drop(m);
                            s2.active.fetch_sub(1, Ordering::SeqCst);
                            s2.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                            refuse(&s2, conn);
                            continue;
                        }
                        *n += 1;
                        ip = Some(a.ip());
                    }
                }
                let guard = ConnGuard(s2.clone(), ip);
                let s3 = s2.clone();
                // If the spawn fails the closure (and its guard) is dropped, releasing the slot.
                let _ = std::thread::Builder::new().spawn(move || {
                    let _guard = guard;
                    handle_conn(&s3, conn, false)
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
    /// Manifest writes whose `If-Match` was stale and that the server reconciled instead of answering 412.
    /// (batch commits, writes they carried): writes / batches is the mean batch size.
    pub fn batch_stats(&self) -> (u64, u64) {
        (
            self.shared.batches.load(Ordering::Relaxed),
            self.shared.batched_writes.load(Ordering::Relaxed),
        )
    }
    pub fn merged_writes(&self) -> u64 {
        self.shared.merged_writes.load(Ordering::Relaxed)
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
    // Consume what the client already sent so that closing does not reset the connection and destroy the
    // reply in flight. This runs on the single accept thread, so it must never wait: a blocking 5 ms read
    // per refused connection capped refusals at ~200/s (3000 idle sockets against a full server took 13 s
    // to be turned away, and a /healthz probe got 503 meanwhile; docs/research/infra-review.md).
    let _ = conn.set_nonblocking(true);
    let _ = conn.read(&mut [0u8; 4096]);
    let _ = conn.set_nonblocking(false);
    let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
    sh.metrics.observe("OTHER", 503, "none", Duration::ZERO, 0, 0);
    let r = text(503, "too many connections").with("Retry-After", "1");
    let _ = write_response(&mut conn, &r, false);
    let _ = conn.shutdown(std::net::Shutdown::Write);
}

trait Io: Read + Write + Send {
    /// DER of the verified client leaf certificate (mutual TLS), once the handshake has run.
    fn peer_leaf(&self) -> Option<Vec<u8>> {
        None
    }
}
impl Io for DeadlineTcp {}
#[cfg(feature = "tls")]
impl Io for rustls::StreamOwned<rustls::ServerConnection, DeadlineTcp> {
    fn peer_leaf(&self) -> Option<Vec<u8>> {
        self.conn.peer_certificates()?.first().map(|c| c.as_ref().to_vec())
    }
}

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
    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        let t = self.budget()?;
        self.tcp.set_write_timeout(Some(t))?;
        self.tcp.write_vectored(bufs)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.tcp.flush()
    }
}

/// Bytes reserved from the in-flight body budget by the current request; released on every exit path.
struct BodyReservation<'a>(&'a Shared, std::cell::Cell<usize>);
impl BodyReservation<'_> {
    fn release(&self) {
        let r = self.1.take();
        if r > 0 {
            self.0.inflight_body.fetch_sub(r, Ordering::SeqCst);
        }
    }
}
impl Drop for BodyReservation<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

fn handle_conn(sh: &Shared, conn: TcpStream, probe_only: bool) {
    let _ = conn.set_nodelay(true);
    let peer = if sh.fail_throttle.is_some() {
        conn.peer_addr().ok().map(|a| a.ip())
    } else {
        None
    };
    let header_timeout = if probe_only {
        sh.cfg.header_timeout.min(Duration::from_secs(2))
    } else {
        sh.cfg.header_timeout
    };
    let deadline = Arc::new(Mutex::new(Instant::now() + header_timeout));
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
        } else if !has_valid_token(sh, r) {
            // An unauthenticated client must not be able to reserve the in-flight body budget (or push
            // up to max_body bytes) before route() rejects it with 401: small cap, answered 413.
            64 * 1024
        } else if pct_decode(r.path.strip_prefix("/v1/o/").unwrap_or("")).as_deref() == Some(manifest::KEY) {
            sh.cfg.max_manifest_body
        } else {
            sh.cfg.max_body
        }
    };
    let reserved = BodyReservation(sh, std::cell::Cell::new(0usize));
    let mut reserve = |n: usize| -> bool {
        let cap = sh.cfg.max_inflight_body;
        if cap == 0 || n == 0 {
            return true;
        }
        let prev = sh.inflight_body.fetch_add(n, Ordering::SeqCst);
        if prev + n > cap && prev > 0 {
            sh.inflight_body.fetch_sub(n, Ordering::SeqCst);
            return false;
        }
        reserved.1.set(n);
        true
    };
    let body_dl = sh.cfg.body_timeout;
    let dl2 = deadline.clone();
    let mut on_headers = move || *dl2.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + body_dl;
    let mut served = 0usize;
    loop {
        let mut error_reply = false;
        let t0 = Instant::now();
        let mut ctx = Ctx {
            peer,
            principal: "-".into(),
            role: "none",
            request_id: String::new(),
            cert_cn: None,
        };
        let (mut method, mut path, mut bytes_in) = (String::from("-"), String::from("-"), 0u64);
        let (mut resp, head, mut want_ka) = match read_request(&mut rd, &limits, &body_limit, &mut on_headers, &mut reserve) {
            Ok(req) => {
                let head = req.method == "HEAD";
                ctx.request_id = request_id(sh, &req);
                method = req.method.clone();
                path = req.path.clone();
                bytes_in = req.body.len() as u64;
                set_request_deadline(sh.cfg.resilience.request_deadline.map(|d| Instant::now() + d));
                if sh.cfg.client_cert_map.is_some() {
                    ctx.cert_cn = rd.get_ref().peer_leaf().and_then(|d| crate::certmap::subject_common_name(&d));
                }
                let r = if probe_only && !matches!(req.path.as_str(), "/healthz" | "/readyz") {
                    text(503, "too many connections").with("Retry-After", "1")
                } else {
                    route(sh, &req, &mut ctx)
                };
                set_request_deadline(None);
                (r, head, req.keep_alive && !probe_only)
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
            Err(ParseError::Overloaded) => {
                error_reply = true;
                sh.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                (
                    text(503, "server busy: too many request bytes in flight").with("Retry-After", "1"),
                    false,
                    false,
                )
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
        reserved.release();
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
        let bytes_out = if head { 0 } else { resp.body_len() as u64 };
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
    peer: Option<std::net::IpAddr>,
    principal: String,
    role: &'static str,
    request_id: String,
    /// Subject CN of the client certificate on this connection (mutual TLS), if any.
    cert_cn: Option<String>,
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

/// Does an `If-None-Match` header value name `tag`? Backends differ in whether a tag carries its own quotes (an S3 ETag
/// does, a directory backend's does not), and the client strips them all before quoting once: compare without quotes.
fn tag_matches(header: &str, tag: &str) -> bool {
    unquote(header.trim()).replace('"', "") == tag.replace('"', "")
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s)
}

/// True if the request carries a bearer token the table accepts right now (role is checked later).
fn has_valid_token(sh: &Shared, req: &Request) -> bool {
    let Some(t) = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
        .map(str::trim)
    else {
        return false;
    };
    let now = (sh.cfg.clock)();
    let table = sh.tokens.read().unwrap_or_else(|e| e.into_inner()).clone();
    table.authenticate_at(t, now).is_ok()
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
    // reachable but rejecting our (expired) credentials is not ready either: every real call would fail
    let ok = sh.resilient.probe(manifest::KEY) && !vcs_store_s3::credentials_rejected();
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
    if let (Some(t), Some(ip)) = (&sh.fail_throttle, ctx.peer) {
        if let Err(ms) = t.blocked(ip, now) {
            sh.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
            return text(429, "too many failed authentication attempts").with("Retry-After", ms.div_ceil(1000).max(1).to_string());
        }
    }
    let peer = ctx.peer;
    let unauthorized = |msg: &str| {
        sh.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
        if let (Some(t), Some(ip)) = (&sh.fail_throttle, peer) {
            t.fail(ip, now);
        }
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
    if let Some(m) = &sh.cfg.client_cert_map {
        if !m.allows(ctx.cert_cn.as_deref(), &who.name) {
            sh.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
            return text(403, "Forbidden: the client certificate is not mapped to this principal");
        }
    }
    let mutating = matches!(req.method.as_str(), "PUT" | "DELETE") && req.path.starts_with("/v1/o/");
    let mut extra = Extra::default();
    let resp = match &sh.limiter {
        Some(l) => match l.check(&who.name, now) {
            Ok(()) => dispatch(sh, req, who, &mut extra),
            Err(ms) => {
                sh.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                let secs = ms.div_ceil(1000).max(1);
                text(429, "rate limit exceeded").with("Retry-After", secs.to_string())
            }
        },
        None => dispatch(sh, req, who, &mut extra),
    };
    if let (true, Some(a)) = (mutating, &sh.cfg.audit) {
        let key = pct_decode(&req.path["/v1/o/".len()..]).unwrap_or_else(|| req.path.clone());
        if extra.reason.is_none() {
            extra.reason = Some(match resp.status {
                200..=299 => "accepted",
                401 | 403 => "forbidden",
                409 | 412 => "conflict",
                422 | 428 | 400 => "invalid",
                429 => "rate-limited",
                _ => "error",
            });
        }
        match a.record_ext(now, &who.name, &req.method, &key, &resp.status.to_string(), &extra) {
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

/// `POST /v1/ticket?zone=Z&branch=B` blocks (up to `ticket_wait`) until it is this writer's turn to push
/// to Z/B and answers its id (or 202 `wait` when the turn has not come: ask again); `DELETE /v1/ticket?zone=Z&branch=B&id=N` gives the turn back. Advisory (see
/// [`crate::tickets`]): pushes without a ticket are unaffected.
fn ticket(sh: &Shared, req: &Request, who: &Principal) -> Response {
    if who.role < Role::Write {
        return text(403, "Forbidden: read-only principal");
    }
    let (Some(zone), Some(branch)) = (req.query_param("zone"), req.query_param("branch")) else {
        return text(400, "zone and branch are required");
    };
    if vcs_objects::names::validate_zone_name(&zone).is_err() || vcs_objects::names::validate_branch_name(&branch).is_err() {
        return text(400, "bad zone or branch name");
    }
    if who.zones.as_ref().is_some_and(|z| !z.contains(&zone)) {
        return text(403, "Forbidden: zone not in this token's scope");
    }
    if req.method == "DELETE" {
        let Some(id) = req.query_param("id").and_then(|v| v.parse::<u64>().ok()) else {
            return text(400, "id is required");
        };
        sh.tickets.release(&zone, &branch, id);
        return Response::new(204, "");
    }
    match sh.tickets.acquire(&zone, &branch, sh.cfg.ticket_lease, sh.cfg.ticket_wait) {
        crate::tickets::Acquire::Granted(id) => text(200, &id.to_string()),
        crate::tickets::Acquire::Timeout => text(202, "wait"),
    }
}

fn dispatch(sh: &Shared, req: &Request, who: &Principal, extra: &mut Extra) -> Response {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/v1/audit") => audit_page(sh, req, who),
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
        ("POST", "/v1/ticket") | ("DELETE", "/v1/ticket") => ticket(sh, req, who),
        ("GET", "/v1/clock") => {
            if !sh.backend.supports_gc() {
                return text(501, "garbage collection is not supported by this server's backend");
            }
            match sh.backend.now_ms() {
                Ok(t) => text(200, &t.to_string()),
                Err(e) => backend_err(e),
            }
        }
        (m, p) if (m == "GET" && p.starts_with("/v1/mtime/")) || (m == "POST" && p.starts_with("/v1/touch/")) => {
            let touch = m == "POST";
            let rest = &p[if touch { "/v1/touch/".len() } else { "/v1/mtime/".len() }..];
            let Some(key) = pct_decode(rest) else {
                return text(400, "bad key encoding");
            };
            if let Err(e) = validate_key(&key) {
                return backend_err(e);
            }
            if !sh.backend.supports_gc() {
                return text(501, "garbage collection is not supported by this server's backend");
            }
            if touch {
                if who.role < Role::Write {
                    return text(403, "Forbidden: touch needs a write token");
                }
                if (key == manifest::KEY || shadows_manifest(&key) || key.starts_with(vcs_remote::shelf::SHELF_PREFIX))
                    && who.role < Role::Admin
                {
                    return text(403, "Forbidden: only an admin may touch the manifest or shelf keys");
                }
                match sh.backend.touch(&key) {
                    Ok(true) => Response::new(204, ""),
                    Ok(false) => text(404, "no such key"),
                    Err(e) => backend_err(e),
                }
            } else {
                if who.role < Role::Admin {
                    return text(403, "Forbidden: object ages need an admin token");
                }
                match sh.backend.modified_ms(&key) {
                    Ok(Some(t)) => text(200, &t.to_string()),
                    Ok(None) => text(404, "no such key"),
                    Err(e) => backend_err(e),
                }
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
            if key.starts_with(vcs_remote::sharded::SHARD_PREFIX) || key.starts_with(vcs_remote::sharded::DECISION_PREFIX) {
                // sharded-manifest records are the server's own state: clients see them only composed into `manifest`.
                // Remote GC runs in the client and needs to read them (pending records are roots) and to delete old
                // decision records, so an admin may GET, HEAD and DELETE; nobody may PUT.
                if m == "PUT" || who.role < Role::Admin {
                    return text(403, "Forbidden: sharded manifest records are not client-addressable");
                }
            }
            match m {
                "GET" | "HEAD" => object_read(sh, req, who, &key, m == "HEAD"),
                "PUT" => object_put(sh, req, who, &key, extra),
                "DELETE" => object_delete(sh, who, &key),
                _ => text(405, "method not allowed"),
            }
        }
        _ => text(404, "no such route"),
    }
}

/// `GET /v1/audit?after=<seq>&limit=<n>` (admin only): a page of the hash-chained audit log as JSON
/// lines, oldest first, at most 1000 per page (default 100). `X-Audit-Next` is the last seq returned
/// (pass it as `after` for the next page; absent when the page is empty), `X-Audit-Head` the current
/// head hash to anchor. Verify offline with `vcs-server audit verify`.
fn audit_page(sh: &Shared, req: &Request, who: &Principal) -> Response {
    if who.role < Role::Admin {
        return text(403, "Forbidden: the audit log needs an admin token");
    }
    let Some(a) = &sh.cfg.audit else {
        return text(404, "audit log is not enabled");
    };
    let after = match req.query_param("after") {
        None => None,
        Some(v) => match v.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return text(400, "bad `after`"),
        },
    };
    let limit = match req.query_param("limit") {
        None => 100,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => n.min(1000),
            _ => return text(400, "bad `limit`"),
        },
    };
    match a.page(after, limit) {
        Ok((lines, last, head)) => {
            let mut body = lines.join("\n");
            if !body.is_empty() {
                body.push('\n');
            }
            let mut r = Response::new(200, body)
                .with("Content-Type", "application/x-ndjson")
                .with("X-Audit-Head", head);
            if let Some(l) = last {
                r = r.with("X-Audit-Next", l.to_string());
            }
            r
        }
        Err(e) => text(503, &format!("audit log unreadable: {e}")),
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

/// Largest manifest the server keeps a copy of.
const MANIFEST_CACHE_MAX: usize = 8 << 20;

impl Shared {
    fn manifest_cached(&self, tag: &str) -> Option<Arc<Vec<u8>>> {
        let g = self.manifest_cache.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().filter(|(t, _)| t == tag).map(|(_, b)| b.clone())
    }
    fn manifest_parsed(&self, tag: &str) -> Option<Arc<Manifest>> {
        let g = self.manifest_parsed.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().filter(|(t, _)| t == tag).map(|(_, m)| m.clone())
    }
    fn manifest_remember(&self, tag: &str, data: Option<&[u8]>) {
        *self.manifest_parsed.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let mut g = self.manifest_cache.lock().unwrap_or_else(|e| e.into_inner());
        *g = data
            .filter(|d| d.len() <= MANIFEST_CACHE_MAX)
            .map(|d| (tag.to_owned(), Arc::new(d.to_vec())));
    }
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
    // A read-only token under a read policy gets a manifest without the zones it may not read (it never publishes, so
    // it never needs the exact bytes for a compare-and-swap). Ranges are cut from the filtered bytes.
    if key == manifest::KEY && who.role == Role::Read && restricted(sh, who) {
        return filtered_manifest_response(sh, req, who);
    }
    if let (Some(o), Some(l)) = (req.query_param("offset"), req.query_param("len")) {
        let (Ok(o), Ok(l)) = (o.parse::<u64>(), l.parse::<u64>()) else {
            return text(400, "bad offset/len");
        };
        return match sh.backend.get_range_shared(key, o, l) {
            Ok(Some(d)) => Response::shared(200, d),
            Ok(None) => text(404, "not found"),
            Err(e) => backend_err(e),
        };
    }
    if key != manifest::KEY {
        match sh.backend.get_stream(key, sh.cfg.stream_min_bytes) {
            Ok(Some(s)) => {
                if req.header("if-none-match").is_some_and(|t| tag_matches(t, &s.tag)) {
                    return Response::new(304, "").with("ETag", etag(&s.tag));
                }
                return Response::streamed(200, s.reader, s.len).with("ETag", etag(&s.tag));
            }
            Ok(None) => {}
            Err(e) => return backend_err(e),
        }
    }
    let got = sh.backend.get(key);
    if key == manifest::KEY {
        if let Ok(Some(v)) = &got {
            sh.manifest_remember(&v.tag, Some(&v.data));
        }
    }
    match got {
        // Conditional GET: the caller already holds this version, send no body.
        Ok(Some(v)) if req.header("if-none-match").is_some_and(|t| tag_matches(t, &v.tag)) => {
            Response::new(304, "").with("ETag", etag(&v.tag))
        }
        Ok(Some(v)) => Response::new(200, v.data).with("ETag", etag(&v.tag)),
        Ok(None) => text(404, "not found"),
        Err(e) => backend_err(e),
    }
}

/// The manifest as a read-only, read-restricted principal may see it: only refs, tags, locks, zone prefixes and labels of zones
/// it may read. Snapshots (their objects name every zone's heads) and the applied-transaction list are dropped. Other roles get
/// the stored bytes: a writer needs them byte for byte for its compare-and-swap.
fn filtered_manifest_response(sh: &Shared, req: &Request, who: &Principal) -> Response {
    let v = match sh.backend.get(manifest::KEY) {
        Ok(Some(v)) => v,
        Ok(None) => return text(404, "not found"),
        Err(e) => return backend_err(e),
    };
    let mut m = match Manifest::decode(&v.data) {
        Ok(m) => m,
        Err(e) => return text(500, &format!("stored manifest invalid: {e}")),
    };
    let policy = sh.validator.read().unwrap_or_else(|e| e.into_inner()).policy.clone();
    let ok = |z: &str| policy.read_zone_allowed(&who.name, z);
    m.branches.retain(|(z, _), _| ok(z));
    m.tags.retain(|(z, _), _| ok(z));
    m.locks.retain(|(z, _), _| ok(z));
    m.zones.retain(|z, _| ok(z));
    m.labels.retain(|z, _| ok(z));
    m.snapshots.clear();
    m.applied.clear();
    let body = m.encode();
    if req.header("if-none-match").is_some_and(|t| tag_matches(t, &v.tag)) {
        return Response::new(304, "").with("ETag", etag(&v.tag));
    }
    if let (Some(o), Some(l)) = (req.query_param("offset"), req.query_param("len")) {
        let (Ok(o), Ok(l)) = (o.parse::<usize>(), l.parse::<usize>()) else {
            return text(400, "bad offset/len");
        };
        if o >= body.len() {
            return text(404, "not found");
        }
        return Response::new(200, body[o..body.len().min(o.saturating_add(l))].to_vec());
    }
    Response::new(200, body).with("ETag", etag(&v.tag))
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

struct BatchItem {
    body: Vec<u8>,
    if_match: Option<String>,
    who: Principal,
    tx: std::sync::mpsc::Sender<(Response, Extra)>,
}

/// Flat-combining group commit for manifest writes. Every writer enqueues its write; whoever then takes the
/// manifest lock becomes the leader, drains the queue, folds every write into ONE manifest (each validated
/// against the result of the ones before it, stale ones reconciled by the same rules as `groupcommit`), stores it
/// with ONE compare-and-swap and answers everyone. Under load the batch is whatever queued up during the previous
/// store round trip, so a slow store (S3, ~50 ms per PUT) carries many writes per request instead of one; with a
/// single writer the batch is 1 and nothing waits (no timer window). A write that cannot be folded in (conflict,
/// invalid, unknown base) gets exactly the answer it got before (412 or 4xx) and the client retries as before.
fn manifest_combine(sh: &Shared, req: &Request, who: &Principal, if_match: Option<String>, extra: &mut Extra) -> Response {
    let (tx, rx) = std::sync::mpsc::channel();
    sh.batch_q.lock().unwrap_or_else(|e| e.into_inner()).push(BatchItem {
        body: req.body.clone(),
        if_match,
        who: who.clone(),
        tx,
    });
    loop {
        if let Ok((r, e)) = rx.try_recv() {
            *extra = e;
            return r;
        }
        let guard = match sh.manifest_write.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        match guard {
            Some(mut chain) => {
                if let Ok((r, e)) = rx.try_recv() {
                    *extra = e;
                    return r;
                }
                let items = std::mem::take(&mut *sh.batch_q.lock().unwrap_or_else(|e| e.into_inner()));
                if !items.is_empty() {
                    run_batch(sh, &mut chain, items);
                }
            }
            None => {
                if let Ok((r, e)) = rx.recv_timeout(Duration::from_millis(2)) {
                    *extra = e;
                    return r;
                }
            }
        }
    }
}

fn run_batch(sh: &Shared, chain: &mut crate::groupcommit::Chain, items: Vec<BatchItem>) {
    let key = manifest::KEY;
    let n = items.len();
    let mut out: Vec<Option<(Response, Extra)>> = (0..n).map(|_| None).collect();
    let finish = |items: Vec<BatchItem>, out: Vec<Option<(Response, Extra)>>| {
        for (it, o) in items.into_iter().zip(out) {
            let _ = it
                .tx
                .send(o.unwrap_or_else(|| (text(500, "internal: no batch result"), Extra::default())));
        }
    };
    // The version this batch builds on: the one this process knows, else read it.
    let known = sh.manifest_cache.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let (t0, bytes0) = match known {
        Some(k) => k,
        None => match sh.backend.get(key) {
            Ok(Some(v)) => {
                sh.manifest_remember(&v.tag, Some(&v.data));
                (v.tag, Arc::new(v.data))
            }
            Ok(None) => {
                for o in out.iter_mut() {
                    *o = Some((Response::new(412, ""), Extra::default()));
                }
                return finish(items, out);
            }
            Err(e) => {
                for o in out.iter_mut() {
                    *o = Some((backend_err(e.clone()), Extra::default()));
                }
                return finish(items, out);
            }
        },
    };
    let cur0: Arc<Manifest> = match sh.manifest_parsed(&t0) {
        Some(m) => m,
        None => match Manifest::decode(&bytes0) {
            Ok(m) => Arc::new(m),
            Err(e) => {
                for o in out.iter_mut() {
                    *o = Some((text(500, &format!("stored manifest invalid: {e}")), Extra::default()));
                }
                return finish(items, out);
            }
        },
    };
    let vcfg = sh.validator.read().unwrap_or_else(|e| e.into_inner()).clone();
    let mut acc = cur0.clone();
    let mut acc_tag = t0.clone();
    let mut virt: Vec<String> = Vec::new();
    let mut accepted: Vec<(usize, Vec<Rewrite>)> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let mut ex = Extra::default();
        let decoded = match Manifest::decode(&it.body) {
            Ok(m) => m,
            Err(e) => {
                out[i] = Some((text(422, &format!("Invalid: {e}")), ex));
                continue;
            }
        };
        let mut new = if it.if_match.as_deref() == Some(acc_tag.as_str()) {
            decoded
        } else {
            let merged = it.if_match.as_deref().and_then(|b| chain.merge(b, &acc_tag, &acc, &decoded));
            match merged {
                Some(m) => {
                    sh.merged_writes.fetch_add(1, Ordering::Relaxed);
                    m
                }
                None => {
                    out[i] = Some((Response::new(412, ""), ex));
                    continue;
                }
            }
        };
        if sh.cfg.audit.is_some() {
            ex.policy = Some(vcfg.policy.hash());
        }
        let mut rw = Vec::new();
        if let Err(r) = validate_and_report(sh.backend.as_ref(), &acc, &mut new, &it.who, (sh.cfg.clock)(), &vcfg, Some(&mut rw)) {
            if matches!(r, Reject::LockedBy { .. } | Reject::LockRequired { .. }) {
                sh.metrics.lock_conflicts.fetch_add(1, Ordering::Relaxed);
            }
            ex.reason = Some(match &r {
                Reject::Forbidden(_) => "policy",
                Reject::LockedBy { .. } => "lock",
                Reject::LockRequired { .. } => "lock-required",
                Reject::Invalid(_) => "invalid",
                Reject::Unavailable(_) => "unavailable",
            });
            out[i] = Some((reject_response(r), ex));
            continue;
        }
        ex.vtime = Some(new.time_hw);
        let next_tag = format!("batch{}:{}", i, t0);
        chain.record(&acc_tag, &acc, &next_tag, &new);
        virt.push(acc_tag.clone());
        acc = Arc::new(new);
        acc_tag = next_tag;
        out[i] = Some((Response::new(200, ""), ex));
        accepted.push((i, rw));
    }
    // drop the virtual links; the real one (t0 -> stored tag) is recorded below on success
    virt.push(acc_tag.clone());
    chain.forget(&virt);
    if accepted.is_empty() {
        return finish(items, out);
    }
    let body = acc.encode();
    match sh.backend.put_if_match(key, &body, &t0) {
        Ok(Some(t)) => {
            sh.batches.fetch_add(1, Ordering::Relaxed);
            sh.batched_writes.fetch_add(accepted.len() as u64, Ordering::Relaxed);
            chain.record(&t0, &cur0, &t, &acc);
            sh.manifest_remember(&t, Some(&body));
            *sh.manifest_parsed.lock().unwrap_or_else(|e| e.into_inner()) = Some((t.clone(), acc.clone()));
            for (i, rw) in &accepted {
                audit_rewrites(sh, &items[*i].who, rw);
                if let Some((r, _)) = out[*i].as_mut() {
                    *r = Response::new(200, "").with("ETag", etag(&t));
                }
            }
        }
        res => {
            sh.manifest_remember("", None);
            for (i, _) in &accepted {
                if let Some((r, _)) = out[*i].as_mut() {
                    *r = match &res {
                        Ok(_) => Response::new(412, ""),
                        Err(e) => backend_err(e.clone()),
                    };
                }
            }
        }
    }
    finish(items, out)
}

/// Which keys a non-admin may PUT. `gc/`, `tomb/` and `compact/` are the collector's coordination state
/// (a planted `gc/lock` with a far-future lease stalls gc and compaction) and need Admin, as gc and compaction
/// already do because they delete. `packs/` and `tx/` are create-only for a write token: a pack is immutable once
/// written, so a token limited to one zone cannot overwrite other zones' data. Other prefixes stay open to writers
/// (tests and tools use ad-hoc keys); tightening to a strict allowlist is future work. Issue #573.
fn write_keyspace_ok(who: &Principal, key: &str, create_only: bool) -> Result<(), &'static str> {
    if who.role >= Role::Admin {
        return Ok(());
    }
    if ["gc/", "tomb/", "compact/"].iter().any(|p| key.starts_with(p)) {
        return Err("this key is reserved for an admin token (collector coordination state such as gc/lock)");
    }
    if (key.starts_with("packs/") || key.starts_with("tx/")) && !create_only {
        return Err("packs/ and tx/ keys are create-only for a write token");
    }
    Ok(())
}

fn object_put(sh: &Shared, req: &Request, who: &Principal, key: &str, extra: &mut Extra) -> Response {
    if who.role < Role::Write {
        return text(403, "Forbidden: read-only principal");
    }
    let if_none = req.header("if-none-match").map(str::trim) == Some("*");
    let if_match = req.header("if-match").map(|s| unquote(s.trim()).to_owned());
    if !if_none && if_match.is_none() {
        return text(428, "unconditional PUT is not allowed; send If-None-Match: * or If-Match");
    }
    if let Err(m) = write_keyspace_ok(who, key, if_none) {
        return text(403, &format!("Forbidden: {m}"));
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
    if is_manifest && !if_none && sh.cfg.group_commit {
        return manifest_combine(sh, req, who, if_match, extra);
    }
    let mut if_match = if_match;
    // One manifest write at a time per process: concurrent clients queue here instead of losing the swap.
    let mut chain_guard = if is_manifest && !if_none {
        Some(sh.manifest_write.lock().unwrap_or_else(|e| e.into_inner()))
    } else {
        None
    };
    let mut merged = false; // reconciled against a newer version
    let mut body = std::borrow::Cow::Borrowed(req.body.as_slice());
    // Read the manifest we are about to replace; validate against exactly that version.
    let mut cur: Arc<Manifest> = Arc::new(Manifest::default());
    let mut new_parsed: Option<Arc<Manifest>> = None;
    let mut rewrites: Vec<Rewrite> = Vec::new();
    if is_manifest {
        let mut cached = if_match.as_deref().and_then(|t| sh.manifest_cached(t));
        let mut stale_merge: Option<Manifest> = None;
        if cached.is_none() && chain_guard.is_some() && sh.cfg.group_commit {
            // Stale If-Match: try to reconcile with the version this process last wrote.
            if let (Some(base), Some((ctag, cbytes))) = (
                if_match.clone(),
                sh.manifest_cache.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            ) {
                let c = sh.manifest_parsed(&ctag).or_else(|| Manifest::decode(&cbytes).ok().map(Arc::new));
                if let (Some(c), Ok(n)) = (c, Manifest::decode(&req.body)) {
                    if let Some(m) = chain_guard.as_ref().and_then(|g| g.merge(&base, &ctag, &c, &n)) {
                        stale_merge = Some(m);
                        merged = true;
                        if_match = Some(ctag);
                        cached = Some(cbytes);
                    }
                }
            }
        }
        if let Some(bytes) = &cached {
            cur = match if_match.as_deref().and_then(|t| sh.manifest_parsed(t)) {
                Some(m) => m,
                None => match Manifest::decode(bytes) {
                    Ok(m) => Arc::new(m),
                    Err(e) => return text(500, &format!("stored manifest invalid: {e}")),
                },
            };
        } else {
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
                    Ok(m) => Arc::new(m),
                    Err(e) => return text(500, &format!("stored manifest invalid: {e}")),
                };
            }
        }
        let mut new = match stale_merge {
            Some(m) => m,
            None => match Manifest::decode(&req.body) {
                Ok(m) => m,
                Err(e) => return text(422, &format!("Invalid: {e}")),
            },
        };
        let vcfg = sh.validator.read().unwrap_or_else(|e| e.into_inner()).clone();
        if sh.cfg.audit.is_some() {
            extra.policy = Some(vcfg.policy.hash());
        }
        if let Err(r) = validate_and_report(
            sh.backend.as_ref(),
            &cur,
            &mut new,
            who,
            (sh.cfg.clock)(),
            &vcfg,
            Some(&mut rewrites),
        ) {
            if matches!(r, Reject::LockedBy { .. } | Reject::LockRequired { .. }) {
                sh.metrics.lock_conflicts.fetch_add(1, Ordering::Relaxed);
            }
            extra.reason = Some(match &r {
                Reject::Forbidden(_) => "policy",
                Reject::LockedBy { .. } => "lock",
                Reject::LockRequired { .. } => "lock-required",
                Reject::Invalid(_) => "invalid",
                Reject::Unavailable(_) => "unavailable",
            });
            return reject_response(r);
        }
        extra.vtime = Some(new.time_hw);
        // the server may have rewritten lock times: store its version, not the client's bytes
        body = std::borrow::Cow::Owned(new.encode());
        new_parsed = Some(Arc::new(new));
    }
    if if_none {
        if is_manifest {
            sh.manifest_remember("", None); // created without a known tag
        }
        match sh.backend.put_if_absent(key, &body) {
            Ok(true) => Response::new(201, ""),
            Ok(false) => Response::new(412, ""),
            Err(e) => backend_err(e),
        }
    } else {
        let used_tag = if_match.clone();
        match sh.backend.put_if_match(key, &body, &if_match.unwrap_or_default()) {
            Ok(Some(t)) => {
                if is_manifest {
                    if merged {
                        sh.merged_writes.fetch_add(1, Ordering::Relaxed);
                    }
                    if let (Some(g), Some(tag), Some(newm)) = (chain_guard.as_mut(), used_tag.as_deref(), new_parsed.as_ref()) {
                        g.record(tag, &cur, &t, newm);
                    }
                    sh.manifest_remember(&t, Some(&body));
                    if let Some(newm) = new_parsed.take() {
                        *sh.manifest_parsed.lock().unwrap_or_else(|e| e.into_inner()) = Some((t.clone(), newm));
                    }
                }
                audit_rewrites(sh, who, &rewrites);
                Response::new(200, "").with("ETag", etag(&t))
            }
            Ok(None) => {
                if is_manifest {
                    sh.manifest_remember("", None);
                }
                Response::new(412, "")
            }
            Err(e) => {
                if is_manifest {
                    sh.manifest_remember("", None);
                }
                backend_err(e)
            }
        }
    }
}

/// Forced head moves and branch deletions that a manifest write just made are audited on their own
/// lines (`FORCE`, `DELETE-BRANCH`), in addition to the generic `PUT manifest` record, so a
/// reviewer can grep the chain for history rewrites. The old head is named in full: it is the
/// recovery point.
fn audit_rewrites(sh: &Shared, who: &Principal, rewrites: &[Rewrite]) {
    let Some(a) = &sh.cfg.audit else { return };
    let now = (sh.cfg.clock)();
    for r in rewrites {
        let (op, what) = match r.new {
            Some(n) => (
                if r.unverified { "FORCE?" } else { "FORCE" },
                format!("{}/{} {} -> {}", r.zone, r.branch, r.old.to_hex(), n.to_hex()),
            ),
            None => ("DELETE-BRANCH", format!("{}/{} {} -> (deleted)", r.zone, r.branch, r.old.to_hex())),
        };
        if let Err(e) = a.record(now, &who.name, op, &what, "200") {
            sh.metrics.audit_errors.fetch_add(1, Ordering::Relaxed);
            sh.log.error("audit write failed", &[("error", V::S(&e.to_string()))]);
        } else {
            sh.metrics.audit_appends.fetch_add(1, Ordering::Relaxed);
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

#[cfg(test)]
mod tag_tests {
    use super::tag_matches;

    #[test]
    fn if_none_match_matches_tags_with_or_without_their_own_quotes() {
        // a directory backend's tag has no quotes, an S3 ETag keeps them; the client always sends one pair
        assert!(tag_matches("\"abc\"", "abc"));
        assert!(tag_matches("\"abc\"", "\"abc\""));
        assert!(tag_matches(" \"abc\" ", "abc"));
        assert!(!tag_matches("\"abd\"", "\"abc\""));
        assert!(!tag_matches("\"\"", "abc"));
    }
}
