//! Prometheus text-format metrics (hand-rolled, no dependency). Label sets are bounded: methods,
//! roles and error kinds come from fixed lists, never from request data.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Upper bounds of the latency histogram buckets, in seconds.
pub const BUCKETS: [f64; 12] = [0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0, 30.0];

pub struct Metrics {
    start: Instant,
    requests: Mutex<BTreeMap<(&'static str, u16, &'static str), u64>>,
    backend_errors: Mutex<BTreeMap<&'static str, u64>>,
    buckets: [AtomicU64; BUCKETS.len() + 1],
    lat_sum_us: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub auth_failures: AtomicU64,
    pub rate_limited: AtomicU64,
    pub lock_conflicts: AtomicU64,
    pub audit_appends: AtomicU64,
    pub audit_errors: AtomicU64,
    pub backend_retries: AtomicU64,
    pub breaker_trips: AtomicU64,
    pub breaker_open: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn method_label(m: &str) -> &'static str {
    match m {
        "GET" => "GET",
        "HEAD" => "HEAD",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "POST" => "POST",
        "OPTIONS" => "OPTIONS",
        _ => "OTHER",
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            requests: Mutex::default(),
            backend_errors: Mutex::default(),
            buckets: Default::default(),
            lat_sum_us: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            auth_failures: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            lock_conflicts: AtomicU64::new(0),
            audit_appends: AtomicU64::new(0),
            audit_errors: AtomicU64::new(0),
            backend_retries: AtomicU64::new(0),
            breaker_trips: AtomicU64::new(0),
            breaker_open: AtomicU64::new(0),
        }
    }

    /// Record one finished request. `role` is `admin|write|read|none`.
    pub fn observe(&self, method: &str, status: u16, role: &'static str, dur: Duration, bytes_in: u64, bytes_out: u64) {
        *self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((method_label(method), status, role))
            .or_insert(0) += 1;
        let secs = dur.as_secs_f64();
        let i = BUCKETS.iter().position(|b| secs <= *b).unwrap_or(BUCKETS.len());
        self.buckets[i].fetch_add(1, Relaxed);
        self.lat_sum_us.fetch_add(dur.as_micros() as u64, Relaxed);
        self.bytes_in.fetch_add(bytes_in, Relaxed);
        self.bytes_out.fetch_add(bytes_out, Relaxed);
    }

    /// `kind`: `unavailable`, `other`, `timeout` or `circuit_open`.
    pub fn backend_error(&self, kind: &'static str) {
        *self
            .backend_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(kind)
            .or_insert(0) += 1;
    }

    pub fn render(&self, open_connections: usize) -> String {
        let mut o = String::new();
        let counter = |o: &mut String, name: &str, help: &str, v: u64| {
            let _ = write!(o, "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n");
        };
        let _ = write!(
            o,
            "# HELP vcs_server_build_info Build information.\n# TYPE vcs_server_build_info gauge\n\
             vcs_server_build_info{{version=\"{}\",tls=\"{}\"}} 1\n",
            env!("CARGO_PKG_VERSION"),
            cfg!(feature = "tls")
        );
        let _ = write!(
            o,
            "# HELP vcs_server_uptime_seconds Seconds since start.\n# TYPE vcs_server_uptime_seconds gauge\n\
             vcs_server_uptime_seconds {:.3}\n",
            self.start.elapsed().as_secs_f64()
        );
        let _ = write!(
            o,
            "# HELP vcs_server_open_connections Connections being served.\n# TYPE vcs_server_open_connections gauge\n\
             vcs_server_open_connections {open_connections}\n"
        );
        o.push_str("# HELP vcs_server_requests_total Requests by method, status and role.\n# TYPE vcs_server_requests_total counter\n");
        for ((m, s, r), n) in self.requests.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            let _ = writeln!(o, "vcs_server_requests_total{{method=\"{m}\",status=\"{s}\",role=\"{r}\"}} {n}");
        }
        o.push_str("# HELP vcs_server_request_duration_seconds Request latency.\n# TYPE vcs_server_request_duration_seconds histogram\n");
        let mut cum = 0;
        for (i, b) in BUCKETS.iter().enumerate() {
            cum += self.buckets[i].load(Relaxed);
            let _ = writeln!(o, "vcs_server_request_duration_seconds_bucket{{le=\"{b}\"}} {cum}");
        }
        cum += self.buckets[BUCKETS.len()].load(Relaxed);
        let _ = write!(
            o,
            "vcs_server_request_duration_seconds_bucket{{le=\"+Inf\"}} {cum}\n\
             vcs_server_request_duration_seconds_sum {:.6}\nvcs_server_request_duration_seconds_count {cum}\n",
            self.lat_sum_us.load(Relaxed) as f64 / 1e6
        );
        counter(
            &mut o,
            "vcs_server_request_bytes_total",
            "Request body bytes received.",
            self.bytes_in.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_response_bytes_total",
            "Response body bytes sent.",
            self.bytes_out.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_auth_failures_total",
            "Requests rejected with 401.",
            self.auth_failures.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_rate_limited_total",
            "Requests rejected with 429.",
            self.rate_limited.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_lock_conflicts_total",
            "Manifest writes refused by a lock (409/428).",
            self.lock_conflicts.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_audit_appends_total",
            "Audit log records appended.",
            self.audit_appends.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_audit_errors_total",
            "Audit log append failures.",
            self.audit_errors.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_backend_retries_total",
            "Backend calls retried after a transient error.",
            self.backend_retries.load(Relaxed),
        );
        counter(
            &mut o,
            "vcs_server_backend_breaker_trips_total",
            "Times the circuit breaker opened.",
            self.breaker_trips.load(Relaxed),
        );
        let _ = write!(
            o,
            "# HELP vcs_server_backend_circuit_open 1 while the backend circuit breaker is open.\n\
             # TYPE vcs_server_backend_circuit_open gauge\nvcs_server_backend_circuit_open {}\n",
            self.breaker_open.load(Relaxed)
        );
        o.push_str("# HELP vcs_server_backend_errors_total Backend errors by kind.\n# TYPE vcs_server_backend_errors_total counter\n");
        let be = self.backend_errors.lock().unwrap_or_else(|e| e.into_inner());
        for kind in ["unavailable", "other", "timeout", "circuit_open"] {
            let _ = writeln!(
                o,
                "vcs_server_backend_errors_total{{kind=\"{kind}\"}} {}",
                be.get(kind).copied().unwrap_or(0)
            );
        }
        o
    }
}

