//! Subscribe to several zones in one step (used by `clone`).

use crate::error::{msg, Result};
use crate::meta::DEFAULT_BRANCH;
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::BTreeSet;

impl Repo {
    /// [`Repo::zone_add`] for many zones at once, on their default branches. The result is the
    /// same as adding them one by one, but the checks run once over all of them, the files of
    /// all zones are written by one bounded pool, and the index, workspace and operation log
    /// are each written once (one operation, `zone add <n> zones`) instead of once per zone.
    /// Caller holds the repository lock. A workspace with sparse sets, or fewer than two zones,
    /// takes the one-by-one path.
    pub(crate) fn zone_add_many_locked(&self, names: &[String]) -> Result<()> {
        let mut ws = self.workspace()?;
        if names.len() < 2 || !ws.sparse.is_empty() {
            for n in names {
                self.zone_add_locked(n, None)?;
            }
            return Ok(());
        }
        let zones = self.zones()?;
        let mut refs = self.refs()?;
        let mut ids: Vec<String> = Vec::with_capacity(names.len());
        let mut seen = BTreeSet::new();
        let mut merged = Flat::new();
        for n in names {
            let zone = self.resolve_zone(n)?;
            if !zones.exists(&zone) {
                return msg(format!("no such zone {zone:?}"));
            }
            if ws.subs.contains_key(&zone) || !seen.insert(zone.clone()) {
                return msg(format!("zone {zone:?} is already subscribed"));
            }
            self.require_branch_free(&zone, DEFAULT_BRANCH)?;
            self.adopt_tracking(&mut refs, &zone, DEFAULT_BRANCH)?;
            if let Some(h) = refs.head(&zone, DEFAULT_BRANCH) {
                let new = treeops::flatten(&self.store, Some(self.commit_obj(&h)?.root), zones.path_of(&zone).unwrap())?;
                self.transition_check_paths_ws(&ws, &zone, &zones, &Flat::new(), &new)?;
                merged.extend(new);
            }
            ids.push(zone);
        }
        let mut ix = self.index()?;
        crate::portable::require_no_collision(
            merged.keys().map(String::as_str).chain(ix.entries.keys().map(String::as_str)),
            "checkout",
        )?;
        // The zone name only selects sparse sets, and there are none here.
        self.transition_apply_ws(&ws, ids.first().map_or("", String::as_str), &Flat::new(), &merged, &mut ix)?;
        for z in &ids {
            ws.subs.insert(z.clone(), DEFAULT_BRANCH.into());
        }
        self.save_index(&mut ix)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("zone add {} zones", ids.len()), None)?;
        Ok(())
    }
}
