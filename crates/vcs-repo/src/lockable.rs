//! `.vcsattributes` and exclusive checkout of binary assets (docs/locks.md, "Lockable files").
//!
//! The attributes file is tracked in the repository, one per directory (repo root, zone roots, any
//! subdirectory), with gitignore-style patterns (the `.vcsignore` matcher, without `!`):
//!
//! ```text
//! *.psd lockable
//! assets/** lockable
//! *.fbx lockable merge=binary
//! ```
//!
//! Attributes: `lockable` / `-lockable`; `merge=binary` / `merge=text`. Unknown attributes are
//! ignored (forward compatibility). Later lines and deeper files override earlier ones.
//!
//! Files that are `lockable` are checked out read-only unless this checkout holds the lock. Which
//! locks it holds is recorded locally in `<common>/lockable-held` (so it works offline); the
//! remote manifest stays the authority and the CLI re-verifies against it when it can.

use crate::error::{Error, Result};
use crate::ignore::Pattern;
use crate::index::Index;
use crate::repo::Repo;
use crate::treeops::Flat;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use vcs_merge::{Conflict, ConflictKind, MergeError, TreeMerge};

/// Name of the attributes file looked up in every directory.
pub const ATTR_FILE: &str = ".vcsattributes";

/// Resolved attributes of one path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathAttrs {
    pub lockable: bool,
    /// `merge=binary`: never text-merge; report a conflict and let the user choose a side.
    pub merge_binary: bool,
}

#[derive(Debug)]
struct Line {
    pat: Pattern,
    lockable: Option<bool>,
    merge_binary: Option<bool>,
}

/// All attribute files of a worktree, shallow ones first.
#[derive(Debug, Default)]
pub struct Attrs {
    files: Vec<(String, Vec<Line>)>,
}

/// Split off the first whitespace-delimited token; `\ ` keeps a space inside the pattern.
fn split_pattern(line: &str) -> Option<(String, &str)> {
    let mut pat = String::new();
    let mut it = line.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '\\' => {
                if let Some(&(_, n)) = it.peek() {
                    if n == ' ' {
                        pat.push_str("\\ ");
                        it.next();
                        continue;
                    }
                }
                pat.push(c);
            }
            c if c.is_whitespace() => return Some((pat, &line[i..])),
            c => pat.push(c),
        }
    }
    Some((pat, ""))
}

impl Attrs {
    /// Parse the text of the attribute file that lives in directory `base` ("" = root).
    pub fn add_file(&mut self, base: &str, text: &str) {
        let mut lines = Vec::new();
        for raw in text.lines() {
            let t = raw.trim_start();
            if t.is_empty() || t.starts_with('#') || t.starts_with('!') {
                continue;
            }
            let Some((pat, rest)) = split_pattern(t) else { continue };
            let Some(pat) = Pattern::parse(&pat) else { continue };
            let mut l = Line {
                pat,
                lockable: None,
                merge_binary: None,
            };
            for a in rest.split_whitespace() {
                match a {
                    "lockable" => l.lockable = Some(true),
                    "-lockable" => l.lockable = Some(false),
                    "merge=binary" => l.merge_binary = Some(true),
                    "merge=text" | "-merge=binary" => l.merge_binary = Some(false),
                    _ => {}
                }
            }
            lines.push(l);
        }
        self.files.push((base.to_owned(), lines));
        self.files.sort_by_key(|(b, _)| b.matches('/').count() + usize::from(!b.is_empty()));
    }

    pub fn is_empty(&self) -> bool {
        self.files.iter().all(|(_, l)| l.is_empty())
    }

    /// Attributes of the file `rel` (repo-relative).
    pub fn lookup(&self, rel: &str) -> PathAttrs {
        let mut out = PathAttrs::default();
        for (base, lines) in &self.files {
            let sub = if base.is_empty() {
                rel
            } else {
                match rel.strip_prefix(base.as_str()).and_then(|r| r.strip_prefix('/')) {
                    Some(r) => r,
                    None => continue,
                }
            };
            for l in lines {
                if l.pat.matches(sub, false) {
                    if let Some(v) = l.lockable {
                        out.lockable = v;
                    }
                    if let Some(v) = l.merge_binary {
                        out.merge_binary = v;
                    }
                }
            }
        }
        out
    }
}

/// A lock this checkout took (client-side record, not authoritative).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub token: u64,
    /// Local estimate of the lease end, unix ms.
    pub expires_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Make a file writable (`true`) or read-only (`false`). Missing files are ignored.
