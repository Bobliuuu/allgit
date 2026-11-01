//! Tags, revision lookup, `show` and `blame`. See docs/cli.md.
//!
//! Tags are per zone, because commits are. A lightweight tag is a ref straight to a commit; an
//! annotated tag is a ref to an `AnnotatedTag` object that names the commit and carries a
//! message, a tagger and an opaque signature field. Refs are stored in the `refs` file.

use crate::diff;
use crate::error::{msg, Error, Result};
use crate::meta::write_atomic;
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::names::validate_branch_name;
use vcs_objects::{AnnotatedTag, Commit, EntryMode, Object, ObjectId, Signature};
use vcs_store::ObjectStore;

/// One tag as listed.
#[derive(Debug, Clone)]
pub struct TagInfo {
    pub name: String,
    /// The commit the tag points at (annotated tags are peeled).
    pub commit: ObjectId,
    /// Present for annotated tags.
    pub annotation: Option<AnnotatedTag>,
}

/// A commit with its diff against the first parent.
#[derive(Debug, Clone)]
pub struct ShowReport {
    pub id: ObjectId,
    pub commit: Commit,
    /// Set when the revision named an annotated tag.
    pub tag: Option<AnnotatedTag>,
    /// Unified diff against the first parent (against the empty tree for a root commit).
    pub diff: String,
}

/// One attributed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: ObjectId,
    /// 1-based.
    pub line_no: usize,
    pub text: String,
    /// Repository-relative path the line had in `commit` (differs from the blamed path when the
    /// file was renamed and rename following is on).
    pub path: String,
}