/// Parse text-format samples into `(name{labels}, value)`; used by tests and `vcs-server` self-checks.
pub fn parse(text: &str) -> Result<BTreeMap<String, f64>, String> {
    let mut m = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (k, v) = l.rsplit_once(' ').ok_or(format!("line {}: no value", i + 1))?;
        let v: f64 = v.parse().map_err(|_| format!("line {}: bad value", i + 1))?;
        m.insert(k.to_owned(), v);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_parses_and_counts() {
        let m = Metrics::new();
        m.observe("GET", 200, "read", Duration::from_millis(3), 0, 10);
        m.observe("WEIRD", 404, "none", Duration::from_secs(60), 5, 0);
        m.backend_error("timeout");
        let p = parse(&m.render(2)).unwrap();
        assert_eq!(p["vcs_server_requests_total{method=\"GET\",status=\"200\",role=\"read\"}"], 1.0);
        assert_eq!(p["vcs_server_requests_total{method=\"OTHER\",status=\"404\",role=\"none\"}"], 1.0);
        assert_eq!(p["vcs_server_request_duration_seconds_bucket{le=\"0.005\"}"], 1.0);
        assert_eq!(p["vcs_server_request_duration_seconds_bucket{le=\"+Inf\"}"], 2.0);
        assert_eq!(p["vcs_server_request_duration_seconds_count"], 2.0);
        assert_eq!(p["vcs_server_open_connections"], 2.0);
        assert_eq!(p["vcs_server_backend_errors_total{kind=\"timeout\"}"], 1.0);
        assert_eq!(p["vcs_server_response_bytes_total"], 10.0);
    }
}
