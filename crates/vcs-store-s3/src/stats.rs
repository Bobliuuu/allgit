//! Process-wide S3 client counters (#572), exported by `vcs-server` on `/metrics`. Global because a process
//! normally has one S3 backend and the server holds it behind `dyn Backend`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

pub(crate) struct Stats {
    /// HTTP attempts by method: GET, HEAD, PUT, DELETE, POST.
    pub requests: [AtomicU64; 5],
    pub slowdown: AtomicU64,
    pub conflict_409: AtomicU64,
    pub status_5xx: AtomicU64,
    pub timeouts: AtomicU64,
    pub retries: AtomicU64,
    /// Compare-and-swap (`If-Match` / `If-None-Match`) writes lost to another writer (412 or 409).
    pub cas_losses: AtomicU64,
    pub inflight_bytes: AtomicU64,
    pub cred_refreshes: AtomicU64,
    pub cred_refresh_errors: AtomicU64,
    pub creds_rejected: AtomicBool,
}

pub(crate) static STATS: Stats = Stats {
    requests: [const { AtomicU64::new(0) }; 5],
    slowdown: AtomicU64::new(0),
    conflict_409: AtomicU64::new(0),
    status_5xx: AtomicU64::new(0),
    timeouts: AtomicU64::new(0),
    retries: AtomicU64::new(0),
    cas_losses: AtomicU64::new(0),
    inflight_bytes: AtomicU64::new(0),
    cred_refreshes: AtomicU64::new(0),
    cred_refresh_errors: AtomicU64::new(0),
    creds_rejected: AtomicBool::new(false),
};

pub(crate) fn method_index(m: &str) -> usize {
    match m {
        "GET" => 0,
        "HEAD" => 1,
        "PUT" => 2,
        "DELETE" => 3,
        _ => 4,
    }
}

/// Method labels, indexed like [`S3Stats::requests`].
pub const METHODS: [&str; 5] = ["GET", "HEAD", "PUT", "DELETE", "POST"];

/// Point-in-time copy of the counters.
#[derive(Clone, Debug, Default)]
pub struct S3Stats {
    /// Indexed like [`METHODS`].
    pub requests: [u64; 5],
    pub slowdown: u64,
    pub conflict_409: u64,
    pub status_5xx: u64,
    pub timeouts: u64,
    pub retries: u64,
    pub cas_losses: u64,
    pub inflight_bytes: u64,
    pub cred_refreshes: u64,
    pub cred_refresh_errors: u64,
    pub creds_rejected: bool,
}

pub fn stats() -> S3Stats {
    let s = &STATS;
    S3Stats {
        requests: std::array::from_fn(|i| s.requests[i].load(Relaxed)),
        slowdown: s.slowdown.load(Relaxed),
        conflict_409: s.conflict_409.load(Relaxed),
        status_5xx: s.status_5xx.load(Relaxed),
        timeouts: s.timeouts.load(Relaxed),
        retries: s.retries.load(Relaxed),
        cas_losses: s.cas_losses.load(Relaxed),
        inflight_bytes: s.inflight_bytes.load(Relaxed),
        cred_refreshes: s.cred_refreshes.load(Relaxed),
        cred_refresh_errors: s.cred_refresh_errors.load(Relaxed),
        creds_rejected: s.creds_rejected.load(Relaxed),
    }
}

/// True while the last credentialed answer was a 403 for expired or invalid credentials (cleared by the next
/// successful request). `/readyz` fails on it: the backend is reachable but nothing can be read or written.
pub fn credentials_rejected() -> bool {
    STATS.creds_rejected.load(Relaxed)
}

/// Adds `n` to the in-flight byte gauge for its lifetime.
pub(crate) struct Inflight(pub u64);
impl Inflight {
    pub fn new(n: u64) -> Self {
        STATS.inflight_bytes.fetch_add(n, Relaxed);
        Inflight(n)
    }
}
impl Drop for Inflight {
    fn drop(&mut self) {
        STATS.inflight_bytes.fetch_sub(self.0, Relaxed);
    }
}
