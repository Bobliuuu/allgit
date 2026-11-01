//! Small versioned, framed state files: zones, refs, workspace.
//! All are written atomically (temp file + fsync + rename).

use crate::error::{io, msg, Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{paths_overlap, validate_branch_name, validate_path, validate_zone_name};
use vcs_objects::{ObjectId, RefUpdate};

pub const ROOT_ZONE: &str = "root";
pub const DEFAULT_BRANCH: &str = "main";
const STATE_VERSION: u8 = 1;

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_inner(path, bytes, true)
}

/// Like [`write_atomic`] for pure caches (never state): crash injection does not count these
/// writes, because a failed cache write is ignored by design.
pub(crate) fn write_cache_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_inner(path, bytes, false)
}

fn write_atomic_inner(path: &Path, bytes: &[u8], inject: bool) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if inject && crate::failpoint::tick() {
        // Simulated crash mid-write: a torn temp file is left behind, the target is untouched.
        let _ = fs::write(&tmp, &bytes[..bytes.len() / 2]);
        return Err(Error::Io {
            path: path.to_owned(),
            source: std::io::Error::other("injected crash"),
        });
    }
    {
        let mut f = io(&tmp, fs::File::create(&tmp))?;
        io(&tmp, f.write_all(bytes))?;
        io(&tmp, f.sync_all())?;
    }
    io(path, fs::rename(&tmp, path))
}

pub(crate) fn frame(magic: &[u8; 4], body: Vec<u8>) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 5);
    v.extend_from_slice(magic);
    v.push(STATE_VERSION);
    v.extend_from_slice(&body);
    v
}

pub(crate) fn unframe<'a>(magic: &[u8; 4], bytes: &'a [u8], what: &str) -> Result<&'a [u8]> {
    if bytes.len() < 5 || &bytes[..4] != magic {
        return Err(Error::Corrupt(format!("{what}: bad magic")));
    }
    if bytes[4] != STATE_VERSION {
        return Err(Error::Corrupt(format!("{what}: unsupported version {}", bytes[4])));
    }
    Ok(&bytes[5..])
}

fn corrupt(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("{what}: {e}"))
}

/// Zone definitions. Invariant (enforced): non-root zone paths are pairwise non-overlapping.
/// The implicit `root` zone (path "") owns every path not owned by another zone.
#[derive(Clone, Debug)]
pub struct Zones {
    /// Zone id -> path. The id is the name embedded in commits; it never changes.
    defs: BTreeMap<String, String>,
    /// Zone id -> display name, only for renamed zones (absent: the id is the name).
    labels: BTreeMap<String, String>,
}

impl Default for Zones {
    fn default() -> Self {
        Self::new()
    }
}

