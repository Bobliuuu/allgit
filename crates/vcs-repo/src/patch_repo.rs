//! Patch files against a repository: `apply`, `format-patch` and `am`.
//!
//! * [`Repo::apply_patch`] plans every file in memory first (all hunks must fit, no path may
//!   escape the working tree or pass through a symlink) and only then writes, so a patch that
//!   does not apply changes nothing. With `index` the touched paths are staged as well.
//! * [`Repo::format_patches`] writes one mail per commit from the commit's tree difference
//!   against its first parent; a commit that changes a binary file is refused (never mangled).
//! * `am_*` applies mails one by one (apply with index, then commit with the mail's author, date
//!   and message). A mail that does not apply stops the run; the queue is kept in `.vcs/am` for
//!   `--continue` (commit what you staged), `--skip` and `--abort` (back to the heads at start).

use crate::diffpairs::FilePair;
use crate::error::{io, msg, Error, Result};
use crate::mbox;
use crate::patch::{self, FilePatch};
use crate::repo::Repo;
use crate::reset::ResetMode;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use vcs_objects::{EntryMode, ObjectId, Signature};

#[derive(Clone, Debug, Default)]
pub struct ApplyOpts {
    /// Check only: report whether the patch applies, change nothing.
    pub check: bool,
    /// Also stage the touched paths (the files must match the index first).
    pub index: bool,
    /// Apply the patch backwards.
    pub reverse: bool,
    /// Leading path components to strip (git `-p`); `None` = 1.
    pub strip: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyKind {
    Created,
    Modified,
    Deleted,
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub files: Vec<(String, ApplyKind)>,
}

fn mode_of(m: EntryMode) -> u32 {
    match m {
        EntryMode::Executable => 0o100755,
        EntryMode::Symlink => 0o120000,
        _ => 0o100644,
    }
}

/// A repository-relative path that stays inside the working tree and outside `.vcs`.
fn safe_rel(p: &str) -> Result<()> {
    let bad = p.is_empty()
        || p.starts_with('/')
        || p.contains('\0')
        || p.contains('\\')
        || p.split('/').any(|c| c.is_empty() || c == "." || c == ".." || c == ".vcs");
    if bad {
        return msg(format!("refusing unsafe path {p:?} in patch"));
    }
    Ok(())
}

impl Repo {
    fn patch_current(&self, rel: &str) -> Result<Option<(Vec<u8>, u32)>> {
        let abs = self.worktree.join(rel);
        // no path component may be a symlink
        let mut cur = self.worktree.clone();
        let parts: Vec<&str> = rel.split('/').collect();
        for c in &parts[..parts.len() - 1] {
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.file_type().is_symlink() => return msg(format!("{rel}: a directory on the path is a symbolic link; refusing")),
                Ok(m) if !m.is_dir() => return msg(format!("{rel}: {c:?} is a file, not a directory")),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(source) => return Err(Error::Io { path: cur, source }),
            }
        }
        match fs::symlink_metadata(&abs) {
            Ok(m) if m.file_type().is_symlink() => Ok(Some((
                crate::repo::read_link_bytes(&abs).map_err(|source| Error::Io { path: abs.clone(), source })?,
                0o120000,
            ))),
            Ok(m) if m.is_file() => {
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    if m.permissions().mode() & 0o111 != 0 {
                        0o100755
                    } else {
                        0o100644
                    }
                };
                #[cfg(not(unix))]
                let mode = 0o100644;
                Ok(Some((io(&abs, fs::read(&abs))?, mode)))
            }
            Ok(_) => msg(format!("{rel}: is a directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(Error::Io { path: abs, source }),
        }
    }

    /// Apply the unified diff in `data` to the working tree (see [`ApplyOpts`]).
    pub fn apply_patch(&self, data: &[u8], opts: &ApplyOpts) -> Result<ApplyReport> {
        let mut patches = patch::parse(data, opts.strip.unwrap_or(1))?;
        if patches.is_empty() {
            return msg("no patch found in the input");
        }
        if opts.reverse {
            patches = patches.iter().map(FilePatch::reversed).collect();
            patches.reverse();
        }
        self.apply_parsed(&patches, opts)
    }

