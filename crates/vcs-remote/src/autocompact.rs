//! Automatic merge of small remote packs, so a long-lived remote keeps few packs and a clone
//! pays a handful of index and pack reads instead of hundreds.
//!
//! Every publish writes one small pack. [`maybe_compact`] is called after a successful publish
//! (the CLI does it every 8th remote generation, so it costs about one LIST per 8 publishes) and
//! runs one tiered compaction pass ([`crate::compact::compact`], so rules C1 to C5 of that module
//! apply unchanged) when the remote holds more than [`AUTO_MIN_PACKS`] packs. It is best effort:
//! a backend that cannot delete (the compaction probes that before doing any work), a lost race
//! with another compactor or any other error leaves the remote as it was and is returned for the
//! caller to turn into a hint. `VCS_AUTO_COMPACT=0` turns it off.
//!
//! The pass is bounded so the publisher never waits long: it merges at most
//! [`AUTO_MAX_MERGE_BYTES`] per merged pack, and the tiers only ever combine packs of similar
//! size, so big packs are left alone. The output is written as v2 (per-object entries), the format
//! with the best clone wall time in docs/research/walgit-benchmark.md; `vcs gc --remote --compact`
//! still defaults to v3.

use crate::backend::Backend;
use crate::compact::{compact, count_packs, CompactOptions, CompactReport, SUGGEST_PACKS};
use crate::gc::GcError;
use vcs_store::pack::PackFormat;

/// Compact automatically only when the remote holds more packs than this.
pub const AUTO_MIN_PACKS: usize = SUGGEST_PACKS;
/// Largest pack the automatic pass will build (it is built in memory by the publisher).
pub const AUTO_MAX_MERGE_BYTES: u64 = 64 * 1024 * 1024;

/// Whether automatic compaction is enabled (`VCS_AUTO_COMPACT=0` disables it).
pub fn enabled() -> bool {
    std::env::var("VCS_AUTO_COMPACT").map(|v| v != "0").unwrap_or(true)
}

/// Run one bounded compaction pass if the remote holds more than [`AUTO_MIN_PACKS`] packs.
/// `Ok(None)`: disabled or not needed. Never leaves the remote worse: see the module docs.
pub fn maybe_compact(b: &dyn Backend, owner: &str) -> Result<Option<CompactReport>, GcError> {
    if !enabled() || !b.supports_gc() || count_packs(b)? <= AUTO_MIN_PACKS {
        return Ok(None);
    }
    let opts = CompactOptions {
        dry_run: false,
        owner: owner.to_owned(),
        format: PackFormat::V2,
        max_merge_bytes: AUTO_MAX_MERGE_BYTES,
        ..Default::default()
    };
    compact(b, &opts).map(Some)
}
