//! One `getattrlistbulk(2)` call per directory (macOS): the `lstat` results of every regular file in a
//! directory in a handful of syscalls instead of one `fstatat` per file. Only used by
//! [`crate::worktree_fs::StatCursor`] on scans that touch many files of one directory; every entry this
//! cannot describe exactly as `fstatat` would (non-regular files, a missing attribute) is simply left out,
//! and the caller falls back to `fstatat` for it.

use crate::worktree_fs::{FileKind, FsStat};
use std::collections::HashMap;
use std::os::unix::io::RawFd;
use vcs_objects::EntryMode;

const VREG: u32 = 1;
const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_CHGTIME
    | libc::ATTR_CMN_ACCESSMASK
    | libc::ATTR_CMN_FILEID;

/// Cursor over one packed record.
struct Rd<'a>(&'a [u8], usize);

impl Rd<'_> {
    fn u32(&mut self) -> Option<u32> {
        let b = self.0.get(self.1..self.1 + 4)?;
        self.1 += 4;
        Some(u32::from_ne_bytes(b.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        let b = self.0.get(self.1..self.1 + 8)?;
        self.1 += 8;
        Some(u64::from_ne_bytes(b.try_into().ok()?))
    }
    /// `struct timespec` as nanoseconds since the epoch (a time before it reads as 0, like `mtime_ns`).
    fn ts(&mut self) -> Option<u64> {
        let (s, n) = (self.u64()? as i64, self.u64()? as i64);
        Some((s.max(0) as u64).saturating_mul(1_000_000_000).saturating_add(n.max(0) as u64))
    }
}

/// Stat every regular file directly inside the directory open as `fd`. `None` if the call is unsupported
/// on this filesystem (the caller keeps using `fstatat`).
pub(crate) fn stat_dir(fd: RawFd) -> Option<HashMap<Box<[u8]>, FsStat>> {
    let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = COMMON;
    al.fileattr = libc::ATTR_FILE_DATALENGTH;
    let mut buf = vec![0u8; 256 * 1024];
    let mut out = HashMap::new();
    loop {
        // SAFETY: `al` and `buf` outlive the call; `fd` is an open directory.
        let n = unsafe {
            libc::getattrlistbulk(
                fd,
                (&mut al as *mut libc::attrlist).cast(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::FSOPT_PACK_INVAL_ATTRS as u64,
            )
        };
        if n < 0 {
            return None;
        }
        if n == 0 {
            return Some(out);
        }
        let mut off = 0usize;
        for _ in 0..n {
            let rec_len = u32::from_ne_bytes(buf.get(off..off + 4)?.try_into().ok()?) as usize;
            let rec = buf.get(off..off + rec_len)?;
            off += rec_len;
            if let Some((name, st)) = parse(rec) {
                out.insert(name, st);
            }
        }
    }
}

/// One record: length, returned-attribute set, then the requested attributes in bit order.
fn parse(rec: &[u8]) -> Option<(Box<[u8]>, FsStat)> {
    let mut r = Rd(rec, 4);
    let (common, _vol, _dir, file, _fork) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
    let want = COMMON & !libc::ATTR_CMN_RETURNED_ATTRS;
    if common & want != want {
        return None;
    }
    let name_at = r.1;
    let (name_off, name_len) = (r.u32()? as i32, r.u32()? as usize);
    let objtype = r.u32()?;
    let mtime_ns = r.ts()?;
    let ctime_ns = r.ts()?;
    let mode = r.u32()?;
    let ino = r.u64()?;
    if objtype != VREG || file & libc::ATTR_FILE_DATALENGTH == 0 {
        return None; // dirs, links, devices: let fstatat describe them
    }
    let len = r.u64()?;
    let s = (name_at as i64 + name_off as i64) as usize;
    let name = rec.get(s..s + name_len.checked_sub(1)?)?; // length counts the NUL
    Some((
        name.into(),
        FsStat {
            kind: FileKind::File,
            len,
            mtime_ns,
            ctime_ns,
            mode: if mode & 0o111 != 0 {
                EntryMode::Executable
            } else {
                EntryMode::File
            },
            ino,
        },
    ))
}
