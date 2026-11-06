//! Commit notes (`vcs notes`, `vcs git notes`): annotations attached to commits after the fact.
//!
//! A note is a plain Blob. The notes of one zone live in the tree of a commit on the internal ref
//! `(zone, NOTES_BRANCH)` (`refs/notes/commits`), keyed by the id of the annotated commit with a
//! two-hex-digit fan-out: `<aa>/<62 hex>` -> blob. Each change (add, append, remove) is a new commit
//! on that ref, so the history of the notes is kept and the ref is an ordinary branch head as far as
//! the object graph, `gc`, `fsck`, bundles and remotes are concerned. See docs/object-model.md.
//!
//! The ref is not a user branch: it is hidden from branch listings and cannot be created, switched to
//! or detached onto by name. `publish` and `fetch` carry it (see sync.rs); `fetch` fast-forwards or
//! merges it into the local ref ([`Repo::notes_reconcile`]).

use crate::error::{msg, Error, Result};
use crate::repo::{fresh_change_id, now_ns, Repo};
use std::collections::BTreeMap;
use vcs_objects::names::NOTES_BRANCH;
use vcs_objects::{Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Tree, TreeEntry, CHUNK_THRESHOLD};
use vcs_store::ObjectStore;

/// One note as listed: the annotated commit and the blob holding the note text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteInfo {
    pub commit: ObjectId,
    pub blob: ObjectId,
}

/// What [`Repo::notes_reconcile`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotesSync {
    Adopted,
    FastForwarded,
    Merged,
    Unchanged,
}

/// Normalize note text like git's `stripspace`: trailing blank space is dropped and exactly one
/// final newline is kept. Empty text is an error (`remove` deletes a note).
fn clean(text: &str) -> Result<Vec<u8>> {
    let t = text.trim_end_matches(['\n', '\r', ' ', '\t']);
    if t.trim().is_empty() {
        return msg("refusing to store an empty note (use `vcs notes remove` to delete one)");
    }
    let mut out = t.replace("\r\n", "\n").into_bytes();
    out.push(b'\n');
    if out.len() >= CHUNK_THRESHOLD {
        return msg("note is too large (limit 1 MiB)");
    }
    Ok(out)
}

fn split_key(hex: &str) -> (&str, &str) {
    hex.split_at(2)
}

fn system_signature() -> Signature {
    Signature {
        name: "vcs notes".into(),
        email: "notes@localhost".into(),
        timestamp: now_ns() / 1_000_000_000,
    }
}