    fn apply_parsed(&self, patches: &[FilePatch], opts: &ApplyOpts) -> Result<ApplyReport> {
        // paths and binary check first: nothing is touched when any patch is unusable
        for p in patches {
            if p.binary {
                return msg(format!(
                    "{}: binary patches are not supported (nothing was applied); exchange binary changes with `vcs bundle` or a remote",
                    p.display_path()
                ));
            }
            for q in [&p.old_path, &p.new_path].into_iter().flatten() {
                safe_rel(q)?;
            }
        }
        if opts.index {
            let touched: BTreeSet<&str> = patches
                .iter()
                .flat_map(|p| [p.old_path.as_deref(), p.new_path.as_deref()])
                .flatten()
                .collect();
            let st = self.status()?;
            for z in &st.zones {
                for c in z.staged.iter().chain(z.unstaged.iter()) {
                    if touched.contains(c.path.as_str()) {
                        return msg(format!(
                            "{}: does not match the index (it has staged or unstaged changes); commit or restore it first",
                            c.path
                        ));
                    }
                }
            }
        }
        // plan in memory: path -> new (content, mode) or None = removed
        let mut plan: BTreeMap<String, Option<(Vec<u8>, u32)>> = BTreeMap::new();
        let mut kinds: BTreeMap<String, ApplyKind> = BTreeMap::new();
        for p in patches {
            let cur = |plan: &BTreeMap<String, Option<(Vec<u8>, u32)>>, path: &str| -> Result<Option<(Vec<u8>, u32)>> {
                match plan.get(path) {
                    Some(v) => Ok(v.clone()),
                    None => self.patch_current(path),
                }
            };
            let (old, mode) = match &p.old_path {
                None => (Vec::new(), p.new_mode.unwrap_or(0o100644)),
                Some(src) => match cur(&plan, src)? {
                    Some(x) => x,
                    None => return msg(format!("{src}: does not exist in the working tree")),
                },
            };
            if let (None, Some(dst)) = (&p.old_path, &p.new_path) {
                if cur(&plan, dst)?.is_some() {
                    return msg(format!("{dst}: already exists in the working tree"));
                }
            }
            if let (Some(src), Some(dst)) = (&p.old_path, &p.new_path) {
                if src != dst && cur(&plan, dst)?.is_some() {
                    return msg(format!("{dst}: already exists in the working tree (rename target)"));
                }
            }
            let new = patch::apply_hunks(&old, &p.hunks).map_err(|e| Error::Msg(format!("{}: {e}", p.display_path())))?;
            match &p.new_path {
                None => {
                    if !new.is_empty() {
                        return msg(format!("{}: removal patch leaves file contents", p.display_path()));
                    }
                    let src = p.old_path.clone().expect("checked");
                    plan.insert(src.clone(), None);
                    kinds.insert(src, ApplyKind::Deleted);
                }
                Some(dst) => {
                    let nm = p.new_mode.unwrap_or(mode);
                    if nm == 0o120000 {
                        crate::portable::check_symlink_target(dst, &new)
                            .map_err(|e| Error::Msg(format!("{dst}: bad symlink in patch: {e}")))?;
                    }
                    if let Some(src) = &p.old_path {
                        if src != dst && !p.copy {
                            plan.insert(src.clone(), None);
                            kinds.insert(src.clone(), ApplyKind::Deleted);
                        }
                    }
                    let kind = if p.old_path.is_none() || p.old_path.as_ref() != Some(dst) {
                        ApplyKind::Created
                    } else {
                        ApplyKind::Modified
                    };
                    plan.insert(dst.clone(), Some((new, nm)));
                    kinds.insert(dst.clone(), kind);
                }
            }
        }
        let report = ApplyReport {
            files: kinds.into_iter().collect(),
        };
        if opts.check {
            return Ok(report);
        }
        // write
        let mut dirs = BTreeSet::new();
        // removals first so a rename onto a freed name works
        for (path, v) in &plan {
            if v.is_none() {
                let abs = self.worktree.join(path);
                match fs::remove_file(&abs) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => return Err(Error::Io { path: abs, source }),
                }
                if let Some(par) = abs.parent() {
                    dirs.insert(par.to_owned());
                }
            }
        }
        for (path, v) in &plan {
            let Some((content, mode)) = v else { continue };
            let abs = self.worktree.join(path);
            if let Some(par) = abs.parent() {
                io(par, fs::create_dir_all(par))?;
            }
            // re-check the parents now that directories exist
            self.patch_current(path)?;
            if fs::symlink_metadata(&abs).is_ok_and(|m| m.file_type().is_symlink()) {
                io(&abs, fs::remove_file(&abs))?;
            }
            if *mode == 0o120000 {
                write_symlink(&abs, content)?;
            } else {
                io(&abs, fs::write(&abs, content))?;
                set_exec(&abs, *mode == 0o100755)?;
            }
        }
        self.prune_dirs(dirs);
        if opts.index {
            let paths: Vec<String> = plan.keys().cloned().collect();
            self.add(&paths)?;
        }
        Ok(report)
    }

