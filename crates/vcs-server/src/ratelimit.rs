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

#[cfg(test)]
mod tests {
    use super::*;

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
