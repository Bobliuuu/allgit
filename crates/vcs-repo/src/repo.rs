use crate::diff;
use crate::error::{io, msg, Error, Result};
use crate::index::{Index, IndexEntry};
use crate::meta::{write_atomic, LockGuard, Refs, Workspace, Zones, DEFAULT_BRANCH, ROOT_ZONE};
use crate::treeops::{self, Flat};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use vcs_objects::names::{validate_branch_name, validate_component, validate_path, validate_zone_name, META_DIR};
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Snapshot, Transaction};
use vcs_store::{LocalStore, ObjectStore};

const FORMAT_FILE: &str = "format";
const FORMAT_TEXT: &str = "all-git repository format 1\n";
const RACY_NS: u64 = 2_000_000_000;

#[derive(Default)]
struct WalkOut {
    untracked: Vec<String>,
    unsupported: Vec<String>,
}

pub struct Repo {
    pub(crate) worktree: PathBuf,
    pub(crate) vcs: PathBuf,
    pub(crate) store: LocalStore,
    pub(crate) structural: bool,
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

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn mtime_ns(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn mode_of(m: &fs::Metadata) -> EntryMode {
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

enum Wt {
    Clean,
    Modified,
    Deleted,
}

impl Repo {
    /// Enable or disable the structural Rust merge in `merge`/`rebase` (default on; it only has
    /// an effect when built with feature `structural`).
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
            worktree,
            vcs,
            store,
            structural: true,
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
                let store = LocalStore::open(vcs.join("objects"))?;
                return Ok(Repo {
                    worktree: cur,
                    vcs,
                    store,
                    structural: true,
                });
            }
            if !cur.pop() {
                return Err(Error::NotARepo);
            }
        }
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
        LockGuard::acquire(self.vcs.join("lock"))
    }
    pub(crate) fn rd(&self, name: &str) -> Result<Vec<u8>> {
        let p = self.vcs.join(name);
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
    pub(crate) fn save_index(&self, ix: &mut Index) -> Result<()> {
        // Durability barrier (no-op if nothing pending): blobs the index names must be durable.
        self.store.sync()?;
        ix.written_ns = now_ns();
        write_atomic(&self.vcs.join("index"), &ix.encode())
    }
    pub(crate) fn save_ws(&self, ws: &Workspace) -> Result<()> {
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
        for p in rel_paths {
            self.add_one(p, &zones, &ws, &mut ix, &mut rep, &mut todo)?;
        }
        // Read + hash + write the blobs on several threads; the index is updated afterwards.
        self.stage_files(&todo, &mut ix)?;
        self.note_resolved(rel_paths, &ix)?;
        // Durability barrier: every blob the index is about to name must be durable first.
        self.store.sync()?;
        self.save_index(&mut ix)?;
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
                for g in gone {
                    ix.entries.remove(&g);
                    rep.removed += 1;
                }
                Ok(())
            }
            Err(source) => Err(Error::Io { path: full, source }),
            Ok(m) if m.is_dir() => {
                let mut seen = BTreeSet::new();
                self.add_dir(p, zones, ws, rep, &mut seen, todo)?;
                for g in ix.under(p) {
                    if !seen.contains(&g) {
                        ix.entries.remove(&g);
                        rep.removed += 1;
                    }
                }
                Ok(())
            }
            Ok(m) if m.is_file() => {
                validate_path(p)?;
                todo.push(p.to_owned());
                rep.staged += 1;
                Ok(())
            }
            Ok(_) => msg(format!("{p:?}: symlinks and special files are not supported yet")),
        }
    }

    fn add_dir(
        &self,
        dir: &str,
        zones: &Zones,
        ws: &Workspace,
        rep: &mut AddReport,
        seen: &mut BTreeSet<String>,
        todo: &mut Vec<String>,
    ) -> Result<()> {
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
            if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
                rep.skipped.push(format!("{rel} (symlink or special file: unsupported)"));
                continue;
            }
            if !ws.subs.contains_key(zones.owner(&rel)) {
                continue; // not our zone: untouched, not an error
            }
            if ft.is_dir() {
                self.add_dir(&rel, zones, ws, rep, seen, todo)?;
            } else {
                validate_path(&rel)?;
                todo.push(rel.clone());
                seen.insert(rel);
                rep.staged += 1;
            }
        }
        Ok(())
    }

    /// Stat, read, hash and store each file (in parallel when there are many), then record
    /// them in the index. Blob writes only touch `store`, which is safe to share across threads.
    fn stage_files(&self, files: &[String], ix: &mut Index) -> Result<()> {
        let one = |rel: &String| -> Result<IndexEntry> {
            let abs = self.worktree.join(rel);
            let m = io(&abs, fs::metadata(&abs))?; // stat BEFORE read: later edits change mtime/size
            let data = io(&abs, fs::read(&abs))?;
            let id = self.store.put_blob(&data)?;
            Ok(IndexEntry {
                id,
                mode: mode_of(&m),
                size: m.len(),
                mtime_ns: mtime_ns(&m),
            })
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        let mut entries: Vec<Option<Result<IndexEntry>>> = (0..files.len()).map(|_| None).collect();
        if threads <= 1 || files.len() < 64 {
            for (i, f) in files.iter().enumerate() {
                entries[i] = Some(one(f));
            }
        } else {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let parts: Vec<Vec<(usize, Result<IndexEntry>)>> = std::thread::scope(|sc| {
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
        for (f, e) in files.iter().zip(entries) {
            ix.insert(f, e.expect("every file processed")?)?;
        }
        Ok(())
    }

    // ---------- status ----------

    fn wt_state(&self, path: &str, e: &IndexEntry, written_ns: u64) -> Result<Wt> {
        let abs = self.worktree.join(path);
        let m = match fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(x) if x.kind() == std::io::ErrorKind::NotFound => return Ok(Wt::Deleted),
            Err(source) => return Err(Error::Io { path: abs, source }),
        };
        if !m.is_file() {
            return Ok(Wt::Modified);
        }
        let mt = mtime_ns(&m);
        if m.len() == e.size && mt == e.mtime_ns && mode_of(&m) == e.mode && mt.saturating_add(RACY_NS) <= written_ns {
            return Ok(Wt::Clean);
        }
        let data = io(&abs, fs::read(&abs))?;
        Ok(if vcs_objects::blob_id(&data) == e.id && mode_of(&m) == e.mode {
            Wt::Clean
        } else {
            Wt::Modified
        })
    }

    pub fn status(&self) -> Result<Status> {
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let refs = self.refs()?;
        let ix = self.index()?;
        let mut st = Status::default();
        for (zone, branch) in &ws.subs {
            let mine: BTreeMap<&String, &IndexEntry> = ix.entries.iter().filter(|(p, _)| zones.owner(p) == zone).collect();
            // Subtree-skipping diff: hash the tree the index would commit and compare it with
            // HEAD's tree, reading only the HEAD trees on paths that actually differ.
            let head_root = match self.current_head(zone, &ws, &refs) {
                Some(h) => Some(self.commit_obj(&h)?.root),
                None => None,
            };
            let entries = mine.iter().map(|(p, e)| (zones.relative(zone, p), e.mode, e.id));
            let staged: Vec<Change> = treeops::diff_index(&self.store, head_root, entries, zones.path_of(zone).unwrap_or(""))?
                .into_iter()
                .map(|(path, kind)| Change { path, kind })
                .collect();
            let mut unstaged = Vec::new();
            for (p, e) in &mine {
                match self.wt_state(p, e, ix.written_ns)? {
                    Wt::Clean => {}
                    Wt::Modified => unstaged.push(Change {
                        path: (*p).clone(),
                        kind: ChangeKind::Modified,
                    }),
                    Wt::Deleted => unstaged.push(Change {
                        path: (*p).clone(),
                        kind: ChangeKind::Deleted,
                    }),
                }
            }
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
        let budget = AtomicUsize::new(std::thread::available_parallelism().map_or(1, |n| n.get()).min(8) - 1);
        let w = self.walk_untracked("", &zones, &ws, &ix, &budget)?;
        st.untracked = w.untracked;
        st.unsupported = w.unsupported;
        st.merging = self.merge_state()?;
        Ok(st)
    }

    /// Directory scan for untracked and unsupported entries. The scan is syscall-bound (one
    /// `read_dir` per directory), so sibling subdirectories are walked on extra threads while
    /// the shared `budget` lasts; results are assembled in directory order, so the output does
    /// not depend on scheduling.
    fn walk_untracked(&self, dir: &str, zones: &Zones, ws: &Workspace, ix: &Index, budget: &AtomicUsize) -> Result<WalkOut> {
        enum Item {
            Unsupported(String),
            File(String),
            Dir(String, bool),
        }
        let abs = if dir.is_empty() {
            self.worktree.clone()
        } else {
            self.worktree.join(dir)
        };
        let mut ents: Vec<_> = io(&abs, fs::read_dir(&abs))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| Error::Io { path: abs.clone(), source })?;
        ents.sort_by_key(|e| e.file_name());
        let mut items = Vec::new();
        for e in ents {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case(META_DIR) || validate_component(&name).is_err() {
                continue;
            }
            let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            let ft = io(&abs, e.file_type())?;
            if ft.is_symlink() || !(ft.is_dir() || ft.is_file()) {
                items.push(Item::Unsupported(rel));
                continue;
            }
            if !ws.subs.contains_key(zones.owner(&rel)) {
                continue;
            }
            if ft.is_dir() {
                let tracked_below = ix.has_under(&rel);
                items.push(Item::Dir(rel, tracked_below));
            } else if !ix.entries.contains_key(&rel) {
                items.push(Item::File(rel));
            }
        }
        let subdirs: Vec<&String> = items
            .iter()
            .filter_map(|i| if let Item::Dir(r, _) = i { Some(r) } else { None })
            .collect();
        let mut subs: Vec<Option<Result<WalkOut>>> = (0..subdirs.len()).map(|_| None).collect();
        std::thread::scope(|sc| {
            let mut handles = Vec::new();
            for (i, rel) in subdirs.iter().enumerate() {
                let take = budget
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |b| b.checked_sub(1))
                    .is_ok();
                if take {
                    handles.push((
                        i,
                        sc.spawn(move || {
                            let r = self.walk_untracked(rel, zones, ws, ix, budget);
                            budget.fetch_add(1, Ordering::AcqRel);
                            r
                        }),
                    ));
                } else {
                    subs[i] = Some(self.walk_untracked(rel, zones, ws, ix, budget));
                }
            }
            for (i, h) in handles {
                subs[i] = Some(h.join().expect("walk worker panicked"));
            }
        });
        let mut out = WalkOut::default();
        let mut subs = subs.into_iter();
        for it in items {
            match it {
                Item::Unsupported(r) => out.unsupported.push(r),
                Item::File(r) => out.untracked.push(r),
                Item::Dir(rel, tracked_below) => {
                    let mut sub = subs.next().flatten().expect("one result per subdir")?;
                    out.unsupported.append(&mut sub.unsupported);
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
                let o = old.map(|i| self.store.get_blob(&i)).transpose()?;
                let n = new.map(|i| self.store.get_blob(&i)).transpose()?;
                out.push_str(&diff::unified(p, o.as_deref(), n.as_deref()));
            }
        } else {
            for (p, e) in &ix.entries {
                let abs = self.worktree.join(p);
                let cur = match fs::symlink_metadata(&abs) {
                    Ok(m) if m.is_file() => Some(io(&abs, fs::read(&abs))?),
                    Ok(_) => continue,
                    Err(x) if x.kind() == std::io::ErrorKind::NotFound => None,
                    Err(source) => return Err(Error::Io { path: abs, source }),
                };
                let old = self.store.get_blob(&e.id)?;
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
        let zones = self.zones()?;
        let ws = self.workspace()?;
        let mut refs = self.refs()?;
        let ix = self.index()?;
        let op_author = author.clone();
        let mut updates = Vec::new();
        let mut commits = Vec::new();
        for (zone, branch) in &ws.subs {
            let parent = refs.head(zone, branch);
            let parent_root = match parent {
                Some(p) => Some(self.commit_obj(&p)?.root),
                None => None,
            };
            let entries: Vec<(&str, EntryMode, ObjectId)> = ix
                .entries
                .iter()
                .filter(|(p, _)| zones.owner(p) == zone)
                .map(|(p, e)| (zones.relative(zone, p), e.mode, e.id))
                .collect();
            if parent_root.is_none() && entries.is_empty() {
                continue;
            }
            let root = treeops::build(&self.store, entries)?;
            let second = merging
                .as_ref()
                .filter(|m| m.zone == *zone && Some(m.ours) == parent)
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
        write_atomic(&self.vcs.join("refs"), &refs.encode())?; // linearization point
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
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
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
        self.adopt_tracking(&mut refs, zone, branch)?;
        let Some(target) = refs.head(zone, branch) else {
            return msg(format!("no such branch {zone}/{branch}"));
        };
        let st = self.status()?;
        self.require_clean(zone, &st)?;
        let old = self.head_flat(zone, &zones, &ws, &refs)?;
        let new = treeops::flatten(&self.store, Some(self.commit_obj(&target)?.root), zones.path_of(zone).unwrap())?;
        let mut ix = self.index()?;
        self.transition(zone, &zones, &old, &new, &mut ix)?;
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
        write_atomic(&self.vcs.join("zones"), &zones.encode())?;
        ws.subs.insert(name.into(), DEFAULT_BRANCH.into());
        self.save_ws(&ws)?;
        self.record_op(&format!("create zone {name} at {path}"), None)?;
        Ok(())
    }

    /// Subscribe to an existing zone and materialize it (sparse checkout by zone).
    pub fn zone_add(&self, zone: &str, branch: Option<&str>) -> Result<()> {
        let zone = self.resolve_zone(zone)?;
        let zone = zone.as_str();
        let _l = self.lock()?;
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
        self.save_index(&mut ix)?;
        self.save_ws(&ws)?;
        self.record_op(&format!("zone remove {zone}"), None)?;
        Ok(())
    }

    /// Move the working tree + index of one zone from `old` to `new` (flat, full paths).
    /// Preflight validates everything before any file is touched.
    pub(crate) fn transition(&self, zone: &str, zones: &Zones, old: &Flat, new: &Flat, ix: &mut Index) -> Result<()> {
        let changed: Vec<&String> = new.iter().filter(|(p, v)| old.get(*p) != Some(v)).map(|(p, _)| p).collect();
        for p in &changed {
            validate_path(p)?;
            if zones.owner(p) != zone {
                return msg(format!(
                    "branch content for zone {zone:?} contains {p:?}, which belongs to zone {:?}",
                    zones.owner(p)
                ));
            }
            let abs = self.worktree.join(p);
            if !old.contains_key(*p) && fs::symlink_metadata(&abs).is_ok() {
                return msg(format!("untracked path {p:?} would be overwritten; move or remove it first"));
            }
            // no symlinked / file ancestors
            let mut acc = self.worktree.clone();
            let comps: Vec<&str> = p.split('/').collect();
            for c in &comps[..comps.len() - 1] {
                acc.push(c);
                match fs::symlink_metadata(&acc) {
                    Ok(m) if m.file_type().is_symlink() => return msg(format!("refusing to write through symlink at {}", acc.display())),
                    Ok(m) if m.is_file() => {
                        let rel = acc.strip_prefix(&self.worktree).unwrap().to_string_lossy().replace('\\', "/");
                        if !old.contains_key(&rel) || new.contains_key(&rel) {
                            return msg(format!("cannot create {p:?}: {rel:?} is a file"));
                        }
                    }
                    _ => {}
                }
            }
        }
        // deletions
        let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for p in old.keys().filter(|p| !new.contains_key(*p)) {
            let abs = self.worktree.join(p);
            match fs::symlink_metadata(&abs) {
                Ok(m) if m.is_file() => io(&abs, fs::remove_file(&abs))?,
                Ok(_) => return msg(format!("expected file at {p:?}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(Error::Io { path: abs, source }),
            }
            ix.entries.remove(p);
            if let Some(par) = abs.parent() {
                dirs.insert(par.to_owned());
            }
        }
        // writes
        for p in changed {
            let (mode, id) = new[p];
            let data = self.store.get_blob(&id)?;
            let abs = self.worktree.join(p);
            let parent = abs.parent().unwrap();
            io(parent, fs::create_dir_all(parent))?;
            let tmp = parent.join(format!(".vcs-tmp-{}", std::process::id()));
            io(&tmp, fs::write(&tmp, &data))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let bits = if mode == EntryMode::Executable { 0o755 } else { 0o644 };
                io(&tmp, fs::set_permissions(&tmp, fs::Permissions::from_mode(bits)))?;
            }
            io(&abs, fs::rename(&tmp, &abs))?;
            let m = io(&abs, fs::metadata(&abs))?;
            ix.entries.remove(p);
            ix.insert(
                p,
                IndexEntry {
                    id,
                    mode,
                    size: m.len(),
                    mtime_ns: mtime_ns(&m),
                },
            )?;
        }
        // prune now-empty directories (never the worktree root)
        for mut d in dirs {
            while d != self.worktree && d.starts_with(&self.worktree) && fs::remove_dir(&d).is_ok() {
                if !d.pop() {
                    break;
                }
            }
        }
        Ok(())
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
        write_atomic(&self.vcs.join("refs"), &refs.encode())?;
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
        for id in &ids {
            rep.objects += 1;
            match self.store.get(id) {
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
        // The index must never name a blob that is not in the store.
        for (p, e) in &self.index()?.entries {
            missing(self, &e.id, format!("index {p}: missing blob {}", e.id), &mut rep.problems)?;
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
        rep.problems.extend(self.store.verify_packs());
        Ok(rep)
    }

    /// Move all loose objects into a new pack. Returns how many were packed.
    pub fn repack(&self) -> Result<usize> {
        Ok(self.store.repack()?)
    }
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