impl Zones {
    pub fn new() -> Self {
        let mut defs = BTreeMap::new();
        defs.insert(ROOT_ZONE.to_string(), String::new());
        Self {
            defs,
            labels: BTreeMap::new(),
        }
    }
    /// Stable zone ids (the names commits, refs, snapshots and the remote manifest use).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
    }
    /// Display name of zone `id` (the id itself unless the zone was renamed).
    pub fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.labels.get(id).map(String::as_str).unwrap_or(id)
    }
    /// `(id, display name)` of every renamed zone.
    pub fn labels(&self) -> impl Iterator<Item = (&str, &str)> {
        self.labels.iter().map(|(i, l)| (i.as_str(), l.as_str()))
    }
    /// Map a user-supplied name (current display name, or the original id) to the zone id.
    pub fn resolve(&self, name: &str) -> Option<String> {
        self.labels
            .iter()
            .find(|(_, l)| l.as_str() == name)
            .map(|(i, _)| i.clone())
            .or_else(|| self.defs.contains_key(name).then(|| name.to_owned()))
    }
    fn name_taken(&self, name: &str) -> bool {
        self.defs.contains_key(name) || self.labels.values().any(|l| l == name)
    }
    /// Set the display name of zone `id` (no checks; `""`-free). Used when learning labels from a remote.
    pub fn set_label(&mut self, id: &str, label: &str) -> Result<()> {
        validate_zone_name(label)?;
        if !self.defs.contains_key(id) || id == ROOT_ZONE {
            return msg(format!("no such zone {id:?}"));
        }
        if label == id {
            self.labels.remove(id);
        } else if self.labels.get(id).map(String::as_str) != Some(label) {
            if self.name_taken(label) {
                return msg(format!("zone name {label:?} is already in use"));
            }
            self.labels.insert(id.to_owned(), label.to_owned());
        }
        Ok(())
    }
    pub fn path_of(&self, zone: &str) -> Option<&str> {
        self.defs.get(zone).map(String::as_str)
    }
    pub fn exists(&self, zone: &str) -> bool {
        self.defs.contains_key(zone)
    }
    pub fn owner(&self, path: &str) -> &str {
        for (n, p) in &self.defs {
            if n != ROOT_ZONE && (path == p || (path.starts_with(p.as_str()) && path.as_bytes()[p.len()] == b'/')) {
                return n;
            }
        }
        ROOT_ZONE
    }
    /// Zone-relative form of a repo path owned by `zone`.
    pub fn relative<'a>(&self, zone: &str, path: &'a str) -> &'a str {
        let p = &self.defs[zone];
        if p.is_empty() {
            path
        } else {
            &path[p.len() + 1..]
        }
    }
    pub fn full_path(&self, zone: &str, rel: &str) -> String {
        let p = &self.defs[zone];
        if p.is_empty() {
            rel.to_owned()
        } else {
            format!("{p}/{rel}")
        }
    }
    pub fn create(&mut self, name: &str, path: &str) -> Result<()> {
        validate_zone_name(name)?;
        validate_path(path)?;
        if self.name_taken(name) {
            return msg(format!("zone {name:?} already exists"));
        }
        for (n, p) in &self.defs {
            if n != ROOT_ZONE && paths_overlap(p, path) {
                return msg(format!("path {path:?} overlaps zone {n:?} (path {p:?}); zones may not overlap"));
            }
        }
        self.defs.insert(name.to_owned(), path.to_owned());
        Ok(())
    }
    /// Give zone `old` (display name or id) the display name `new`; path and id are unchanged,
    /// so commits, refs and snapshots stay valid. Returns the zone id. Root cannot be renamed.
    pub fn rename(&mut self, old: &str, new: &str) -> Result<String> {
        validate_zone_name(new)?;
        let Some(id) = self.resolve(old) else {
            return msg(format!("no such zone {old:?}"));
        };
        if id == ROOT_ZONE || new == ROOT_ZONE {
            return msg("the root zone cannot be renamed");
        }
        if self.label(&id) == new || (new != id && self.name_taken(new)) {
            return msg(format!("zone {new:?} already exists"));
        }
        self.set_label(&id, new)?;
        Ok(id)
    }
    /// Drop a zone definition; its paths fall back to the root zone. Root cannot be removed.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if name == ROOT_ZONE {
            return msg("the root zone cannot be deleted");
        }
        let Some(id) = self.resolve(name) else {
            return msg(format!("no such zone {name:?}"));
        };
        self.defs.remove(&id);
        self.labels.remove(&id);
        Ok(())
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.defs.len() as u64);
        for (n, p) in &self.defs {
            w.str(n).str(p);
        }
        if self.labels.is_empty() {
            return frame(b"AGAR", w.finish());
        }
        w.varint(self.labels.len() as u64);
        for (i, l) in &self.labels {
            w.str(i).str(l);
        }
        frame(b"AGA2", w.finish())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let with_labels = bytes.starts_with(b"AGA2");
        let body = unframe(if with_labels { b"AGA2" } else { b"AGAR" }, bytes, "zones")?;
        let mut r = Reader::new(body);
        let mut a = Zones {
            defs: BTreeMap::new(),
            labels: BTreeMap::new(),
        };
        let n = r.count(2).map_err(|e| corrupt("zones", e))?;
        for _ in 0..n {
            let name = r.str(64).map_err(|e| corrupt("zones", e))?;
            let path = r.str(4096).map_err(|e| corrupt("zones", e))?;
            validate_zone_name(&name)?;
            if name != ROOT_ZONE {
                validate_path(&path)?;
                for (n2, p2) in &a.defs {
                    if n2 != ROOT_ZONE && paths_overlap(p2, &path) {
                        return Err(Error::Corrupt(format!("zones {name:?} and {n2:?} overlap")));
                    }
                }
            }
            a.defs.insert(name, path);
        }
        if with_labels {
            for _ in 0..r.count(2).map_err(|e| corrupt("zones", e))? {
                let id = r.str(64).map_err(|e| corrupt("zones", e))?;
                let label = r.str(64).map_err(|e| corrupt("zones", e))?;
                if id == ROOT_ZONE || !a.defs.contains_key(&id) || label == id {
                    return Err(Error::Corrupt(format!("zones: bad label for {id:?}")));
                }
                if a.name_taken(&label) {
                    return Err(Error::Corrupt(format!("zones: duplicate name {label:?}")));
                }
                validate_zone_name(&label)?;
                a.labels.insert(id, label);
            }
        }
        r.finish().map_err(|e| corrupt("zones", e))?;
        if a.defs.get(ROOT_ZONE).map(|p| !p.is_empty()) != Some(false) {
            return Err(Error::Corrupt("zones: missing/invalid root zone".into()));
        }
        Ok(a)
    }
}