    // ---------- format-patch ----------

    /// Commits of `zone`'s checked-out branch to turn into patches, oldest first, merges skipped.
    /// `count`: the last N commits; `since`: those not reachable from that revision (up to `head`,
    /// default the branch tip); `root`: everything.
    pub fn patch_range(
        &self,
        zone: &str,
        since: Option<&str>,
        head: Option<&str>,
        count: Option<usize>,
        root: bool,
    ) -> Result<Vec<ObjectId>> {
        let zone = self.resolve_zone(zone)?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let tip = match head {
            Some(h) => self.resolve_rev(&zone, h)?,
            None => self
                .current_head(&zone, &ws, &refs)
                .ok_or_else(|| Error::Msg(format!("zone {zone:?} has no commits")))?,
        };
        let base = match since {
            Some(s) => {
                let sid = self.resolve_rev(&zone, s)?;
                match self.merge_bases(&tip, &sid)?.as_slice() {
                    [b] => Some(*b),
                    [] => return msg(format!("{s} shares no history with the branch")),
                    _ => return msg(format!("{s} has several merge bases with the branch; give the exact commit")),
                }
            }
            None => None,
        };
        let mut out = Vec::new();
        let mut cur = Some(tip);
        while let Some(c) = cur {
            if Some(c) == base {
                break;
            }
            let cm = self.commit_obj(&c)?;
            if cm.parents.len() <= 1 && !(cm.parents.is_empty() && !root && base.is_some()) {
                out.push(c);
            }
            if let Some(n) = count {
                if out.len() >= n {
                    break;
                }
            }
            cur = cm.parents.first().copied();
        }
        out.reverse();
        Ok(out)
    }

    /// Mail text for each commit: `(file name, text)`.
    pub fn format_patches(
        &self,
        zone: &str,
        ids: &[ObjectId],
        numbered: bool,
        start: usize,
        prefix: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let zone = self.resolve_zone(zone)?;
        let total = ids.len();
        let mut out = Vec::with_capacity(total);
        for (k, id) in ids.iter().enumerate() {
            let c = self.commit_obj(id)?;
            let pairs = self.commit_pairs(&zone, id)?;
            let n = start + k;
            let subject = c.message.lines().next().unwrap_or("").trim_end();
            let body: String = c.message.lines().skip(1).collect::<Vec<_>>().join("\n");
            let body = body.trim_matches('\n');
            let tag = if numbered && total > 1 {
                format!("[{prefix} {n}/{}] ", start + total - 1)
            } else {
                format!("[{prefix}] ")
            };
            let mut text = mbox::format_header(&id.to_string(), &c.author, &tag, subject);
            text.push('\n');
            if !body.is_empty() {
                text.push_str(body);
                text.push('\n');
            }
            text.push_str("---\n");
            let mut diff = String::new();
            let (mut ins, mut del) = (0usize, 0usize);
            let mut stat = String::new();
            for p in &pairs {
                let (d, i, r) = file_diff(p).map_err(|e| Error::Msg(format!("commit {}: {e}", id.short())))?;
                ins += i;
                del += r;
                stat.push_str(&format!(" {} | {}\n", p.path, i + r));
                diff.push_str(&d);
            }
            text.push_str(&stat);
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            text.push_str(&format!(
                " {} file{} changed, {ins} insertion{}(+), {del} deletion{}(-)\n\n",
                pairs.len(),
                plural(pairs.len()),
                plural(ins),
                plural(del)
            ));
            text.push_str(&diff);
            text.push_str("-- \nvcs\n\n");
            out.push((patch_file_name(n, subject), text.into_bytes()));
        }
        Ok(out)
    }

