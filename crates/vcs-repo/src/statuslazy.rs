//! `status` without decoding the index into a map.
//!
//! The reference path (`Repo::status_with`) turns the whole index into a `BTreeMap` (about 285 bytes
//! per tracked file) and a `Vec` of references to it before it stats the first file. This one keeps
//! the index as the mapped `AGI5` image:
//!
//! * staged changes come from the cache-tree walk over the image (`IndexTrees`), which decodes only
//!   the directories whose cached tree id differs from HEAD's;
//! * unstaged changes are found by a block-parallel scan of the mapped base (each worker decodes one
//!   block of `BLOCK5` records at a time and drops it), keeping only the changed paths;
//! * the untracked walk asks the image for the tracked names of one directory at a time;
//! * the racy-entry refresh (`written_ns`) is computed from two minima gathered during the scan
//!   instead of a second pass over a decoded map.
//!
//! The answer equals the reference's (tests compare them); older index layouts fall back to it.

use crate::error::Result;
use crate::index::IndexEntry;
use crate::indexview::IndexView;
use crate::repo::{zones_fp, Change, Repo, Status, StatusOpts, Wt, ZoneStatus};
use crate::treeops;
use crate::ChangeKind;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use vcs_objects::EntryMode;

/// `fetch_min` that first looks (a shared read) so a hot minimum does not bounce a cache line.
fn lower(slot: &AtomicU64, v: u64) {
    if v < slot.load(Ordering::Relaxed) {
        slot.fetch_min(v, Ordering::Relaxed);
    }
}

impl Repo {
    /// The reference `status` (whole index decoded into a map), for tests that compare it with the lazy one.
    #[doc(hidden)]
    pub fn status_eager(&self, opts: StatusOpts) -> Result<Status> {
        let ix = self.index()?;
        self.status_inner(opts, &ix, true, None)
    }

    /// `status_with` over the mapped index; `None` when the index is not an `AGI5` image.
    pub(crate) fn status_lazy(&self, opts: StatusOpts) -> Result<Option<Status>> {
        let img = self.index_image()?;
        let Some(view) = IndexView::of_image(&img)? else { return Ok(None) };
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let fp = zones_fp(&zones);
        let single_zone = zones.names().count() <= 1;
        let old = img.written_ns;
        let racy = self.index_racy_ns;

        // One pass over the entries serves every subscribed zone.
        let sub_idx: BTreeMap<&str, usize> = ws.subs.keys().enumerate().map(|(i, z)| (z.as_str(), i)).collect();
        let zone_names: Vec<&String> = ws.subs.keys().collect();
        let m_any = AtomicU64::new(u64::MAX); // lowest racy edge above `old` (could the refresh unlock anything?)
        let m_unver = AtomicU64::new(u64::MAX); // lowest such edge not verified clean by this scan (what holds it back)
        let blocks: Vec<usize> = (0..view_blocks(&view)).collect();
        let threads = crate::writer_threads::status_threads().min(8);
        let work = |cur: &mut crate::worktree_fs::StatCursor, p: &str, e: &IndexEntry| -> Result<Option<(usize, Change)>> {
            let edge = if e.mode == EntryMode::Symlink { 0 } else { e.stat_edge(racy) };
            let racy_entry = edge > old;
            if racy_entry {
                lower(&m_any, edge);
            }
            let zi = if single_zone {
                (!sub_idx.is_empty()).then_some(0)
            } else {
                sub_idx.get(zones.owner(p)).copied()
            };
            let Some(zi) = zi.filter(|&zi| !ws.skips(zone_names[zi], p)) else {
                if racy_entry {
                    lower(&m_unver, edge);
                }
                return Ok(None);
            };
            let (state, read) = self.wt_state_seq(cur, p, e, old)?;
            if racy_entry && !(matches!(state, Wt::Clean) && read) {
                lower(&m_unver, edge);
            }
            let kind = match state {
                Wt::Clean => return Ok(None),
                Wt::Modified => ChangeKind::Modified,
                Wt::Deleted => ChangeKind::Deleted,
            };
            vcs_objects::names::validate_path(p)?;
            Ok(Some((zi, Change { path: p.to_owned(), kind })))
        };
        let changed = view.par_filter_map(&blocks, threads, &work)?;
        let mut unstaged: Vec<Vec<Change>> = (0..zone_names.len()).map(|_| Vec::new()).collect();
        for (zi, c) in changed {
            unstaged[zi].push(c);
        }

        let mut st = Status::default();
        for (zi, (zone, branch)) in ws.subs.iter().enumerate() {
            let head_root = match self.current_head(zone, &ws, &refs) {
                Some(h) => Some(self.commit_obj(&h)?.root),
                None => None,
            };
            let cached = img.trees()?.filter(|(f, _)| *f == fp).map(|(_, m)| m);
            let mut it = treeops::IndexTrees::new(&self.store, &img, &zones, zone, cached);
            let staged: Vec<Change> = it
                .diff(zones.path_of(zone).unwrap_or(""), head_root)?
                .into_iter()
                .map(|(path, kind)| Change { path, kind })
                .collect();
            let staged = self.drop_layout_only(staged, head_root, &zones, zone, &|p| img.get(p).ok().flatten())?;
            st.zones.push(ZoneStatus {
                zone: zone.clone(),
                path: zones.path_of(zone).unwrap_or("").to_owned(),
                branch: branch.clone(),
                head: self.current_head(zone, &ws, &refs),
                staged,
                unstaged: std::mem::take(&mut unstaged[zi]),
            });
        }
        for n in zones.names() {
            if !ws.subs.contains_key(n) {
                st.unsubscribed.push(n.to_owned());
            }
        }

        // Racy refresh, as `Index::settled_written_ns` / `unlocks` compute it (best effort).
        let (any, unver) = (m_any.load(Ordering::Relaxed), m_unver.load(Ordering::Relaxed));
        let now = crate::repo::now_ns();
        let next = old.max(if unver <= now { unver - 1 } else { now });
        if any <= next {
            if let Ok(_l) = self.lock() {
                if let Ok(cur) = self.index_image() {
                    if cur.digest() == img.digest() {
                        let _ = cur.commit(&self.vcs, vec![crate::indeximage::Op::WrittenNs(next)]);
                    }
                }
            }
        }

        let (untracked, unsupported) = self.scan_untracked(opts, &img, &zones, &ws)?;
        st.untracked = untracked;
        st.unsupported = unsupported;
        st.merging = self.merge_state()?;
        Ok(Some(st))
    }
}

fn view_blocks(v: &IndexView<'_>) -> usize {
    v.block_count()
}
