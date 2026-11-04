//! `shelve` / `unshelve`: park working-tree changes on the remote as a private, named shelf and
//! pick them up elsewhere. The remote side (keys, meta, GC roots, verification) is
//! `vcs_remote::shelf`; docs/shelves.md has the design.
//!
//! A shelf holds, per zone with changes, an ephemeral commit (parent: the zone head the work
//! was based on) that is never added to any branch. Objects the remote already has (everything
//! reachable from a base commit that is on the remote) are left out of the shelf's pack, and
//! the base commit becomes a GC root for as long as the shelf lives. `unshelve` verifies every
//! object, then applies the changes through the same three-way machinery as `stash pop`.

use crate::error::{msg, Error, Result};
use crate::gc::children;
use crate::repo::{fresh_change_id, Repo};
use crate::stash::{StashEntry, StashPopOutcome, StashZone};
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use vcs_objects::{Commit, EntryMode, Object, ObjectId, Signature};
use vcs_remote::shelf::{self, ShelfMeta, ShelfPack, ShelfZone};
use vcs_remote::Backend;
use vcs_store::pack::{build_pack_fmt, PackFormat, PackIndex};
use vcs_store::ObjectStore;

fn serr(e: shelf::ShelfError) -> Error {
    Error::Msg(e.to_string())
}

fn now_ms(b: &dyn Backend) -> u64 {
    b.now_ms().unwrap_or_else(|_| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    })
}

/// Optional settings of [`Repo::shelve`].
#[derive(Clone, Debug, Default)]
pub struct ShelveOpts<'a> {
    pub message: Option<&'a str>,
    /// Shelve only this zone (id or label).
    pub zone: Option<&'a str>,
    /// Expiry, relative to now, that remote GC may act on.
    pub expires_in_ms: Option<u64>,
}

