//! Remote operations: publish, fetch, sync, clone.
//!
//! Remote-tracking refs live in `.vcs/remote-refs` (same framing as `refs`) and record the
//! last remote heads this repository *observed*. Local branches are never moved by `fetch`.

use crate::error::{msg, Error, Result};
use crate::meta::{write_atomic, Zones, Refs, Workspace, ROOT_ZONE};
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use vcs_objects::{Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_remote::{fetch_heads, fetch_object, load_manifest, Backend, Outcome, PublishError, Publisher};

#[derive(Debug)]
pub struct PublishReport {
    /// (zone, branch, old remote head, new remote head)
    pub updated: Vec<(String, String, Option<ObjectId>, ObjectId)>,
    pub outcome: Option<Outcome>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyncAction {
    UpToDate,
    FastForwarded {
        from: Option<ObjectId>,
        to: ObjectId,
    },
    /// Local branch has commits the remote lacks.
    Ahead,
    /// Both sides have new commits; needs merge/rebase (not implemented yet).
    Diverged,
    /// Zone has uncommitted changes; left untouched.
    Dirty,
    /// Nothing known on the remote for this branch.
    NoRemote,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub actions: Vec<(String, String, SyncAction)>,
}

fn publish_err(e: PublishError) -> Error {
    match e {
        PublishError::Diverged {
            zone,
            branch,
            expected,
            found,
        } => {
            let f = |i: Option<ObjectId>| i.map(|i| i.short()).unwrap_or_else(|| "(none)".into());
            Error::Msg(format!(
                "Cannot publish {zone}/{branch}:\n\nRemote branch moved from {} to {} while publishing.\n\nOptions:\n  vcs sync",
                f(expected),
                f(found)
            ))
        }
        other => Error::Msg(other.to_string()),
    }
}

impl Repo {
    pub fn remote_url(&self) -> Result<Option<String>> {
        match fs::read_to_string(self.vcs.join("remote")) {
            Ok(s) => Ok(Some(s.trim().to_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io {
                path: self.vcs.join("remote"),
                source,
            }),
        }
    }

    pub fn set_remote(&self, url: &str) -> Result<()> {
        if url.is_empty() || url.contains('\n') || url.contains('\0') {
            return msg("invalid remote url");
        }
        write_atomic(&self.vcs.join("remote"), url.as_bytes())
    }

    pub fn remote_refs(&self) -> Result<Refs> {
        let p = self.vcs.join("remote-refs");
        match fs::read(&p) {
            Ok(b) => Refs::decode(&b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Refs::default()),
            Err(source) => Err(Error::Io { path: p, source }),
        }
    }

    fn save_remote_refs(&self, r: &Refs) -> Result<()> {
        write_atomic(&self.vcs.join("remote-refs"), &r.encode())
    }

    /// If `zone/branch` has no local ref but is known from the remote, create the local
    /// branch at the remote-tracking head (like git's DWIM checkout). Caller holds the lock.
    pub(crate) fn adopt_tracking(&self, refs: &mut Refs, zone: &str, branch: &str) -> Result<bool> {
        if refs.head(zone, branch).is_some() {
            return Ok(false);
        }
        let Some(r) = self.remote_refs()?.head(zone, branch) else {
            return Ok(false);
        };
        refs.apply(&[RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old: None,
            new: r,
        }])?;
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
        Ok(true)
    }

    /// Is `anc` reachable from `desc` (or equal)? Linear in history; a commit-graph index is M5.
    pub fn is_ancestor(&self, anc: ObjectId, desc: ObjectId) -> Result<bool> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![desc];
        while let Some(id) = stack.pop() {
            if id == anc {
                return Ok(true);
            }
            if !seen.insert(id) {
                continue;
            }
            if !self.store.contains_commit(&id) {
                continue; // not available locally: cannot be a descendant path
            }
            stack.extend(self.commit_obj(&id)?.parents);
        }
        Ok(false)
    }

    /// Publish checked-out branches (all subscribed zones, or `zones`) in ONE atomic remote
    /// transaction. Refuses non-fast-forward; never overwrites remote work.
    pub fn publish(&self, backend: &dyn Backend, zones: Option<&[String]>, author: Signature) -> Result<PublishReport> {
        let _l = self.lock()?;
        let defs = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let tracked = self.remote_refs()?;
        let (manifest, _) = load_manifest(backend).map_err(publish_err)?;
        let mut updates = Vec::new();
        let mut zone_defs = BTreeMap::new();
        for (zone, branch) in &ws.subs {
            if zones.is_some_and(|w| !w.contains(zone)) {
                continue;
            }
            let Some(local) = refs.head(zone, branch) else { continue };
            let remote = manifest.head(zone, branch);
            if remote == Some(local) {
                continue;
            }
            // Unchanged since we last observed the remote: nothing of ours to publish, even if
            // the remote has since advanced (that is `sync`'s job, and must not block other zones).
            if tracked.head(zone, branch) == Some(local) {
                continue;
            }
            if let Some(r) = remote {
                if !self.is_ancestor(r, local)? {
                    return msg(format!(
                        "Cannot publish {zone}/{branch}:\n\nRemote branch advanced to {}.\n\nYour local commit: {}\nRemote commit:     {}\n\nOptions:\n  vcs sync",
                        r.short(),
                        local.short(),
                        r.short()
                    ));
                }
            }
            updates.push(RefUpdate {
                zone: zone.clone(),
                branch: branch.clone(),
                old: remote,
                new: local,
            });
            if zone != ROOT_ZONE {
                zone_defs.insert(zone.clone(), defs.path_of(zone).unwrap().to_owned());
            }
        }
        if updates.is_empty() {
            return Ok(PublishReport {
                updated: vec![],
                outcome: None,
            });
        }
        updates.sort();
        let tx = Transaction {
            updates: updates.clone(),
            author,
            message: "publish".into(),
        };
        let outcome = Publisher::new(backend, &self.store, tx)
            .with_zones(zone_defs)
            .run()
            .map_err(publish_err)?;
        let mut rr = self.remote_refs()?;
        rr.branches
            .extend(updates.iter().map(|u| ((u.zone.clone(), u.branch.clone()), u.new)));
        self.save_remote_refs(&rr)?;
        Ok(PublishReport {
            updated: updates.into_iter().map(|u| (u.zone, u.branch, u.old, u.new)).collect(),
            outcome: Some(outcome),
        })
    }

    /// Download objects for `zones` (default: subscribed) and update remote-tracking refs.
    /// Unsubscribed zones are not downloaded. Also learns new zone definitions.
    pub fn fetch(&self, backend: &dyn Backend, zones: Option<&[String]>) -> Result<Vec<(String, String, ObjectId)>> {
        let _l = self.lock()?;
        self.fetch_locked(backend, zones)
    }

    fn fetch_locked(&self, backend: &dyn Backend, zones: Option<&[String]>) -> Result<Vec<(String, String, ObjectId)>> {
        let ws = self.workspace()?;
        let want: Vec<String> = match zones {
            Some(a) => a.to_vec(),
            None => ws.subs.keys().cloned().collect(),
        };
        let (m, _) = load_manifest(backend).map_err(publish_err)?;
        let mut defs = self.zones()?;
        let mut changed = false;
        for (n, p) in &m.zones {
            match defs.path_of(n) {
                None => {
                    defs.create(n, p)?;
                    changed = true;
                }
                Some(q) if q == p => {}
                Some(q) => return msg(format!("zone {n:?} is at {q:?} locally but {p:?} on the remote")),
            }
        }
        if changed {
            write_atomic(&self.vcs.join("zones"), &defs.encode())?;
        }
        let got = fetch_heads(backend, &self.store, Some(&want)).map_err(publish_err)?;
        let mut rr = self.remote_refs()?;
        for (a, b, id) in &got {
            rr.branches.insert((a.clone(), b.clone()), *id);
        }
        self.save_remote_refs(&rr)?;
        Ok(got)
    }

    /// Lazily fetch one missing object: download, verify its hash, then store and sync it.
    /// A corrupt remote object is rejected before anything is written locally.
    pub fn fetch_missing(&self, backend: &dyn Backend, id: &ObjectId) -> Result<()> {
        let obj = fetch_object(backend, id).map_err(publish_err)?;
        use vcs_store::ObjectStore;
        self.store.put(&obj)?;
        self.store.sync()?;
        Ok(())
    }

    /// Run `f`; whenever it fails because an object is absent locally, fetch that object from
    /// the remote and run it again. A corrupt local object is never refetched (see docs/cache.md),
    /// and an id that is still missing after being fetched is an error, so this terminates.
    pub fn with_lazy_fetch<T>(&self, backend: &dyn Backend, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let mut fetched = BTreeSet::new();
        loop {
            match f() {
                Err(Error::Store(vcs_store::StoreError::NotFound(id))) if fetched.insert(id) => {
                    self.fetch_missing(backend, &id)?;
                }
                r => return r,
            }
        }
    }

    /// Fetch, then fast-forward each subscribed zone's checked-out branch when safe.
    pub fn sync(&self, backend: &dyn Backend) -> Result<SyncReport> {
        let _l = self.lock()?;
        self.fetch_locked(backend, None)?;
        let defs = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let rr = self.remote_refs()?;
        let mut ix = self.index()?;
        let mut rep = SyncReport::default();
        let subs: Vec<(String, String)> = ws.subs.iter().map(|(a, b)| (a.clone(), b.clone())).collect();
        for (zone, branch) in subs {
            let local = refs.head(&zone, &branch);
            let act = match (local, rr.head(&zone, &branch)) {
                (_, None) => SyncAction::NoRemote,
                (l, Some(r)) if l == Some(r) => SyncAction::UpToDate,
                (Some(l), Some(r)) if self.is_ancestor(r, l)? => SyncAction::Ahead,
                (l, Some(r)) if l.is_none() || self.is_ancestor(l.unwrap(), r)? => {
                    let st = self.status()?;
                    let a = st.zones.iter().find(|x| x.zone == zone).expect("subscribed");
                    if !a.staged.is_empty() || !a.unstaged.is_empty() {
                        SyncAction::Dirty
                    } else {
                        let old = self.head_flat(&zone, &defs, &ws, &refs)?;
                        let new = treeops::flatten(&self.store, Some(self.commit_obj(&r)?.root), defs.path_of(&zone).unwrap())?;
                        self.transition(&zone, &defs, &old, &new, &mut ix)?;
                        // index/worktree first, ref last: a crash in between leaves a clean-looking
                        // diff against the old head, never a lost commit.
                        self.save_index(&mut ix)?;
                        refs.apply(&[RefUpdate {
                            zone: zone.clone(),
                            branch: branch.clone(),
                            old: l,
                            new: r,
                        }])?;
                        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
                        SyncAction::FastForwarded { from: l, to: r }
                    }
                }
                _ => SyncAction::Diverged,
            };
            rep.actions.push((zone, branch, act));
        }
        let _ = &mut ws;
        let moved: Vec<String> = rep
            .actions
            .iter()
            .filter(|(_, _, a)| matches!(a, SyncAction::FastForwarded { .. }))
            .map(|(a, b, _)| format!("{a}/{b}"))
            .collect();
        if !moved.is_empty() {
            self.record_op(&format!("sync: fast-forward {}", moved.join(", ")), None)?;
        }
        Ok(rep)
    }

    /// Create a new repository at `dest` from the remote, materializing only `zones`
    /// (default: all remote zones plus root). Unlisted zones are defined but not downloaded.
    pub fn clone_from(backend: &dyn Backend, url: &str, dest: &Path, zones: Option<&[String]>) -> Result<Repo> {
        let (m, _) = load_manifest(backend).map_err(publish_err)?;
        if dest.exists() && fs::read_dir(dest).map(|mut d| d.next().is_some()).unwrap_or(true) {
            return msg(format!("destination {} exists and is not empty", dest.display()));
        }
        let repo = Repo::init(dest)?;
        let r = (|| -> Result<()> {
            repo.set_remote(url)?;
            let _l = repo.lock()?;
            let mut defs = Zones::new();
            for (n, p) in &m.zones {
                defs.create(n, p)?;
            }
            write_atomic(&repo.vcs.join("zones"), &defs.encode())?;
            let want: Vec<String> = match zones {
                Some(a) => {
                    for x in a {
                        if !defs.exists(x) {
                            return msg(format!("remote has no zone {x:?}"));
                        }
                    }
                    a.to_vec()
                }
                None => defs.names().map(str::to_owned).collect(),
            };
            repo.save_ws(&Workspace::default())?;
            let got = fetch_heads(backend, &repo.store, Some(&want)).map_err(publish_err)?;
            let mut rr = Refs::default();
            let mut local = Refs::default();
            for (a, b, id) in &got {
                rr.branches.insert((a.clone(), b.clone()), *id);
                local.branches.insert((a.clone(), b.clone()), *id);
            }
            repo.save_remote_refs(&rr)?;
            write_atomic(&repo.vcs.join("refs"), &local.encode())?;
            drop(_l);
            for a in &want {
                repo.zone_add(a, None)?;
            }
            Ok(())
        })();
        r.map(|()| repo)
    }
}

// small helper so is_ancestor can skip commits we never fetched
trait ContainsCommit {
    fn contains_commit(&self, id: &ObjectId) -> bool;
}
impl ContainsCommit for vcs_store::LocalStore {
    fn contains_commit(&self, id: &ObjectId) -> bool {
        use vcs_store::ObjectStore;
        matches!(self.get(id), Ok(Object::Commit(_)))
    }
}
