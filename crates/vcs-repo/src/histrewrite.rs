//! `Repo::rewrite_history`: `git filter-branch` / `git filter-repo` for one zone.
//!
//! Every commit reachable from the chosen branches is rebuilt, parents first, through the same
//! pipeline (path filters, then tree filter, env filter, message filter), and the branches and
//! tags are moved onto the rebuilt commits. All of it happens in the object store first; refs
//! move last, through the same view-restore machinery as `vcs op undo` (so the working tree and
//! index follow), and the whole run is ONE operation: `vcs op undo` puts every branch and tag back.
//!
//! Model limits, stated rather than faked:
//!
//! * A commit carries ONE signature (the author). `GIT_COMMITTER_*` are exported to the env filter
//!   with the author's values, and changes to them are ignored (counted in the report).
//! * Annotated tags lose their signature when they are moved (it signed the old commit).
//! * Paths are zone-relative (a zone's history is its own); other zones are never touched.

use crate::error::{msg, Error, Result};
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};
use vcs_objects::{AnnotatedTag, Commit, EntryMode, Object, ObjectId, Signature};
use vcs_store::ObjectStore;

/// What to rewrite and how. All filters are optional; with none the run is a no-op.
#[derive(Clone, Debug, Default)]
pub struct RewriteSpec {
    /// Make this directory the new root of every commit (`--subdirectory-filter`).
    pub subdirectory: Option<String>,
    /// Keep only these files / directories (`filter-repo --path`).
    pub keep: Vec<String>,
    /// Drop these files / directories (`filter-repo --path ... --invert-paths`).
    pub remove: Vec<String>,
    /// `sh -c` command; the old message on stdin, the new message on stdout.
    pub msg_filter: Option<String>,
    /// `sh -c` script that may change `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_AUTHOR_DATE`.
    pub env_filter: Option<String>,
    /// `sh -c` command run in a checkout of each commit's tree; what it leaves there is the new tree.
    pub tree_filter: Option<String>,
    /// `sh -c` command mapping a tag name (stdin) to its new name (stdout).
    pub tag_name_filter: Option<String>,
    /// Rewrite every branch of the zone, not only the checked-out one.
    pub all_branches: bool,
    /// Drop commits that the filters made empty (the default of `filter-repo`).
    pub prune_empty: bool,
    /// Rewrite even though a remote-tracking branch or tag holds some of the changed commits.
    pub force: bool,
    /// Compute and report, move nothing.
    pub dry_run: bool,
}

impl RewriteSpec {
    fn has_filter(&self) -> bool {
        self.subdirectory.is_some()
            || !self.keep.is_empty()
            || !self.remove.is_empty()
            || self.msg_filter.is_some()
            || self.env_filter.is_some()
            || self.tree_filter.is_some()
            || self.tag_name_filter.is_some()
    }
}

/// What a rewrite did (or, with `dry_run`, would do).
#[derive(Debug, Default)]
pub struct RewriteReport {
    pub zone: String,
    /// Commits reachable from the rewritten branches.
    pub seen: usize,
    /// Commits replaced by a changed copy.
    pub rewritten: usize,
    /// Commits removed because they became empty.
    pub dropped: usize,
    /// `(branch, old head, new head)` for every branch that moved.
    pub branches: Vec<(String, ObjectId, ObjectId)>,
    /// `(old name, new name, new target)` for every tag that moved or was renamed.
    pub tags: Vec<(String, String, ObjectId)>,
    /// Tags whose commit vanished (all its content was filtered away) and were deleted.
    pub tags_deleted: Vec<String>,
    /// Commits where the env filter changed a `GIT_COMMITTER_*` variable (not representable).
    pub committer_changes_ignored: usize,
    /// Published (remote-tracking) commits that changed; non-zero only with `force`.
    pub published_changed: usize,
    pub dry_run: bool,
}

fn norm(p: &str) -> String {
    p.trim_matches('/').to_owned()
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/')
}

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Run `sh -c script`, feeding `stdin`, in `dir` with `env`; stdout on success, a clear error otherwise.
fn run_sh(what: &str, script: &str, dir: Option<&Path>, env: &[(String, String)], stdin: &[u8]) -> Result<Vec<u8>> {
    let mut c = Command::new("sh");
    c.arg("-c").arg(script);
    if let Some(d) = dir {
        c.current_dir(d);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    let mut child = c.spawn().map_err(|e| Error::Msg(format!("{what}: cannot start sh: {e}")))?;
    let mut sin = child.stdin.take().expect("piped");
    let data = stdin.to_vec();
    let feeder = std::thread::spawn(move || {
        use std::io::Write;
        let _ = sin.write_all(&data); // a filter that ignores stdin closes the pipe early: not an error
    });
    let out = child.wait_with_output().map_err(|e| Error::Msg(format!("{what}: {e}")))?;
    let _ = feeder.join();
    if !out.status.success() {
        return msg(format!("{what} failed ({}); nothing was changed", out.status));
    }
    Ok(out.stdout)
}

/// Parse `GIT_AUTHOR_DATE`: `@SECS [+ZZZZ]`, `SECS`, or `SECS +ZZZZ`.
fn parse_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let first = s.split_whitespace().next()?;
    first.trim_start_matches('@').parse().ok()
}

