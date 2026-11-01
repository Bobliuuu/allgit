//! `stash`: set tracked work aside and bring it back through the merge engine.
//!
//! A stash entry is, per zone that had changes, a commit object holding the working-tree
//! content of every tracked path (parent: the zone head the work was based on), plus an entry
//! in the ref list `.vcs/stash` (framed, atomic, newest first). `push` then resets the working
//! tree and index of those zones to their heads. `pop` three-way merges
//! (base = recorded parent, ours = current head, theirs = stashed content) with the same
//! engine `merge` uses; the result is staged. A pop that conflicts leaves the stash entry in
//! place and writes marker files that show up as unstaged modifications.
//!
//! Untracked files are not stashed. Stash commits are garbage-collection roots.

use crate::error::{io, msg, Error, Result};
use crate::meta::{frame, unframe, write_atomic};
use crate::repo::{fresh_change_id, Repo};
use crate::treeops::{self, Flat};
use std::fs;
use vcs_merge::{merge_flat_with, Algorithm, ConflictKind, MarkerLabels, StoreBlobs};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::{Commit, EntryMode, Object, ObjectId, Signature};
use vcs_store::ObjectStore;

const FILE: &str = "stash";
const MAGIC: &[u8; 4] = b"AGSH";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashZone {
    pub zone: String,
    pub branch: String,
    /// Zone head the stashed work was based on (None: the branch had no commits).
    pub base: Option<ObjectId>,
    /// Commit holding the stashed working-tree content.
    pub work: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashEntry {
    pub message: String,
    pub time_ns: u64,
    pub zones: Vec<StashZone>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StashPopOutcome {
    /// Applied cleanly (changes are staged) and the entry was dropped.
    Applied,
    /// Applied with conflicts in these paths; the entry was kept.
    Conflicted { paths: Vec<String> },
}

fn dec(e: vcs_objects::DecodeError) -> Error {
    Error::Corrupt(format!("stash: {e}"))
}

fn encode(list: &[StashEntry]) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(list.len() as u64);
    for e in list {
        w.str(&e.message).varint(e.time_ns).varint(e.zones.len() as u64);
        for z in &e.zones {
            w.str(&z.zone).str(&z.branch);
            match &z.base {
                Some(b) => {
                    w.u8(1).id(b);
                }
                None => {
                    w.u8(0);
                }
            }
            w.id(&z.work);
        }
    }
    frame(MAGIC, w.finish())
}

fn decode(bytes: &[u8]) -> Result<Vec<StashEntry>> {
    let body = unframe(MAGIC, bytes, "stash")?;
    let mut r = Reader::new(body);
    let n = r.count(3).map_err(dec)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let message = r.str(1 << 16).map_err(dec)?;
        let time_ns = r.varint().map_err(dec)?;
        let nz = r.count(35).map_err(dec)?;
        let mut zones = Vec::with_capacity(nz);
        for _ in 0..nz {
            let zone = r.str(256).map_err(dec)?;
            let branch = r.str(256).map_err(dec)?;
            let base = match r.u8().map_err(dec)? {
                0 => None,
                1 => Some(r.id().map_err(dec)?),
                _ => return Err(Error::Corrupt("stash: bad base flag".into())),
            };
            let work = r.id().map_err(dec)?;
            zones.push(StashZone { zone, branch, base, work });
        }
        out.push(StashEntry { message, time_ns, zones });
    }
    r.finish().map_err(dec)?;
    Ok(out)
}

