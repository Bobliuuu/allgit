//! `vcs prefetch` (#117): download, in batches, the file blobs of the branch heads a workspace
//! subscribes to, so the workspace works offline afterwards.

use crate::error::Result;
use crate::repo::Repo;
use vcs_remote::Backend;
use vcs_store::ObjectStore;

impl Repo {
    /// Fetch every missing file blob (and chunk) of the head of each subscribed zone's branch
    /// (the local head and the remote-tracking head; respects the zone's sparse set). `zones`
    /// restricts to those zones. Trees and commits must be present (`fetch` stores them); a zone
    /// whose head commit is not stored is skipped. Returns how many objects were fetched. Blobs
    /// are hash-verified before they are stored; a failing backend leaves the store with only
    /// verified objects and the worktree untouched.
    pub fn prefetch_subscribed(&self, backend: &dyn Backend, zones: Option<&[String]>) -> Result<usize> {
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let rr = self.remote_refs()?;
        let mut total = 0;
        for (zone, branch) in ws.subs.iter() {
            if zones.is_some_and(|z| !z.contains(zone)) {
                continue;
            }
            // the checked-out branch head and the remote-tracking head (what `sync` would move to)
            let mut heads = vec![refs.head(zone, branch), rr.head(zone, branch)];
            heads.dedup();
            for h in heads.into_iter().flatten() {
                if self.store.contains(&h)? {
                    total += self.prefetch_zone_files(backend, zone, self.commit_obj(&h)?.root)?;
                }
            }
        }
        Ok(total)
    }
}
