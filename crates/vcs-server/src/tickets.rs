//! Per-branch ticket queue (leader election by turn) for writers that all push to ONE branch.
//!
//! Without it, N writers on one branch each fetch, build and push; one wins and N-1 get `Diverged`, refetch
//! and try again, so work and requests grow with N squared (docs/research/many-writers-s3.md, finding 2).
//! With it a writer first takes a ticket for (zone, branch): the server grants tickets one at a time in
//! arrival order, a waiter blocks on a condition variable (no polling), and the holder fetches the
//! CURRENT head, builds on it, pushes and releases. The head the holder sees cannot move under it except
//! through writers that ignore the queue, so the push wins. It is advisory: a push without a ticket is
//! accepted exactly as before, a crashed holder's ticket expires after `lease`, and nothing here touches
//! the manifest or the CAS protocol, so correctness never depends on it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    next_id: u64,
    queue: VecDeque<u64>,
    holder: Option<(u64, Instant)>,
}

#[derive(Default)]
struct Slot {
    st: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
pub struct Tickets {
    slots: Mutex<HashMap<(String, String), Arc<Slot>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Acquire {
    Granted(u64),
    /// Waited `max_wait` without a turn; the client should ask again.
    Timeout,
}

impl Tickets {
    fn slot(&self, zone: &str, branch: &str) -> Arc<Slot> {
        let mut m = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 4096 {
            // drop idle slots so a scan of many branch names cannot grow the map without bound
            m.retain(|_, s| {
                let st = s.st.lock().unwrap_or_else(|e| e.into_inner());
                st.holder.is_some() || !st.queue.is_empty()
            });
        }
        m.entry((zone.to_owned(), branch.to_owned())).or_default().clone()
    }

    /// Wait for this writer's turn on (zone, branch). FIFO. A holder that never releases loses the
    /// ticket after `lease`.
    pub fn acquire(&self, zone: &str, branch: &str, lease: Duration, max_wait: Duration) -> Acquire {
        let slot = self.slot(zone, branch);
        let deadline = Instant::now() + max_wait;
        let mut st = slot.st.lock().unwrap_or_else(|e| e.into_inner());
        let id = st.next_id;
        st.next_id += 1;
        st.queue.push_back(id);
        loop {
            let now = Instant::now();
            if let Some((_, exp)) = st.holder {
                if now >= exp {
                    st.holder = None; // lease expired: the holder is gone
                }
            }
            if st.holder.is_none() && st.queue.front() == Some(&id) {
                st.queue.pop_front();
                st.holder = Some((id, now + lease));
                return Acquire::Granted(id);
            }
            if now >= deadline {
                st.queue.retain(|q| *q != id);
                slot.cv.notify_all();
                return Acquire::Timeout;
            }
            let mut wait = deadline - now;
            if let Some((_, exp)) = st.holder {
                wait = wait.min(exp.saturating_duration_since(now) + Duration::from_millis(1));
            }
            st = slot.cv.wait_timeout(st, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Give the turn back. Releasing a ticket that is not (or no longer) held is a no-op.
    pub fn release(&self, zone: &str, branch: &str, id: u64) {
        let slot = self.slot(zone, branch);
        let mut st = slot.st.lock().unwrap_or_else(|e| e.into_inner());
        if st.holder.is_some_and(|(h, _)| h == id) {
            st.holder = None;
            slot.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_one_holder_at_a_time() {
        let t = Arc::new(Tickets::default());
        let order = Arc::new(Mutex::new(vec![]));
        let in_crit = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = match t.acquire("z", "b", Duration::from_secs(5), Duration::from_secs(5)) {
            Acquire::Granted(i) => i,
            o => panic!("{o:?}"),
        };
        let hs: Vec<_> = (0..8)
            .map(|k| {
                let (t, order, in_crit) = (t.clone(), order.clone(), in_crit.clone());
                // stagger arrivals so FIFO is observable
                std::thread::sleep(Duration::from_millis(15));
                std::thread::spawn(move || {
                    let Acquire::Granted(id) = t.acquire("z", "b", Duration::from_secs(5), Duration::from_secs(10)) else {
                        panic!("timeout")
                    };
                    assert_eq!(in_crit.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0, "two holders");
                    order.lock().unwrap().push(k);
                    std::thread::sleep(Duration::from_millis(5));
                    in_crit.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    t.release("z", "b", id);
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(50));
        t.release("z", "b", first);
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), (0..8).collect::<Vec<_>>(), "arrival order");
    }

    #[test]
    fn expired_lease_and_timeout() {
        let t = Tickets::default();
        let Acquire::Granted(_) = t.acquire("z", "b", Duration::from_millis(30), Duration::from_secs(1)) else {
            panic!()
        };
        // the holder crashed: the next waiter gets the turn after the lease
        let s = Instant::now();
        assert!(matches!(
            t.acquire("z", "b", Duration::from_secs(1), Duration::from_secs(1)),
            Acquire::Granted(_)
        ));
        assert!(s.elapsed() >= Duration::from_millis(25));
        // another branch is independent; a held branch times out
        assert!(matches!(
            t.acquire("z", "c", Duration::from_secs(1), Duration::from_millis(10)),
            Acquire::Granted(_)
        ));
        assert_eq!(
            t.acquire("z", "b", Duration::from_secs(1), Duration::from_millis(20)),
            Acquire::Timeout
        );
    }
}