impl Repo {
    /// Stash entries, newest first (index 0 is what `pop` takes by default).
    pub fn stash_list(&self) -> Result<Vec<StashEntry>> {
        match fs::read(self.vcs.join(FILE)) {
            Ok(b) => decode(&b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(Error::Io {
                path: self.vcs.join(FILE),
                source,
            }),
        }
    }

    fn save_stash(&self, list: &[StashEntry]) -> Result<()> {
        self.store.sync()?; // the stash commits must be durable before the list names them
        write_atomic(&self.vcs.join(FILE), &encode(list))
    }

    /// Commits the stash list pins (garbage-collection roots).
    pub(crate) fn stash_roots(&self) -> Result<Vec<ObjectId>> {
        let mut v = Vec::new();
        for e in self.stash_list()? {
            for z in e.zones {
                v.push(z.work);
                v.extend(z.base);
            }
        }
        Ok(v)
    }

    /// Working-tree content of every tracked path of `zone` (deleted files are absent).
    fn work_flat(&self, zone: &str, zones: &crate::meta::Zones, ix: &crate::index::Index) -> Result<Flat> {
        let mut flat = Flat::new();
        let ws = self.workspace()?;
        for (p, e) in ix.entries.iter().filter(|(p, _)| zones.owner(p) == zone) {
            if ws.skips(zone, p) {
                flat.insert(p.clone(), (e.mode, e.id)); // not materialized: unchanged by definition
                continue;
            }
            let abs = self.worktree.join(p);
            let m = match fs::symlink_metadata(&abs) {
                Ok(m) => m,
                Err(x) if x.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::Io { path: abs, source }),
            };
            if m.file_type().is_symlink() {
                // a link is stashed as a link (its target bytes), never followed or refused
                let target = io(&abs, crate::repo::read_link_bytes(&abs))?;
                flat.insert(p.clone(), (EntryMode::Symlink, self.store.put_blob(&target)?));
                continue;
            }
            if !m.is_file() {
                return msg(format!("cannot stash: {p:?} is no longer a regular file"));
            }
            let data = io(&abs, fs::read(&abs))?;
            let id = self.store.put_blob(&data)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if m.permissions().mode() & 0o111 != 0 {
                    EntryMode::Executable
                } else {
                    EntryMode::File
                }
            };
            #[cfg(not(unix))]
            let mode = e.mode;
            let _ = e;
            flat.insert(p.clone(), (mode, id));
        }
        Ok(flat)
    }

    /// Save the tracked changes (staged and unstaged) of every subscribed zone and reset those
    /// zones to their heads. Returns the zones that were stashed.
    pub fn stash_push(&self, message: Option<&str>, author: Signature) -> Result<Vec<String>> {
        let _l = self.lock()?;
        self.rebase_guard("stash")?;
        if self.merge_state()?.is_some() {
            return msg("cannot stash in the middle of a merge; resolve and commit, or `vcs merge --abort`");
        }
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let mut ix = self.index()?;
        struct Job {
            zone: String,
            branch: String,
            base: Option<ObjectId>,
            work: Flat,
            head: Flat,
        }
        let mut jobs = Vec::new();
        for (zone, branch) in &ws.subs {
            let head = self.head_flat(zone, &zones, &ws, &refs)?;
            let work = self.work_flat(zone, &zones, &ix)?;
            if work != head {
                jobs.push(Job {
                    zone: zone.clone(),
                    branch: branch.clone(),
                    base: refs.head(zone, branch),
                    work,
                    head,
                });
            }
        }
        if jobs.is_empty() {
            return msg("no local changes to stash");
        }
        let desc = match message {
            Some(m) if !m.trim().is_empty() => m.trim().to_owned(),
            _ => format!(
                "WIP on {}",
                jobs.iter()
                    .map(|j| format!("{}/{}", j.zone, j.branch))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let mut sz = Vec::new();
        for j in &jobs {
            let entries: Vec<(&str, EntryMode, ObjectId)> = j.work.iter().map(|(p, (m, i))| (zones.relative(&j.zone, p), *m, *i)).collect();
            let root = treeops::build(&self.store, entries)?;
            let c = Commit {
                zone: j.zone.clone(),
                parents: j.base.into_iter().collect(),
                root,
                change_id: fresh_change_id(&j.zone, &root, j.base.as_ref(), &author),
                author: author.clone(),
                message: format!("stash: {desc}"),
            };
            let work = self.store.put(&Object::Commit(c))?;
            sz.push(StashZone {
                zone: j.zone.clone(),
                branch: j.branch.clone(),
                base: j.base,
                work,
            });
        }
        let mut list = self.stash_list()?;
        list.insert(
            0,
            StashEntry {
                message: desc.clone(),
                time_ns: author.timestamp,
                zones: sz,
            },
        );
        // List first (objects are already stored): a crash afterwards loses nothing.
        self.save_stash(&list)?;
        for j in &jobs {
            self.transition(&j.zone, &zones, &j.work, &j.head, &mut ix)?;
            // The index must now equal the head exactly (staged-only content and files deleted
            // from the working tree are not covered by the work -> head transition).
            ix.entries.retain(|p, _| zones.owner(p) != j.zone || j.head.contains_key(p));
            for (p, (mode, id)) in &j.head {
                if ix.entries.get(p).map(|e| e.id) != Some(*id) {
                    let abs = self.worktree.join(p);
                    let m = io(&abs, fs::metadata(&abs))?;
                    ix.entries.remove(p);
                    ix.insert(
                        p,
                        crate::index::IndexEntry {
                            id: *id,
                            mode: *mode,
                            size: m.len(),
                            mtime_ns: 0, // unknown stat: always re-hashed
                            ctime_ns: 0,
                        },
                    )?;
                }
            }
        }
        self.save_index(&mut ix)?;
        self.record_op(&format!("stash push: {desc}"), None)?;
        Ok(jobs.into_iter().map(|j| j.zone).collect())
    }

    /// Apply stash entry `n` (0 = newest) to the working tree and drop it, unless it conflicts.
    pub fn stash_pop(&self, n: usize) -> Result<StashPopOutcome> {
        let _l = self.lock()?;
        self.rebase_guard("stash pop")?;
        let mut list = self.stash_list()?;
        if n >= list.len() {
            return msg(format!("no such stash entry {n} ({} saved)", list.len()));
        }
        let entry = list[n].clone();
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let status = self.status()?;
        // Plan everything before touching any file.
        struct Plan {
            zone: String,
            ours: Flat,
            new: Flat,
            conflicts: Vec<String>,
        }
        let mut plans = Vec::new();
        for z in &entry.zones {
            if !ws.subs.contains_key(&z.zone) {
                return msg(format!("zone {:?} of this stash is not subscribed in this workspace", z.zone));
            }
            self.require_clean(&z.zone, &status)?;
            let prefix = zones.path_of(&z.zone).unwrap_or("").to_owned();
            let flat_of = |id: &ObjectId| -> Result<Flat> { treeops::flatten(&self.store, Some(self.commit_obj(id)?.root), &prefix) };
            let base = match z.base {
                Some(b) => flat_of(&b)?,
                None => Flat::new(),
            };
            let theirs = flat_of(&z.work)?;
            let ours = self.head_flat(&z.zone, &zones, &ws, &refs)?;
            let tm = merge_flat_with(&StoreBlobs(&self.store), Algorithm::Myers, self.structural, &base, &ours, &theirs)
                .map_err(|e| Error::Msg(format!("stash merge: {e}")))?;
            if let Some(c) = tm.conflicts.iter().find(|c| c.kind == ConflictKind::FileDirectory) {
                return msg(format!(
                    "{:?} is a file on one side and a directory on the other; file/directory conflicts are not supported yet (nothing was changed)",
                    c.path
                ));
            }
            for (id, data) in &tm.new_blobs {
                if self.store.put_blob(data)? != *id {
                    return Err(Error::Corrupt("merged blob id mismatch".into()));
                }
            }
            self.require_changes_materialized(&z.zone, &ours, &theirs, "stash pop")?;
            let mut new = tm.entries.clone();
            let labels = MarkerLabels {
                left: "Updated upstream",
                base: "base",
                right: "Stashed changes",
            };
            for c in &tm.conflicts {
                let (mode, id, _) = self.placeholder(c, &labels)?;
                new.insert(c.path.clone(), (mode, id));
            }
            plans.push(Plan {
                zone: z.zone.clone(),
                ours,
                new,
                conflicts: tm.conflicts.iter().map(|c| c.path.clone()).collect(),
            });
        }
        let mut ix = self.index()?;
        let mut conflicted = Vec::new();
        for p in &plans {
            self.transition(&p.zone, &zones, &p.ours, &p.new, &mut ix)?;
            for path in &p.conflicts {
                // Show the conflict as an unstaged change against the head, not as staged work.
                ix.entries.remove(path);
                if let Some((mode, id)) = p.ours.get(path) {
                    ix.insert(
                        path,
                        crate::index::IndexEntry {
                            id: *id,
                            mode: *mode,
                            size: 0,
                            mtime_ns: 0,
                            ctime_ns: 0,
                        },
                    )?;
                }
                conflicted.push(path.clone());
            }
        }
        self.save_index(&mut ix)?;
        if conflicted.is_empty() {
            list.remove(n);
            self.save_stash(&list)?;
            self.record_op(&format!("stash pop: {}", entry.message), None)?;
            Ok(StashPopOutcome::Applied)
        } else {
            self.record_op(&format!("stash pop (conflicts, kept): {}", entry.message), None)?;
            conflicted.sort();
            Ok(StashPopOutcome::Conflicted { paths: conflicted })
        }
    }

    /// Forget stash entry `n` (0 = newest). Its objects stay until garbage collected.
    pub fn stash_drop(&self, n: usize) -> Result<StashEntry> {
        let _l = self.lock()?;
        let mut list = self.stash_list()?;
        if n >= list.len() {
            return msg(format!("no such stash entry {n} ({} saved)", list.len()));
        }
        let e = list.remove(n);
        self.save_stash(&list)?;
        self.record_op(&format!("stash drop: {}", e.message), None)?;
        Ok(e)
    }
}
