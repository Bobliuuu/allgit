//! Crash-point injection for the atomic state-file writes (test support).
//!
//! Every pointer-file write (`refs`, `index`, `workspace`, `zones`, `op_head`, ...) goes through
//! one atomic write helper. A test can count those writes and arm a simulated crash at the
//! n-th one: that write fails after leaving a torn temp file, as a power cut would. State is
//! per thread, so parallel tests do not interfere; when not armed this costs one thread-local
//! read per write.

use std::cell::Cell;

thread_local! {
    static COUNT: Cell<usize> = const { Cell::new(0) };
    static ARMED: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Zero this thread's write counter.
pub fn reset() {
    COUNT.with(|c| c.set(0));
}

/// State-file writes made by this thread since the last [`reset`].
pub fn writes() -> usize {
    COUNT.with(|c| c.get())
}

/// Make the `n`-th (0-based, counted from the last [`reset`]) write fail; `None` disarms.
pub fn arm(n: Option<usize>) {
    ARMED.with(|a| a.set(n));
}

/// Called by every atomic write: true when this one must "crash".
pub(crate) fn tick() -> bool {
    let n = COUNT.with(|c| {
        let n = c.get();
        c.set(n + 1);
        n
    });
    ARMED.with(|a| a.get()) == Some(n)
}