impl Repo {
    /// Shelve the tracked changes (staged and unstaged) of the subscribed zones (or just `zone`)
    /// as `principal/name` on `b`. The working tree is not changed. `expires_in_ms` sets an expiry
    /// that remote GC may act on. Returns the stored meta.
    pub fn shelve(&self, b: &dyn Backend, principal: &str, name: &str, opts: &ShelveOpts, author: Signature) -> Result<ShelfMeta> {
        let ShelveOpts {
            message,
            zone,
            expires_in_ms,
        } = *opts;
        shelf::validate_principal(principal).map_err(serr)?;
        shelf::validate_name(name).map_err(serr)?;
        let _l = self.lock()?;
        self.rebase_guard("shelve")?;
        if self.merge_state()?.is_some() {
            return msg("cannot shelve in the middle of a merge; resolve and commit, or `vcs merge --abort`");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let ix = self.index()?;
        let only = match zone {
            Some(z) => Some(zones.resolve(z).ok_or_else(|| Error::Msg(format!("no such zone {z:?}")))?),
            None => None,
        };
        let mut szs: Vec<ShelfZone> = Vec::new();
        for (z, branch) in &ws.subs {
            if only.as_ref().is_some_and(|o| o != z) {
                continue;
            }
            let head = self.head_flat(z, &zones, &ws, &refs)?;
            let work = self.work_flat(z, &zones, &ix)?;
            if work == head {
                continue;
            }
            let base = refs.head(z, branch);
            let entries: Vec<(&str, EntryMode, ObjectId)> = work.iter().map(|(p, (m, i))| (zones.relative(z, p), *m, *i)).collect();
            let root = treeops::build(&self.store, entries)?;
            let c = Commit {
                zone: z.clone(),
                parents: base.into_iter().collect(),
                root,
                change_id: fresh_change_id(z, &root, base.as_ref(), &author),
                author: author.clone(),
                message: format!("shelf: {}", message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or(name)),
            };
            let commit = self.store.put(&Object::Commit(c))?;
            szs.push(ShelfZone {
                zone: z.clone(),
                branch: branch.clone(),
                base,
                commit,
            });
        }
        if szs.is_empty() {
            return msg("no local changes to shelve");
        }
        self.store.sync()?;
        // Which base commits does the remote already have? Those (and everything below them) stay out of the pack.
        let wanted: Vec<ObjectId> = szs.iter().filter_map(|z| z.base).collect();
        let on_remote: BTreeSet<ObjectId> = vcs_remote::packfetch::probe_kinds(b, &wanted)
            .map_err(|e| Error::Msg(format!("remote: {e}")))?
            .into_iter()
            .filter(|(_, k)| k.is_some())
            .map(|(i, _)| i)
            .collect();
        let mut have: BTreeSet<ObjectId> = BTreeSet::new();
        let mut stack: Vec<ObjectId> = on_remote.iter().copied().collect();
        while let Some(id) = stack.pop() {
            if have.insert(id) {
                children(&self.store.get(&id)?, &mut stack);
            }
        }
        let mut objs: BTreeMap<ObjectId, Vec<u8>> = BTreeMap::new();
        let mut stack: Vec<ObjectId> = szs.iter().map(|z| z.commit).collect();
        while let Some(id) = stack.pop() {
            if have.contains(&id) || objs.contains_key(&id) {
                continue;
            }
            let o = self.store.get(&id).map_err(|e| match e {
                vcs_store::StoreError::NotFound(i) => Error::Msg(format!(
                    "cannot shelve: object {i} is not in this repository (a sparse clone must fetch it first)"
                )),
                e => e.into(),
            })?;
            children(&o, &mut stack);
            objs.insert(id, o.encode());
        }
        let built = build_pack_fmt(objs.into_iter().collect(), PackFormat::V2);
        let idx = PackIndex::parse(&built.idx, Path::new("shelf")).map_err(|e| Error::Msg(format!("shelf index: {e}")))?;
        let created = now_ms(b);
        let meta = ShelfMeta {
            principal: principal.to_owned(),
            name: name.to_owned(),
            message: message.map(str::trim).filter(|m| !m.is_empty()).unwrap_or(name).to_owned(),
            author: format!("{} <{}>", author.name, author.email),
            created_ms: created,
            expires_ms: expires_in_ms.map(|d| created.saturating_add(d)),
            zones: szs,
            bases: on_remote.into_iter().collect(),
            objects: idx.len() as u32,
            pack_bytes: built.pack.len() as u64,
            pack_checksum: shelf::checksum_hex(&idx),
        };
        shelf::upload(
            b,
            &meta,
            &ShelfPack {
                pack: built.pack,
                idx: built.idx,
            },
        )
        .map_err(serr)?;
        Ok(meta)
    }

    /// Fetch shelf `principal/name`, verify every object, and apply it to the working tree like
    /// `stash pop` (changes staged; conflicts leave markers). The shelf stays on the remote.
    pub fn unshelve(&self, b: &dyn Backend, principal: &str, name: &str) -> Result<(ShelfMeta, StashPopOutcome)> {
        let _l = self.lock()?;
        self.rebase_guard("unshelve")?;
        let meta = shelf::load_meta(b, principal, name).map_err(serr)?;
        let objs = shelf::download(b, &meta).map_err(serr)?;
        let have: BTreeMap<ObjectId, Object> = objs
            .iter()
            .map(|(id, enc)| {
                Object::decode(enc)
                    .map(|o| (*id, o))
                    .map_err(|e| Error::Corrupt(format!("shelf object {id}: {e}")))
            })
            .collect::<Result<_>>()?;
        // Shape checks before anything is stored.
        for z in &meta.zones {
            match have.get(&z.commit) {
                Some(Object::Commit(c)) if c.zone == z.zone && c.parents == z.base.into_iter().collect::<Vec<_>>() => {}
                _ => {
                    return Err(Error::Corrupt(format!(
                        "shelf {principal}/{name}: commit for zone {} is missing or inconsistent",
                        z.zone
                    )))
                }
            }
        }
        for (id, o) in &have {
            if self.store.put(o)? != *id {
                return Err(Error::Corrupt(format!("shelf object {id} is not canonical")));
            }
        }
        // Bring in what the shelf needs from the shared store and the local repo lacks (verified fetch).
        let mut wave: Vec<ObjectId> = meta.zones.iter().flat_map(|z| z.base).collect();
        let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
        while !wave.is_empty() {
            let mut missing = Vec::new();
            let mut next = Vec::new();
            for id in wave {
                if !seen.insert(id) {
                    continue;
                }
                if let Some(o) = have.get(&id) {
                    children(o, &mut next);
                } else if !self.store.contains(&id)? {
                    missing.push(id);
                }
            }
            if !missing.is_empty() {
                for (id, o) in vcs_remote::fetch_objects(b, &missing).map_err(|e| Error::Msg(format!("remote: {e}")))? {
                    if self.store.put(&o)? != id {
                        return Err(Error::Corrupt(format!("fetched object {id} is not canonical")));
                    }
                    children(&o, &mut next);
                }
            }
            wave = next;
        }
        let entry = StashEntry {
            message: meta.message.clone(),
            time_ns: 0,
            zones: meta
                .zones
                .iter()
                .map(|z| StashZone {
                    zone: z.zone.clone(),
                    branch: z.branch.clone(),
                    base: z.base,
                    work: z.commit,
                })
                .collect(),
        };
        self.store.sync()?;
        let out = self.apply_entry(&entry, "unshelve", "Shelved changes")?;
        self.record_op(&format!("unshelve {principal}/{name}"), None)?;
        Ok((meta, out))
    }
}
