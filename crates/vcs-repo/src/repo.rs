use crate::diff;
use crate::dircache::{stamp_of, DirCache, Kind, Listing};
use crate::error::{io, msg, Error, Result};
use crate::fsmonitor::{self, Changed, FsMonitor};
use crate::ignore::IgnoreStack;
use crate::index::{Index, IndexEntry};
use crate::meta::{write_atomic, LockGuard, Refs, Workspace, Zones, DEFAULT_BRANCH, ROOT_ZONE};
use crate::treeops::{self, Flat};
use crate::worktree_fs::{FileKind, RealFs, WorktreeFs};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_objects::names::{validate_branch_name, validate_component, validate_path, validate_zone_name, META_DIR};
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Snapshot, Transaction};
use vcs_store::{LocalStore, ObjectStore};

pub(crate) const FORMAT_FILE: &str = "format";
pub(crate) const FORMAT_TEXT: &str = "all-git repository format 1\n";
const RACY_NS: u64 = 2_000_000_000;

#[derive(Default)]
struct WalkOut {
    untracked: Vec<String>,
    unsupported: Vec<String>,
    /// Directories whose cached listing was reused (kept in the rewritten cache).
    hits: Vec<String>,
    /// Directories listed from disk whose stamp was trustworthy (to be added to the cache).
    fresh: Vec<(String, Listing)>,
    /// Directories read from disk this walk (cache misses, trustworthy or not).
    scanned: usize,
}

/// Saved status adjusted by a monitor's changed paths (see `status_monitored`).
pub(crate) struct Incremental {
    dirty: BTreeMap<String, ChangeKind>,
    rewalk: bool,
    untracked: Vec<String>,
    unsupported: Vec<String>,
}

/// Per-walk settings for the directory cache.
struct WalkCtx {
    cache: DirCache,
    use_cache: bool,
    scan_start_ns: u64,
    racy_ns: u64,
}

/// Options for [`Repo::status_with`]. `Default` is what [`Repo::status`] uses.
#[derive(Clone, Copy, Debug)]
pub struct StatusOpts {
    /// Use (and refresh) the directory listing cache in `.vcs/dircache`.
    pub dir_cache: bool,
    /// A directory stamp younger than this (ns) relative to the scan start is never trusted.
    #[doc(hidden)]
    pub racy_ns: u64,
}

impl Default for StatusOpts {
    fn default() -> Self {
        StatusOpts {
            dir_cache: std::env::var_os("VCS_NO_DIRCACHE").is_none(),
            racy_ns: RACY_NS,
        }
    }
}

pub struct Repo {
    pub(crate) worktree: PathBuf,
    /// Per-worktree state directory (`<worktree>/.vcs`): index, workspace, merge/rebase state, stash.
    pub(crate) vcs: PathBuf,
    /// Shared state directory: objects, refs, zones, remote, op log, lock. Equal to `vcs` for the
    /// main worktree and for every repository that predates linked worktrees (docs/worktrees.md).
    pub(crate) common: PathBuf,
    pub(crate) store: LocalStore,
    pub(crate) structural: bool,
    pub(crate) renames: bool,
    pub(crate) use_commit_graph: bool,
    /// Width of the index "racy" window (ns); only tests shorten it.
    pub(crate) index_racy_ns: u64,
    pub(crate) counters: Counters,
    /// The working tree, behind a trait so checkout/status can run on an in-memory fake.
    pub(crate) wfs: Arc<dyn WorktreeFs>,
}

/// Work counters for tests and benchmarks (see [`Repo::counters`]).
#[derive(Default)]
pub(crate) struct Counters {
    content_reads: AtomicU64,
    tree_puts: AtomicU64,
}

/// A snapshot of [`Repo`]'s work counters.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkCounts {
    /// Worktree files read and hashed by add or status.
    pub content_reads: u64,
    /// Tree objects written by commit.
    pub tree_puts: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug)]
pub struct ZoneStatus {
    pub zone: String,
    pub path: String,
    pub branch: String,
    pub head: Option<ObjectId>,
    pub staged: Vec<Change>,
    pub unstaged: Vec<Change>,
}

#[derive(Debug, Default)]
pub struct Status {
    pub zones: Vec<ZoneStatus>,
    pub untracked: Vec<String>,
    /// Defined zones this workspace does not materialize (not "deleted"!).
    pub unsubscribed: Vec<String>,
    /// Symlinks / special files that vcs cannot track yet.
    pub unsupported: Vec<String>,
    /// In-progress merge (`.vcs/conflicts`), if any.
    pub merging: Option<crate::merge::MergeState>,
}

#[derive(Debug, Default)]
pub struct AddReport {
    pub staged: usize,
    pub removed: usize,
    pub skipped: Vec<String>,
}

#[derive(Debug)]
pub struct CommitReport {
    pub transaction: ObjectId,
    /// (zone, branch, new commit)
    pub commits: Vec<(String, String, ObjectId)>,
}

#[derive(Debug, Default)]
pub struct FsckReport {
    pub objects: usize,
    pub problems: Vec<String>,
    /// Objects that are absent but were fetched from the remote (reachable from a remote-tracking
    /// head), so they are an evictable cache miss, not damage. See docs/cache.md.
    pub evictable: Vec<ObjectId>,
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Inode change time in ns (0 where the platform does not provide one).
pub(crate) fn ctime_ns(m: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (m.ctime().max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(m.ctime_nsec().max(0) as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        0
    }
}

pub(crate) fn mtime_ns(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub(crate) fn mode_of(m: &fs::Metadata) -> EntryMode {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o111 != 0 {
            return EntryMode::Executable;
        }
    }
    let _ = m;
    EntryMode::File
}

/// Link target as the bytes stored in a symlink blob.
pub(crate) fn read_link_bytes(abs: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let t = fs::read_link(abs)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(t.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(t.to_string_lossy().into_owned().into_bytes())
    }
}

/// Create a symlink at `link` pointing at the raw `target` bytes (unix only).
pub(crate) fn make_symlink(target: &[u8], link: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let t = std::ffi::OsStr::from_bytes(target);
        io(link, std::os::unix::fs::symlink(t, link))
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        msg(format!(
            "cannot create symlink {}: symlink checkout is only supported on unix (see docs/object-model.md)",
            link.display()
        ))
    }
}

fn is_not_a_directory(e: &std::io::Error) -> bool {
    // ErrorKind::NotADirectory needs Rust 1.83; the MSRV is 1.80. ENOTDIR is 20 on Linux and macOS.
    cfg!(unix) && e.raw_os_error() == Some(20)
}

fn zones_fp(zones: &Zones) -> ObjectId {
    vcs_objects::blob_id(&zones.encode())
}

pub(crate) enum Wt {
    Clean,
    Modified,
    Deleted,
}

impl Repo {
    /// Enable or disable the structural Rust merge in `merge`/`rebase` (default on; it only has
    /// an effect when built with feature `structural`).
    /// Detect file renames/moves in `merge`/`rebase`/`cherry-pick` and merge edits across them
    /// (default off).
    pub fn set_renames(&mut self, on: bool) {
        self.renames = on;
    }

    pub fn set_structural(&mut self, on: bool) {
        self.structural = on;
    }

    // ---------- open / init ----------

    pub fn init(path: &Path) -> Result<Repo> {
        io(path, fs::create_dir_all(path))?;
        let worktree = io(path, path.canonicalize())?;
        let vcs = worktree.join(META_DIR);
        if vcs.exists() {
            return msg(format!("repository already exists at {}", worktree.display()));
        }
        io(&vcs, fs::create_dir_all(vcs.join("objects")))?;
        let store = LocalStore::open(vcs.join("objects"))?;
        let r = Repo {
            wfs: Arc::new(RealFs::new(worktree.clone())),
            worktree,
            common: vcs.clone(),
            vcs,
            store,
            structural: true,
            renames: false,
            use_commit_graph: true,
            index_racy_ns: RACY_NS,
            counters: Counters::default(),
        };
        write_atomic(&r.vcs.join("zones"), &Zones::new().encode())?;
        write_atomic(&r.vcs.join("refs"), &Refs::default().encode())?;
        let mut ws = Workspace::default();
        ws.subs.insert(ROOT_ZONE.into(), DEFAULT_BRANCH.into());
        write_atomic(&r.vcs.join("workspace"), &ws.encode())?;
        write_atomic(&r.vcs.join("index"), &Index::default().encode())?;
        // format marker written last: a crashed init leaves no valid repo
        write_atomic(&r.vcs.join(FORMAT_FILE), FORMAT_TEXT.as_bytes())?;
        r.record_op("init repository", None)?;
        Ok(r)
    }

    pub fn discover(start: &Path) -> Result<Repo> {
        let mut cur = io(start, start.canonicalize())?;
        loop {
            let vcs = cur.join(META_DIR);
            if vcs.join(FORMAT_FILE).is_file() {
                let f = io(&vcs, fs::read_to_string(vcs.join(FORMAT_FILE)))?;
                if f != FORMAT_TEXT {
                    return msg(format!("unsupported repository format: {:?}", f.trim()));
                }
                let common = crate::worktree::resolve_common(&vcs)?;
                let store = LocalStore::open(common.join("objects"))?;
                return Ok(Repo {
                    wfs: Arc::new(RealFs::new(cur.clone())),
                    worktree: cur,
                    vcs,
                    common,
                    store,
                    structural: true,
                    renames: false,
                    use_commit_graph: true,
                    index_racy_ns: RACY_NS,
                    counters: Counters::default(),
                });
            }
            if !cur.pop() {
                return Err(Error::NotARepo);
            }
        }
    }

    /// Use `wfs` for the working tree (checkout writes and status reads go through it). `.vcs/`
    /// state stays on disk. The default is [`RealFs`] rooted at [`Self::worktree`].
    pub fn with_worktree_fs(mut self, wfs: Arc<dyn WorktreeFs>) -> Repo {
        self.wfs = wfs;
        self
    }

    pub fn worktree(&self) -> &Path {
        &self.worktree
    }
    pub fn store(&self) -> &dyn ObjectStore {
        &self.store
    }