    // ---------- am ----------

    fn am_dir(&self) -> std::path::PathBuf {
        self.vcs.join("am")
    }

    pub fn am_in_progress(&self) -> bool {
        self.am_dir().join("orig").exists()
    }

    fn require_all_clean(&self, what: &str) -> Result<()> {
        let st = self.status()?;
        for z in &st.zones {
            if !z.staged.is_empty() || !z.unstaged.is_empty() {
                return msg(format!(
                    "cannot {what}: zone {:?} has uncommitted changes ({} staged, {} unstaged); commit or restore them first",
                    z.zone,
                    z.staged.len(),
                    z.unstaged.len()
                ));
            }
        }
        Ok(())
    }

    /// Apply the mails in `inputs` (mbox contents) as commits on the checked-out branches.
    pub fn am_start(&self, inputs: &[Vec<u8>]) -> Result<AmOutcome> {
        if self.am_in_progress() {
            return msg("an am session is already in progress\n\nOptions:\n  vcs am --continue\n  vcs am --skip\n  vcs am --abort");
        }
        self.rebase_guard("apply patches")?;
        if self.merge_state()?.is_some() {
            return msg("cannot apply patches: a merge is in progress");
        }
        self.require_all_clean("apply patches")?;
        let mut msgs: Vec<Vec<u8>> = Vec::new();
        for inp in inputs {
            msgs.extend(mbox::split_mbox(inp).into_iter().map(<[u8]>::to_vec));
        }
        if msgs.is_empty() {
            return msg("no patches found in the input");
        }
        for (k, m) in msgs.iter().enumerate() {
            mbox::parse_mail(m).map_err(|e| Error::Msg(format!("patch {}: {e} (nothing was applied)", k + 1)))?;
            let mail = mbox::parse_mail(m)?;
            let ps = patch::parse(&mail.diff, 1).map_err(|e| {
                Error::Msg(format!(
                    "patch {} ({}): {e} (nothing was applied)",
                    k + 1,
                    subject_of(&mail.message)
                ))
            })?;
            if let Some(b) = ps.iter().find(|p| p.binary) {
                return msg(format!(
                    "patch {} ({}): {}: binary patches are not supported (nothing was applied)",
                    k + 1,
                    subject_of(&mail.message),
                    b.display_path()
                ));
            }
        }
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let mut orig = String::new();
        for (z, b) in &ws.subs {
            if let Some(h) = refs.head(z, b) {
                orig.push_str(&format!("{z}\t{b}\t{h}\n"));
            }
        }
        let total = msgs.len();
        self.am_run(msgs, 1, total, &orig)
    }

    fn am_apply_one(&self, raw: &[u8]) -> Result<(String, ObjectId)> {
        let mail = mbox::parse_mail(raw)?;
        let ps = patch::parse(&mail.diff, 1)?;
        self.apply_parsed(
            &ps,
            &ApplyOpts {
                index: true,
                ..Default::default()
            },
        )?;
        self.am_commit(&mail)
    }

    fn am_commit(&self, mail: &mbox::Mail) -> Result<(String, ObjectId)> {
        let st = self.status()?;
        if st.zones.iter().all(|z| z.staged.is_empty()) {
            return msg("the patch applies no changes (already applied?); use `vcs am --skip`");
        }
        let rep = self.commit(&mail.message, mail.author.clone())?;
        Ok((subject_of(&mail.message), rep.commits.first().map(|c| c.2).expect("committed")))
    }

    fn am_run(&self, msgs: Vec<Vec<u8>>, first_no: usize, total: usize, orig: &str) -> Result<AmOutcome> {
        let mut applied = Vec::new();
        for (k, raw) in msgs.iter().enumerate() {
            match self.am_apply_one(raw) {
                Ok((subj, id)) => applied.push((subj, id)),
                Err(e) => {
                    let subject = mbox::parse_mail(raw).map(|m| subject_of(&m.message)).unwrap_or_default();
                    self.am_save(orig, &msgs[k..], first_no + k, total)?;
                    return Ok(AmOutcome::Stopped {
                        applied,
                        number: first_no + k,
                        total,
                        subject,
                        reason: e.to_string(),
                    });
                }
            }
        }
        self.am_clear();
        Ok(AmOutcome::Done { applied })
    }