/// Branch heads and named snapshots. One file, replaced atomically: a multi-zone
/// transaction is one `apply` + one rename, hence atomic for local readers.
#[derive(Clone, Debug, Default)]
pub struct Refs {
    pub branches: BTreeMap<(String, String), ObjectId>,
    pub snapshots: BTreeMap<String, ObjectId>,
    /// (zone, tag name) -> a commit (lightweight tag) or an annotated tag object.
    pub tags: BTreeMap<(String, String), ObjectId>,
}

impl Refs {
    pub fn head(&self, zone: &str, branch: &str) -> Option<ObjectId> {
        self.branches.get(&(zone.to_owned(), branch.to_owned())).copied()
    }
    pub fn branches_of(&self, zone: &str) -> Vec<(String, ObjectId)> {
        self.branches
            .iter()
            .filter(|((a, _), _)| a == zone)
            .map(|((_, b), id)| (b.clone(), *id))
            .collect()
    }
    /// Compare-and-set a batch of updates, all-or-nothing.
    pub fn apply(&mut self, updates: &[RefUpdate]) -> Result<()> {
        for u in updates {
            let cur = self.head(&u.zone, &u.branch);
            if cur != u.old {
                return msg(format!(
                    "ref {}/{} moved: expected {:?}, found {:?}",
                    u.zone,
                    u.branch,
                    u.old.map(|i| i.short()),
                    cur.map(|i| i.short())
                ));
            }
        }
        for u in updates {
            self.branches.insert((u.zone.clone(), u.branch.clone()), u.new);
        }
        Ok(())
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.branches.len() as u64);
        for ((a, b), id) in &self.branches {
            w.str(a).str(b).id(id);
        }
        w.varint(self.snapshots.len() as u64);
        for (n, id) in &self.snapshots {
            w.str(n).id(id);
        }
        // Appended only when non-empty so repositories without tags keep their original bytes.
        if !self.tags.is_empty() {
            w.varint(self.tags.len() as u64);
            for ((a, n), id) in &self.tags {
                w.str(a).str(n).id(id);
            }
        }
        frame(b"AGRF", w.finish())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(b"AGRF", bytes, "refs")?;
        let mut r = Reader::new(body);
        let mut refs = Refs::default();
        let e = |x| corrupt("refs", x);
        for _ in 0..r.count(34).map_err(e)? {
            let a = r.str(64).map_err(e)?;
            let b = r.str(200).map_err(e)?;
            validate_zone_name(&a)?;
            validate_branch_name(&b)?;
            refs.branches.insert((a, b), r.id().map_err(e)?);
        }
        for _ in 0..r.count(33).map_err(e)? {
            let n = r.str(200).map_err(e)?;
            validate_branch_name(&n)?;
            refs.snapshots.insert(n, r.id().map_err(e)?);
        }
        if r.remaining() > 0 {
            let n = r.count(34).map_err(e)?;
            if n == 0 {
                return Err(corrupt("refs", "empty tag list"));
            }
            for _ in 0..n {
                let a = r.str(64).map_err(e)?;
                let t = r.str(200).map_err(e)?;
                validate_zone_name(&a)?;
                validate_branch_name(&t)?;
                refs.tags.insert((a, t), r.id().map_err(e)?);
            }
        }
        r.finish().map_err(e)?;
        Ok(refs)
    }
}

