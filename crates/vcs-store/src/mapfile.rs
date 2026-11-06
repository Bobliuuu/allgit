//! Read-only view of a whole cache file: memory mapped on unix (opening is O(1), pages are read
//! only when a query touches them, and they are reclaimable page cache, not anonymous memory),
//! a plain `Vec` otherwise or when built from bytes.
//!
//! The caches that use this (commit-graph, reach cache, path filters) are replaced by rename and
//! never modified in place, so a live mapping keeps seeing the old inode's bytes. Anything that
//! truncates such a file in place would make a reader fault; nothing in this repo does.

use std::fs;
use std::io;
use std::ops::Deref;
use std::path::Path;

enum Inner {
    Heap(Vec<u8>),
    #[cfg(unix)]
    Map {
        ptr: *const u8,
        len: usize,
    },
}

/// blake3 of a big buffer on all cores (the digest is the same as `blake3::hash`); whole-file verification of
/// multi-GB caches is the one place this matters.
pub fn blake3_big(b: &[u8]) -> blake3::Hash {
    if b.len() >= 1 << 20 {
        blake3::Hasher::new().update_rayon(b).finalize()
    } else {
        blake3::hash(b)
    }
}

pub struct MapFile(Inner);

// SAFETY: the mapping is PROT_READ and the file is never modified in place (see module docs).
unsafe impl Send for MapFile {}
unsafe impl Sync for MapFile {}

impl MapFile {
    /// Wrap bytes already in memory.
    pub fn from_vec(v: Vec<u8>) -> MapFile {
        MapFile(Inner::Heap(v))
    }

    /// Map (or, off unix, read) the file. `random`: expect point lookups, so no read-ahead.
    pub fn open(path: &Path, random: bool) -> io::Result<MapFile> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let f = fs::File::open(path)?;
            let len = f.metadata()?.len();
            if len > 0 && len <= isize::MAX as u64 {
                let len = len as usize;
                // SAFETY: plain read-only private mapping of an open file; MAP_FAILED is checked.
                let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
                if p != libc::MAP_FAILED {
                    if random {
                        // SAFETY: p/len is the mapping just created.
                        unsafe { libc::madvise(p, len, libc::MADV_RANDOM) };
                    }
                    return Ok(MapFile(Inner::Map { ptr: p as *const u8, len }));
                }
            }
        }
        let _ = random;
        fs::read(path).map(MapFile::from_vec)
    }

    /// Hint that the whole file is about to be read front to back (full verification).
    pub fn advise_sequential(&self) {
        #[cfg(unix)]
        if let Inner::Map { ptr, len } = &self.0 {
            // SAFETY: ptr/len is a live mapping owned by self.
            unsafe { libc::madvise(*ptr as *mut libc::c_void, *len, libc::MADV_SEQUENTIAL) };
        }
    }

    /// Is this a real mapping (not heap bytes)?
    pub fn is_mapped(&self) -> bool {
        !matches!(self.0, Inner::Heap(_))
    }
}

impl Deref for MapFile {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.0 {
            Inner::Heap(v) => v,
            #[cfg(unix)]
            // SAFETY: ptr/len describe a live PROT_READ mapping owned by self.
            Inner::Map { ptr, len } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
        }
    }
}

impl Drop for MapFile {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Inner::Map { ptr, len } = &self.0 {
            // SAFETY: unmapping the mapping created in `open`, once.
            unsafe { libc::munmap(*ptr as *mut libc::c_void, *len) };
        }
    }
}

impl std::fmt::Debug for MapFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MapFile({} bytes, mapped={})", self.len(), self.is_mapped())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_hash_equals_plain_hash_and_map_reads_back() {
        let mut v = vec![0u8; 5_000_001];
        let mut x = 0x1234_5678_9abc_def0u64;
        for b in v.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        assert_eq!(blake3_big(&v), blake3::hash(&v));
        assert_eq!(blake3_big(&v[..1000]), blake3::hash(&v[..1000]));
        let dir = std::env::temp_dir().join(format!("mapfile-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f");
        std::fs::write(&p, &v).unwrap();
        let m = MapFile::open(&p, true).unwrap();
        assert_eq!(&m[..], &v[..]);
        assert_eq!(m.is_mapped(), cfg!(unix));
        std::fs::write(&p, b"").unwrap();
        assert_eq!(MapFile::open(&p, false).unwrap().len(), 0);
        assert!(MapFile::open(&dir.join("missing"), false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