    /// Convert a user path (relative to `cwd` or absolute) into a normalized repo-relative
    /// path ("" = repo root). Lexical: `..` is resolved textually and must stay inside.
    pub fn rel_path(&self, cwd: &Path, arg: &str) -> Result<String> {
        let cwd = io(cwd, cwd.canonicalize())?;
        let joined = cwd.join(arg);
        let mut parts: Vec<std::ffi::OsString> = Vec::new();
        for c in joined.components() {
            match c {
                Component::Prefix(_) | Component::RootDir => parts.clear(),
                Component::CurDir => {}
                Component::ParentDir => {
                    parts.pop();
                }
                Component::Normal(s) => parts.push(s.to_owned()),
            }
        }
        let wt: Vec<_> = self
            .worktree
            .components()
            .filter_map(|c| if let Component::Normal(s) = c { Some(s.to_owned()) } else { None })
            .collect();
        if parts.len() < wt.len() || parts[..wt.len()] != wt[..] {
            return msg(format!("path {arg:?} is outside the repository"));
        }
        let rel: Vec<String> = parts[wt.len()..]
            .iter()
            .map(|s| {
                s.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Msg(format!("non-UTF-8 path component in {arg:?}")))
            })
            .collect::<Result<_>>()?;
        for c in &rel {
            validate_component(c).map_err(|_| Error::Msg(format!("path {arg:?} contains reserved or invalid component {c:?}")))?;
        }
        Ok(rel.join("/"))
    }

    // ---------- state ----------

    pub(crate) fn lock(&self) -> Result<LockGuard> {
        LockGuard::acquire(self.common.join("lock"))
    }
    /// Path of a state file: shared files live in the common directory, the rest per worktree.
    pub(crate) fn state_path(&self, name: &str) -> PathBuf {
        if matches!(
            name,
            "refs" | "zones" | "remote" | "remote-refs" | "remote-manifest" | "op_head" | "git-export" | "git-import" | "p4-import"
        ) {
            self.common.join(name)
        } else {
            self.vcs.join(name)
        }
    }
    pub(crate) fn rd(&self, name: &str) -> Result<Vec<u8>> {
        let p = self.state_path(name);
        io(&p, fs::read(&p))
    }
    /// Map a user-supplied zone name (display name or original id) to the zone id used by
    /// commits, refs and snapshots. Unknown names are returned unchanged so callers report them.
    pub fn resolve_zone(&self, name: &str) -> Result<String> {
        Ok(self.zones()?.resolve(name).unwrap_or_else(|| name.to_owned()))
    }
    pub fn zones(&self) -> Result<Zones> {
        Zones::decode(&self.rd("zones")?)
    }
    pub fn refs(&self) -> Result<Refs> {
        Refs::decode(&self.rd("refs")?)
    }
    pub fn workspace(&self) -> Result<Workspace> {
        Workspace::decode(&self.rd("workspace")?)
    }
    pub(crate) fn index(&self) -> Result<Index> {
        Index::decode(&self.rd("index")?)
    }
    /// Shorten the index racy window (tests only; production uses 2 s).
    #[doc(hidden)]
    pub fn set_index_racy_ns(&mut self, ns: u64) {
        self.index_racy_ns = ns;
    }
    /// Work done by this handle so far (tests and benchmarks).
    #[doc(hidden)]
    pub fn counters(&self) -> WorkCounts {
        WorkCounts {
            content_reads: self.counters.content_reads.load(Ordering::Relaxed),
            tree_puts: self.counters.tree_puts.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn save_index(&self, ix: &mut Index) -> Result<()> {
        self.save_index_verified(ix, |_| false)
    }
    /// Save the index. `verified(path)` says the entry's content was checked against the file
    /// after its racy window had closed, which lets `written_ns` move past that window.
    pub(crate) fn save_index_verified(&self, ix: &mut Index, verified: impl Fn(&str) -> bool) -> Result<()> {
        // Durability barrier (no-op if nothing pending): blobs the index names must be durable.
        self.store.sync()?;
        ix.written_ns = ix.settled_written_ns(now_ns(), self.index_racy_ns, verified);
        write_atomic(&self.vcs.join("index"), &ix.encode())
    }
    pub(crate) fn save_ws(&self, ws: &Workspace) -> Result<()> {
        // Backstop for every path that changes a subscription: a branch of a zone is checked out
        // in at most one worktree. (Callers that move files check earlier, before touching any.)
        let old = self.workspace().unwrap_or_default();
        for (z, b) in &ws.subs {
            if old.subs.get(z) != Some(b) {
                self.require_branch_free(z, b)?;
            }
        }
        write_atomic(&self.vcs.join("workspace"), &ws.encode())
    }

    pub(crate) fn commit_obj(&self, id: &ObjectId) -> Result<Commit> {
        match self.store.get(id)? {
            Object::Commit(c) => Ok(c),
            _ => Err(Error::Corrupt(format!("{id} is not a commit"))),
        }
    }

    /// Head of the branch checked out for `zone` (subscribed) else of its `main`.
    pub(crate) fn current_head(&self, zone: &str, ws: &Workspace, refs: &Refs) -> Option<ObjectId> {
        let b = ws.subs.get(zone).map(String::as_str).unwrap_or(DEFAULT_BRANCH);
        refs.head(zone, b)
    }

    pub(crate) fn head_flat(&self, zone: &str, zones: &Zones, ws: &Workspace, refs: &Refs) -> Result<Flat> {
        let root = match self.current_head(zone, ws, refs) {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        treeops::flatten(&self.store, root, zones.path_of(zone).unwrap_or(""))
    }

    // ---------- add ----------

    pub fn add(&self, rel_paths: &[String]) -> Result<AddReport> {
        let _l = self.lock()?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut ix = self.index()?;
        let mut rep = AddReport::default();
        let mut todo = Vec::new();
        let rev0 = ix.entries.revision();
        for p in rel_paths {
            self.add_one(p, &zones, &ws, &mut ix, &mut rep, &mut todo)?;
        }
        crate::portable::require_no_collision(ix.entries.keys().map(String::as_str).chain(todo.iter().map(String::as_str)), "add")?;
        // Read + hash + write the blobs on several threads; the index is updated afterwards.
        let verified = self.stage_files(&todo, &mut ix)?;
        self.note_resolved(rel_paths, &ix)?;
        // Durability barrier: every blob the index is about to name must be durable first.
        self.store.sync()?;
        let next = ix.settled_written_ns(now_ns(), self.index_racy_ns, |p| verified.contains(p));
        if ix.entries.revision() == rev0 && !ix.unlocks(next, self.index_racy_ns) {
            return Ok(rep); // nothing changed: do not rewrite the index
        }
        self.save_index_verified(&mut ix, |p| verified.contains(p))?;
        Ok(rep)
    }

    fn add_one(&self, p: &str, zones: &Zones, ws: &Workspace, ix: &mut Index, rep: &mut AddReport, todo: &mut Vec<String>) -> Result<()> {
        let full = if p.is_empty() {
            self.worktree.clone()
        } else {
            self.worktree.join(p)
        };
        if !p.is_empty() {
            let owner = zones.owner(p);
            if !ws.subs.contains_key(owner) {
                return msg(format!(
                    "{p:?} belongs to zone {owner:?}, which this workspace has not subscribed to (vcs zone add {owner})"
                ));
            }
        }
        match fs::symlink_metadata(&full) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let gone = ix.under(p);
                if gone.is_empty() {
                    return msg(format!("pathspec {p:?} did not match any files"));
                }
                let mut any = false;
                for g in gone {
                    if ws.skips(zones.owner(&g), &g) {
                        continue; // not materialized: absent from disk by design, not deleted
                    }
                    any = true;
                    ix.entries.remove(&g);
                    rep.removed += 1;
                }
                if !any {
                    return msg(format!(
                        "{p:?} is outside this workspace's sparse set (not materialized, NOT deleted); `vcs sparse add` it first"
                    ));
                }
                Ok(())
            }
            Err(source) => Err(Error::Io { path: full, source }),
            Ok(m) if m.is_dir() => {
                let mut seen = BTreeSet::new();
                let stack = self.ignore_stack_above(p)?;
                if !p.is_empty() && stack.is_ignored(p, true) {
                    if !ix.has_under(p) {
                        rep.skipped.push(format!("{p} (ignored)"));
                        return Ok(());
                    }
                    self.add_dir(p, zones, ws, ix, rep, &mut seen, todo, &stack.inherit())?;
                } else {
                    self.add_dir(p, zones, ws, ix, rep, &mut seen, todo, &stack)?;
                }
                for g in ix.under(p) {
                    if !seen.contains(&g) && !ws.skips(zones.owner(&g), &g) {
                        ix.entries.remove(&g);
                        rep.removed += 1;
                    }
                }
                Ok(())
            }
            Ok(m) if m.is_file() => {
                validate_path(p)?;
                if ws.skips(zones.owner(p), p) {
                    return msg(format!(
                        "{p:?} is outside this workspace's sparse set; `vcs sparse add` it before staging"
                    ));
                }
                if !ix.entries.contains_key(p) && self.ignore_stack_above(p)?.is_ignored(p, false) {
                    rep.skipped.push(format!("{p} (ignored)"));
                    return Ok(());
                }
                todo.push(p.to_owned());
                rep.staged += 1;
                Ok(())
            }
            Ok(m) if m.file_type().is_symlink() => {
                validate_path(p)?;
                self.check_link(p)?;
                todo.push(p.to_owned());
                rep.staged += 1;
                Ok(())
            }
            Ok(_) => msg(format!("{p:?}: special files are not supported")),
        }
    }

    /// Refuse a symlink whose target could leave the worktree.
    fn check_link(&self, rel: &str) -> Result<()> {
        let abs = self.worktree.join(rel);
        let t = io(&abs, read_link_bytes(&abs))?;
        crate::portable::check_symlink_target(rel, &t).or_else(|why| msg(format!("refusing symlink {rel:?}: {why}")))
    }

    /// Ignore stack in force for the entries of directory `dir`'s parent chain, i.e. what applies
    /// to the path `rel` itself: the ignore files of every proper ancestor directory. A directory
    /// that is ignored on the way down makes everything below it ignored.
    fn ignore_stack_above(&self, rel: &str) -> Result<IgnoreStack> {
        let mut stack = IgnoreStack::default().descend(self.wfs.as_ref(), &self.worktree, "")?;
        let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
        let mut acc = String::new();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(c);
            if stack.is_ignored(&acc, true) {
                stack = stack.inherit();
            }
            stack = stack.descend(self.wfs.as_ref(), &self.worktree, &acc)?;
        }
        Ok(stack)
    }