pub fn set_writable(path: &Path, writable: bool) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut perm = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = perm.mode();
        let new = if writable { m | 0o200 } else { m & !0o222 };
        if new == m {
            return Ok(());
        }
        perm.set_mode(new);
    }
    #[cfg(not(unix))]
    {
        if perm.readonly() == !writable {
            return Ok(());
        }
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(!writable);
    }
    fs::set_permissions(path, perm)
}

/// Is the file read-only for its owner?
pub fn is_read_only(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false)
}

/// Does `hay` contain `needle` (libc `memmem`: vectorized, no allocation)?
fn bytes_contain(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.len() < needle.len() {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: both pointers are valid for the given lengths.
        let p = unsafe { libc::memmem(hay.as_ptr().cast(), hay.len(), needle.as_ptr().cast(), needle.len()) };
        !p.is_null()
    }
    #[cfg(not(unix))]
    {
        hay.windows(needle.len()).any(|w| w == needle)
    }
}

impl Repo {
    /// Attribute files of the tracked tree (those in the index plus a root one on disk).
    pub fn attrs(&self) -> Result<Attrs> {
        let mut bases: BTreeSet<String> = BTreeSet::new();
        bases.insert(String::new());
        // Paths are stored verbatim in the index file, so an index that never mentions the
        // attribute file's name holds none: skip decoding it (a status or checkout has already
        // paid for one decode of the same file).
        let raw = self.rd("index")?;
        if bytes_contain(&raw, ATTR_FILE.as_bytes()) {
            let ix = Index::decode(&raw)?;
            for p in ix.entries.keys() {
                if p == ATTR_FILE {
                    continue;
                }
                if let Some(dir) = p.strip_suffix(ATTR_FILE).and_then(|d| d.strip_suffix('/')) {
                    bases.insert(dir.to_owned());
                }
            }
        }
        let mut a = Attrs::default();
        for b in bases {
            let rel = if b.is_empty() {
                ATTR_FILE.to_owned()
            } else {
                format!("{b}/{ATTR_FILE}")
            };
            if let Ok(bytes) = fs::read(self.worktree().join(&rel)) {
                a.add_file(&b, &String::from_utf8_lossy(&bytes));
            }
        }
        Ok(a)
    }

    fn held_file(&self) -> std::path::PathBuf {
        self.common.join("lockable-held")
    }

