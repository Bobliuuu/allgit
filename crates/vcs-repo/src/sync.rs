//! Remote operations: publish, fetch, sync, clone.
//!
//! Remote-tracking refs live in `.vcs/remote-refs` (same framing as `refs`) and record the
//! last remote heads this repository *observed*. Local branches are never moved by `fetch`.

use crate::error::{msg, Error, Result};
use crate::meta::{write_atomic, Refs, Workspace, Zones, ROOT_ZONE};
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use vcs_objects::{LockOp, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_remote::{fetch_heads_lazy, fetch_object, fetch_objects, load_manifest, Backend, Outcome, PublishError, Publisher};

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
        PublishError::LockedBy {
            zone,
            path,
            holder,
            expires_at,
        } => Error::Msg(format!(
            "Cannot publish: {path} (zone {zone}) is locked by {holder} until {expires_at} (unix ms).\n\nYour local commits are kept. Options:\n  ask {holder} to `vcs unlock {path}`, or wait for the lease to expire\n  drop your change to that path and publish the rest"
        )),
        PublishError::LockBroken { zone, path } => Error::Msg(format!(
            "Cannot publish: your lock on {path} (zone {zone}) is no longer valid (expired and re-taken, or broken).\n\nYour local commits are kept. Run `vcs locks` to see the current holder."
        )),
        other => Error::Msg(other.to_string()),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One row of `vcs locks`: zone display name, zone-relative path, lock record.
pub type LockRow = (String, String, vcs_remote::Lock);

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

    /// Is `anc` reachable from `desc` (or equal)? Uses the commit-graph when available.
    pub fn is_ancestor(&self, anc: ObjectId, desc: ObjectId) -> Result<bool> {
        // A short walk answers the common near-tip case without loading the graph; past the
        // budget (long or negative searches) the generation numbers prune the walk instead.
        let mut budget = 256usize;
        let mut seen = BTreeSet::new();
        let mut stack = vec![desc];
        while let Some(id) = stack.pop() {
            if id == anc {
                return Ok(true);
            }
            if seen.len() == budget {
                budget = usize::MAX;
                if let Some(g) = self.commit_graph(&[anc, desc]) {
                    return vcs_merge::is_ancestor(&g, &anc, &desc).map_err(|e| Error::Corrupt(e.to_string()));
                }
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

    /// Resolve user-supplied zone names (display names or ids) of a publish/fetch filter to ids.
    fn resolve_filter(&self, zones: Option<&[String]>) -> Result<Option<Vec<String>>> {
        let defs = self.zones()?;
        Ok(zones.map(|w| w.iter().map(|x| defs.resolve(x).unwrap_or_else(|| x.clone())).collect()))
    }

    /// Publish checked-out branches (all subscribed zones, or `zones`) in ONE atomic remote
    /// transaction. Refuses non-fast-forward; never overwrites remote work.
    pub fn publish(&self, backend: &dyn Backend, zones: Option<&[String]>, author: Signature) -> Result<PublishReport> {
        let principal = author.email.clone();
        self.publish_as(backend, zones, author, &principal)
    }

    /// Like [`Repo::publish`], acting as lock principal `principal`. The locks that principal
    /// holds in the published zones are presented with their fencing tokens.
    pub fn publish_as(&self, backend: &dyn Backend, zones: Option<&[String]>, author: Signature, principal: &str) -> Result<PublishReport> {
        let _l = self.lock()?;
        let defs = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let tracked = self.remote_refs()?;
        let (manifest, _) = load_manifest(backend).map_err(publish_err)?;
        let zones = self.resolve_filter(zones)?;
        let zones = zones.as_deref();
        let mut updates = Vec::new();
        let mut zone_defs = BTreeMap::new();
        let mut label_defs = BTreeMap::new();
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
                if defs.label(zone) != zone {
                    label_defs.insert(zone.clone(), defs.label(zone).to_owned());
                }
            }
        }
        if updates.is_empty() {
            return Ok(PublishReport {
                updated: vec![],
                outcome: None,
            });
        }
        updates.sort();
        let now = now_ms();
        let lock_ops: Vec<LockOp> = manifest
            .locks
            .iter()
            .filter(|((z, _), l)| l.holder == principal && l.expires_at > now && updates.iter().any(|u| &u.zone == z))
            .map(|((z, p), l)| LockOp::Present {
                zone: z.clone(),
                path: p.clone(),
                token: l.token,
            })
            .collect();
        let tx = Transaction {
            updates: updates.clone(),
            author,
            message: "publish".into(),
            lock_ops,
        };
        let outcome = Publisher::new(backend, &self.store, tx)
            .with_principal(principal)
            .with_zones(zone_defs)
            .with_labels(label_defs)
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

    /// Re-upload every object reachable from the local branch heads that the remote lacks, even
    /// beneath objects it has (a remote that lost a descendant). Unbounded: one existence check
    /// per reachable object. Moves no ref. Returns how many objects were uploaded.
    pub fn repair_remote(&self, backend: &dyn Backend) -> Result<usize> {
        let _l = self.lock()?;
        let refs = self.refs()?;
        let roots: Vec<ObjectId> = refs.branches.values().chain(refs.tags.values()).copied().collect();
        vcs_remote::repair_remote(backend, &self.store, &roots).map_err(publish_err)
    }

    /// Lock repo-relative `paths` (files) for `principal` for `lease_ms`, in one transaction.
    /// Returns (zone id, zone-relative path, fencing token) per path.
    pub fn lock_paths(
        &self,
        backend: &dyn Backend,
        paths: &[String],
        lease_ms: u64,
        reason: &str,
        author: Signature,
        principal: &str,
    ) -> Result<Vec<(String, String, u64)>> {
        let defs = self.zones()?;
        let ops: Vec<LockOp> = paths
            .iter()
            .map(|p| {
                let z = defs.owner(p).to_owned();
                LockOp::Acquire {
                    path: defs.relative(&z, p).to_owned(),
                    zone: z,
                    lease_ms,
                    reason: reason.to_owned(),
                }
            })
            .collect();
        self.lock_tx(backend, &ops, "lock", author, principal)?;
        let (m, _) = load_manifest(backend).map_err(publish_err)?;
        Ok(ops
            .iter()
            .filter_map(|o| {
                m.locks
                    .get(&(o.zone().to_owned(), o.path().to_owned()))
                    .map(|l| (o.zone().to_owned(), o.path().to_owned(), l.token))
            })
            .collect())
    }

    /// Release locks held by `principal` on repo-relative `paths`. With `admin_break`, remove
    /// them regardless of holder (unrestricted until authn/authz exists).
    pub fn unlock_paths(
        &self,
        backend: &dyn Backend,
        paths: &[String],
        admin_break: bool,
        author: Signature,
        principal: &str,
    ) -> Result<()> {
        let defs = self.zones()?;
        let (m, _) = load_manifest(backend).map_err(publish_err)?;
        let mut ops = Vec::new();
        for p in paths {
            let z = defs.owner(p).to_owned();
            let rel = defs.relative(&z, p).to_owned();
            let Some(l) = m.locks.get(&(z.clone(), rel.clone())) else {
                return msg(format!("{p} is not locked"));
            };
            ops.push(if admin_break {
                LockOp::Break { zone: z, path: rel }
            } else {
                LockOp::Release {
                    zone: z,
                    path: rel,
                    token: l.token,
                }
            });
        }
        self.lock_tx(backend, &ops, "unlock", author, principal)
    }

    /// Locks recorded in the remote manifest, with zone display names.
    pub fn list_locks(&self, backend: &dyn Backend) -> Result<Vec<LockRow>> {
        let defs = self.zones()?;
        let (m, _) = load_manifest(backend).map_err(publish_err)?;
        Ok(m.locks.into_iter().map(|((z, p), l)| (defs.label(&z).to_owned(), p, l)).collect())
    }

    fn lock_tx(&self, backend: &dyn Backend, ops: &[LockOp], what: &str, author: Signature, principal: &str) -> Result<()> {
        let tx = Transaction {
            updates: vec![],
            author,
            // unique per invocation: a later re-lock must not be deduplicated as "already applied"
            message: format!("{what} {}", now_ms()),
            lock_ops: ops.to_vec(),
        };
        Publisher::new(backend, &self.store, tx)
            .with_principal(principal)
            .run()
            .map_err(publish_err)?;
        Ok(())
    }

    /// Download objects for `zones` (default: subscribed) and update remote-tracking refs.
    /// Unsubscribed zones are not downloaded. Also learns new zone definitions.
    pub fn fetch(&self, backend: &dyn Backend, zones: Option<&[String]>) -> Result<Vec<(String, String, ObjectId)>> {
        let _l = self.lock()?;
        self.fetch_locked(backend, zones)
    }

    fn fetch_locked(&self, backend: &dyn Backend, zones: Option<&[String]>) -> Result<Vec<(String, String, ObjectId)>> {
        let ws = self.workspace()?;
        let zones = self.resolve_filter(zones)?;
        let want: Vec<String> = match zones.as_deref() {
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
        // Adopt a remote display name only where this repository has not renamed the zone itself
        // (or the name is taken locally); a local rename wins until it is published.
        for (id, l) in &m.labels {
            if defs.exists(id) && defs.label(id) == id && defs.set_label(id, l).is_ok() {
                changed = true;
            }
        }
        if changed {
            write_atomic(&self.vcs.join("zones"), &defs.encode())?;
        }
        let got = fetch_heads_lazy(backend, &self.store, Some(&want)).map_err(publish_err)?;
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

    /// [`Repo::zone_add`], but first fetches the branch head's files in one batch so the
    /// checkout does not stop at each missing blob. Uses the local branch head, else the
    /// remote-tracking head; with neither, behaves as `zone_add`.
    pub fn zone_add_fetching(&self, backend: &dyn Backend, zone: &str, branch: Option<&str>) -> Result<()> {
        let z = self.resolve_zone(zone)?;
        let b = branch.unwrap_or(crate::meta::DEFAULT_BRANCH);
        let head = self.refs()?.head(&z, b).or(self.remote_refs()?.head(&z, b));
        if let Some(h) = head {
            use vcs_store::ObjectStore;
            if self.store.contains(&h)? {
                self.prefetch_files(backend, self.commit_obj(&h)?.root)?;
            }
        }
        self.zone_add(&z, branch)
    }

    /// Download, in one batch, every file blob (and chunk of a chunked file) reachable from the
    /// tree `root` that is not already stored. Trees must be present (fetch stores them eagerly).
    /// Returns how many objects were fetched. Every object is hash-verified before it is stored.
    pub fn prefetch_files(&self, backend: &dyn Backend, root: ObjectId) -> Result<usize> {
        use vcs_objects::EntryMode;
        use vcs_store::ObjectStore;
        let mut trees = vec![root];
        let mut seen = BTreeSet::new();
        let mut files = BTreeSet::new();
        while let Some(t) = trees.pop() {
            if !seen.insert(t) {
                continue;
            }
            let Object::Tree(t) = self.store.get(&t)? else {
                return msg(format!("object {t} is not a tree"));
            };
            for e in t.entries() {
                if e.mode == EntryMode::Directory {
                    trees.push(e.id);
                } else if !self.store.contains(&e.id)? {
                    files.insert(e.id);
                }
            }
        }
        let mut total = 0;
        let mut batch: Vec<ObjectId> = files.into_iter().collect();
        while !batch.is_empty() {
            let mut next = Vec::new();
            for (id, obj) in fetch_objects(backend, &batch).map_err(publish_err)? {
                if let Object::Chunked(m) = &obj {
                    for c in &m.chunks {
                        if !self.store.contains(&c.id)? {
                            next.push(c.id);
                        }
                    }
                }
                self.store.put(&obj)?;
                debug_assert_eq!(obj.id(), id);
                total += 1;
            }
            batch = next;
        }
        self.store.sync()?;
        Ok(total)
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
                        self.prefetch_files(backend, self.commit_obj(&r)?.root)?;
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
            for (id, l) in &m.labels {
                if defs.exists(id) {
                    let _ = defs.set_label(id, l);
                }
            }
            write_atomic(&repo.vcs.join("zones"), &defs.encode())?;
            let want: Vec<String> = match zones {
                Some(a) => {
                    let mut ids = Vec::new();
                    for x in a {
                        match defs.resolve(x) {
                            Some(id) => ids.push(id),
                            None => return msg(format!("remote has no zone {x:?}")),
                        }
                    }
                    ids
                }
                None => defs.names().map(str::to_owned).collect(),
            };
            repo.save_ws(&Workspace::default())?;
            let got = fetch_heads_lazy(backend, &repo.store, Some(&want)).map_err(publish_err)?;
            let mut rr = Refs::default();
            let mut local = Refs::default();
            for (a, b, id) in &got {
                rr.branches.insert((a.clone(), b.clone()), *id);
                local.branches.insert((a.clone(), b.clone()), *id);
            }
            repo.save_remote_refs(&rr)?;
            write_atomic(&repo.vcs.join("refs"), &local.encode())?;
            drop(_l);
            // Checkout needs the default branch's files: fetch them in one batch, not one by one.
            for a in &want {
                if let Some(h) = local.head(a, crate::meta::DEFAULT_BRANCH) {
                    repo.prefetch_files(backend, repo.commit_obj(&h)?.root)?;
                }
            }
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