impl Repo {
    /// Rewrite the history of `zone` (checked-out branch, or every branch with `all_branches`).
    /// Refuses with uncommitted changes, an in-progress merge/rebase, linked worktrees, or (without
    /// `force`) when a changed commit is held by a remote-tracking branch or tag as last fetched.
    pub fn rewrite_history(&self, zone: &str, spec: &RewriteSpec) -> Result<RewriteReport> {
        let zone = self.resolve_zone(zone)?;
        if !self.zones()?.exists(&zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        if !spec.has_filter() {
            return msg("nothing to do: give at least one of --subdirectory-filter, --keep, --remove, --msg-filter, --env-filter, --tree-filter, --tag-name-filter");
        }
        let _l = self.lock()?;
        self.rebase_guard("rewrite")?;
        if self.merge_state()?.is_some() {
            return msg("a merge is in progress; finish or abort it first");
        }
        self.require_no_linked_worktrees("rewrite")?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let Some(cur) = ws.subs.get(&zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        let st = self.status_tracked()?;
        self.require_clean(&zone, &st)?;

        // which branches
        let mut targets: Vec<(String, ObjectId)> = if spec.all_branches {
            refs.branches_of(&zone)
        } else {
            match refs.head(&zone, &cur) {
                Some(h) => vec![(cur.clone(), h)],
                None => vec![],
            }
        };
        targets.sort();
        if targets.is_empty() {
            return msg(format!("zone {zone:?} has no commits to rewrite"));
        }

        // all commits, parents first
        let order = self.topo_order(&targets.iter().map(|(_, h)| *h).collect::<Vec<_>>())?;
        let tmp_root = self.common.join(format!("rewrite-tmp-{}", std::process::id()));
        let result = self.rewrite_commits(&zone, spec, &order, &tmp_root);
        let _ = std::fs::remove_dir_all(&tmp_root);
        let (memo, mut rep) = result?;
        rep.zone = zone.clone();
        rep.seen = order.len();
        rep.dry_run = spec.dry_run;

        // branches
        let mut view = self.current_view()?;
        for (b, old) in &targets {
            match memo[old] {
                Some(new) if new != *old => {
                    rep.branches.push((b.clone(), *old, new));
                    view.branches.insert((zone.clone(), b.clone()), new);
                }
                Some(_) => {}
                None => {
                    return msg(format!(
                        "every commit of {zone}/{b} would be removed (the filters leave nothing); nothing was changed"
                    ))
                }
            }
        }

        // tags
        let mut tag_changes: Vec<(String, Option<(String, ObjectId)>)> = Vec::new();
        let mut all_tags: Vec<(String, ObjectId)> = Vec::new();
        for ((a, n), id) in &refs.tags {
            if *a == zone {
                all_tags.push((n.clone(), *id));
            }
        }
        let mut taken: BTreeSet<String> = all_tags.iter().map(|(n, _)| n.clone()).collect();
        let mut peel_memo = HashMap::new();
        for (name, id) in all_tags {
            let (commit, ann) = self.peel_memo(&zone, id, &mut peel_memo)?;
            let Some(mapped) = memo.get(&commit).copied() else { continue }; // not in the rewritten set
            let new_name = match &spec.tag_name_filter {
                Some(f) => {
                    let out = run_sh("--tag-name-filter", f, None, &[], name.as_bytes())?;
                    let n = String::from_utf8_lossy(&out).trim().to_owned();
                    vcs_objects::names::validate_branch_name(&n)
                        .map_err(|e| Error::Msg(format!("--tag-name-filter gave {n:?} for tag {name:?}: {e}")))?;
                    n
                }
                None => name.clone(),
            };
            let Some(new_commit) = mapped else {
                tag_changes.push((name.clone(), None));
                rep.tags_deleted.push(name);
                continue;
            };
            if new_commit == commit && new_name == name {
                continue;
            }
            if new_name != name {
                taken.remove(&name);
                if !taken.insert(new_name.clone()) {
                    return msg(format!(
                        "--tag-name-filter would rename tag {name:?} to {new_name:?}, which already exists; nothing was changed"
                    ));
                }
            }
            let target = match ann {
                None => new_commit,
                Some(t) if !spec.dry_run => self.store.put(&Object::Tag(AnnotatedTag {
                    zone: zone.clone(),
                    name: new_name.clone(),
                    target: new_commit,
                    tagger: t.tagger,
                    message: t.message,
                    signature: Vec::new(),
                }))?,
                Some(_) => new_commit,
            };
            rep.tags.push((name.clone(), new_name.clone(), new_commit));
            tag_changes.push((name, Some((new_name, target))));
        }

        // published?
        let changed: BTreeSet<ObjectId> = memo.iter().filter(|(o, n)| **n != Some(**o)).map(|(o, _)| *o).collect();
        rep.published_changed = self.published_among(&zone, &changed)?;
        if rep.published_changed > 0 && !spec.force {
            return msg(format!(
                "{} commit(s) this rewrite would change are already published (held by a remote-tracking branch or tag of origin, as last fetched); \
                 other clones still have them. Nothing was changed. Rewrite anyway with --force, then replace the remote with `vcs publish --force`",
                rep.published_changed
            ));
        }
        if spec.dry_run || (rep.branches.is_empty() && tag_changes.is_empty()) {
            return Ok(rep);
        }

        let mut tags = view.tags.take().unwrap_or_default();
        for (old, new) in &tag_changes {
            tags.remove(&(zone.clone(), old.clone()));
            if let Some((n, id)) = new {
                tags.insert((zone.clone(), n.clone()), *id);
            }
        }
        view.tags = Some(tags);
        self.store.sync()?;
        self.restore_view(&view)?;
        let what = if spec.all_branches {
            format!("{zone} (all branches)")
        } else {
            format!("{zone}/{cur}")
        };
        self.record_op(
            &format!(
                "rewrite history of {what}: {} commit(s) rewritten, {} dropped, {} tag(s) moved",
                rep.rewritten,
                rep.dropped,
                rep.tags.len()
            ),
            None,
        )?;
        Ok(rep)
    }

    /// Commits reachable from `tips`, parents before children.
    fn topo_order(&self, tips: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        let mut state: HashMap<ObjectId, bool> = HashMap::new(); // false = entered, true = done
        for t in tips {
            let mut stack = vec![(*t, false)];
            while let Some((id, expanded)) = stack.pop() {
                if expanded {
                    if state.insert(id, true) != Some(true) {
                        out.push(id);
                    }
                    continue;
                }
                if state.contains_key(&id) {
                    continue;
                }
                state.insert(id, false);
                stack.push((id, true));
                for p in self.commit_obj(&id)?.parents.into_iter().rev() {
                    if !state.contains_key(&p) {
                        stack.push((p, false));
                    }
                }
            }
        }
        Ok(out)
    }

    /// How many of `changed` are reachable from a remote-tracking branch head or tag of `zone`.
    fn published_among(&self, zone: &str, changed: &BTreeSet<ObjectId>) -> Result<usize> {
        if changed.is_empty() {
            return Ok(0);
        }
        let remote = self.remote_refs()?;
        let mut tips: Vec<ObjectId> = remote.branches_of(zone).into_iter().map(|(_, h)| h).collect();
        for ((a, _), id) in &remote.tags {
            if a == zone {
                if let Ok((c, _)) = self.peel(zone, *id) {
                    tips.push(c);
                }
            }
        }
        let mut seen = BTreeSet::new();
        let mut hit = BTreeSet::new();
        let mut todo = tips;
        while let Some(id) = todo.pop() {
            if !seen.insert(id) {
                continue;
            }
            if changed.contains(&id) {
                hit.insert(id);
            }
            // fetched tips may name commits we do not hold (lazy clone): not an error here
            if let Ok(Object::Commit(c)) = self.store.get(&id) {
                todo.extend(c.parents);
            }
        }
        Ok(hit.len())
    }

    /// Rebuild `order` (parents first). `memo[old]` = the new commit, `Some(old)` when unchanged,
    /// `None` when removed and nothing replaces it (a pruned root).
    fn rewrite_commits(
        &self,
        zone: &str,
        spec: &RewriteSpec,
        order: &[ObjectId],
        tmp_root: &Path,
    ) -> Result<(BTreeMap<ObjectId, Option<ObjectId>>, RewriteReport)> {
        let sub = spec.subdirectory.as_deref().map(norm).filter(|s| !s.is_empty());
        let keep: Vec<String> = spec.keep.iter().map(|p| norm(p)).collect();
        let remove: Vec<String> = spec.remove.iter().map(|p| norm(p)).collect();
        let path_filtering = sub.is_some() || !keep.is_empty() || !remove.is_empty();
        let mut memo: BTreeMap<ObjectId, Option<ObjectId>> = BTreeMap::new();
        let mut rep = RewriteReport::default();
        let mut tree_of_new: HashMap<ObjectId, ObjectId> = HashMap::new();
        let mut n_tree = 0usize;
        for id in order {
            let c = self.commit_obj(id)?;
            // parents: the replacement of each, first-seen order, no duplicates
            let mut parents: Vec<ObjectId> = Vec::new();
            for p in &c.parents {
                if let Some(n) = memo.get(p).copied().flatten() {
                    if !parents.contains(&n) {
                        parents.push(n);
                    }
                }
            }
            // tree
            let mut root = c.root;
            if path_filtering || spec.tree_filter.is_some() {
                let mut flat = treeops::flatten(&self.store, Some(c.root), "")?;
                if let Some(d) = &sub {
                    let prefix = format!("{d}/");
                    flat = flat
                        .into_iter()
                        .filter(|(p, _)| p.starts_with(&prefix))
                        .map(|(p, v)| (p[prefix.len()..].to_owned(), v))
                        .collect();
                }
                if !keep.is_empty() {
                    flat.retain(|p, _| keep.iter().any(|k| under(p, k)));
                }
                if !remove.is_empty() {
                    flat.retain(|p, _| !remove.iter().any(|k| under(p, k)));
                }
                if let Some(script) = &spec.tree_filter {
                    n_tree += 1;
                    flat = self.run_tree_filter(script, &flat, id, zone, &tmp_root.join(n_tree.to_string()))?;
                }
                root = treeops::build(&self.store, flat.iter().map(|(p, (m, i))| (p.as_str(), *m, *i)))?;
            }
            // identity
            let mut author = c.author.clone();
            if let Some(script) = &spec.env_filter {
                let (a, ignored) = self.run_env_filter(script, &c.author, id, zone)?;
                author = a;
                if ignored {
                    rep.committer_changes_ignored += 1;
                }
            }
            // message
            let mut message = c.message.clone();
            if let Some(f) = &spec.msg_filter {
                let env = [
                    ("VCS_COMMIT".to_owned(), id.to_hex()),
                    ("GIT_COMMIT".to_owned(), id.to_hex()),
                    ("VCS_ZONE".to_owned(), zone.to_owned()),
                ];
                let out = run_sh("--msg-filter", f, None, &env, c.message.as_bytes())?;
                message = String::from_utf8(out).map_err(|_| Error::Msg("--msg-filter wrote a message that is not UTF-8".into()))?;
            }
            // pruning: became empty (or degenerate merge) through the filters, was not empty before
            if spec.prune_empty && (path_filtering || spec.tree_filter.is_some()) {
                let parent_tree = match parents.as_slice() {
                    [] => None,
                    [p] => Some(match tree_of_new.get(p) {
                        Some(t) => *t,
                        None => self.commit_obj(p)?.root,
                    }),
                    _ => None,
                };
                let empty_now = match parents.as_slice() {
                    [] => treeops::flatten(&self.store, Some(root), "")?.is_empty(),
                    [_] => parent_tree == Some(root),
                    _ => false,
                };
                let was_empty_before = match c.parents.as_slice() {
                    [] => false,
                    [p] => self.commit_obj(p)?.root == c.root,
                    _ => false,
                };
                if empty_now && !was_empty_before {
                    memo.insert(*id, parents.first().copied());
                    rep.dropped += 1;
                    continue;
                }
            }
            if parents == c.parents && root == c.root && author == c.author && message == c.message {
                memo.insert(*id, Some(*id));
                tree_of_new.insert(*id, root);
                continue;
            }
            let nc = Commit {
                zone: zone.to_owned(),
                parents,
                root,
                change_id: c.change_id,
                author,
                message,
            };
            let nid = self.store.put(&Object::Commit(nc))?;
            tree_of_new.insert(nid, root);
            memo.insert(*id, Some(nid));
            rep.rewritten += 1;
        }
        Ok((memo, rep))
    }

    fn run_env_filter(&self, script: &str, a: &Signature, id: &ObjectId, zone: &str) -> Result<(Signature, bool)> {
        let date = format!("@{} +0000", a.timestamp);
        let wrapper = "eval \"$VCS_FILTER_SCRIPT\"\nprintf '%s\\0%s\\0%s\\0%s\\0%s\\0%s\\0' \"$GIT_AUTHOR_NAME\" \"$GIT_AUTHOR_EMAIL\" \"$GIT_AUTHOR_DATE\" \"$GIT_COMMITTER_NAME\" \"$GIT_COMMITTER_EMAIL\" \"$GIT_COMMITTER_DATE\"";
        let mut env = vec![
            ("VCS_FILTER_SCRIPT".to_owned(), script.to_owned()),
            ("VCS_COMMIT".to_owned(), id.to_hex()),
            ("GIT_COMMIT".to_owned(), id.to_hex()),
            ("VCS_ZONE".to_owned(), zone.to_owned()),
        ];
        for (p, n, e, d) in [("AUTHOR", &a.name, &a.email, &date), ("COMMITTER", &a.name, &a.email, &date)] {
            env.push((format!("GIT_{p}_NAME"), n.clone()));
            env.push((format!("GIT_{p}_EMAIL"), e.clone()));
            env.push((format!("GIT_{p}_DATE"), d.clone()));
        }
        let out = run_sh("--env-filter", wrapper, None, &env, b"")?;
        let parts: Vec<String> = out.split(|b| *b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        if parts.len() < 6 {
            return msg("--env-filter: could not read the variables back");
        }
        let (name, email) = (parts[0].clone(), parts[1].clone());
        if name.is_empty() || email.is_empty() {
            return msg(format!(
                "--env-filter left an empty author name or email on commit {}; nothing was changed",
                id.short()
            ));
        }
        let timestamp = if parts[2] == date {
            a.timestamp
        } else {
            parse_date(&parts[2]).ok_or_else(|| {
                Error::Msg(format!(
                    "--env-filter: cannot read GIT_AUTHOR_DATE {:?} (use '@SECONDS' or SECONDS)",
                    parts[2]
                ))
            })?
        };
        let ignored = parts[3] != a.name || parts[4] != a.email || parts[5] != date;
        Ok((Signature { name, email, timestamp }, ignored))
    }

    /// Check `flat` out into `dir`, run `script` there, read the result back as a flat tree.
    fn run_tree_filter(&self, script: &str, flat: &treeops::Flat, id: &ObjectId, zone: &str, dir: &Path) -> Result<treeops::Flat> {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        for (p, (mode, oid)) in flat {
            let abs = dir.join(p);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            }
            let data = self.content(oid)?;
            match mode {
                #[cfg(unix)]
                EntryMode::Symlink => {
                    use std::os::unix::ffi::OsStrExt;
                    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&data), &abs).map_err(|e| io_err(&abs, e))?;
                }
                _ => {
                    std::fs::write(&abs, &data).map_err(|e| io_err(&abs, e))?;
                    #[cfg(unix)]
                    if *mode == EntryMode::Executable {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o755)).map_err(|e| io_err(&abs, e))?;
                    }
                }
            }
        }
        let env = [
            ("VCS_COMMIT".to_owned(), id.to_hex()),
            ("GIT_COMMIT".to_owned(), id.to_hex()),
            ("VCS_ZONE".to_owned(), zone.to_owned()),
        ];
        run_sh("--tree-filter", script, Some(dir), &env, b"")?;
        let mut out = treeops::Flat::new();
        self.ingest_dir(dir, "", &mut out)?;
        Ok(out)
    }

    fn ingest_dir(&self, dir: &Path, rel: &str, out: &mut treeops::Flat) -> Result<()> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| io_err(dir, e))?
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| io_err(dir, e))?;
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let relp = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let ft = e.file_type().map_err(|x| io_err(&path, x))?;
            if ft.is_dir() {
                self.ingest_dir(&path, &relp, out)?;
            } else if ft.is_symlink() {
                let target = crate::repo::read_link_bytes(&path).map_err(|x| io_err(&path, x))?;
                out.insert(relp, (EntryMode::Symlink, self.store.put_blob(&target)?));
            } else if ft.is_file() {
                let data = std::fs::read(&path).map_err(|x| io_err(&path, x))?;
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    let m = std::fs::metadata(&path).map_err(|x| io_err(&path, x))?;
                    if m.permissions().mode() & 0o111 != 0 {
                        EntryMode::Executable
                    } else {
                        EntryMode::File
                    }
                };
                #[cfg(not(unix))]
                let mode = EntryMode::File;
                out.insert(relp, (mode, self.store.put_blob(&data)?));
            }
        }
        Ok(())
    }
}