    fn am_save(&self, orig: &str, rest: &[Vec<u8>], number: usize, total: usize) -> Result<()> {
        let d = self.am_dir();
        let _ = fs::remove_dir_all(&d);
        io(&d, fs::create_dir_all(&d))?;
        for (k, m) in rest.iter().enumerate() {
            let p = d.join(format!("{k:06}"));
            io(&p, fs::write(&p, m))?;
        }
        let p = d.join("meta");
        io(&p, fs::write(&p, format!("{number} {total}\n")))?;
        let p = d.join("orig");
        io(&p, fs::write(&p, orig))
    }

    fn am_clear(&self) {
        let _ = fs::remove_dir_all(self.am_dir());
    }

    fn am_load(&self) -> Result<(Vec<Vec<u8>>, usize, usize, String)> {
        let d = self.am_dir();
        if !self.am_in_progress() {
            return msg("no am session in progress");
        }
        let orig = io(&d.join("orig"), fs::read_to_string(d.join("orig")))?;
        let meta = io(&d.join("meta"), fs::read_to_string(d.join("meta")))?;
        let mut it = meta.split_whitespace().map(|x| x.parse::<usize>().unwrap_or(0));
        let (number, total) = (it.next().unwrap_or(1), it.next().unwrap_or(1));
        let mut names: Vec<String> = io(&d, fs::read_dir(&d))?
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit()))
            .collect();
        names.sort();
        let mut msgs = Vec::new();
        for n in names {
            msgs.push(io(&d.join(&n), fs::read(d.join(&n)))?);
        }
        Ok((msgs, number, total, orig))
    }

    /// Commit what was staged for the stopped patch (author, date and message from the mail) and go on.
    pub fn am_continue(&self) -> Result<AmOutcome> {
        let (mut msgs, number, total, orig) = self.am_load()?;
        if msgs.is_empty() {
            self.am_clear();
            return msg("no patch left");
        }
        let st = self.status()?;
        if st.zones.iter().any(|z| !z.unstaged.is_empty()) {
            return msg("cannot continue: tracked files have unstaged changes; `vcs add` them (or `vcs restore` them) first");
        }
        let first = msgs.remove(0);
        let mail = mbox::parse_mail(&first)?;
        let (subj, id) = self.am_commit(&mail)?;
        match self.am_run(msgs, number + 1, total, &orig)? {
            AmOutcome::Done { mut applied } => {
                applied.insert(0, (subj, id));
                Ok(AmOutcome::Done { applied })
            }
            AmOutcome::Stopped {
                mut applied,
                number,
                total,
                subject,
                reason,
            } => {
                applied.insert(0, (subj, id));
                Ok(AmOutcome::Stopped {
                    applied,
                    number,
                    total,
                    subject,
                    reason,
                })
            }
        }
    }

    /// Drop the stopped patch (the working tree goes back to the branch heads) and go on.
    pub fn am_skip(&self) -> Result<AmOutcome> {
        let (mut msgs, number, total, orig) = self.am_load()?;
        if msgs.is_empty() {
            self.am_clear();
            return msg("no patch left");
        }
        msgs.remove(0);
        self.am_reset_to_heads()?;
        self.am_run(msgs, number + 1, total, &orig)
    }

    fn am_reset_to_heads(&self) -> Result<()> {
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let st = self.status()?;
        for z in &st.zones {
            if z.staged.is_empty() && z.unstaged.is_empty() {
                continue;
            }
            if let Some(h) = refs.head(&z.zone, ws.subs.get(&z.zone).map(String::as_str).unwrap_or("main")) {
                self.reset(&z.zone, &h, ResetMode::Hard)?;
            }
        }
        Ok(())
    }

    /// Back to the branch heads from before `am_start`; the working tree is cleaned.
    pub fn am_abort(&self) -> Result<()> {
        let (_, _, _, orig) = self.am_load()?;
        let ws = self.workspace()?;
        for line in orig.lines() {
            let mut it = line.split('\t');
            let (Some(z), Some(b), Some(h)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            if ws.subs.get(z).map(String::as_str) != Some(b) {
                continue;
            }
            let id = ObjectId::from_hex(h).map_err(|_| Error::Corrupt("am: bad head".into()))?;
            self.reset(z, &id, ResetMode::Hard)?;
        }
        self.am_clear();
        Ok(())
    }
}