/// Which zones this workspace materializes, and which branch of each is checked out.
///
/// `sparse` lists, per subscribed zone, the repo-relative path prefixes (directories or files) the
/// workspace materializes. A zone without an entry is fully materialized. Paths of a sparse zone
/// outside its set stay tracked in the index (so commits carry them over unchanged) but are not
/// written to disk and are never reported as deleted.
///
/// On disk: `AGWS` v1 is the legacy form (subscriptions only; still read, and still written when
/// no zone is sparse so older binaries keep working); `AGW2` v1 appends the sparse sets.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub subs: BTreeMap<String, String>,
    pub sparse: BTreeMap<String, BTreeSet<String>>,
}

const WS2_MAGIC: &[u8; 4] = b"AGW2";

impl Workspace {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.subs.len() as u64);
        for (a, b) in &self.subs {
            w.str(a).str(b);
        }
        if self.sparse.is_empty() {
            return frame(b"AGWS", w.finish());
        }
        w.varint(self.sparse.len() as u64);
        for (z, set) in &self.sparse {
            w.str(z);
            w.varint(set.len() as u64);
            for p in set {
                w.str(p);
            }
        }
        frame(WS2_MAGIC, w.finish())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let v2 = bytes.len() >= 4 && &bytes[..4] == WS2_MAGIC;
        let body = unframe(if v2 { WS2_MAGIC } else { b"AGWS" }, bytes, "workspace")?;
        let mut r = Reader::new(body);
        let e = |x| corrupt("workspace", x);
        let mut ws = Workspace::default();
        for _ in 0..r.count(2).map_err(e)? {
            let a = r.str(64).map_err(e)?;
            let b = r.str(200).map_err(e)?;
            validate_zone_name(&a)?;
            validate_branch_name(&b)?;
            ws.subs.insert(a, b);
        }
        if v2 {
            for _ in 0..r.count(2).map_err(e)? {
                let z = r.str(64).map_err(e)?;
                validate_zone_name(&z)?;
                let mut set = BTreeSet::new();
                for _ in 0..r.count(1).map_err(e)? {
                    let p = r.str(4096).map_err(e)?;
                    validate_path(&p)?;
                    set.insert(p);
                }
                if !set.is_empty() {
                    ws.sparse.insert(z, set);
                }
            }
        }
        r.finish().map_err(e)?;
        Ok(ws)
    }

    /// Is `path` (repo-relative, owned by `zone`) outside the zone's sparse set?
    pub fn skips(&self, zone: &str, path: &str) -> bool {
        match self.sparse.get(zone) {
            None => false,
            Some(set) => !in_sparse_set(set, path),
        }
    }
}

/// Could any path beneath directory `dir` be in `set`? True when `dir` is a prefix of, equal to,
/// or inside one of the set's prefixes.
pub fn sparse_may_contain(set: &BTreeSet<String>, dir: &str) -> bool {
    if in_sparse_set(set, dir) {
        return true;
    }
    let lo = format!("{dir}/");
    set.range(lo.clone()..).next().is_some_and(|p| p.starts_with(&lo))
}

/// Is `path` equal to, or beneath, one of the prefixes in `set`?
pub fn in_sparse_set(set: &BTreeSet<String>, path: &str) -> bool {
    // A prefix of `path` ends at a '/' boundary (or is the whole path): probe each.
    if set.contains(path) {
        return true;
    }
    let mut end = path.len();
    while let Some(i) = path[..end].rfind('/') {
        end = i;
        if set.contains(&path[..end]) {
            return true;
        }
    }
    false
}

