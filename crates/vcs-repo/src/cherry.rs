//! `cherry-pick A..B` (several commits in order) and `cherry` (which commits of a branch have an
//! equivalent patch on another). Both are native: the pick engine is `pick.rs`; equivalence is a
//! patch id, a hash of each commit's diff against its first parent with line numbers dropped.

use crate::diff;
use crate::error::{msg, Result};
use crate::merge::MergeOutcome;
use crate::repo::Repo;
use crate::revrange::RevSpec;
use crate::treeops;
use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use vcs_objects::{ObjectId, Signature};
use vcs_store::ObjectStore;

/// Result of [`Repo::cherry_pick_many`].
#[derive(Debug, Default)]
pub struct PickSeq {
    /// (source commit, new commit), in order.
    pub done: Vec<(ObjectId, ObjectId)>,
    /// Source commits whose change was already present (nothing to commit); skipped.
    pub skipped: Vec<ObjectId>,
    /// Set when a pick stopped on conflicts: the source commit, the conflicted paths, and the
    /// commits not yet attempted (in order).
    pub stopped: Option<(ObjectId, Vec<String>, Vec<ObjectId>)>,
}

impl Repo {
    /// Expand cherry-pick arguments into source commits, oldest first. An argument is a revision
    /// or a range `A..B` (commits of B that A lacks, oldest first, as `git cherry-pick A..B`).
    pub fn pick_sources(&self, zone: &str, args: &[String]) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        for a in args {
            // `A..B`: the commits of B that A lacks. `A...B`: the commits of either side that the
            // other lacks (everything but their merge bases' history), as `git cherry-pick A...B`.
            if a.contains("..") {
                let spec = RevSpec::parse(std::slice::from_ref(a))?;
                let mut ids = self.rev_list_ids(zone, &spec, usize::MAX)?;
                ids.reverse();
                if ids.is_empty() {
                    return msg(format!("range {a:?} contains no commits"));
                }
                out.extend(ids);
            } else {
                out.push(self.resolve_commit(zone, a)?);
            }
        }
        Ok(out)
    }

    /// Cherry-pick `args` (revisions and ranges) one after another onto the checked-out branch.
    /// All sources are validated first (same zone, no merge commits), so a refusal changes
    /// nothing. A pick that stops on conflicts ends the run (finish it with `vcs commit`, then
    /// pick the commits reported as remaining); a pick that would change nothing is skipped.
    pub fn cherry_pick_many(&self, zone: &str, args: &[String], author: Signature) -> Result<PickSeq> {
        self.cherry_pick_many_mainline(zone, args, None, author)
    }

    /// [`Repo::cherry_pick_many`] with `-m <parent>`: merge commits among the sources are picked
    /// against that (1-based) parent. Without it a merge commit is refused, as in git.
    pub fn cherry_pick_many_mainline(&self, zone: &str, args: &[String], mainline: Option<usize>, author: Signature) -> Result<PickSeq> {
        let zone = self.resolve_zone(zone)?;
        let ids = self.pick_sources(&zone, args)?;
        for id in &ids {
            let c = self.commit_obj(id)?;
            if c.zone != zone {
                return msg(format!("{} belongs to zone {:?}, not {zone:?}", id.short(), c.zone));
            }
            if c.parents.len() > 1 && mainline.is_none() {
                return msg(format!(
                    "{} is a merge commit with {} parents; give the mainline parent with `-m <parent>` (nothing was changed)",
                    id.short(),
                    c.parents.len()
                ));
            }
            if let Some(m) = mainline {
                if c.parents.len() > 1 && (m == 0 || m > c.parents.len()) {
                    return msg(format!(
                        "{} has {} parents; -m {m} is out of range (nothing was changed)",
                        id.short(),
                        c.parents.len()
                    ));
                }
            }
        }
        let mut seq = PickSeq::default();
        for (k, id) in ids.iter().enumerate() {
            // -m names a parent only a merge commit has; a plain commit in the list is picked normally
            let m = if self.commit_obj(id)?.parents.len() > 1 { mainline } else { None };
            match self.cherry_pick_id_mainline(&zone, *id, m, author.clone()) {
                Ok(MergeOutcome::Merged { commit }) => seq.done.push((*id, commit)),
                Ok(MergeOutcome::Conflicted { paths }) => {
                    seq.stopped = Some((*id, paths, ids[k + 1..].to_vec()));
                    return Ok(seq);
                }
                Ok(_) => seq.skipped.push(*id),
                Err(e) if e.to_string().contains("changes nothing here") => seq.skipped.push(*id),
                Err(e) => return Err(e),
            }
        }
        Ok(seq)
    }

    /// Patch id of a (non-merge) commit: a hash of its diff against the first parent with the
    /// `@@` line numbers removed, so the same change on another base gets the same id when its
    /// context is the same.
    pub fn patch_id(&self, zone: &str, id: &ObjectId) -> Result<u64> {
        let c = self.commit_obj(id)?;
        let prefix = self.zones()?.path_of(zone).unwrap_or("").to_owned();
        let parent_root = match c.parents.first() {
            Some(p) => Some(self.commit_obj(p)?.root),
            None => None,
        };
        let d = treeops::diff_trees_files(&self.store, parent_root, Some(c.root), &prefix)?;
        let mut paths: BTreeSet<&String> = d.old.keys().collect();
        paths.extend(d.new.keys());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in paths {
            let (o, n) = (d.old.get(p), d.new.get(p));
            let ob = o.map(|(_, i)| self.store.get_content(i)).transpose()?;
            let nb = n.map(|(_, i)| self.store.get_content(i)).transpose()?;
            p.hash(&mut h);
            o.map(|x| x.0 as u8).hash(&mut h);
            n.map(|x| x.0 as u8).hash(&mut h);
            for line in diff::unified(p, ob.as_deref(), nb.as_deref()).lines() {
                if !line.starts_with("@@") {
                    line.hash(&mut h);
                }
            }
        }
        Ok(h.finish())
    }

    /// `git cherry`: the non-merge commits of `head` that `upstream` lacks, oldest first, each
    /// with `true` when a commit of `upstream` (not in `head`) has the same patch id.
    pub fn cherry(&self, zone: &str, upstream: &str, head: &str) -> Result<Vec<(ObjectId, bool, String)>> {
        let zone = self.resolve_zone(zone)?;
        let mine = RevSpec {
            include: vec![head.to_owned()],
            exclude: vec![upstream.to_owned()],
            ..Default::default()
        };
        let theirs = RevSpec {
            include: vec![upstream.to_owned()],
            exclude: vec![head.to_owned()],
            ..Default::default()
        };
        let mut mine = self.rev_list_ids(&zone, &mine, usize::MAX)?;
        mine.reverse();
        let mut seen = HashSet::new();
        for id in self.rev_list_ids(&zone, &theirs, usize::MAX)? {
            if self.commit_obj(&id)?.parents.len() <= 1 {
                seen.insert(self.patch_id(&zone, &id)?);
            }
        }
        let mut out = Vec::new();
        for id in mine {
            let c = self.commit_obj(&id)?;
            if c.parents.len() > 1 {
                continue;
            }
            let subject = c.message.lines().next().unwrap_or("").to_owned();
            out.push((id, seen.contains(&self.patch_id(&zone, &id)?), subject));
        }
        Ok(out)
    }
}