    /// Is `rel` (a repo-relative path) ignored by `.vcsignore` files? Tracked files never are.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> Result<bool> {
        if self.index()?.entries.contains_key(rel) {
            return Ok(false);
        }
        Ok(self.ignore_stack_above(rel)?.is_ignored(rel, is_dir))
    }

    #[allow(clippy::too_many_arguments)]
    fn add_dir(
        &self,
        dir: &str,
        zones: &Zones,
        ws: &Workspace,
        ix: &Index,
        rep: &mut AddReport,
        seen: &mut BTreeSet<String>,
        todo: &mut Vec<String>,
        parent_stack: &IgnoreStack,
    ) -> Result<()> {
        let stack = parent_stack.descend(self.wfs.as_ref(), &self.worktree, dir)?;
        let abs = if dir.is_empty() {
            self.worktree.clone()
        } else {
            self.worktree.join(dir)
        };
        let mut names: Vec<_> = io(&abs, fs::read_dir(&abs))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| Error::Io { path: abs.clone(), source })?;
        names.sort_by_key(|e| e.file_name());
        for e in names {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case(META_DIR) {
                continue;
            }
            let rel = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
            if validate_component(&name).is_err() || e.file_name().to_str().is_none() {
                rep.skipped.push(format!("{rel} (invalid name)"));
                continue;
            }
            let ft = io(&abs, e.file_type())?;
            if !(ft.is_symlink() || ft.is_dir() || ft.is_file()) {
                rep.skipped.push(format!("{rel} (special file: unsupported)"));
                continue;
            }
            if !ws.subs.contains_key(zones.owner(&rel)) {
                continue; // not our zone: untouched, not an error
            }
            if !ft.is_dir() && ws.skips(zones.owner(&rel), &rel) {
                continue; // outside the sparse set: left alone
            }
            let tracked = if ft.is_dir() {
                ix.has_under(&rel)
            } else {
                ix.entries.contains_key(&rel)
            };
            let ignored = !tracked && stack.is_ignored(&rel, ft.is_dir());
            if ignored {
                continue;
            }
            if ft.is_symlink() {
                if let Err(e) = self.check_link(&rel) {
                    rep.skipped.push(format!("{rel} ({e})"));
                    continue;
                }
                validate_path(&rel)?;
                todo.push(rel.clone());
                seen.insert(rel);
                rep.staged += 1;
            } else if ft.is_dir() {
                // a directory kept only for tracked files below it ignores everything else
                if stack.is_ignored(&rel, true) {
                    self.add_dir(&rel, zones, ws, ix, rep, seen, todo, &stack.inherit())?;
                } else {
                    self.add_dir(&rel, zones, ws, ix, rep, seen, todo, &stack)?;
                }
            } else {
                validate_path(&rel)?;
                todo.push(rel.clone());
                seen.insert(rel);
                rep.staged += 1;
            }
        }
        Ok(())
    }

    /// True if any of these (few) files is large enough to be worth a thread each. Stats at most 64.
    fn has_big_file(&self, files: &[String]) -> bool {
        files
            .iter()
            .any(|f| fs::symlink_metadata(self.worktree.join(f)).is_ok_and(|m| m.len() >= (vcs_objects::CHUNK_THRESHOLD as u64) / 4))
    }

    /// Stat, read, hash and store each file (in parallel when there are many), then record
    /// them in the index. Blob writes only touch `store`, which is safe to share across threads.
    ///
    /// A file whose size, mtime and mode equal its index entry, and whose mtime is older than
    /// the index's racy window, is not read at all. Otherwise it is read; if the content hash
    /// equals the entry's id the object is already stored and is not written again. Returns the
    /// paths whose content is known to match the entry recorded after their racy window closed.
    fn stage_files(&self, files: &[String], ix: &mut Index) -> Result<BTreeSet<String>> {
        let racy = self.index_racy_ns;
        let old_w = ix.written_ns;
        let prev = &ix.entries;
        // (entry to record if it changed, verified)
        let one = |rel: &String| -> Result<(Option<IndexEntry>, bool)> {
            let abs = self.worktree.join(rel);
            let t_stat = now_ns();
            let m = io(&abs, fs::symlink_metadata(&abs))?; // stat BEFORE read: later edits change mtime/size
            if m.file_type().is_symlink() {
                let target = io(&abs, read_link_bytes(&abs))?;
                let id = self.store.put_blob(&target)?;
                return Ok((
                    Some(IndexEntry {
                        id,
                        mode: EntryMode::Symlink,
                        size: target.len() as u64,
                        mtime_ns: mtime_ns(&m),
                        ctime_ns: ctime_ns(&m),
                    }),
                    true,
                ));
            }
            let (mt, ct, mode) = (mtime_ns(&m), ctime_ns(&m), mode_of(&m));
            let old = prev.get(rel.as_str());
            if let Some(e) = old {
                if e.mode == mode
                    && e.size == m.len()
                    && e.mtime_ns == mt
                    && e.ctime_ns == ct
                    && ct != 0
                    && mt.max(ct).saturating_add(racy) <= old_w
                {
                    return Ok((None, true)); // stat cache hit
                }
            }
            let data = io(&abs, fs::read(&abs))?;
            self.counters.content_reads.fetch_add(1, Ordering::Relaxed);
            let verified = mt.max(ct).saturating_add(racy) <= t_stat;
            let id = match old {
                // same size: likely the same content, and then the object is known stored
                Some(e) if e.size == m.len() && vcs_objects::content_id(&data) == e.id => e.id,
                _ => self.store.put_content(&data)?,
            };
            let ne = IndexEntry {
                id,
                mode,
                size: m.len(),
                mtime_ns: mt,
                ctime_ns: ct,
            };
            Ok((if old == Some(&ne) { None } else { Some(ne) }, verified))
        };
        // Few files can still be a lot of work (chunking multi-MiB files), so only a handful of
        // small ones stay on the calling thread. The pool is bounded at 8 threads.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).min(files.len());
        type Staged = Result<(Option<IndexEntry>, bool)>;
        type Out = Option<Staged>;
        let mut entries: Vec<Out> = (0..files.len()).map(|_| None).collect();
        if threads <= 1 || (files.len() < 64 && !self.has_big_file(files)) {
            for (i, f) in files.iter().enumerate() {
                entries[i] = Some(one(f));
            }
        } else {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let parts: Vec<Vec<(usize, Staged)>> = std::thread::scope(|sc| {
                let hs: Vec<_> = (0..threads)
                    .map(|_| {
                        sc.spawn(|| {
                            let mut mine = Vec::new();
                            loop {
                                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(f) = files.get(i) else { break };
                                mine.push((i, one(f)));
                            }
                            mine
                        })
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().expect("stage worker panicked")).collect()
            });
            for (i, r) in parts.into_iter().flatten() {
                entries[i] = Some(r);
            }
        }
        let mut verified = BTreeSet::new();
        for (f, e) in files.iter().zip(entries) {
            let (ne, ok) = e.expect("every file processed")?;
            if let Some(ne) = ne {
                ix.insert(f, ne)?;
            }
            if ok {
                verified.insert(f.clone());
            }
        }
        Ok(verified)
    }

    // ---------- status ----------

    pub(crate) fn wt_state(&self, path: &str, e: &IndexEntry, written_ns: u64) -> Result<Wt> {
        Ok(self.wt_state_read(path, e, written_ns)?.0)
    }

    /// [`Self::wt_state`] plus whether the verdict came from reading the file's content.
    pub(crate) fn wt_state_read(&self, path: &str, e: &IndexEntry, written_ns: u64) -> Result<(Wt, bool)> {
        let abs = || self.worktree.join(path);
        let m = match self.wfs.stat(path) {
            Ok(m) => m,
            // A path whose parent became a file (`a/b` tracked, `a` replaced by a file) is ENOTDIR.
            Err(x) if x.kind() == std::io::ErrorKind::NotFound || is_not_a_directory(&x) => return Ok((Wt::Deleted, false)),
            Err(source) => return Err(Error::Io { path: abs(), source }),
        };
        if e.mode == EntryMode::Symlink {
            if m.kind != FileKind::Symlink {
                return Ok((Wt::Modified, false));
            }
            let t = io(&abs(), self.wfs.read_link(path))?;
            return Ok((
                if vcs_objects::blob_id(&t) == e.id {
                    Wt::Clean
                } else {
                    Wt::Modified
                },
                false,
            ));
        }
        if m.kind != FileKind::File {
            return Ok((Wt::Modified, false));
        }
        let ct_ok = e.ctime_ns == 0 || e.ctime_ns == m.ctime_ns;
        if m.len == e.size && m.mtime_ns == e.mtime_ns && ct_ok && m.mode == e.mode && e.stat_edge(self.index_racy_ns) <= written_ns {
            return Ok((Wt::Clean, false));
        }
        let data = io(&abs(), self.wfs.read(path))?;
        self.counters.content_reads.fetch_add(1, Ordering::Relaxed);
        Ok((
            if vcs_objects::content_id(&data) == e.id && m.mode == e.mode {
                Wt::Clean
            } else {
                Wt::Modified
            },
            true,
        ))
    }

    pub fn status(&self) -> Result<Status> {
        self.status_auto(StatusOpts::default())
    }

    /// `status` that never reads or writes the directory cache: the reference implementation the
    /// cached path must always agree with.
    pub fn status_full(&self) -> Result<Status> {
        self.status_with(StatusOpts {
            dir_cache: false,
            ..StatusOpts::default()
        })
    }

    /// `status`, consulting the filesystem monitor `.vcs/fsmon` (written by `vcs watch`) when one
    /// is running; identical to [`Self::status_with`] otherwise. `VCS_NO_FSMONITOR` disables it.
    pub fn status_auto(&self, opts: StatusOpts) -> Result<Status> {
        if std::env::var_os("VCS_NO_FSMONITOR").is_none() {
            if let Some(m) = crate::fsmonitor::JournalMonitor::open(&self.vcs) {
                return self.status_monitored(opts, &m);
            }
        }
        self.status_with(opts)
    }

    /// `status` using `mon` to skip the stat of unchanged files and the untracked walk. The
    /// result always equals [`Self::status_full`]; any doubt makes this a full scan (see
    /// `fsmonitor`). The last result is kept in `.vcs/fsmon/state`, keyed by index, zone layout
    /// and the monitor token it was computed at.
    pub fn status_monitored(&self, opts: StatusOpts, mon: &dyn FsMonitor) -> Result<Status> {
        let ix_bytes = self.rd("index")?;
        let ix = Index::decode(&ix_bytes)?;
        let fp = fsmonitor::fingerprint(&ix_bytes, &self.rd("zones")?, &self.rd("workspace")?);
        let prev = fsmonitor::State::load(&self.vcs).filter(|s| s.fp == fp);
        let ans = mon.changed_since(prev.as_ref().map(|s| s.token.as_str()));
        let (token, inc) = match (prev.as_ref(), ans) {
            (Some(p), Changed::Paths { paths, new_token }) => (Some(new_token), self.incremental(&ix, p, &paths)?),
            (_, Changed::Paths { new_token, .. }) => (Some(new_token), None),
            (_, Changed::FullScan { new_token }) => (new_token, None),
        };
        let st = self.status_inner(opts, &ix, true, inc.as_ref())?;
        let Some(token) = token else { return Ok(st) };
        let mut dirty = BTreeMap::new();
        for z in &st.zones {
            for c in &z.unstaged {
                dirty.insert(c.path.clone(), c.kind);
            }
        }
        // A racy-entry refresh may have rewritten the index (new `written_ns`, same entries);
        // bind the state to what is on disk now only if the entries are unchanged.
        let fp_now = match self.rd("index") {
            Ok(b) if b == ix_bytes => fp,
            Ok(b) if Index::decode(&b).is_ok_and(|n| n.entries == ix.entries) => {
                fsmonitor::fingerprint(&b, &self.rd("zones")?, &self.rd("workspace")?)
            }
            _ => return Ok(st),
        };
        let new = fsmonitor::State {
            fp: fp_now,
            token,
            dirty,
            untracked: st.untracked.clone(),
            unsupported: st.unsupported.clone(),
        };
        if prev.as_ref() != Some(&new) {
            new.store(&self.vcs);
        }
        Ok(st)
    }

    /// Apply the monitor's changed paths to the saved state: re-check only tracked files at or
    /// beneath a changed path; any other kind of change (new or vanished names, ignore files)
    /// asks for the untracked walk. `None`: too much or odd, do the full scan.
    fn incremental(&self, ix: &Index, prev: &fsmonitor::State, paths: &[String]) -> Result<Option<Incremental>> {
        if paths.len() > 20_000 || !paths.iter().all(|p| fsmonitor::plain_path(p)) {
            return Ok(None);
        }
        let mut inc = Incremental {
            dirty: prev.dirty.clone(),
            rewalk: false,
            untracked: prev.untracked.clone(),
            unsupported: prev.unsupported.clone(),
        };
        for p in paths {
            // A tracked name that is now a directory can hide new untracked names beneath it.
            if p.rsplit('/').next() == Some(crate::ignore::IGNORE_FILE)
                || !ix.entries.contains_key(p.as_str())
                || self.wfs.stat(p).is_ok_and(|m| m.kind == FileKind::Dir)
            {
                inc.rewalk = true;
            }
            for t in ix.under(p) {
                let e = &ix.entries[t.as_str()];
                match self.wt_state(&t, e, ix.written_ns)? {
                    Wt::Clean => {
                        inc.dirty.remove(&t);
                    }
                    Wt::Modified => {
                        inc.dirty.insert(t, ChangeKind::Modified);
                    }
                    Wt::Deleted => {
                        inc.dirty.insert(t, ChangeKind::Deleted);
                    }
                }
            }
        }
        Ok(Some(inc))
    }

    pub fn status_with(&self, opts: StatusOpts) -> Result<Status> {
        let ix = self.index()?;
        self.status_inner(opts, &ix, true, None)
    }

    /// `status_with` on an index the caller already loaded; `untracked: false` skips the walk
    /// for untracked files (`Status::untracked` / `unsupported` are then left empty), which is
    /// all a caller that only needs staged/unstaged/merge state has to pay for.
    pub(crate) fn status_inner(&self, opts: StatusOpts, ix: &Index, untracked: bool, inc: Option<&Incremental>) -> Result<Status> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let fp = zones_fp(&zones);
        let mut racy_clean: BTreeSet<String> = BTreeSet::new();
        let mut st = Status::default();
        let single_zone = zones.names().count() <= 1;
        for (zone, branch) in &ws.subs {
            let mine: Vec<(&String, &IndexEntry)> = ix
                .entries
                .iter()
                .filter(|(p, _)| (single_zone || zones.owner(p) == zone) && !ws.skips(zone, p))
                .collect();
            // Subtree-skipping diff: directories whose cached or computed tree id equals HEAD's
            // are skipped; HEAD's trees are read only on paths that actually differ.
            let head_root = match self.current_head(zone, &ws, &refs) {
                Some(h) => Some(self.commit_obj(&h)?.root),
                None => None,
            };
            let mut it = treeops::IndexTrees::new(&self.store, &ix.entries, &zones, zone, ix.entries.cached_trees(&fp));
            let staged: Vec<Change> = it
                .diff(zones.path_of(zone).unwrap_or(""), head_root)?
                .into_iter()
                .map(|(path, kind)| Change { path, kind })
                .collect();
            let (unstaged, clean) = match inc {
                Some(i) => (
                    i.dirty
                        .iter()
                        .filter(|(p, _)| (single_zone || zones.owner(p) == zone) && !ws.skips(zone, p))
                        .map(|(p, k)| Change { path: p.clone(), kind: *k })
                        .collect(),
                    Vec::new(),
                ),
                None => self.unstaged_changes(&mine, ix.written_ns)?,
            };
            racy_clean.extend(clean);
            st.zones.push(ZoneStatus {
                zone: zone.clone(),
                path: zones.path_of(zone).unwrap_or("").to_owned(),
                branch: branch.clone(),
                head: self.current_head(zone, &ws, &refs),
                staged,
                unstaged,
            });
        }
        for n in zones.names() {
            if !ws.subs.contains_key(n) {
                st.unsubscribed.push(n.to_owned());
            }
        }
        if !untracked {
            st.merging = self.merge_state()?;
            return Ok(st);
        }
        if let Some(i) = inc.filter(|i| !i.rewalk) {
            // The monitor vouches that no untracked name appeared or went away.
            st.untracked = i.untracked.clone();
            st.unsupported = i.unsupported.clone();
            st.merging = self.merge_state()?;
            return Ok(st);
        }
        let budget = AtomicUsize::new(std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) - 1);
        let ctx = WalkCtx {
            cache: if opts.dir_cache {
                DirCache::load(&self.vcs)
            } else {
                DirCache::default()
            },
            use_cache: opts.dir_cache,
            scan_start_ns: now_ns(),
            racy_ns: opts.racy_ns,
        };
        let mut w = self.walk_untracked("", &zones, &ws, ix, &budget, &IgnoreStack::default(), &ctx)?;
        st.untracked = std::mem::take(&mut w.untracked);
        st.unsupported = std::mem::take(&mut w.unsupported);
        if opts.dir_cache && (w.scanned > 0 || w.hits.len() != ctx.cache.dirs.len()) {
            // Rewrite only when something changed: a fully warm walk costs no write.
            let mut next = DirCache::default();
            let mut old = ctx.cache;
            for h in w.hits {
                if let Some(l) = old.dirs.remove(&h) {
                    next.dirs.insert(h, l);
                }
            }
            next.dirs.extend(w.fresh);
            next.store(&self.vcs);
        }
        st.merging = self.merge_state()?;
        if !racy_clean.is_empty() {
            self.refresh_racy(ix, &racy_clean);
        }
        Ok(st)
    }

    /// Working-tree changes of the tracked `entries`, in entry order, plus the paths whose
    /// content was read and found clean (racy entries). The per-file `lstat` (and re-hash of
    /// racy/changed files) is independent per path, so a big list is split across at most 8
    /// threads.
    fn unstaged_changes(&self, list: &[(&String, &IndexEntry)], written_ns: u64) -> Result<(Vec<Change>, Vec<String>)> {
        let check = |part: &[(&String, &IndexEntry)]| -> Result<(Vec<Change>, Vec<String>)> {
            let (mut out, mut clean) = (Vec::new(), Vec::new());
            for (p, e) in part {
                let (state, read) = self.wt_state_read(p, e, written_ns)?;
                match state {
                    Wt::Clean => {
                        if read {
                            clean.push((*p).clone());
                        }
                    }
                    Wt::Modified => out.push(Change {
                        path: (*p).clone(),
                        kind: ChangeKind::Modified,
                    }),
                    Wt::Deleted => out.push(Change {
                        path: (*p).clone(),
                        kind: ChangeKind::Deleted,
                    }),
                }
            }
            Ok((out, clean))
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(16);
        if threads <= 1 || list.len() < 2048 {
            return check(list);
        }
        let per = list.len().div_ceil(threads);
        let parts: Vec<Result<(Vec<Change>, Vec<String>)>> = std::thread::scope(|sc| {
            let check = &check;
            let hs: Vec<_> = list.chunks(per).map(|part| sc.spawn(move || check(part))).collect();
            hs.into_iter().map(|h| h.join().expect("status worker panicked")).collect()
        });
        let (mut out, mut clean) = (Vec::new(), Vec::new());
        for p in parts {
            let (o, c) = p?;
            out.extend(o);
            clean.extend(c);
        }
        Ok((out, clean))
    }

    /// Record that the `verified` entries (read just now and found clean) are no longer racy, by
    /// moving the index's `written_ns` forward. Best effort: skipped when the repository lock is
    /// held or the index changed since `ix` was loaded, so a concurrent writer is never clobbered.
    fn refresh_racy(&self, ix: &Index, verified: &BTreeSet<String>) {
        let next = ix.settled_written_ns(now_ns(), self.index_racy_ns, |p| verified.contains(p));
        if !ix.unlocks(next, self.index_racy_ns) {
            return;
        }
        let Ok(_l) = self.lock() else { return };
        let bytes = ix.encode();
        if self.rd("index").ok().as_deref() != Some(bytes.as_slice()) {
            return;
        }
        let mut ix = ix.clone();
        ix.written_ns = next;
        let _ = crate::meta::write_cache_atomic(&self.vcs.join("index"), &ix.encode());
    }

    /// Directory scan for untracked and unsupported entries. The scan is syscall-bound (one
    /// `read_dir` per directory), so sibling subdirectories are walked on extra threads while
    /// the shared `budget` lasts; results are assembled in directory order, so the output does
    /// not depend on scheduling.
    #[allow(clippy::too_many_arguments)]
    fn walk_untracked(
        &self,
        dir: &str,
        zones: &Zones,
        ws: &Workspace,
        ix: &Index,
        budget: &AtomicUsize,
        parent_stack: &IgnoreStack,
        ctx: &WalkCtx,
    ) -> Result<WalkOut> {
        let stack = parent_stack.descend(self.wfs.as_ref(), &self.worktree, dir)?;
        enum Item {
            Unsupported(String),
            File(String),
            Dir(String, bool, bool),
        }
        let abs = || {
            if dir.is_empty() {
                self.worktree.clone()
            } else {
                self.worktree.join(dir)
            }
        };
        let mut out = WalkOut::default();
        let stamp0 = if ctx.use_cache {
            self.wfs.stat(dir).ok().and_then(|m| stamp_of(&m))
        } else {
            None
        };
        let cached = match (stamp0, ctx.cache.dirs.get(dir)) {
            (Some(s), Some(l)) if l.stamp == s && s.settled(ctx.scan_start_ns, ctx.racy_ns) => Some(&l.entries),
            _ => None,
        };
        let listed;
        let entries: &Vec<(String, Kind)> = match cached {
            Some(e) => {
                out.hits.push(dir.to_owned());
                e
            }
            None => {
                let mut ents = io(&abs(), self.wfs.read_dir(dir))?;
                ents.sort_by(|a, b| a.name.cmp(&b.name));
                let mut v = Vec::with_capacity(ents.len());
                for e in ents {
                    let name = e.name;
                    if name.eq_ignore_ascii_case(META_DIR) || validate_component(&name).is_err() {
                        continue;
                    }
                    let kind = match e.kind {
                        FileKind::Other => Kind::Other,
                        FileKind::Dir => Kind::Dir,
                        _ => Kind::File,
                    };
                    v.push((name, kind));
                }
                out.scanned += 1;
                // Trust the stamp only if it was identical before and after the listing.
                let stamp1 = if ctx.use_cache {
                    self.wfs.stat(dir).ok().and_then(|m| stamp_of(&m))
                } else {
                    None
                };
                listed = v;
                if let (Some(a), Some(b)) = (stamp0, stamp1) {
                    if a == b && a.settled(ctx.scan_start_ns, ctx.racy_ns) {
                        out.fresh.push((
                            dir.to_owned(),
                            Listing {
                                stamp: a,
                                entries: listed.clone(),
                            },
                        ));
                    }
                }
                &listed
            }
        };
        let mut items = Vec::new();
        // Tracked names directly in this directory, as two sorted lists: matching the (sorted)
        // listing against them is a linear merge, with no per-name lookup or path allocation.
        let ch = ix.entries.children(dir);
        let mut fp = 0usize;
        for (name, kind) in entries {
            while fp < ch.files.len() && ch.files[fp] < name.as_str() {
                fp += 1;
            }
            let file_tracked = fp < ch.files.len() && ch.files[fp] == name.as_str();
            if file_tracked && *kind == Kind::File {
                continue; // tracked file: nothing to report whichever zone owns it
            }
            let rel = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
            if *kind == Kind::Other {
                items.push(Item::Unsupported(rel));
                continue;
            }
            if !ws.subs.contains_key(zones.owner(&rel)) {
                continue;
            }
            if *kind == Kind::Dir {
                let tracked_below = file_tracked || ch.dirs.binary_search(&name.as_str()).is_ok();
                let ignored = stack.is_ignored(&rel, true);
                if ignored && !tracked_below {
                    continue;
                }
                items.push(Item::Dir(rel, tracked_below, ignored));
            } else if !stack.is_ignored(&rel, false) {
                items.push(Item::File(rel));
            }
        }
        let subdirs: Vec<(&String, bool)> = items
            .iter()
            .filter_map(|i| if let Item::Dir(r, _, ig) = i { Some((r, *ig)) } else { None })
            .collect();
        let mut subs: Vec<Option<Result<WalkOut>>> = (0..subdirs.len()).map(|_| None).collect();
        // Workers pull subdirectories from a shared counter: at most `budget` extra threads per
        // directory (not one thread per subdirectory, which costs a spawn for each of thousands
        // of small directories); this thread works too.
        let next = AtomicUsize::new(0);
        let work = |next: &AtomicUsize| -> Vec<(usize, Result<WalkOut>)> {
            let mut done = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::AcqRel);
                let Some((rel, ig)) = subdirs.get(i) else { break };
                let child = if *ig { stack.inherit() } else { stack.clone() };
                done.push((i, self.walk_untracked(rel, zones, ws, ix, budget, &child, ctx)));
            }
            done
        };
        let mut extra = 0usize;
        while extra + 1 < subdirs.len() {
            // `fetch_update` is renamed `try_update` on newer toolchains; keep the name older ones know.
            #[allow(deprecated)]
            let take = budget
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| b.checked_sub(1))
                .is_ok();
            if !take {
                break;
            }
            extra += 1;
        }
        std::thread::scope(|sc| {
            let work = &work;
            let next = &next;
            let hs: Vec<_> = (0..extra)
                .map(|_| {
                    sc.spawn(move || {
                        let r = work(next);
                        budget.fetch_add(1, Ordering::AcqRel);
                        r
                    })
                })
                .collect();
            let mut all = work(next);
            for h in hs {
                all.extend(h.join().expect("walk worker panicked"));
            }
            for (i, r) in all {
                subs[i] = Some(r);
            }
        });
        let mut subs = subs.into_iter();
        for it in items {
            match it {
                Item::Unsupported(r) => out.unsupported.push(r),
                Item::File(r) => out.untracked.push(r),
                Item::Dir(rel, tracked_below, _) => {
                    let mut sub = subs.next().flatten().expect("one result per subdir")?;
                    out.unsupported.append(&mut sub.unsupported);
                    out.hits.append(&mut sub.hits);
                    out.fresh.append(&mut sub.fresh);
                    out.scanned += sub.scanned;
                    if !sub.untracked.is_empty() {
                        if tracked_below {
                            out.untracked.append(&mut sub.untracked);
                        } else {
                            out.untracked.push(format!("{rel}/"));
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    // ---------- diff ----------

    /// Unified diff: working tree vs index, or (staged) index vs branch heads.
    pub fn diff(&self, staged: bool) -> Result<String> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let ix = self.index()?;
        let mut out = String::new();
        if staged {
            let mut heads = Flat::new();
            for zone in ws.subs.keys() {
                heads.extend(self.head_flat(zone, &zones, &ws, &refs)?);
            }
            let mut paths: BTreeSet<&String> = heads.keys().collect();
            paths.extend(ix.entries.keys());
            for p in paths {
                let old = heads.get(p).map(|(_, id)| *id);
                let new = ix.entries.get(p).map(|e| e.id);
                if old == new && heads.get(p).map(|h| h.0) == ix.entries.get(p).map(|e| e.mode) {
                    continue;
                }
                let o = old.map(|i| self.store.get_content(&i)).transpose()?;
                let n = new.map(|i| self.store.get_content(&i)).transpose()?;
                out.push_str(&diff::unified(p, o.as_deref(), n.as_deref()));
            }
        } else {
            let (ws, zones) = (self.workspace()?, self.zones()?);
            for (p, e) in &ix.entries {
                if ws.skips(zones.owner(p), p) {
                    continue;
                }
                let abs = self.worktree.join(p);
                let cur = match fs::symlink_metadata(&abs) {
                    Ok(m) if m.file_type().is_symlink() && e.mode == EntryMode::Symlink => Some(io(&abs, read_link_bytes(&abs))?),
                    Ok(m) if m.is_file() && e.mode != EntryMode::Symlink => Some(io(&abs, fs::read(&abs))?),
                    Ok(_) => continue,
                    Err(x) if x.kind() == std::io::ErrorKind::NotFound => None,
                    Err(source) => return Err(Error::Io { path: abs, source }),
                };
                let old = self.store.get_content(&e.id)?;
                out.push_str(&diff::unified(p, Some(&old), cur.as_deref()));
            }
        }
        Ok(out)
    }

    // ---------- commit ----------

    /// Commit staged changes in every subscribed zone that has any, as ONE atomic
    /// transaction: all zone branch heads advance together or not at all.
    pub fn commit(&self, message: &str, author: Signature) -> Result<CommitReport> {
        if message.trim().is_empty() {
            return msg("empty commit message");
        }
        let _l = self.lock()?;
        self.rebase_guard("commit")?;
        let merging = self.merge_commit_guard()?;
        // A cherry-pick/revert stopped on conflicts commits a single-parent commit.
        let is_pick = match (&merging, self.pick_marker()?) {
            (Some(m), Some((_, src))) => m.theirs == src,
            _ => false,
        };
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let mut ix = self.index()?;
        let fp = zones_fp(&zones);
        let mut fresh_trees: BTreeMap<String, ObjectId> = BTreeMap::new();
        let op_author = author.clone();
        let mut updates = Vec::new();
        let mut commits = Vec::new();
        for (zone, branch) in &ws.subs {
            let parent = refs.head(zone, branch);
            let parent_root = match parent {
                Some(p) => Some(self.commit_obj(&p)?.root),
                None => None,
            };
            let zpath = zones.path_of(zone).unwrap_or("");
            let mut it = treeops::IndexTrees::new(&self.store, &ix.entries, &zones, zone, ix.entries.cached_trees(&fp));
            if parent_root.is_none() && it.is_empty(zpath)? {
                continue;
            }
            let root = it.build_root(zpath, parent_root)?;
            self.counters.tree_puts.fetch_add(it.puts as u64, Ordering::Relaxed);
            fresh_trees.extend(it.cacheable());
            let second = merging
                .as_ref()
                .filter(|m| !is_pick && m.zone == *zone && Some(m.ours) == parent)
                .map(|m| m.theirs);
            if Some(root) == parent_root && second.is_none() {
                continue;
            }
            let change_id = fresh_change_id(zone, &root, parent.as_ref(), &author);
            let c = Commit {
                zone: zone.clone(),
                parents: parent.into_iter().chain(second).collect(),
                root,
                change_id,
                author: author.clone(),
                message: message.to_owned(),
            };
            let cid = self.store.put(&Object::Commit(c))?;
            updates.push(RefUpdate {
                zone: zone.clone(),
                branch: branch.clone(),
                old: parent,
                new: cid,
            });
            commits.push((zone.clone(), branch.clone(), cid));
        }
        if updates.is_empty() {
            return Err(Error::NothingToCommit);
        }
        updates.sort();
        // Transaction object is written BEFORE refs move (immutable data first, pointer last).
        let tx = self.store.put(&Object::Transaction(Transaction {
            updates: updates.clone(),
            author,
            message: message.to_owned(),
            lock_ops: vec![],
        }))?;
        refs.apply(&updates)?;
        // Durability barrier: trees, commits and the transaction are durable before refs name them.
        self.store.sync()?;
        write_atomic(&self.common.join("refs"), &refs.encode())?; // linearization point
                                                                  // Remember the tree ids just computed so the next commit/status can skip unchanged
                                                                  // directories. `written_ns` is deliberately untouched: no entry was re-verified. Best
                                                                  // effort: a lost cache write only costs time.
        if !fresh_trees.is_empty() {
            ix.entries.store_trees(fp, fresh_trees);
            let _ = crate::meta::write_cache_atomic(&self.vcs.join("index"), &ix.encode());
        }
        if merging.is_some() {
            self.clear_merge_state()?;
        }
        let first = message.lines().next().unwrap_or("");
        self.record_op(&format!("commit: {first}"), Some(op_author))?;
        Ok(CommitReport { transaction: tx, commits })
    }

    // ---------- log ----------

    /// History reachable from the branch checked out for `zone` (or `main` if unsubscribed),
    /// newest first (by timestamp, ties by id).
    pub fn log(&self, zone: &str, branch: Option<&str>, limit: usize) -> Result<Vec<(ObjectId, Commit)>> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let zones = self.zones()?;
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let head = match branch {
            Some(b) => refs
                .head(zone, b)
                .ok_or_else(|| Error::Msg(format!("no such branch {zone}/{b}")))?
                .into(),
            None => self.current_head(zone, &ws, &refs),
        };
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut heap: BinaryHeap<(u64, ObjectId)> = BinaryHeap::new();
        let mut cache: BTreeMap<ObjectId, Commit> = BTreeMap::new();
        if let Some(h) = head {
            let c = self.commit_obj(&h)?;
            heap.push((c.author.timestamp, h));
            cache.insert(h, c);
        }
        while let Some((_, id)) = heap.pop() {
            if out.len() >= limit {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            let c = cache.remove(&id).expect("cached");
            for p in &c.parents {
                if !seen.contains(p) && !cache.contains_key(p) {
                    let pc = self.commit_obj(p)?;
                    if pc.zone != c.zone {
                        return Err(Error::Corrupt(format!("commit {} has parent from different zone", id)));
                    }
                    heap.push((pc.author.timestamp, *p));
                    cache.insert(*p, pc);
                }
            }
            out.push((id, c));
        }
        Ok(out)
    }

    // ---------- branches ----------

    pub fn branches(&self, zone: &str) -> Result<Vec<(String, Option<ObjectId>, bool)>> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let zones = self.zones()?;
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let cur = ws.subs.get(zone);
        let mut v: BTreeMap<String, Option<ObjectId>> = refs.branches_of(zone).into_iter().map(|(b, i)| (b, Some(i))).collect();
        if let Some(c) = cur {
            v.entry(c.clone()).or_insert(None);
        }
        Ok(v.into_iter().map(|(b, i)| (b.clone(), i, cur == Some(&b))).collect())
    }

    /// Create `branch` in `zone` at the current head of the checked-out branch.
    pub fn create_branch(&self, zone: &str, branch: &str) -> Result<ObjectId> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(branch)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        if refs.head(zone, branch).is_some() {
            return msg(format!("branch {zone}/{branch} already exists"));
        }
        let Some(head) = self.current_head(zone, &ws, &refs) else {
            return msg(format!("zone {zone:?} has no commits yet; commit first"));
        };
        refs.apply(&[RefUpdate {
            zone: zone.into(),
            branch: branch.into(),
            old: None,
            new: head,
        }])?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.record_op(&format!("create branch {zone}/{branch}"), None)?;
        Ok(head)
    }

    pub(crate) fn require_clean(&self, zone: &str, st: &Status) -> Result<()> {
        if st.merging.as_ref().is_some_and(|m| m.zone == zone) {
            return msg(format!(
                "zone {zone:?} is in the middle of a merge; resolve and commit, or `vcs merge --abort`"
            ));
        }
        let a = st.zones.iter().find(|a| a.zone == zone).expect("subscribed");
        if !a.staged.is_empty() || !a.unstaged.is_empty() {
            return msg(format!(
                "zone {zone:?} has uncommitted changes ({} staged, {} unstaged); commit or revert them first",
                a.staged.len(),
                a.unstaged.len()
            ));
        }
        Ok(())
    }

    /// Switch the checked-out branch of one zone. Other zones are untouched.
    pub fn checkout(&self, zone: &str, branch: &str) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        validate_branch_name(branch)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        let Some(cur) = ws.subs.get(zone).cloned() else {
            return msg(format!("zone {zone:?} is not subscribed in this workspace"));
        };
        if cur == branch {
            return Ok(());
        }
        self.require_branch_free(zone, branch)?;
        self.adopt_tracking(&mut refs, zone, branch)?;
        let Some(target) = refs.head(zone, branch) else {
            return msg(format!("no such branch {zone}/{branch}"));
        };
        let mut ix = self.index()?;
        // Only staged/unstaged/merge state matters here, so skip the untracked-file walk.
        let st = self.status_inner(StatusOpts::default(), &ix, false, None)?;
        self.require_clean(zone, &st)?;
        let prefix = zones.path_of(zone).unwrap();
        let old_root = match self.current_head(zone, &ws, &refs) {
            Some(h) => Some(self.commit_obj(&h)?.root),
            None => None,
        };
        let new_root = self.commit_obj(&target)?.root;
        // Subtrees that are identical on both branches are never read.
        let diff = treeops::diff_trees(&self.store, old_root, Some(new_root), prefix)?;
        self.transition_diff(zone, &zones, &diff, &mut ix)?;
        ws.subs.insert(zone.into(), branch.into());
        self.save_index(&mut ix)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("checkout {zone}/{branch}"), None)?;
        Ok(())
    }

    // ---------- zones ----------

    pub fn create_zone(&self, name: &str, path: &str) -> Result<()> {
        validate_zone_name(name)?;
        let _l = self.lock()?;
        let mut zones = self.zones()?;
        let mut ws = self.workspace()?;
        let refs = self.refs()?;
        let ix = self.index()?;
        zones.create(name, path)?; // validates path + non-overlap
                                   // The new path must not already hold root-zone content.
        if !ix.under(path).is_empty() {
            return msg(format!(
                "cannot create zone at {path:?}: tracked files exist there (root zone owns them)"
            ));
        }
        let root_flat = self.head_flat(ROOT_ZONE, &Zones::new(), &ws, &refs)?;
        if root_flat.keys().any(|k| k == path || k.starts_with(&format!("{path}/"))) {
            return msg(format!("cannot create zone at {path:?}: committed root-zone content exists there"));
        }
        write_atomic(&self.common.join("zones"), &zones.encode())?;
        ws.subs.insert(name.into(), DEFAULT_BRANCH.into());
        self.save_ws(&ws)?;
        self.record_op(&format!("create zone {name} at {path}"), None)?;
        Ok(())
    }

    /// Subscribe to an existing zone and materialize it (sparse checkout by zone).
    pub fn zone_add(&self, zone: &str, branch: Option<&str>) -> Result<()> {
        let _l = self.lock()?;
        self.zone_add_locked(zone, branch)
    }

    /// [`Repo::zone_add`] for a caller that already holds the repository lock.
    pub(crate) fn zone_add_locked(&self, zone: &str, branch: Option<&str>) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let mut refs = self.refs()?;
        if !zones.exists(zone) {
            return msg(format!("no such zone {zone:?}"));
        }
        if ws.subs.contains_key(zone) {
            return msg(format!("zone {zone:?} is already subscribed"));
        }
        let branch = branch.unwrap_or(DEFAULT_BRANCH);
        validate_branch_name(branch)?;
        self.require_branch_free(zone, branch)?;
        self.adopt_tracking(&mut refs, zone, branch)?;
        let head = refs.head(zone, branch);
        if head.is_none() && branch != DEFAULT_BRANCH {
            return msg(format!("no such branch {zone}/{branch}"));
        }
        let new = match head {
            Some(h) => treeops::flatten(&self.store, Some(self.commit_obj(&h)?.root), zones.path_of(zone).unwrap())?,
            None => Flat::new(),
        };
        let mut ix = self.index()?;
        self.transition(zone, &zones, &Flat::new(), &new, &mut ix)?;
        ws.subs.insert(zone.into(), branch.into());
        self.save_index(&mut ix)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("zone add {zone}/{branch}"), None)?;
        Ok(())
    }

    /// Unsubscribe: remove the zone's tracked files from the working tree. Refuses if dirty.
    pub fn zone_remove(&self, zone: &str) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        if zone == ROOT_ZONE {
            return msg("the root zone cannot be removed from the workspace");
        }
        let _l = self.lock()?;
        let zones = self.zones()?;
        let mut ws = self.workspace()?;
        let refs = self.refs()?;
        if !ws.subs.contains_key(zone) {
            return msg(format!("zone {zone:?} is not subscribed"));
        }
        let st = self.status()?;
        self.require_clean(zone, &st)?;
        let old = self.head_flat(zone, &zones, &ws, &refs)?;
        let mut ix = self.index()?;
        self.transition(zone, &zones, &old, &Flat::new(), &mut ix)?;
        ws.subs.remove(zone);
        ws.sparse.remove(zone);
        self.save_index(&mut ix)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("zone remove {zone}"), None)?;
        Ok(())
    }

    /// [`Repo::transition`] for a [`treeops::TreeDiff`]: the same checks and writes restricted to
    /// the paths that differ. The case-folding collision scan looks at the entry names of every
    /// directory that is new on the target side (two colliding paths always share a directory
    /// whose entries collide), instead of folding every tracked path.
    pub(crate) fn transition_diff(&self, zone: &str, zones: &Zones, diff: &treeops::TreeDiff, ix: &mut Index) -> Result<()> {
        for names in &diff.new_dir_names {
            crate::portable::require_no_collision(names.iter().map(String::as_str), "checkout")?;
        }
        self.transition_check_paths(zone, zones, &diff.old, &diff.new)?;
        self.transition_apply(zone, &diff.old, &diff.new, ix)
    }

    /// Move the working tree + index of one zone from `old` to `new` (flat, full paths).
    /// Preflight validates everything before any file is touched.
    pub(crate) fn transition(&self, zone: &str, zones: &Zones, old: &Flat, new: &Flat, ix: &mut Index) -> Result<()> {
        self.transition_check(zone, zones, old, new, ix)?;
        self.transition_apply(zone, old, new, ix)
    }

    /// The preflight half of [`Repo::transition`]: validates without touching any file, so a
    /// caller moving several zones can check all of them before changing any.
    pub(crate) fn transition_check(&self, zone: &str, zones: &Zones, old: &Flat, new: &Flat, ix: &Index) -> Result<()> {
        crate::portable::require_no_collision(
            new.keys()
                .map(String::as_str)
                .chain(ix.entries.keys().filter(|p| !old.contains_key(*p)).map(String::as_str)),
            "checkout",
        )?;
        self.transition_check_paths(zone, zones, old, new)
    }

    /// Everything in the preflight except the whole-tree collision scan. `old` and `new` may be
    /// the changed parts of the two trees only (unchanged paths in neither).
    fn transition_check_paths(&self, zone: &str, zones: &Zones, old: &Flat, new: &Flat) -> Result<()> {
        let ws = self.workspace()?;
        self.transition_check_paths_ws(&ws, zone, zones, old, new)
    }

    pub(crate) fn transition_check_paths_ws(&self, ws: &Workspace, zone: &str, zones: &Zones, old: &Flat, new: &Flat) -> Result<()> {
        let changed: Vec<&String> = new.iter().filter(|(p, v)| old.get(*p) != Some(v)).map(|(p, _)| p).collect();
        for p in &changed {
            validate_path(p)?;
            if zones.owner(p) != zone {
                return msg(format!(
                    "branch content for zone {zone:?} contains {p:?}, which belongs to zone {:?}",
                    zones.owner(p)
                ));
            }
            if new[*p].0 == EntryMode::Symlink {
                let t = self.store.get_blob(&new[*p].1)?;
                crate::portable::check_symlink_target(p, &t).or_else(|why| msg(format!("refusing to check out symlink {p:?}: {why}")))?;
                if !cfg!(unix) {
                    return msg(format!("cannot check out symlink {p:?}: symlinks are only supported on unix"));
                }
            }
            if ws.skips(zone, p) {
                continue; // not materialized: nothing is written, so nothing on disk can be in the way
            }
            if !old.contains_key(*p) && self.wfs.stat(p).is_ok() && !replaces_folded_old_path(&self.worktree, p, old, new) {
                return msg(format!("untracked path {p:?} would be overwritten; move or remove it first"));
            }
            // no symlinked / file ancestors
            let mut rel = String::new();
            let comps: Vec<&str> = p.split('/').collect();
            for c in &comps[..comps.len() - 1] {
                if !rel.is_empty() {
                    rel.push('/');
                }
                rel.push_str(c);
                match self.wfs.stat(&rel) {
                    Ok(m) if m.kind == FileKind::Symlink => {
                        return msg(format!(
                            "refusing to write through symlink at {}",
                            self.worktree.join(&rel).display()
                        ))
                    }
                    Ok(m) if m.kind == FileKind::File && (!old.contains_key(&rel) || new.contains_key(&rel)) => {
                        return msg(format!("cannot create {p:?}: {rel:?} is a file"));
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn transition_apply(&self, zone: &str, old: &Flat, new: &Flat, ix: &mut Index) -> Result<()> {
        let ws = self.workspace()?;
        self.transition_apply_ws(&ws, zone, old, new, ix)
    }

    /// [`Self::transition_apply`] under an explicit workspace (its sparse sets decide which paths
    /// reach the disk). Paths outside the zone's sparse set only move in the index, with an empty
    /// stat, so the commit that follows carries them over from the tree unchanged.
    pub(crate) fn transition_apply_ws(&self, ws: &Workspace, zone: &str, old: &Flat, new: &Flat, ix: &mut Index) -> Result<()> {
        let skip = |p: &str| ws.skips(zone, p);
        let all_changed: Vec<&String> = new.iter().filter(|(p, v)| old.get(*p) != Some(v)).map(|(p, _)| p).collect();
        let (virt, changed): (Vec<&String>, Vec<&String>) = all_changed.into_iter().partition(|p| skip(p));
        // deletions
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for p in old.keys().filter(|p| !new.contains_key(*p)) {
            if skip(p) {
                ix.entries.remove(p);
                continue;
            }
            match self.wfs.stat(p) {
                Ok(m) if matches!(m.kind, FileKind::File | FileKind::Symlink) => io(&self.worktree.join(p), self.wfs.remove(p))?,
                Ok(_) => return msg(format!("expected file at {p:?}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(Error::Io {
                        path: self.worktree.join(p),
                        source,
                    })
                }
            }
            ix.entries.remove(p);
            dirs.insert(p.rsplit_once('/').map_or("", |(d, _)| d).to_owned());
        }
        // writes: independent per path, so a big batch runs on a bounded pool (8 threads)
        let write_one = |p: &String| -> Result<IndexEntry> {
            let (mode, id) = new[p];
            let data = self.store.get_content(&id)?;
            let abs = || self.worktree.join(p);
            let m = if mode == EntryMode::Symlink {
                io(&abs(), self.wfs.symlink(&data, p))?
            } else {
                io(&abs(), self.wfs.write(p, &data, mode))?
            };
            Ok(IndexEntry {
                id,
                mode,
                size: m.len,
                mtime_ns: m.mtime_ns,
                ctime_ns: m.ctime_ns,
            })
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let written: Vec<Result<IndexEntry>> = if threads <= 1 || changed.len() < 64 {
            changed.iter().map(|p| write_one(p)).collect()
        } else {
            let per = changed.len().div_ceil(threads);
            std::thread::scope(|sc| {
                let write_one = &write_one;
                let hs: Vec<_> = changed
                    .chunks(per)
                    .map(|part| sc.spawn(move || part.iter().map(|p| write_one(p)).collect::<Vec<_>>()))
                    .collect();
                hs.into_iter().flat_map(|h| h.join().expect("checkout writer panicked")).collect()
            })
        };
        for (p, e) in changed.iter().zip(written) {
            let e = e?;
            ix.entries.remove(p);
            ix.insert(p, e)?;
        }
        for p in virt {
            let (mode, id) = new[p];
            ix.entries.remove(p);
            ix.insert(
                p,
                IndexEntry {
                    id,
                    mode,
                    size: 0,
                    mtime_ns: 0,
                    ctime_ns: 0,
                },
            )?;
        }
        // prune now-empty directories (never the worktree root)
        self.prune_dirs_fs(dirs);
        Ok(())
    }

    /// Remove each directory in `dirs` (worktree-relative) and its parents while they are empty.
    pub(crate) fn prune_dirs_fs(&self, dirs: BTreeSet<String>) {
        for mut d in dirs {
            while !d.is_empty() && self.wfs.remove_dir(&d).is_ok() {
                d = d.rsplit_once('/').map_or("", |(p, _)| p).to_owned();
            }
        }
    }

    // ---------- snapshots ----------

    /// Pin the current head of every zone (checked-out branch if subscribed, else `main`).
    pub fn snapshot_create(&self, name: &str, message: &str, author: Signature) -> Result<ObjectId> {
        validate_branch_name(name)?;
        let _l = self.lock()?;
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        if refs.snapshots.contains_key(name) {
            return msg(format!("snapshot {name:?} already exists"));
        }
        let mut pinned = BTreeMap::new();
        for a in zones.names() {
            if let Some(h) = self.current_head(a, &ws, &refs) {
                pinned.insert(a.to_owned(), h);
            }
        }
        if pinned.is_empty() {
            return msg("nothing to snapshot: no zone has commits");
        }
        let id = self.store.put(&Object::Snapshot(Snapshot {
            zones: pinned,
            author,
            message: message.to_owned(),
        }))?;
        refs.snapshots.insert(name.to_owned(), id);
        self.store.sync()?;
        write_atomic(&self.common.join("refs"), &refs.encode())?;
        self.record_op(&format!("snapshot create {name}"), None)?;
        Ok(id)
    }

    pub fn snapshot_get(&self, name: &str) -> Result<(ObjectId, Snapshot)> {
        let refs = self.refs()?;
        let id = *refs
            .snapshots
            .get(name)
            .ok_or_else(|| Error::Msg(format!("no such snapshot {name:?}")))?;
        match self.store.get(&id)? {
            Object::Snapshot(s) => Ok((id, s)),
            _ => Err(Error::Corrupt(format!("{id} is not a snapshot"))),
        }
    }

    // ---------- fsck ----------

    /// Ids the remote is known to have: everything reachable, through objects we hold, from a
    /// remote-tracking head (whose whole closure was verified and fetched). Missing ids are
    /// included as leaves.
    fn remote_closure(&self) -> Result<BTreeSet<ObjectId>> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<ObjectId> = self.remote_refs()?.branches.values().copied().collect();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Ok(o) = self.store.get(&id) {
                crate::gc::children(&o, &mut stack);
            }
        }
        Ok(seen)
    }

    /// `store.get` for every id, on up to 8 threads, results in the order of `ids`. A blob is
    /// reduced to an empty blob plus its length (`Listed::Blob`), so memory does not grow with
    /// repository size.
    fn fsck_fetch(&self, ids: &[ObjectId]) -> Vec<(std::result::Result<Object, vcs_store::StoreError>, Listed)> {
        let one = |id: &ObjectId| match self.store.get(id) {
            Err(e) => (Err(e), Listed::Failed),
            Ok(Object::Blob(b)) => (Ok(Object::Blob(Vec::new())), Listed::Blob(b.len())),
            Ok(o) => (Ok(o), Listed::Other),
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        if threads <= 1 || ids.len() < 64 {
            return ids.iter().map(one).collect();
        }
        let per = ids.len().div_ceil(threads);
        std::thread::scope(|sc| {
            let one = &one;
            let hs: Vec<_> = ids
                .chunks(per)
                .map(|part| sc.spawn(move || part.iter().map(one).collect::<Vec<_>>()))
                .collect();
            hs.into_iter().flat_map(|h| h.join().expect("fsck worker panicked")).collect()
        })
    }

    /// Verify every object (hash + canonical decode) and graph invariants.
    pub fn fsck(&self) -> Result<FsckReport> {
        let mut rep = FsckReport::default();
        let ids = self.store.list()?;
        let refetchable = self.remote_closure()?;
        let mut commits: BTreeMap<ObjectId, Commit> = BTreeMap::new();
        let mut evictable: BTreeSet<ObjectId> = BTreeSet::new();
        // A missing object is an evictable cache miss only if the remote is known to have it.
        let mut missing = |this: &Self, id: &ObjectId, what: String, problems: &mut Vec<String>| -> Result<()> {
            if this.store.contains(id)? {
                return Ok(());
            }
            if refetchable.contains(id) {
                evictable.insert(*id);
            } else {
                problems.push(what);
            }
            Ok(())
        };
        // Read + hash + decode every object on a few threads. Blob contents are reduced to their
        // length right away (a chunk manifest check only needs that), so memory stays small.
        let fetched = self.fsck_fetch(&ids);
        let listed: std::collections::HashMap<ObjectId, Listed> = ids.iter().zip(&fetched).map(|(id, (_, l))| (*id, *l)).collect();
        for (id, (fetched, _)) in ids.iter().zip(fetched) {
            rep.objects += 1;
            match fetched {
                Err(e) => rep.problems.push(format!("{id}: {e}")),
                Ok(Object::Commit(c)) => {
                    commits.insert(*id, c);
                }
                Ok(Object::Tree(t)) => {
                    for e in t.entries() {
                        missing(
                            self,
                            &e.id,
                            format!("tree {id}: missing entry {} -> {}", e.name, e.id),
                            &mut rep.problems,
                        )?;
                    }
                }
                Ok(Object::Snapshot(s)) => {
                    for (a, c) in &s.zones {
                        missing(
                            self,
                            c,
                            format!("snapshot {id}: missing commit {c} for zone {a}"),
                            &mut rep.problems,
                        )?;
                    }
                }
                Ok(Object::Chunked(m)) => {
                    for c in &m.chunks {
                        missing(
                            self,
                            &c.id,
                            format!("chunk manifest {id}: missing chunk {}", c.id),
                            &mut rep.problems,
                        )?;
                        // A present chunk must be a plain blob of the recorded length. Chunks were
                        // already read and hashed above; only an unlisted one is read again.
                        let state = match listed.get(&c.id) {
                            Some(l) => *l,
                            None if self.store.contains(&c.id)? => match self.store.get(&c.id) {
                                Ok(Object::Blob(b)) => Listed::Blob(b.len()),
                                Ok(_) => Listed::Other,
                                Err(_) => Listed::Failed,
                            },
                            None => continue,
                        };
                        match state {
                            Listed::Blob(n) if n == c.len as usize => {}
                            Listed::Failed => {} // reported when the chunk itself is checked
                            _ => rep
                                .problems
                                .push(format!("chunk manifest {id}: chunk {} is not a blob of {} bytes", c.id, c.len)),
                        }
                    }
                }
                Ok(_) => {}
            }
        }
        for (id, c) in &commits {
            missing(
                self,
                &c.root,
                format!("commit {id}: missing root tree {}", c.root),
                &mut rep.problems,
            )?;
            for p in &c.parents {
                match commits.get(p) {
                    None => missing(self, p, format!("commit {id}: missing parent {p}"), &mut rep.problems)?,
                    Some(pc) if pc.zone != c.zone => rep
                        .problems
                        .push(format!("commit {id}: parent {p} is in zone {} not {}", pc.zone, c.zone)),
                    _ => {}
                }
            }
        }
        // No index of any worktree may name a blob that is not in the store.
        let indexes = self.fsck_worktrees(&mut rep.problems);
        let linked = indexes.len() > 1;
        for (wt, ix) in &indexes {
            for (p, e) in &ix.entries {
                let what = if linked {
                    format!("index {p} (worktree {wt}): missing blob {}", e.id)
                } else {
                    format!("index {p}: missing blob {}", e.id)
                };
                missing(self, &e.id, what, &mut rep.problems)?;
            }
        }
        let refs = self.refs()?;
        for ((a, b), h) in &refs.branches {
            match commits.get(h) {
                None if refetchable.contains(h) && !self.store.contains(h)? => {
                    evictable.insert(*h);
                }
                None => rep.problems.push(format!("ref {a}/{b}: points to missing/non-commit {h}")),
                Some(c) if &c.zone != a => rep.problems.push(format!("ref {a}/{b}: commit belongs to zone {}", c.zone)),
                _ => {}
            }
        }
        rep.evictable = evictable.into_iter().collect();
        self.fsck_ops(&mut rep.problems);
        self.fsck_commit_graph(&mut rep.problems);
        rep.problems.extend(self.store.verify_packs());
        Ok(rep)
    }

    /// Default minimum age of a loose object before an automatic repack packs it.
    pub const AUTO_REPACK_GRACE: std::time::Duration = std::time::Duration::from_secs(3600);
    /// Default `gc.autoPackLimit`, as git's `gc.auto`.
    pub const AUTO_PACK_LIMIT: usize = 6700;

    /// Automatic repack: if the loose-object estimate exceeds `limit` (0 disables), pack loose
    /// objects older than `grace` into one pack under the repository lock. `force` skips the
    /// threshold (`vcs repack --auto` still honours it; tests use `force`). Returns the number
    /// packed, or `None` when nothing was due or another process holds the lock (try again later).
    pub fn auto_repack_at(
        &self,
        now: std::time::SystemTime,
        limit: usize,
        grace: std::time::Duration,
        force: bool,
    ) -> Result<Option<usize>> {
        if !force && (limit == 0 || self.store.loose_estimate() <= limit) {
            return Ok(None);
        }
        let _lock = match self.lock() {
            Ok(l) => l,
            Err(Error::Locked(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(Some(self.store.repack_older_than(now, grace)?))
    }

    /// [`Repo::auto_repack_at`] as of now.
    pub fn auto_repack(&self, limit: usize, grace: std::time::Duration) -> Result<Option<usize>> {
        self.auto_repack_at(std::time::SystemTime::now(), limit, grace, false)
    }

    /// Move all loose objects into a new pack. Returns how many were packed.
    pub fn repack(&self) -> Result<usize> {
        Ok(self.store.repack()?)
    }

    /// Repack in an explicit pack format; also rewrites existing packs of the other format.
    /// `vcs repack --format v1` is the rollback to the format older binaries read.
    /// Returns `(loose objects packed, packs converted)`.
    pub fn repack_as(&self, fmt: vcs_store::pack::PackFormat) -> Result<(usize, usize)> {
        Ok(self.store.repack_as(fmt, true)?)
    }
}

/// What fsck saw when it read a listed object, kept for checking chunk manifests.
#[derive(Clone, Copy)]
enum Listed {
    Blob(usize),
    Other,
    Failed,
}

pub(crate) fn fresh_change_id(zone: &str, root: &ObjectId, parent: Option<&ObjectId>, author: &Signature) -> ChangeId {
    // Unique per new logical change: mixes wall-clock nanos, pid, and a process-wide counter
    // with content context. Not a secret; collision resistance only.
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let mut h = blake3::Hasher::new_derive_key("all-git 2026 change id v1");
    h.update(zone.as_bytes());
    h.update(root.as_bytes());
    if let Some(p) = parent {
        h.update(p.as_bytes());
    }
    h.update(author.name.as_bytes());
    h.update(&now_ns().to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&CTR.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h.finalize().as_bytes()[..16]);
    ChangeId::from_bytes(b)
}

/// On a case- or normalization-folding filesystem (APFS, HFS+, NTFS) a path of `new` that is not
/// in `old` can still find a file on disk: the tracked file this transition removes under a
/// differently spelled name (`readme` -> `README`). That file is ours to replace, not an
/// untracked file in the way. Proven by identity: both spellings must name the same inode, so on
/// a case-sensitive filesystem an unrelated untracked `README` still blocks the checkout.
#[cfg(unix)]
fn replaces_folded_old_path(root: &Path, p: &str, old: &Flat, new: &Flat) -> bool {
    use std::os::unix::fs::MetadataExt;
    let fold = |s: &str| s.split('/').map(crate::portable::fold_component).collect::<Vec<_>>().join("/");
    let want = fold(p);
    let Ok(here) = std::fs::symlink_metadata(root.join(p)) else { return false };
    old.keys().filter(|k| !new.contains_key(*k) && fold(k) == want).any(|k| {
        std::fs::symlink_metadata(root.join(k)).is_ok_and(|m| m.dev() == here.dev() && m.ino() == here.ino())
    })
}

#[cfg(not(unix))]
fn replaces_folded_old_path(_root: &Path, _p: &str, _old: &Flat, _new: &Flat) -> bool {
    false
}