impl Repo {
    fn known_zone(&self, zone: &str) -> Result<String> {
        let zone = self.resolve_zone(zone)?;
        if !self.zones()?.exists(&zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        Ok(zone)
    }

    /// Follow a tag ref to (commit, annotation).
    pub(crate) fn peel(&self, zone: &str, id: ObjectId) -> Result<(ObjectId, Option<AnnotatedTag>)> {
        match self.store.get(&id)? {
            Object::Commit(c) if c.zone == zone => Ok((id, None)),
            Object::Tag(t) if t.zone == zone => {
                self.commit_obj(&t.target)?;
                Ok((t.target, Some(t)))
            }
            _ => Err(Error::Corrupt(format!("tag target {id} is not a commit or tag of zone {zone}"))),
        }
    }

    /// Create a tag in `zone` at `rev` (default: the zone's current head). With `annotation`
    /// `(message, tagger, signature)` an annotated tag object is stored; otherwise the ref points
    /// straight at the commit. Refuses an existing name. Returns the commit tagged.
    pub fn tag_create(
        &self,
        zone: &str,
        name: &str,
        rev: Option<&str>,
        annotation: Option<(&str, Signature, Vec<u8>)>,
    ) -> Result<ObjectId> {
        validate_branch_name(name)?;
        let zone = self.known_zone(zone)?;
        let _l = self.lock()?;
        let mut refs = self.refs()?;
        let key = (zone.clone(), name.to_owned());
        if refs.tags.contains_key(&key) {
            return msg(format!("tag {zone}/{name} already exists; delete it first with `vcs tag -d`"));
        }
        let commit = self.resolve_rev(&zone, rev.unwrap_or("HEAD"))?;
        let target = match annotation {
            None => commit,
            Some((message, tagger, signature)) => {
                if signature.len() > vcs_objects::object::MAX_TAG_SIGNATURE {
                    return msg("tag signature is too long");
                }
                self.store.put(&Object::Tag(AnnotatedTag {
                    zone: zone.clone(),
                    name: name.to_owned(),
                    target: commit,
                    tagger,
                    message: message.to_owned(),
                    signature,
                }))?
            }
        };
        refs.tags.insert(key, target);
        self.store.sync()?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        let kind = if target == commit { "" } else { " (annotated)" };
        self.record_op(&format!("tag create {zone}/{name}{kind} at {}", commit.short()), None)?;
        Ok(commit)
    }

    /// Delete a tag. The commit and any annotation object stay in the store. Returns the commit
    /// it pointed at.
    pub fn tag_delete(&self, zone: &str, name: &str) -> Result<ObjectId> {
        let zone = self.known_zone(zone)?;
        let _l = self.lock()?;
        let mut refs = self.refs()?;
        let Some(id) = refs.tags.remove(&(zone.clone(), name.to_owned())) else {
            return msg(format!("no such tag {zone}/{name}"));
        };
        let (commit, _) = self.peel(&zone, id)?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.record_op(&format!("tag delete {zone}/{name} (was {})", commit.short()), None)?;
        Ok(commit)
    }

    /// Tags of a zone, sorted by name.
    pub fn tag_list(&self, zone: &str) -> Result<Vec<TagInfo>> {
        let zone = self.known_zone(zone)?;
        let refs = self.refs()?;
        let mut out = Vec::new();
        for ((a, n), id) in &refs.tags {
            if *a == zone {
                let (commit, annotation) = self.peel(&zone, *id)?;
                out.push(TagInfo {
                    name: n.clone(),
                    commit,
                    annotation,
                });
            }
        }
        Ok(out)
    }

    /// Resolve a revision of `zone` to a commit. Forms: `HEAD`, a tag, a local branch, or a hex
    /// commit-id prefix (at least 4 digits, unambiguous among the zone's commits reachable from
    /// its branches and tags), optionally followed by `~N` and/or `^` (first parent).
    pub fn resolve_rev(&self, zone: &str, rev: &str) -> Result<ObjectId> {
        let zone = self.known_zone(zone)?;
        let split = rev.find(['~', '^']).unwrap_or(rev.len());
        let (base, mut suffix) = rev.split_at(split);
        let mut cur = self.resolve_base(&zone, base)?;
        while !suffix.is_empty() {
            let steps = if let Some(r) = suffix.strip_prefix('^') {
                suffix = r;
                1
            } else if let Some(r) = suffix.strip_prefix('~') {
                let digits = r.bytes().take_while(u8::is_ascii_digit).count();
                suffix = &r[digits..];
                if digits == 0 {
                    1
                } else {
                    r[..digits]
                        .parse::<usize>()
                        .map_err(|_| Error::Msg(format!("bad revision {rev:?}")))?
                }
            } else {
                return msg(format!("bad revision {rev:?}"));
            };
            if steps >= 64 {
                // a long way back: follow first parents in the commit-graph cache instead of reading each commit
                if let Some(g) = self.commit_graph(&[cur]) {
                    let mut c = cur;
                    let mut ok = true;
                    for _ in 0..steps {
                        match vcs_merge::CommitGraph::parents(&g, &c).ok().and_then(|p| p.first().copied()) {
                            Some(p) => c = p,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        cur = c;
                        continue;
                    }
                }
            }
            for _ in 0..steps {
                match self.commit_obj(&cur)?.parents.first() {
                    Some(p) => cur = *p,
                    None => return msg(format!("revision {rev:?}: {} has no parent", cur.short())),
                }
            }
        }
        Ok(cur)
    }

    fn resolve_base(&self, zone: &str, base: &str) -> Result<ObjectId> {
        let ws = self.workspace()?;
        let refs = self.refs()?;
        if base == "HEAD" {
            return self
                .current_head(zone, &ws, &refs)
                .ok_or_else(|| Error::Msg(format!("zone {zone:?} has no commits yet")));
        }
        if let Some(id) = refs.tags.get(&(zone.to_owned(), base.to_owned())) {
            return Ok(self.peel(zone, *id)?.0);
        }
        if let Some(h) = refs.head(zone, base) {
            return Ok(h);
        }
        let p = base.to_ascii_lowercase();
        if p.len() >= 4 && p.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut roots: Vec<ObjectId> = refs.branches_of(zone).into_iter().map(|(_, h)| h).collect();
            for ((a, _), id) in &refs.tags {
                if a == zone {
                    roots.push(self.peel(zone, *id)?.0);
                }
            }
            let mut seen = BTreeSet::new();
            let mut hits = Vec::new();
            while let Some(id) = roots.pop() {
                if !seen.insert(id) {
                    continue;
                }
                if id.to_hex().starts_with(&p) {
                    hits.push(id);
                }
                roots.extend(self.commit_obj(&id)?.parents);
            }
            match hits.as_slice() {
                [one] => return Ok(*one),
                [] => {}
                _ => return msg(format!("revision {base:?} is ambiguous ({} commits match)", hits.len())),
            }
        }
        msg(format!(
            "unknown revision {base:?} in zone {zone:?} (expected HEAD, a tag, a branch or a commit id prefix)"
        ))
    }

    fn flat_of(&self, commit: Option<&Commit>, prefix: &str) -> Result<Flat> {
        treeops::flatten(&self.store, commit.map(|c| c.root), prefix)
    }

    /// Commit metadata and its diff against the first parent.
    pub fn show(&self, zone: &str, rev: &str) -> Result<ShowReport> {
        let zone = self.known_zone(zone)?;
        let id = self.resolve_rev(&zone, rev)?;
        let tag = {
            let refs = self.refs()?;
            match refs.tags.get(&(zone.clone(), rev.to_owned())) {
                Some(t) => self.peel(&zone, *t)?.1,
                None => None,
            }
        };
        let commit = self.commit_obj(&id)?;
        let prefix = self.zones()?.path_of(&zone).unwrap_or("").to_owned();
        let parent = match commit.parents.first() {
            Some(p) => Some(self.commit_obj(p)?),
            None => None,
        };
        let old = self.flat_of(parent.as_ref(), &prefix)?;
        let new = self.flat_of(Some(&commit), &prefix)?;
        let mut paths: BTreeSet<&String> = old.keys().collect();
        paths.extend(new.keys());
        let mut out = String::new();
        for p in paths {
            let (o, n) = (old.get(p), new.get(p));
            if o == n {
                continue;
            }
            let ob = o.map(|(_, i)| self.store.get_content(i)).transpose()?;
            let nb = n.map(|(_, i)| self.store.get_content(i)).transpose()?;
            let d = diff::unified(p, ob.as_deref(), nb.as_deref());
            if d.is_empty() {
                out.push_str(&format!("diff --vcs a/{p} b/{p}\nmode changed\n"));
            } else {
                out.push_str(&d);
            }
        }
        Ok(ShowReport {
            id,
            commit,
            tag,
            diff: out,
        })
    }

    /// Blob of zone-relative `rel` in `root`, or `None` if absent. Errors if it is a directory.
    fn blob_at(&self, root: ObjectId, rel: &str) -> Result<Option<ObjectId>> {
        let mut tree = root;
        let parts: Vec<&str> = rel.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let Object::Tree(t) = self.store.get(&tree)? else {
                return Err(Error::Corrupt(format!("{tree} is not a tree")));
            };
            let Some(e) = t.get(part) else { return Ok(None) };
            let last = i + 1 == parts.len();
            match (e.mode, last) {
                (EntryMode::Directory, true) => return msg(format!("{rel:?} is a directory, not a file")),
                (EntryMode::Directory, false) => tree = e.id,
                (_, true) => return Ok(Some(e.id)),
                (_, false) => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Attribute each line of `path` (repo-relative) to the commit that last changed it,
    /// following only first parents of `rev` (default: the zone's current head). The zone is the
    /// one owning `path`. A line brought in by a merge belongs to the merge commit.
    pub fn blame(&self, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>> {
        self.blame_follow(path, rev, None)
    }

    /// Like [`Repo::blame`]; with `follow = Some(threshold)` (per mille, see
    /// `vcs_merge::rename::DEFAULT_THRESHOLD`) a file that appears in a commit without existing in
    /// its first parent is matched against the files that commit removed, and blame continues
    /// through the old name when one is similar enough.
    pub fn blame_follow(&self, path: &str, rev: Option<&str>, follow: Option<u32>) -> Result<Vec<BlameLine>> {
        let zones = self.zones()?;
        let zone = zones.owner(path).to_owned();
        if path.is_empty() || zones.path_of(&zone) == Some(path) {
            return msg("blame needs a file path");
        }
        let mut rel = zones.relative(&zone, path).to_owned();
        let zprefix = zones.path_of(&zone).unwrap_or("").to_owned();
        if rel.is_empty() {
            return msg("blame needs a file path");
        }
        let head = self.resolve_rev(&zone, rev.unwrap_or("HEAD"))?;
        let mut commit = self.commit_obj(&head)?;
        let Some(mut blob) = self.blob_at(commit.root, &rel)? else {
            return msg(format!("{path:?} does not exist at {} in zone {zone:?}", head.short()));
        };
        let mut content = self.store.get_content(&blob)?;
        if diff::is_binary(&content) {
            return msg(format!("{path:?} is binary; blame works on text files"));
        }
        let final_content = content.clone();
        let n = diff::lines_of(&final_content).len();
        let mut owner: BTreeMap<usize, (ObjectId, String)> = BTreeMap::new();
        // (final line, line index in the commit being examined)
        let mut pending: Vec<(usize, usize)> = (0..n).map(|i| (i, i)).collect();
        let mut id = head;
        let mut skipper = crate::histwalk::FirstParentSkip::new(self, &zone, head);
        while !pending.is_empty() {
            // commits that leave this file as it is in their first parent change nothing here
            let reached = skipper.skip(id, &rel);
            if reached != id {
                id = reached;
                commit = self.commit_obj(&id)?;
            }
            skipper.note(&id, &commit);
            let parent = match commit.parents.first() {
                Some(p) => Some((*p, self.commit_obj(p)?)),
                None => None,
            };
            let mut pblob = match &parent {
                Some((_, pc)) => self.blob_at(pc.root, &rel)?,
                None => None,
            };
            let mut prev_rel = rel.clone();
            if let (None, Some(th), Some((_, pc))) = (pblob, follow, &parent) {
                // renamed in this commit? match against the files it removed
                // only what differs: a path in both trees with different content is neither gone nor new
                let d = treeops::diff_trees(&self.store, Some(pc.root), Some(commit.root), "")?;
                let (old, new) = (d.old, d.new);
                let gone: Vec<(&String, Vec<u8>)> = old
                    .iter()
                    .filter(|(p, _)| !new.contains_key(*p))
                    .map(|(p, (_, i))| Ok((p, self.store.get_content(i)?)))
                    .collect::<Result<_>>()?;
                let names: Vec<(&str, &[u8])> = gone.iter().map(|(p, d)| (p.as_str(), d.as_slice())).collect();
                let cur = [(rel.as_str(), content.as_slice())];
                if let Some((i, _, _)) = vcs_merge::pair_by_content(&names, &cur, th).into_iter().next() {
                    prev_rel = names[i].0.to_owned();
                    pblob = self.blob_at(pc.root, &prev_rel)?;
                }
            }
            let (Some((pid, pc)), Some(pb)) = (parent, pblob) else {
                for (f, _) in pending.drain(..) {
                    owner.insert(f, (id, rel.clone()));
                }
                break;
            };
            if pb != blob {
                let pcontent = self.store.get_content(&pb)?;
                let origins = diff::line_origins(&pcontent, &content);
                let mut next = Vec::new();
                for (f, l) in pending.drain(..) {
                    match origins[l] {
                        Some(pl) => next.push((f, pl)),
                        None => {
                            owner.insert(f, (id, rel.clone()));
                        }
                    }
                }
                pending = next;
                content = pcontent;
            }
            blob = pb;
            rel = prev_rel;
            id = pid;
            commit = pc;
        }
        Ok(diff::lines_of(&final_content)
            .into_iter()
            .enumerate()
            .map(|(i, l)| BlameLine {
                commit: owner[&i].0,
                path: join_prefix(&zprefix, &owner[&i].1),
                line_no: i + 1,
                text: String::from_utf8_lossy(l).trim_end_matches('\n').to_owned(),
            })
            .collect())
    }
}

fn join_prefix(prefix: &str, rel: &str) -> String {
    if prefix.is_empty() {
        rel.to_owned()
    } else {
        format!("{prefix}/{rel}")
    }
}
