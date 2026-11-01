//! **DESTRUCTIVE** tiered compaction of remote packs (issue #248, docs/gc.md "Compaction").
//!
//! A publish writes one small pack, so a long-lived remote accumulates hundreds of them and every
//! reader pays one index GET per pack. [`compact`] merges packs of similar size into one (a
//! tiered policy: [`plan_tiers`]) and deletes the inputs.
//!
//! # Pack discovery: LIST, no catalog
//!
//! Packs are found by listing `packs/`; the manifest does not name them. That stays, because the
//! compaction rule below needs no agreement with publishers or readers beyond what a `LIST` plus
//! the existing immutable, content-named packs already give. (A catalog versioned with the manifest
//! CAS would make every compaction a manifest write and a publisher/compactor conflict; it buys
//! nothing the rules below do not.)
//!
//! # Safety rules (each is model-checked and mutation-checked in `tests/compact_real.rs`)
//!
//! * **C1 union**: the output holds every object of every input (checked by reading the uploaded
//!   index back before anything is deleted). Compaction never drops anything; garbage is GC's job.
//! * **C2 output first**: the new pack, then its index, are durable (and verified) before the first
//!   input is deleted. The index is the commit point of a pack (no index, no objects), so a crash
//!   at any step leaves duplicates, never a missing object.
//! * **C3 index first**: an input is removed by deleting its `.idx`, then its `.pack`. Readers
//!   enumerate indexes, so none is ever pointed at a vanishing pack.
//! * **C4 readers retry**: a reader that finds an index or pack gone re-lists and looks again
//!   (`packfetch`), so a compaction that finishes between its LIST and its GET costs a retry, not
//!   a missing object. By C2 the objects are in the output, which that re-list sees.
//! * **C5 one at a time**: on a backend with a trusted clock, compaction holds `gc/lock`, the
//!   sweeper's lock, so a sweeper never sees half a compaction. (Without a clock there is no
//!   sweeper either, and two compactors are harmless: C1 makes the merge idempotent.)
//!
//! Publishers need nothing new: a publish that deduplicated against an input only relies on the
//! object existing, and C1/C2 keep it existing. Pack age is laundered (the output is young), so
//! garbage in an input waits one more grace period before a sweep can take it.

use crate::backend::{Backend, BackendError};
use crate::gc::{acquire, GcError};
use crate::packfetch::PACK_PREFIX;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_store::pack::{build_pack_blocks, BlockCache, PackFormat, PackIndex, BLOCK_TARGET};

/// Tier ratio: a tier holds packs within a factor of this of each other; this many in one tier
/// are merged (simulation: 1,000 equal publishes compacted after each end at 6 packs for 6.3x the
/// published bytes; tests/compact_real.rs asserts the bounds with the real planner).
pub const DEFAULT_TIER_RATIO: u64 = 3;
/// Packs up to this size are all tier 0.
pub const DEFAULT_TIER_BASE: u64 = 4 * 1024;
/// A pack is built in memory: never merge more than this in one pass.
pub const DEFAULT_MAX_MERGE_BYTES: u64 = 512 * 1024 * 1024;
/// `vcs publish` suggests compaction when the remote holds more than this many packs.
pub const SUGGEST_PACKS: usize = 24;

#[derive(Clone, Debug)]
pub struct CompactOptions {
    pub tier_ratio: u64,
    pub tier_base: u64,
    pub max_merge_bytes: u64,
    /// Report the plan only: no lock, no writes.
    pub dry_run: bool,
    /// Merge every pack into one regardless of tiers (`--all`).
    pub all: bool,
    pub lock_ttl_ms: u64,
    pub owner: String,
    /// Format of the output pack: v3 (independently compressed blocks) unless `VCS_PACK_FORMAT`
    /// says v2 or v1 (the rollback; see [`PackFormat::for_compaction`]).
    pub format: PackFormat,
    /// Uncompressed bytes per v3 block.
    pub block_bytes: usize,
    /// Put commits and trees first and file content grouped by path (`vcs_store::layout`).
    pub ordered: bool,
}