impl Repo {
    fn notes_zone(&self, zone: &str) -> Result<String> {
        let zone = self.resolve_zone(zone)?;
        if !self.zones()?.exists(&zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        Ok(zone)
    }

    fn notes_tree(&self, id: &ObjectId) -> Result<Tree> {
        match self.store.get(id)? {
            Object::Tree(t) => Ok(t),
            _ => Err(Error::Corrupt(format!("notes: object {id} is not a tree"))),
        }
    }

    fn notes_blob_id_in(&self, root: &ObjectId, hex: &str) -> Result<Option<ObjectId>> {
        let (aa, rest) = split_key(hex);
        let top = self.notes_tree(root)?;
        let Some(dir) = top.get(aa) else { return Ok(None) };
        if dir.mode != EntryMode::Directory {
            return Err(Error::Corrupt(format!("notes: {aa} is not a directory")));
        }
        Ok(self.notes_tree(&dir.id)?.get(rest).map(|e| e.id))
    }

    /// New root with `hex` set to `blob` (or removed when `None`). Empty fan-out directories vanish.
    fn notes_tree_update(&self, root: Option<ObjectId>, hex: &str, blob: Option<ObjectId>) -> Result<ObjectId> {
        let (aa, rest) = split_key(hex);
        let mut top: Vec<TreeEntry> = match root {
            Some(r) => self.notes_tree(&r)?.into_entries(),
            None => vec![],
        };
        let at = top.iter().position(|e| e.name == aa);
        let mut sub: Vec<TreeEntry> = match at {
            Some(i) => self.notes_tree(&top[i].id)?.into_entries(),
            None => vec![],
        };
        sub.retain(|e| e.name != rest);
        if let Some(b) = blob {
            sub.push(TreeEntry {
                name: rest.to_owned(),
                mode: EntryMode::File,
                id: b,
            });
        }
        if let Some(i) = at {
            top.remove(i);
        }
        if !sub.is_empty() {
            let sid = self.store.put(&Object::Tree(Tree::new(sub)?))?;
            top.push(TreeEntry {
                name: aa.to_owned(),
                mode: EntryMode::Directory,
                id: sid,
            });
        }
        Ok(self.store.put(&Object::Tree(Tree::new(top)?))?)
    }

    fn notes_flat(&self, root: &ObjectId) -> Result<BTreeMap<String, ObjectId>> {
        let mut out = BTreeMap::new();
        for d in self.notes_tree(root)?.entries() {
            if d.mode != EntryMode::Directory {
                return Err(Error::Corrupt(format!("notes: unexpected entry {}", d.name)));
            }
            for e in self.notes_tree(&d.id)?.entries() {
                out.insert(format!("{}{}", d.name, e.name), e.id);
            }
        }
        Ok(out)
    }

    fn notes_build(&self, flat: &BTreeMap<String, ObjectId>) -> Result<ObjectId> {
        let mut dirs: BTreeMap<&str, Vec<TreeEntry>> = BTreeMap::new();
        for (k, id) in flat {
            let (aa, rest) = split_key(k);
            dirs.entry(aa).or_default().push(TreeEntry {
                name: rest.to_owned(),
                mode: EntryMode::File,
                id: *id,
            });
        }
        let mut top = Vec::new();
        for (aa, es) in dirs {
            let sid = self.store.put(&Object::Tree(Tree::new(es)?))?;
            top.push(TreeEntry {
                name: aa.to_owned(),
                mode: EntryMode::Directory,
                id: sid,
            });
        }
        Ok(self.store.put(&Object::Tree(Tree::new(top)?))?)
    }

    fn notes_blob_text(&self, id: &ObjectId) -> Result<Vec<u8>> {
        match self.store.get(id)? {
            Object::Blob(b) => Ok(b),
            _ => Err(Error::Corrupt(format!("notes: {id} is not a blob"))),
        }
    }

    /// The commit a revision names in `zone`, checked to be a commit of that zone.
    fn notes_target(&self, zone: &str, rev: &str) -> Result<ObjectId> {
        let id = self.resolve_rev(zone, rev)?;
        let c = self.commit_obj(&id)?;
        if c.zone != zone {
            return msg(format!("{rev} is not a commit of zone {zone}"));
        }
        Ok(id)
    }

    /// Current notes root of `zone` (None: no notes yet), with the ref head it came from.
    fn notes_state(&self, refs: &crate::meta::Refs, zone: &str) -> Result<(Option<ObjectId>, Option<ObjectId>)> {
        let head = refs.notes_head(zone);
        let root = match head {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        Ok((head, root))
    }

    /// Record a new notes commit and move the ref. The caller holds the repository lock.
    fn notes_advance(
        &self,
        zone: &str,
        refs: &mut crate::meta::Refs,
        root: ObjectId,
        parents: Vec<ObjectId>,
        (message, op): (&str, &str),
        author: Signature,
    ) -> Result<ObjectId> {
        let old = refs.notes_head(zone);
        let change_id = fresh_change_id(zone, &root, old.as_ref(), &author);
        let cid = self.store.put(&Object::Commit(Commit {
            zone: zone.to_owned(),
            parents,
            root,
            change_id,
            author,
            message: message.to_owned(),
        }))?;
        refs.apply(&[RefUpdate {
            zone: zone.to_owned(),
            branch: NOTES_BRANCH.to_owned(),
            old,
            new: cid,
        }])?;
        self.store.sync()?;
        self.commit_refs_op_lazy(refs, op, None)?;
        Ok(cid)
    }

    /// The note on `rev` in `zone`: the commit it is attached to and its text.
    pub fn notes_show(&self, zone: &str, rev: &str) -> Result<Option<(ObjectId, Vec<u8>)>> {
        let zone = self.notes_zone(zone)?;
        let target = self.notes_target(&zone, rev)?;
        let (_, root) = self.notes_state(&self.refs()?, &zone)?;
        let Some(root) = root else { return Ok(None) };
        match self.notes_blob_id_in(&root, &target.to_hex())? {
            Some(b) => Ok(Some((target, self.notes_blob_text(&b)?))),
            None => Ok(None),
        }
    }

    /// Every note of `zone`, ordered by the annotated commit's id.
    pub fn notes_list(&self, zone: &str) -> Result<Vec<NoteInfo>> {
        let zone = self.notes_zone(zone)?;
        let (_, root) = self.notes_state(&self.refs()?, &zone)?;
        let Some(root) = root else { return Ok(vec![]) };
        self.notes_flat(&root)?
            .into_iter()
            .map(|(k, blob)| {
                Ok(NoteInfo {
                    commit: ObjectId::from_hex(&k).map_err(|_| Error::Corrupt(format!("notes: bad key {k}")))?,
                    blob,
                })
            })
            .collect()
    }

    /// Text of a note blob returned by [`Repo::notes_list`].
    pub fn notes_read_blob(&self, blob: &ObjectId) -> Result<Vec<u8>> {
        self.notes_blob_text(blob)
    }

    /// Attach `text` to `rev`. Refuses when the commit already has a note unless `force`.
    pub fn notes_add(&self, zone: &str, rev: &str, text: &str, force: bool, author: Signature) -> Result<ObjectId> {
        let body = clean(text)?;
        let zone = self.notes_zone(zone)?;
        let _l = self.lock()?;
        let target = self.notes_target(&zone, rev)?;
        let mut refs = self.refs()?;
        let (head, root) = self.notes_state(&refs, &zone)?;
        let hex = target.to_hex();
        let had = match &root {
            Some(r) => self.notes_blob_id_in(r, &hex)?.is_some(),
            None => false,
        };
        if had && !force {
            return msg(format!(
                "cannot add notes: found existing notes for commit {}; use -f to overwrite, or `vcs notes append`",
                target.short()
            ));
        }
        let blob = self.store.put(&Object::Blob(body))?;
        let new_root = self.notes_tree_update(root, &hex, Some(blob))?;
        let what = if had { "overwritten" } else { "added" };
        self.notes_advance(
            &zone,
            &mut refs,
            new_root,
            head.into_iter().collect(),
            (
                &format!("Notes {what} by 'vcs notes add'"),
                &format!("notes add {zone} {}", target.short()),
            ),
            author,
        )?;
        Ok(target)
    }

    /// Append `text` to the note on `rev` (a blank line between the old text and the new), or
    /// create the note when there is none.
    pub fn notes_append(&self, zone: &str, rev: &str, text: &str, author: Signature) -> Result<ObjectId> {
        let body = clean(text)?;
        let zone = self.notes_zone(zone)?;
        let _l = self.lock()?;
        let target = self.notes_target(&zone, rev)?;
        let mut refs = self.refs()?;
        let (head, root) = self.notes_state(&refs, &zone)?;
        let hex = target.to_hex();
        let old = match &root {
            Some(r) => self.notes_blob_id_in(r, &hex)?,
            None => None,
        };
        let combined = match old {
            Some(b) => {
                let mut t = self.notes_blob_text(&b)?;
                t.push(b'\n');
                t.extend_from_slice(&body);
                if t.len() >= CHUNK_THRESHOLD {
                    return msg("note is too large (limit 1 MiB)");
                }
                t
            }
            None => body,
        };
        let blob = self.store.put(&Object::Blob(combined))?;
        let new_root = self.notes_tree_update(root, &hex, Some(blob))?;
        self.notes_advance(
            &zone,
            &mut refs,
            new_root,
            head.into_iter().collect(),
            (
                "Notes added by 'vcs notes append'",
                &format!("notes append {zone} {}", target.short()),
            ),
            author,
        )?;
        Ok(target)
    }

    /// Remove the note on `rev`. An error when there is none.
    pub fn notes_remove(&self, zone: &str, rev: &str, author: Signature) -> Result<ObjectId> {
        let zone = self.notes_zone(zone)?;
        let _l = self.lock()?;
        let target = self.notes_target(&zone, rev)?;
        let mut refs = self.refs()?;
        let (head, root) = self.notes_state(&refs, &zone)?;
        let hex = target.to_hex();
        let (Some(h), Some(root)) = (head, root) else {
            return msg(format!("commit {} has no note", target.short()));
        };
        if self.notes_blob_id_in(&root, &hex)?.is_none() {
            return msg(format!("commit {} has no note", target.short()));
        }
        let new_root = self.notes_tree_update(Some(root), &hex, None)?;
        self.notes_advance(
            &zone,
            &mut refs,
            new_root,
            vec![h],
            (
                "Notes removed by 'vcs notes remove'",
                &format!("notes remove {zone} {}", target.short()),
            ),
            author,
        )?;
        Ok(target)
    }

    /// Bring the local notes ref of `zone` up to the remote-tracking head `remote`: create it,
    /// fast-forward it, leave it alone when it is already ahead, or, when both moved, record a
    /// merge commit holding the union of both trees (a commit annotated on both sides gets the
    /// local text, a blank line, then the remote text; a note removed on one side comes back from
    /// the other, because there is no per-note base). The remote commit and its blobs must be
    /// stored. The caller holds the repository lock.
    pub(crate) fn notes_reconcile(&self, zone: &str, remote: ObjectId) -> Result<NotesSync> {
        let rc = self.commit_obj(&remote)?;
        if rc.zone != zone {
            return Err(Error::Corrupt(format!(
                "notes ref of zone {zone} points at a commit of zone {}",
                rc.zone
            )));
        }
        self.notes_tree(&rc.root)?;
        let mut refs = self.refs()?;
        let Some(local) = refs.notes_head(zone) else {
            refs.apply(&[RefUpdate {
                zone: zone.to_owned(),
                branch: NOTES_BRANCH.to_owned(),
                old: None,
                new: remote,
            }])?;
            self.commit_refs_op_lazy(&refs, &format!("notes adopt {zone} {}", remote.short()), None)?;
            return Ok(NotesSync::Adopted);
        };
        if local == remote || self.is_ancestor(remote, local)? {
            return Ok(NotesSync::Unchanged);
        }
        if self.is_ancestor(local, remote)? {
            refs.apply(&[RefUpdate {
                zone: zone.to_owned(),
                branch: NOTES_BRANCH.to_owned(),
                old: Some(local),
                new: remote,
            }])?;
            self.commit_refs_op_lazy(&refs, &format!("notes fast-forward {zone} {}", remote.short()), None)?;
            return Ok(NotesSync::FastForwarded);
        }
        let mut mine = self.notes_flat(&self.commit_obj(&local)?.root)?;
        for (k, theirs) in self.notes_flat(&rc.root)? {
            match mine.get(&k).copied() {
                None => {
                    mine.insert(k, theirs);
                }
                Some(m) if m == theirs => {}
                Some(m) => {
                    let mut t = self.notes_blob_text(&m)?;
                    t.push(b'\n');
                    t.extend_from_slice(&self.notes_blob_text(&theirs)?);
                    mine.insert(k, self.store.put(&Object::Blob(t))?);
                }
            }
        }
        let root = self.notes_build(&mine)?;
        self.notes_advance(
            zone,
            &mut refs,
            root,
            vec![local, remote],
            ("Merged notes from the remote", &format!("notes merge {zone} {}", remote.short())),
            system_signature(),
        )?;
        Ok(NotesSync::Merged)
    }
}
