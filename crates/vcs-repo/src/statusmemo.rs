//! A finished `status` answer, saved next to the monitor state (`.vcs/fsmon/state`).
//!
//! When the index file is the very same file (inode, size, mtime, ctime), the small state files
//! (zones, workspace, refs, config) have the same bytes, and the monitor reports no change since
//! the state's token, the answer is what was stored: `status` then decodes no index at all.
//! Anything else (or any doubt) takes the normal monitored path, which stays the reference.
//!
//! Racy rule (as for git's index): a memo is only stored when the index file's mtime is at least
//! the racy window older than the moment the memo is written, so a rewrite that leaves
//! inode/size/mtime/ctime unchanged cannot happen after the memo was taken.

use crate::repo::{Change, ChangeKind, ZoneStatus};
use std::path::Path;
use vcs_objects::codec::{Reader, Writer};

/// Identity of the index file as the kernel reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IdxStat {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: i128,
}

impl IdxStat {
    #[cfg(unix)]
    pub fn of(m: &std::fs::Metadata) -> Option<IdxStat> {
        use std::os::unix::fs::MetadataExt;
        Some(IdxStat {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
            ctime_ns: m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
        })
    }
    #[cfg(not(unix))]
    pub fn of(_m: &std::fs::Metadata) -> Option<IdxStat> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Memo {
    pub idx: IdxStat,
    /// Hash of the small state files the answer also depends on.
    pub small: [u8; 32],
    pub unsubscribed: Vec<String>,
    pub zones: Vec<MemoZone>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemoZone {
    pub zone: String,
    pub path: String,
    pub branch: String,
    pub head: Option<vcs_objects::ObjectId>,
    pub staged: Vec<Change>,
    pub unstaged: Vec<Change>,
}

impl MemoZone {
    pub fn of(z: &ZoneStatus) -> MemoZone {
        MemoZone {
            zone: z.zone.clone(),
            path: z.path.clone(),
            branch: z.branch.clone(),
            head: z.head,
            staged: z.staged.clone(),
            unstaged: z.unstaged.clone(),
        }
    }
    pub fn into_status(self) -> ZoneStatus {
        ZoneStatus {
            zone: self.zone,
            path: self.path,
            branch: self.branch,
            head: self.head,
            staged: self.staged,
            unstaged: self.unstaged,
        }
    }
}

/// Hash of the bytes of the files besides the index that `status` reads: zone layout, workspace,
/// refs and repository config (each length-prefixed; a missing file hashes as empty).
pub(crate) fn small_hash(repo: &crate::repo::Repo) -> Option<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    for n in ["zones", "workspace", "refs", "config"] {
        let b = match std::fs::read(repo.state_path(n)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && n == "config" => Vec::new(),
            Err(_) => return None,
        };
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(&b);
    }
    Some(*h.finalize().as_bytes())
}

pub(crate) fn stat_index(path: &Path) -> Option<IdxStat> {
    IdxStat::of(&std::fs::metadata(path).ok()?)
}

fn put_changes(w: &mut Writer, l: &[Change]) {
    w.varint(l.len() as u64);
    for c in l {
        w.str(&c.path).u8(match c.kind {
            ChangeKind::Added => 0,
            ChangeKind::Modified => 1,
            ChangeKind::Deleted => 2,
        });
    }
}

fn get_changes(r: &mut Reader) -> Option<Vec<Change>> {
    let n = r.count(2).ok()?;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let path = r.str(4096).ok()?;
        let kind = match r.u8().ok()? {
            0 => ChangeKind::Added,
            1 => ChangeKind::Modified,
            2 => ChangeKind::Deleted,
            _ => return None,
        };
        v.push(Change { path, kind });
    }
    Some(v)
}

impl Memo {
    pub fn encode(&self, w: &mut Writer) {
        w.raw(&self.idx.dev.to_le_bytes())
            .raw(&self.idx.ino.to_le_bytes())
            .raw(&self.idx.size.to_le_bytes())
            .raw(&self.idx.mtime_ns.to_le_bytes())
            .raw(&self.idx.ctime_ns.to_le_bytes())
            .raw(&self.small);
        w.varint(self.unsubscribed.len() as u64);
        for u in &self.unsubscribed {
            w.str(u);
        }
        w.varint(self.zones.len() as u64);
        for z in &self.zones {
            w.str(&z.zone).str(&z.path).str(&z.branch);
            match &z.head {
                Some(h) => {
                    w.u8(1).id(h);
                }
                None => {
                    w.u8(0);
                }
            }
            put_changes(w, &z.staged);
            put_changes(w, &z.unstaged);
        }
    }

    pub fn decode(r: &mut Reader) -> Option<Memo> {
        let u64r = |r: &mut Reader| -> Option<u64> { Some(u64::from_le_bytes(r.raw(8).ok()?.try_into().ok()?)) };
        let i128r = |r: &mut Reader| -> Option<i128> { Some(i128::from_le_bytes(r.raw(16).ok()?.try_into().ok()?)) };
        let idx = IdxStat {
            dev: u64r(r)?,
            ino: u64r(r)?,
            size: u64r(r)?,
            mtime_ns: i128r(r)?,
            ctime_ns: i128r(r)?,
        };
        let mut small = [0u8; 32];
        small.copy_from_slice(r.raw(32).ok()?);
        let n = r.count(1).ok()?;
        let mut unsubscribed = Vec::with_capacity(n);
        for _ in 0..n {
            unsubscribed.push(r.str(4096).ok()?);
        }
        let n = r.count(4).ok()?;
        let mut zones = Vec::with_capacity(n);
        for _ in 0..n {
            let zone = r.str(4096).ok()?;
            let path = r.str(4096).ok()?;
            let branch = r.str(4096).ok()?;
            let head = match r.u8().ok()? {
                0 => None,
                1 => Some(r.id().ok()?),
                _ => return None,
            };
            let staged = get_changes(r)?;
            let unstaged = get_changes(r)?;
            zones.push(MemoZone {
                zone,
                path,
                branch,
                head,
                staged,
                unstaged,
            });
        }
        Some(Memo {
            idx,
            small,
            unsubscribed,
            zones,
        })
    }
}