    /// Locks this repository (all its worktrees) believes it holds, repo-relative path -> record.
    pub fn lockable_held(&self) -> Result<BTreeMap<String, Held>> {
        let text = match fs::read_to_string(self.held_file()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: self.held_file(),
                    source,
                })
            }
        };
        let mut m = BTreeMap::new();
        for l in text.lines() {
            let mut f = l.splitn(3, '\t');
            let (Some(t), Some(e), Some(p)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            if let (Ok(token), Ok(expires_ms)) = (t.parse(), e.parse()) {
                m.insert(p.to_owned(), Held { token, expires_ms });
            }
        }
        Ok(m)
    }

    fn save_held(&self, m: &BTreeMap<String, Held>) -> Result<()> {
        let mut s = String::new();
        for (p, h) in m {
            s.push_str(&format!("{}\t{}\t{p}\n", h.token, h.expires_ms));
        }
        crate::meta::write_atomic(&self.held_file(), s.as_bytes())
    }

    /// Paths among `paths` that are lockable and not (still) held by this checkout.
    pub fn lockable_unlocked(&self, paths: &[String]) -> Result<Vec<String>> {
        let attrs = self.attrs()?;
        if attrs.is_empty() {
            return Ok(vec![]);
        }
        let held = self.lockable_held()?;
        let now = now_ms();
        Ok(paths
            .iter()
            .filter(|p| attrs.lookup(p).lockable && !held.get(p.as_str()).is_some_and(|h| h.expires_ms > now))
            .cloned()
            .collect())
    }

    /// Paths among `paths` that are lockable and held per the local record.
    pub fn lockable_held_among(&self, paths: &[String]) -> Result<Vec<String>> {
        let held = self.lockable_held()?;
        let now = now_ms();
        Ok(paths
            .iter()
            .filter(|p| held.get(p.as_str()).is_some_and(|h| h.expires_ms > now))
            .cloned()
            .collect())
    }

    /// Record freshly acquired locks and make the files writable in every worktree.
    pub fn lockable_note_acquired(&self, locks: &[(String, u64)], lease_ms: u64) -> Result<()> {
        let mut m = self.lockable_held()?;
        for (p, token) in locks {
            if p.contains(['\t', '\n']) {
                continue;
            }
            m.insert(
                p.clone(),
                Held {
                    token: *token,
                    expires_ms: now_ms().saturating_add(lease_ms),
                },
            );
        }
        self.save_held(&m)?;
        let paths: Vec<String> = locks.iter().map(|l| l.0.clone()).collect();
        self.each_worktree(|r| r.set_modes(&paths, true))
    }

    /// Forget locks (released, broken or lost) and make lockable files read-only again.
    pub fn lockable_note_released(&self, paths: &[String]) -> Result<()> {
        let mut m = self.lockable_held()?;
        for p in paths {
            m.remove(p);
        }
        self.save_held(&m)?;
        self.each_worktree(|r| {
            let attrs = r.attrs()?;
            let ro: Vec<String> = paths.iter().filter(|p| attrs.lookup(p).lockable).cloned().collect();
            r.set_modes(&ro, false)
        })
    }

    fn set_modes(&self, paths: &[String], writable: bool) -> Result<()> {
        for p in paths {
            let full = self.worktree().join(p);
            set_writable(&full, writable).map_err(|source| Error::Io { path: full, source })?;
        }
        Ok(())
    }

    fn each_worktree(&self, f: impl Fn(&Repo) -> Result<()>) -> Result<()> {
        f(self)?;
        for w in self.worktree_list().unwrap_or_default() {
            if w.current || !w.path.exists() {
                continue;
            }
            if let Ok(r) = Repo::discover(&w.path) {
                f(&r)?;
            }
        }
        Ok(())
    }

    /// `lockable_refresh_modes` in this and every other worktree of the repository.
    pub fn lockable_refresh_all(&self) -> Result<()> {
        self.each_worktree(|r| r.lockable_refresh_modes().map(|_| ()))
    }

    /// Re-apply the read-only bit to every tracked lockable file of this worktree (writable only
    /// when its lock is held). Cheap no-op without attribute files. Returns the files changed.
    pub fn lockable_refresh_modes(&self) -> Result<usize> {
        let attrs = self.attrs()?;
        if attrs.is_empty() {
            return Ok(0);
        }
        let held = self.lockable_held()?;
        let now = now_ms();
        let ix = self.index()?;
        let mut n = 0;
        for p in ix.entries.keys() {
            if !attrs.lookup(p).lockable {
                continue;
            }
            let full = self.worktree().join(p);
            let want_w = held.get(p.as_str()).is_some_and(|h| h.expires_ms > now);
            let before = is_read_only(&full);
            set_writable(&full, want_w).map_err(|source| Error::Io {
                path: full.clone(),
                source,
            })?;
            if before != is_read_only(&full) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Tree merge that never text-merges `merge=binary` paths: a path changed differently on
    /// both sides becomes a structured conflict without hunks (ours stays in the worktree; pick a
    /// side with `vcs resolve --ours|--theirs`).
    pub(crate) fn tree3(&self, base: &Flat, ours: &Flat, theirs: &Flat) -> std::result::Result<TreeMerge, MergeError> {
        let attrs = match self.attrs() {
            Ok(a) if !a.is_empty() => a,
            _ => return self.tree3_plain(base, ours, theirs),
        };
        let mut paths: BTreeSet<&String> = BTreeSet::new();
        paths.extend(base.keys());
        paths.extend(ours.keys());
        paths.extend(theirs.keys());
        let diverged: Vec<String> = paths
            .into_iter()
            .filter(|p| {
                let (b, l, r) = (base.get(*p), ours.get(*p), theirs.get(*p));
                l != r && r != b && l != b && attrs.lookup(p).merge_binary
            })
            .cloned()
            .collect();
        if diverged.is_empty() {
            return self.tree3_plain(base, ours, theirs);
        }
        let (mut b2, mut o2, mut t2) = (base.clone(), ours.clone(), theirs.clone());
        for p in &diverged {
            b2.remove(p);
            o2.remove(p);
            t2.remove(p);
        }
        let mut tm = self.tree3_plain(&b2, &o2, &t2)?;
        for p in diverged {
            let (b, l, r) = (base.get(&p), ours.get(&p), theirs.get(&p));
            let kind = match (b, l, r) {
                (None, Some(_), Some(_)) => ConflictKind::AddAdd,
                (Some(_), None, _) | (Some(_), _, None) => ConflictKind::DeleteModify,
                _ => ConflictKind::ModifyModify,
            };
            tm.conflicts.push(Conflict {
                path: p,
                kind,
                base: b.map(|e| e.1),
                left: l.map(|e| e.1),
                right: r.map(|e| e.1),
                base_mode: b.map(|e| e.0),
                left_mode: l.map(|e| e.0),
                right_mode: r.map(|e| e.0),
                hunks: None,
            });
        }
        tm.conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(tm)
    }
}
