//! Local pack compaction: merge small packs geometrically (`vcs repack`).
//!
//! The same tier policy as remote compaction ([`crate::layout::plan_tiers`]) and the same object
//! order ([`crate::layout::locality_order`]). Crash order is the sweep's: the new pack is durable
//! (pack file first, index last) before the old index, then the old pack, are removed, so a crash
//! leaves duplicates or an index-less pack, never a missing object. A process that opened before
//! the merge reloads its pack list when a pack file has vanished.

use super::*;
use crate::layout::{locality_order, plan_tiers};
use std::collections::HashSet;

/// A pack counts as tier 0 up to this size; a tier is this many times bigger than the last.
pub const LOCAL_TIER_BASE: u64 = 1024 * 1024;
pub const LOCAL_TIER_RATIO: u64 = 4;
/// Never merge more than this in one pass (a pack is built in memory).
pub const LOCAL_MAX_MERGE: u64 = 512 * 1024 * 1024;

/// What a compaction did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalCompaction {
    /// Merges performed.
    pub merges: usize,
    /// Packs removed (inputs), and packs written (outputs).
    pub packs_removed: usize,
    pub packs_written: usize,
    /// Objects in the outputs.
    pub objects: usize,
    /// Packs left.
    pub packs_after: usize,
}

impl LocalStore {
    /// Number of packs currently open.
    pub fn pack_count(&self) -> usize {
        self.packs.read().unwrap().len()
    }

    /// Merge packs of similar size (tiered, see module docs) into packs of `fmt`, ordering
    /// objects commits, trees, then files by path. The caller holds the repository lock.
    pub fn compact_packs(&self, fmt: pack::PackFormat) -> Result<LocalCompaction, StoreError> {
        self.compact_packs_with(fmt, LOCAL_TIER_RATIO, LOCAL_TIER_BASE, LOCAL_MAX_MERGE, true)
    }

    /// [`LocalStore::compact_packs`] with an explicit policy (a huge `base` makes everything
    /// tier 0, so a `ratio` of 2 merges all packs).
    pub fn compact_packs_with(
        &self,
        fmt: pack::PackFormat,
        ratio: u64,
        base: u64,
        max_bytes: u64,
        ordered: bool,
    ) -> Result<LocalCompaction, StoreError> {
        self.sync()?;
        let mut guard = self.packs.write().unwrap();
        let mut report = LocalCompaction::default();
        let sizes: Vec<(String, u64)> = guard
            .iter()
            .map(|p| {
                (
                    p.pack_path.to_string_lossy().into_owned(),
                    fs::metadata(&p.pack_path).map(|m| m.len()).unwrap_or(0),
                )
            })
            .collect();
        let plan = plan_tiers(&sizes, ratio, base, max_bytes);
        for group in plan {
            let members: HashSet<&str> = group.iter().map(String::as_str).collect();
            let (inputs, rest): (Vec<Pack>, Vec<Pack>) = std::mem::take(&mut *guard)
                .into_iter()
                .partition(|p| members.contains(p.pack_path.to_string_lossy().as_ref()));
            let r = (|| -> Result<Pack, StoreError> {
                let mut seen = HashSet::new();
                let mut objs = Vec::new();
                for p in &inputs {
                    for id in p.ids() {
                        if !seen.insert(id) {
                            continue;
                        }
                        let b = p.read(&id)?.ok_or(StoreError::NotFound(id))?;
                        decode_verified(&id, &b)?; // never repack something that does not hash to its name
                        objs.push((id, b));
                    }
                }
                let objs = if ordered { locality_order(objs) } else { objs };
                let n = objs.len();
                let np = pack::write_pack_ordered(&self.dir.join("pack"), &self.dir.join("tmp"), objs, fmt)?;
                report.objects += n;
                Ok(np)
            })();
            match r {
                Ok(np) => {
                    // the output exists durably; now drop the inputs, index first
                    let mut removal = Ok(());
                    for p in &inputs {
                        if p.pack_path == np.pack_path {
                            continue; // identical content: it IS the output
                        }
                        let idx_path = p.pack_path.with_extension("idx");
                        let r = io(&idx_path, remove_if_present(&idx_path)).and_then(|_| io(&p.pack_path, remove_if_present(&p.pack_path)));
                        if let Err(e) = r {
                            removal = Err(e);
                            break;
                        }
                        report.packs_removed += 1;
                    }
                    report.packs_written += 1;
                    report.merges += 1;
                    if removal.is_err() {
                        // a half-removed input: let the disk say what is left
                        *guard = Self::load_packs(&self.dir)?;
                        return removal.map(|_| report);
                    }
                    let mut keep = rest;
                    keep.push(np);
                    *guard = keep;
                }
                Err(e) => {
                    let mut keep = rest;
                    keep.extend(inputs);
                    *guard = keep;
                    return Err(e);
                }
            }
        }
        report.packs_after = guard.len();
        Ok(report)
    }
}
