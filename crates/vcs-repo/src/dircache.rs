//! Directory listing cache for `status` (an "untracked cache" in the style of other VCSs).
//!
//! `status` must `read_dir` every directory to find untracked and unsupported entries; on a deep
//! tree that is one `open`+`getdirentries`+`close` per directory. This cache remembers each
//! directory's listing together with a stamp of the directory itself (inode, mtime, ctime). When
//! the stamp still matches, the listing is reused and `read_dir` is skipped. Tracked files are
//! always re-stat'ed by `status`, so this cache can only ever affect which *names* exist.
//!
//! Safety rules ("fall back to the full walk on any doubt"):
//! - A stamp is trusted only if it differs from the time the listing was read by more than the
//!   racy window (default 2 s), so a change in the same timestamp tick can never be missed.
//! - ctime is part of the stamp. Unlike mtime it cannot be set from user space, so resetting a
//!   directory's mtime after adding or removing an entry (`touch -r`) still invalidates it.
//! - The directory is stamped before and after it is read; if the two differ, it is not cached.
//! - Any unreadable, corrupt or unsupported-platform cache is treated as empty.
//! - Only directories visited by the current walk are kept, so stale paths do not accumulate.
//!
//! The file lives at `.vcs/dircache`, is written without fsync (losing it only costs a rescan)
//! and is replaced atomically, so concurrent `status` runs each see a self-consistent file.

use crate::meta::{frame, unframe};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use vcs_objects::codec::{Reader, Writer};

pub(crate) const FILE: &str = "dircache";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub ino: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
}

/// Kind of a raw directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir = 0,
    File = 1,
    /// Symlink or special file.
    Other = 2,
}

#[derive(Clone, Debug)]
pub(crate) struct Listing {
    pub stamp: Stamp,
    /// Sorted by name; names already filtered of `.vcs` and invalid components.
    pub entries: Vec<(String, Kind)>,
}

#[derive(Default)]
pub(crate) struct DirCache {
    pub dirs: HashMap<String, Listing>,
}

/// Stamp of a directory, or `None` when the platform cannot provide a trustworthy one.
#[cfg(unix)]
pub(crate) fn stamp_of(m: &fs::Metadata) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    if !m.is_dir() {
        return None;
    }
    let ns = |s: i64, n: i64| (s.max(0) as u64).saturating_mul(1_000_000_000).saturating_add(n.max(0) as u64);
    Some(Stamp {
        ino: m.ino(),
        mtime_ns: ns(m.mtime(), m.mtime_nsec()),
        ctime_ns: ns(m.ctime(), m.ctime_nsec()),
    })
}

#[cfg(not(unix))]
pub(crate) fn stamp_of(_m: &fs::Metadata) -> Option<Stamp> {
    None
}

impl Stamp {
    /// Old enough relative to `scan_start_ns` that a later change must produce a different stamp.
    pub fn settled(&self, scan_start_ns: u64, racy_ns: u64) -> bool {
        self.mtime_ns.max(self.ctime_ns).saturating_add(racy_ns) <= scan_start_ns
    }
}

impl DirCache {
    pub fn load(vcs: &Path) -> DirCache {
        fs::read(vcs.join(FILE)).ok().and_then(|b| Self::decode(&b)).unwrap_or_default()
    }

    /// Best-effort atomic replace; errors are ignored (the cache is only an optimisation).
    pub fn store(&self, vcs: &Path) {
        let path = vcs.join(FILE);
        let tmp = vcs.join(format!("{FILE}.tmp{}", std::process::id()));
        if fs::write(&tmp, self.encode()).is_ok() && fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut keys: Vec<&String> = self.dirs.keys().collect();
        keys.sort();
        let mut w = Writer::new();
        w.varint(keys.len() as u64);
        for k in keys {
            let l = &self.dirs[k];
            w.str(k).varint(l.stamp.ino).varint(l.stamp.mtime_ns).varint(l.stamp.ctime_ns);
            w.varint(l.entries.len() as u64);
            for (n, kind) in &l.entries {
                w.str(n).u8(*kind as u8);
            }
        }
        frame(b"AGDC", w.finish())
    }

    fn decode(bytes: &[u8]) -> Option<DirCache> {
        let body = unframe(b"AGDC", bytes, "dircache").ok()?;
        let mut r = Reader::new(body);
        let n = r.count(5).ok()?;
        let mut dirs = HashMap::with_capacity(n);
        for _ in 0..n {
            let k = r.str(4096).ok()?;
            let stamp = Stamp {
                ino: r.varint().ok()?,
                mtime_ns: r.varint().ok()?,
                ctime_ns: r.varint().ok()?,
            };
            let m = r.count(2).ok()?;
            let mut entries = Vec::with_capacity(m);
            for _ in 0..m {
                let name = r.str(4096).ok()?;
                let kind = match r.u8().ok()? {
                    0 => Kind::Dir,
                    1 => Kind::File,
                    2 => Kind::Other,
                    _ => return None,
                };
                entries.push((name, kind));
            }
            dirs.insert(k, Listing { stamp, entries });
        }
        r.finish().ok()?;
        Some(DirCache { dirs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_garbage() {
        let mut c = DirCache::default();
        c.dirs.insert(
            "a/b".into(),
            Listing {
                stamp: Stamp {
                    ino: 7,
                    mtime_ns: 8,
                    ctime_ns: 9,
                },
                entries: vec![("x".into(), Kind::File), ("y".into(), Kind::Dir)],
            },
        );
        let d = DirCache::decode(&c.encode()).unwrap();
        assert_eq!(d.dirs["a/b"].stamp, c.dirs["a/b"].stamp);
        assert_eq!(d.dirs["a/b"].entries, c.dirs["a/b"].entries);
        assert!(DirCache::decode(b"junk").is_none());
        let mut b = c.encode();
        b.truncate(b.len() - 1);
        assert!(DirCache::decode(&b).is_none());
    }
    #[test]
    fn settled_requires_margin() {
        let s = Stamp {
            ino: 1,
            mtime_ns: 100,
            ctime_ns: 150,
        };
        assert!(!s.settled(200, 100));
        assert!(s.settled(250, 100));
    }
}