impl Default for CompactOptions {
    fn default() -> Self {
        CompactOptions {
            tier_ratio: DEFAULT_TIER_RATIO,
            tier_base: DEFAULT_TIER_BASE,
            max_merge_bytes: DEFAULT_MAX_MERGE_BYTES,
            dry_run: true,
            all: false,
            lock_ttl_ms: 6 * 3600 * 1000,
            owner: "compact".into(),
            format: PackFormat::for_compaction(),
            block_bytes: BLOCK_TARGET,
            ordered: true,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct CompactReport {
    pub dry_run: bool,
    /// Packs with an index when the run started.
    pub packs_before: usize,
    /// Packs left after the run (dry run: after the plan).
    pub packs_after: usize,
    /// One entry per merge: (input stems, output stem; empty in a dry run).
    pub merges: Vec<(Vec<String>, String)>,
    pub objects_read: usize,
    pub objects_written: usize,
    /// Objects present in more than one input (dropped from the output).
    pub duplicates: usize,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub deleted_packs: Vec<String>,
}

/// Points at which a test can interleave other actors with the compactor.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactPoint {
    /// Inputs read and the output built; nothing written.
    Planned,
    PackPut,
    IdxPut,
    Verified,
    /// About to delete the n-th key of the removal sequence.
    BeforeDelete(usize),
    Done,
}

/// Deliberately broken variants for the mutation tests. Never set outside tests.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactQuirks {
    /// Delete the inputs before the output exists (breaks C2).
    pub delete_before_upload: bool,
    /// Delete an input's `.pack` before its `.idx` (breaks C3).
    pub delete_pack_before_idx: bool,
    /// Upload the output index before the output pack (index without pack).
    pub idx_before_pack: bool,
    /// Leave one object out of the output (breaks C1) ...
    pub drop_object: bool,
    /// ... and do not read the output back before deleting.
    pub skip_verify: bool,
}

/// A pack found on the remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInfo {
    pub stem: String,
    pub len: u64,
}

/// The groups of packs to merge (each of at least two); see [`vcs_store::layout::plan_tiers`].
pub fn plan_tiers(packs: &[PackInfo], ratio: u64, base: u64, max_bytes: u64) -> Vec<Vec<String>> {
    let items: Vec<(String, u64)> = packs.iter().map(|p| (p.stem.clone(), p.len)).collect();
    vcs_store::layout::plan_tiers(&items, ratio, base, max_bytes)
}

/// List the complete packs (those with an index) and their sizes: one LIST and one GET per index.
pub fn load_packs(b: &dyn Backend) -> Result<Vec<(PackInfo, PackIndex)>, GcError> {
    let mut v = Vec::new();
    for k in b.list(PACK_PREFIX)? {
        let Some(stem) = k.strip_suffix(".idx") else { continue };
        let Some(g) = b.get(&k)? else { continue };
        let idx = PackIndex::parse(&g.data, Path::new(stem)).map_err(|e| GcError::Damaged(format!("{k}: {e}")))?;
        v.push((
            PackInfo {
                stem: stem.to_owned(),
                len: idx.pack_len,
            },
            idx,
        ));
    }
    Ok(v)
}

/// Number of packs on the remote (one LIST; counts `.idx` keys).
pub fn count_packs(b: &dyn Backend) -> Result<usize, BackendError> {
    Ok(b.list(PACK_PREFIX)?.iter().filter(|k| k.ends_with(".idx")).count())
}

/// Read and verify one whole pack: length and trailer against its index, every entry against its
/// id. Returns (id, encoded bytes) in file order.
fn read_pack(b: &dyn Backend, p: &PackInfo, idx: &PackIndex) -> Result<Vec<(ObjectId, Vec<u8>)>, GcError> {
    let damaged = |why: String| GcError::Damaged(format!("pack {}: {why}", p.stem));
    let Some(g) = b.get(&format!("{}.pack", p.stem))? else {
        return Err(damaged("pack file missing (changed under the compaction; run it again)".into()));
    };
    if g.data.len() as u64 != idx.pack_len || g.data.len() < 32 {
        return Err(damaged("length differs from its index".into()));
    }
    let (body, trailer) = g.data.split_at(g.data.len() - 32);
    if trailer != idx.checksum() || blake3::hash(body).as_bytes() != idx.checksum() {
        return Err(damaged("checksum differs from its index".into()));
    }
    let mut ents: Vec<(ObjectId, u64, u32)> = idx.iter().collect();
    ents.sort_by_key(|e| e.1);
    let mut out = Vec::with_capacity(ents.len());
    let mut blocks = BlockCache::default();
    for (id, off, len) in ents {
        let s = off as usize;
        let slice = g
            .data
            .get(s..s + len as usize)
            .ok_or_else(|| damaged(format!("entry for {id} outside the pack")))?;
        let enc = idx
            .decode_entry_cached(&id, off, slice, &mut blocks)
            .map_err(|e| damaged(format!("{id}: {e}")))?;
        if id_of_encoded(&enc) != id {
            return Err(damaged(format!("{id} fails hash verification")));
        }
        out.push((id, enc.into_owned()));
    }
    Ok(out)
}

/// **DESTRUCTIVE unless `opts.dry_run`.** Plan and run one compaction pass.
pub fn compact(b: &dyn Backend, opts: &CompactOptions) -> Result<CompactReport, GcError> {
    compact_with(b, opts, CompactQuirks::default(), &mut |_| {})
}