/// Exclusive repository lock (create_new). Released on drop.
pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let _ = writeln!(f, "pid {}", std::process::id());
                Ok(Self { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::Locked(path)),
            Err(source) => Err(Error::Io { path, source }),
        }
    }
}
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_ownership_and_overlap() {
        let mut a = Zones::new();
        a.create("engine", "src/engine").unwrap();
        a.create("assets", "assets").unwrap();
        assert!(a.create("x", "src/engine/sub").is_err(), "nested");
        assert!(a.create("y", "src").is_err(), "parent");
        assert!(a.create("engine", "other").is_err(), "dup name");
        a.create("srcother", "src/engine2").unwrap(); // sibling with common string prefix is fine
        assert_eq!(a.owner("src/engine/a.rs"), "engine");
        assert_eq!(a.owner("src/engine2/a.rs"), "srcother");
        assert_eq!(a.owner("src/engineX"), "root");
        assert_eq!(a.owner("README"), "root");
        assert_eq!(a.relative("engine", "src/engine/a/b.rs"), "a/b.rs");
        let b = Zones::decode(&a.encode()).unwrap();
        assert_eq!(b.owner("assets/x"), "assets");
    }

    #[test]
    fn zone_rename_and_remove() {
        let mut a = Zones::new();
        a.create("engine", "src/engine").unwrap();
        a.create("assets", "assets").unwrap();
        assert!(a.rename("engine", "assets").is_err(), "target exists");
        assert!(a.rename("root", "x").is_err());
        assert!(a.rename("engine", "root").is_err());
        assert!(a.rename("nope", "x").is_err());
        assert!(a.rename("engine", "bad name").is_err());
        assert_eq!(a.rename("engine", "core").unwrap(), "engine");
        // the id stays the storage key; the new name and the old one both resolve to it
        assert_eq!(a.path_of("engine"), Some("src/engine"));
        assert_eq!(a.label("engine"), "core");
        assert_eq!(a.resolve("core").as_deref(), Some("engine"));
        assert_eq!(a.resolve("engine").as_deref(), Some("engine"));
        assert_eq!(a.owner("src/engine/x"), "engine");
        assert!(a.create("core", "other").is_err(), "label taken");
        assert!(a.create("engine", "other").is_err(), "id taken");
        assert!(a.rename("assets", "engine").is_err(), "another zone's id");
        let b = Zones::decode(&a.encode()).unwrap();
        assert_eq!(b.label("engine"), "core");
        // renaming back to the id clears the label (and keeps the legacy encoding)
        a.rename("core", "engine").unwrap();
        assert_eq!(a.label("engine"), "engine");
        assert!(a.encode().starts_with(b"AGAR"));
        a.rename("engine", "core").unwrap();
        assert!(a.remove("root").is_err());
        assert!(a.remove("nope").is_err());
        a.remove("core").unwrap();
        assert_eq!(a.owner("src/engine/x"), "root");
        assert_eq!(a.labels().count(), 0);
    }

    #[test]
    fn refs_cas_is_all_or_nothing() {
        let id = |n| ObjectId::from_bytes([n; 32]);
        let mut r = Refs::default();
        let u = |a: &str, old, new| RefUpdate {
            zone: a.into(),
            branch: "main".into(),
            old,
            new,
        };
        r.apply(&[u("a", None, id(1)), u("b", None, id(2))]).unwrap();
        // second update stale => nothing applied
        let err = r.apply(&[u("a", Some(id(1)), id(3)), u("b", None, id(4))]);
        assert!(err.is_err());
        assert_eq!(r.head("a", "main"), Some(id(1)));
        let r2 = Refs::decode(&r.encode()).unwrap();
        assert_eq!(r2.head("b", "main"), Some(id(2)));
    }

    #[test]
    fn corrupt_state_rejected() {
        assert!(Zones::decode(b"nope").is_err());
        assert!(Refs::decode(&frame(b"AGRF", vec![0xff])).is_err());
    }
}
