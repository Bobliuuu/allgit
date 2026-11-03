//! Per-principal token-bucket rate limiting. The caller supplies the clock, so tests are deterministic.
//! State is in memory and per server process: with N replicas a principal can use N times the rate.

use std::collections::HashMap;
use std::sync::Mutex;

/// `burst` requests may be made at once; the bucket refills at `per_sec` tokens per second.
/// There are deliberately no defaults: rate limiting is off unless the operator configures it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateLimit {
    pub burst: u32,
    pub per_sec: f64,
}

struct Bucket {
    tokens: f64,
    last_ms: u64,
}

pub struct Limiter {
    cfg: RateLimit,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Limiter {
    pub fn new(cfg: RateLimit) -> Self {
        Self {
            cfg,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `principal` at `now_ms`. `Err(ms)` = retry after at least that many milliseconds.
    pub fn check(&self, principal: &str, now_ms: u64) -> Result<(), u64> {
        let burst = f64::from(self.cfg.burst.max(1));
        let rate = self.cfg.per_sec.max(0.0);
        let mut m = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let b = m.entry(principal.to_owned()).or_insert(Bucket {
            tokens: burst,
            last_ms: now_ms,
        });
        let dt = now_ms.saturating_sub(b.last_ms) as f64 / 1000.0;
        b.last_ms = b.last_ms.max(now_ms);
        b.tokens = (b.tokens + dt * rate).min(burst);
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else if rate <= 0.0 {
            Err(u64::MAX / 4)
        } else {
            Err((((1.0 - b.tokens) / rate) * 1000.0).ceil() as u64)
        }
    }
}

/// Per-address throttle on failed authentication (security review finding 14). Each failure adds one
/// to the address's level, which leaks at `per_sec`; at `burst` the address is blocked (requests are
/// refused with 429 before the token is even looked at, so guessing cannot continue) until it leaks
/// below it. Successful requests cost nothing, and an idle server pays one relaxed atomic load.
pub struct FailThrottle {
    cfg: RateLimit,
    live: std::sync::atomic::AtomicUsize,
    levels: Mutex<HashMap<std::net::IpAddr, (f64, u64)>>,
}

/// Distinct addresses tracked at once; beyond this, fully drained entries are dropped first.
const MAX_TRACKED: usize = 50_000;

impl FailThrottle {
    pub fn new(cfg: RateLimit) -> Self {
        Self {
            cfg,
            live: std::sync::atomic::AtomicUsize::new(0),
            levels: Mutex::new(HashMap::new()),
        }
    }

    fn leaked(&self, level: f64, last: u64, now_ms: u64) -> f64 {
        (level - now_ms.saturating_sub(last) as f64 / 1000.0 * self.cfg.per_sec.max(0.0)).max(0.0)
    }

    /// `Err(retry_after_ms)` while `ip` is blocked.
    pub fn blocked(&self, ip: std::net::IpAddr, now_ms: u64) -> Result<(), u64> {
        if self.live.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return Ok(());
        }
        let burst = f64::from(self.cfg.burst.max(1));
        let m = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        let Some(&(lvl, last)) = m.get(&ip) else { return Ok(()) };
        let lvl = self.leaked(lvl, last, now_ms);
        if lvl < burst {
            return Ok(());
        }
        let rate = self.cfg.per_sec.max(0.0);
        Err(if rate <= 0.0 {
            u64::MAX / 4
        } else {
            ((((lvl - burst) / rate) * 1000.0).ceil() as u64).max(1)
        })
    }

    /// Record one failed authentication from `ip`.
    pub fn fail(&self, ip: std::net::IpAddr, now_ms: u64) {
        let mut m = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= MAX_TRACKED && !m.contains_key(&ip) {
            let cfgc = (self.cfg.per_sec.max(0.0), now_ms);
            m.retain(|_, (l, t)| *l - cfgc.1.saturating_sub(*t) as f64 / 1000.0 * cfgc.0 > 0.0);
            if m.len() >= MAX_TRACKED {
                return; // table full of live offenders: stop growing
            }
        }
        let e = m.entry(ip).or_insert((0.0, now_ms));
        let lvl = self.leaked(e.0, e.1, now_ms);
        *e = (lvl + 1.0, now_ms.max(e.1));
        self.live.store(m.len(), std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_throttle_blocks_after_burst_and_recovers() {
        let t = FailThrottle::new(RateLimit { burst: 3, per_sec: 1.0 });
        let a: std::net::IpAddr = "10.0.0.1".parse().unwrap();
        let b: std::net::IpAddr = "10.0.0.2".parse().unwrap();
        assert!(t.blocked(a, 0).is_ok(), "idle: no entries");
        for _ in 0..2 {
            t.fail(a, 0);
            assert!(t.blocked(a, 0).is_ok());
        }
        t.fail(a, 0);
        let wait = t.blocked(a, 0).unwrap_err();
        assert!((1..=1000).contains(&wait), "{wait}");
        assert_eq!(t.blocked(a, 0).unwrap_err(), 1, "level exactly at the burst: any leak unblocks");
        assert!(t.blocked(b, 0).is_ok(), "other addresses are unaffected");
        assert!(t.blocked(a, 500).is_ok(), "half a failure leaked");
        // a blocked address that keeps failing stays blocked longer
        t.fail(a, 1_000);
        t.fail(a, 1_000);
        assert!(t.blocked(a, 2_000).is_err());
        assert!(t.blocked(a, 10_000).is_ok());
        // clock going backwards is safe
        t.fail(a, 5);
        let _ = t.blocked(a, 0);
    }

    #[test]
    fn burst_then_refill_deterministic() {
        let l = Limiter::new(RateLimit { burst: 3, per_sec: 2.0 });
        for _ in 0..3 {
            assert!(l.check("a", 1000).is_ok());
        }
        assert_eq!(l.check("a", 1000), Err(500));
        assert!(l.check("a", 1400).is_err());
        assert!(l.check("a", 1500).is_ok());
        assert!(l.check("a", 1500).is_err());
        // long idle refills only up to the burst
        for _ in 0..3 {
            assert!(l.check("a", 1_000_000).is_ok());
        }
        assert!(l.check("a", 1_000_000).is_err());
    }

    #[test]
    fn principals_are_independent_and_clock_going_backwards_is_safe() {
        let l = Limiter::new(RateLimit { burst: 1, per_sec: 1.0 });
        assert!(l.check("a", 5000).is_ok());
        assert!(l.check("a", 5000).is_err());
        assert!(l.check("b", 5000).is_ok());
        assert!(l.check("a", 1).is_err(), "time going backwards grants nothing");
        assert!(l.check("a", 6000).is_ok());
    }

    #[test]
    fn zero_rate_never_refills() {
        let l = Limiter::new(RateLimit { burst: 1, per_sec: 0.0 });
        assert!(l.check("a", 0).is_ok());
        assert!(l.check("a", u64::MAX / 2).is_err());
    }
}