/// [`compact`] with quirks and an interleaving hook (tests).
#[doc(hidden)]
pub fn compact_with(
    b: &dyn Backend,
    opts: &CompactOptions,
    quirks: CompactQuirks,
    hook: &mut dyn FnMut(CompactPoint),
) -> Result<CompactReport, GcError> {
    let mut rep = CompactReport {
        dry_run: opts.dry_run,
        ..Default::default()
    };
    // C5. The lock needs the backend's clock; without one there is no sweeper to exclude.
    let _lock = if opts.dry_run || !b.supports_gc() {
        None
    } else {
        Some(acquire(b, &opts.owner, opts.lock_ttl_ms, b.now_ms()?)?)
    };
    if !opts.dry_run {
        // Fail before doing any work on a backend that cannot delete (idempotent on absent keys).
        b.delete("compact/probe")?;
    }
    let loaded = load_packs(b)?;
    rep.packs_before = loaded.len();
    let infos: Vec<PackInfo> = loaded.iter().map(|(p, _)| p.clone()).collect();
    let groups = if opts.all {
        if infos.len() >= 2 {
            vec![infos.iter().map(|p| p.stem.clone()).collect()]
        } else {
            vec![]
        }
    } else {
        plan_tiers(&infos, opts.tier_ratio, opts.tier_base, opts.max_merge_bytes)
    };
    let merged_away: usize = groups.iter().map(|g| g.len() - 1).sum();
    rep.packs_after = rep.packs_before - merged_away;
    if opts.dry_run {
        for g in groups {
            rep.merges.push((g, String::new()));
        }
        return Ok(rep);
    }
    let by_stem: BTreeMap<&str, &(PackInfo, PackIndex)> = loaded.iter().map(|l| (l.0.stem.as_str(), l)).collect();
    for g in groups {
        // Read and verify the inputs.
        let mut union: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for stem in &g {
            let (p, idx) = by_stem[stem.as_str()];
            rep.bytes_read += p.len;
            for (id, enc) in read_pack(b, p, idx)? {
                rep.objects_read += 1;
                if seen.insert(id) {
                    union.push((id, enc));
                } else {
                    rep.duplicates += 1;
                }
            }
        }
        let want: Vec<ObjectId> = union.iter().map(|o| o.0).collect();
        if quirks.drop_object && union.len() > 1 {
            union.pop();
        }
        let union = if opts.ordered {
            vcs_store::layout::locality_order(union)
        } else {
            union
        };
        let built = build_pack_blocks(union, opts.format, opts.block_bytes);
        let new_stem = format!("{PACK_PREFIX}{}", built.name);
        hook(CompactPoint::Planned);

        let mut delete_seq: Vec<String> = Vec::new();
        for stem in g.iter().filter(|s| **s != new_stem) {
            if quirks.delete_pack_before_idx {
                delete_seq.push(format!("{stem}.pack"));
                delete_seq.push(format!("{stem}.idx"));
            } else {
                delete_seq.push(format!("{stem}.idx"));
                delete_seq.push(format!("{stem}.pack"));
            }
        }
        let run_deletes = |hook: &mut dyn FnMut(CompactPoint)| -> Result<(), GcError> {
            for (n, k) in delete_seq.iter().enumerate() {
                hook(CompactPoint::BeforeDelete(n));
                b.delete(k)?;
            }
            Ok(())
        };
        if quirks.delete_before_upload {
            run_deletes(hook)?;
        }
        // C2: output pack, then index (the commit point).
        let (pk, ik) = (format!("{new_stem}.pack"), format!("{new_stem}.idx"));
        if quirks.idx_before_pack {
            b.put_if_absent(&ik, &built.idx)?;
            hook(CompactPoint::IdxPut);
            b.put_if_absent(&pk, &built.pack)?;
            hook(CompactPoint::PackPut);
        } else {
            b.put_if_absent(&pk, &built.pack)?;
            hook(CompactPoint::PackPut);
            b.put_if_absent(&ik, &built.idx)?;
            hook(CompactPoint::IdxPut);
        }
        rep.bytes_written += (built.pack.len() + built.idx.len()) as u64;
        // C1: read the index back from the remote and require every input object in it.
        if !quirks.skip_verify {
            let got = b
                .get(&ik)?
                .ok_or_else(|| GcError::Damaged(format!("{ik} missing right after upload")))?;
            let idx = PackIndex::parse(&got.data, Path::new(&new_stem)).map_err(|e| GcError::Damaged(format!("{ik}: {e}")))?;
            if let Some(m) = want.iter().find(|id| idx.find(id).is_none()) {
                return Err(GcError::Damaged(format!(
                    "compacted pack {new_stem} lacks {m}; inputs left in place"
                )));
            }
            let tail = b.get_range(&pk, idx.pack_len.saturating_sub(32), 33)?;
            if tail.as_deref() != Some(&idx.checksum()[..]) {
                return Err(GcError::Damaged(format!(
                    "compacted pack {pk} is not complete on the remote; inputs left in place"
                )));
            }
        }
        hook(CompactPoint::Verified);
        rep.objects_written += want.len();
        // C3: inputs go, index first.
        if !quirks.delete_before_upload {
            run_deletes(hook)?;
        }
        rep.deleted_packs.extend(g.iter().filter(|s| **s != new_stem).cloned());
        rep.merges.push((g, new_stem));
        hook(CompactPoint::Done);
    }
    Ok(rep)
}
