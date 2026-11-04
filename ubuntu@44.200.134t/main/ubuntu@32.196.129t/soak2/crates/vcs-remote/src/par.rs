//! Bounded-concurrency helper for remote I/O: run independent requests (ranged GETs, create-only
//! PUTs) on a few threads so wall time is bounded by latency, not by latency times request count.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Default number of concurrent requests; `VCS_REMOTE_PARALLEL` overrides it (1 = sequential).
pub const DEFAULT_PARALLEL: usize = 8;

/// The configured concurrency (at least 1, at most 64).
pub fn parallelism() -> usize {
    std::env::var("VCS_REMOTE_PARALLEL")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_PARALLEL)
        .clamp(1, 64)
}

/// `f` over every item with at most `limit` running at once. Results come back in input order.
/// No new item is started after one has failed (items already running finish); the first error in
/// input order is returned. With `limit <= 1` or a single item it runs on the calling thread.
pub fn map_bounded<T: Sync, R: Send, E: Send>(items: &[T], limit: usize, f: impl Fn(&T) -> Result<R, E> + Sync) -> Result<Vec<R>, E> {
    let limit = limit.max(1).min(items.len().max(1));
    if limit == 1 {
        return items.iter().map(&f).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Result<R, E>>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let failed = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..limit {
            s.spawn(|| loop {
                if failed.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                if r.is_err() {
                    failed.store(true, Ordering::Relaxed);
                }
                *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            });
        }
    });
    let mut out = Vec::with_capacity(items.len());
    for slot in slots {
        match slot.into_inner().unwrap_or_else(|e| e.into_inner()) {
            Some(Ok(v)) => out.push(v),
            Some(Err(e)) => return Err(e),
            None => break, // skipped after a failure; the error is in an earlier slot
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn preserves_order_and_bounds_concurrency() {
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let items: Vec<u32> = (0..40).collect();
        let out = map_bounded(&items, 4, |&i| -> Result<u32, ()> {
            let n = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(3));
            live.fetch_sub(1, Ordering::SeqCst);
            Ok(i * 2)
        })
        .unwrap();
        assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 4 && peak.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn first_error_wins_and_stops_new_work() {
        let started = AtomicUsize::new(0);
        let items: Vec<u32> = (0..200).collect();
        let r = map_bounded(&items, 4, |&i| {
            started.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
            if i == 3 {
                Err("boom")
            } else {
                Ok(i)
            }
        });
        assert_eq!(r.unwrap_err(), "boom");
        assert!(started.load(Ordering::SeqCst) < 200, "work stopped after the failure");
    }
}