#[derive(Debug)]
pub enum AmOutcome {
    Done {
        applied: Vec<(String, ObjectId)>,
    },
    Stopped {
        applied: Vec<(String, ObjectId)>,
        number: usize,
        total: usize,
        subject: String,
        reason: String,
    },
}

fn subject_of(message: &str) -> String {
    message.lines().next().unwrap_or("").to_owned()
}

fn patch_file_name(n: usize, subject: &str) -> String {
    let mut slug = String::new();
    for ch in subject.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches(|c| c == '-' || c == '.');
    let slug: String = slug.chars().take(52).collect();
    let slug = slug.trim_end_matches(['-', '.']);
    if slug.is_empty() {
        format!("{n:04}.patch")
    } else {
        format!("{n:04}-{slug}.patch")
    }
}

#[cfg(unix)]
fn write_symlink(abs: &Path, target: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    io(abs, std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), abs))
}
#[cfg(not(unix))]
fn write_symlink(_abs: &Path, _target: &[u8]) -> Result<()> {
    msg("symbolic links in patches are not supported on this platform")
}

#[cfg(unix)]
fn set_exec(abs: &Path, exec: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let m = io(abs, fs::metadata(abs))?;
    let mut mode = m.permissions().mode();
    let want = if exec { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 };
    if want != mode {
        mode = want;
        io(abs, fs::set_permissions(abs, fs::Permissions::from_mode(mode)))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn set_exec(_abs: &Path, _exec: bool) -> Result<()> {
    Ok(())
}

/// One file's git-style diff text and its (insertions, deletions). Binary content is an error.
fn file_diff(p: &FilePair) -> std::result::Result<(String, usize, usize), String> {
    let path = &p.path;
    if path.contains(['"', '\\', '\n', '\t', '\r']) {
        return Err(format!("path {path:?} has characters a patch cannot carry"));
    }
    let (ob, nb) = (p.old.as_deref(), p.new.as_deref());
    if ob.is_some_and(crate::diff::is_binary) || nb.is_some_and(crate::diff::is_binary) {
        return Err(format!(
            "{path} is binary; format-patch does not carry binary changes (use `vcs bundle` or a remote)"
        ));
    }
    let (om, nm) = (p.old_mode.map(mode_of), p.new_mode.map(mode_of));
    let mut out = format!("diff --git a/{path} b/{path}\n");
    match (ob, nb) {
        (None, _) => out.push_str(&format!("new file mode {:o}\n", nm.unwrap_or(0o100644))),
        (_, None) => out.push_str(&format!("deleted file mode {:o}\n", om.unwrap_or(0o100644))),
        _ if om != nm => out.push_str(&format!(
            "old mode {:o}\nnew mode {:o}\n",
            om.unwrap_or(0o100644),
            nm.unwrap_or(0o100644)
        )),
        _ => {}
    }
    let u = crate::diff::unified(path, ob, nb);
    let mut body = String::new();
    let mut seen_header = false;
    for l in u.split_inclusive('\n') {
        if !seen_header {
            if l.starts_with("--- ") {
                seen_header = true;
            } else {
                continue;
            }
        }
        body.push_str(l);
    }
    if !body.contains("\n@@ ") {
        body.clear(); // empty file created/removed, or mode change only: no ---/+++ lines
    }
    let (mut ins, mut del) = (0, 0);
    let mut in_hunk = false;
    for l in body.lines() {
        if l.starts_with("@@ ") {
            in_hunk = true;
        } else if in_hunk {
            match l.as_bytes().first() {
                Some(b'+') => ins += 1,
                Some(b'-') => del += 1,
                _ => {}
            }
        }
    }
    out.push_str(&body);
    Ok((out, ins, del))
}

/// Used by tests and the CLI: signature of the first applied commit's author, for display.
pub fn author_line(s: &Signature) -> String {
    format!("{} <{}>", s.name, s.email)
}
