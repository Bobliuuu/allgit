//! Small versioned, framed state files: zones, refs, workspace.
//! All are written atomically (temp file + fsync + rename).

use crate::error::{io, msg, Error, Result};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_zone_name, validate_branch_name, validate_path};
use vcs_objects::{ObjectId, RefUpdate};

pub const ROOT_ZONE: &str = "root";
pub const DEFAULT_BRANCH: &str = "main";
const STATE_VERSION: u8 = 1;

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
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

/// True if `a` and `b` are equal or one is a component-wise prefix of the other.
fn paths_overlap(a: &str, b: &str) -> bool {
    let pre = |x: &str, y: &str| x == y || (y.starts_with(x) && y.as_bytes()[x.len()] == b'/');
    pre(a, b) || pre(b, a)
}

/// Zone definitions. Invariant (enforced): non-root zone paths are pairwise non-overlapping.
/// The implicit `root` zone (path "") owns every path not owned by another zone.
#[derive(Clone, Debug)]
pub struct Zones {
    defs: BTreeMap<String, String>,
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
        Self { defs }
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
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
        if self.defs.contains_key(name) {
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
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.defs.len() as u64);
        for (n, p) in &self.defs {
            w.str(n).str(p);
        }
        frame(b"AGAR", w.finish())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(b"AGAR", bytes, "zones")?;
        let mut r = Reader::new(body);
        let mut a = Zones { defs: BTreeMap::new() };
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
        r.finish().map_err(e)?;
        Ok(refs)
    }
}

/// Which zones this workspace materializes, and which branch of each is checked out.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub subs: BTreeMap<String, String>,
}

impl Workspace {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.varint(self.subs.len() as u64);
        for (a, b) in &self.subs {
            w.str(a).str(b);
        }
        frame(b"AGWS", w.finish())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let body = unframe(b"AGWS", bytes, "workspace")?;
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
        r.finish().map_err(e)?;
        Ok(ws)
    }
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
