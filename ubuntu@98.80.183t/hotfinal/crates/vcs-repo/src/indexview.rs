//! Read-only, borrowed view of an `AGI3` index: the header and cache-tree section are parsed, the
//! entries stay in the file bytes in their blocks. Diff-like readers use it to
//!
//! * decide "index tree equals this tree" from the cache-tree alone (no entry is decoded),
//! * decode only the blocks that can hold a requested path range, and
//! * check the working tree while decoding (paths borrowed from the file, no `BTreeMap`, no
//!   per-entry allocation), on several threads.
//!
//! Older layouts (`AGI2`, `AGIX`) give `None` and the caller decodes the whole index as before.

use crate::error::{Error, Result};
use crate::index::IndexEntry;
use crate::worktree_fs::StatCursor;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use vcs_objects::codec::Reader;
use vcs_objects::{EntryMode, ObjectId};

pub(crate) struct IndexView<'a> {
    pub written_ns: u64,
    blocks: Vec<(&'a [u8], usize)>,
    pub trees_fp: Option<ObjectId>,
    pub trees: BTreeMap<String, ObjectId>,
}

fn corrupt(e: vcs_objects::DecodeError) -> Error {
    Error::Corrupt(format!("index: {e}"))
}

impl<'a> IndexView<'a> {
    /// `None` when `bytes` is not an `AGI3` index.
    pub fn parse(bytes: &'a [u8]) -> Result<Option<Self>> {
        if !bytes.starts_with(b"AGI3") {
            return Ok(None);
        }
        let body = crate::meta::unframe(b"AGI3", bytes, "index")?;
        let mut r = Reader::new(body);
        let written_ns = r.varint().map_err(corrupt)?;
        let n = r.count(36).map_err(corrupt)?;
        let per = r.varint().map_err(corrupt)? as usize;
        let nblocks = r.count(1).map_err(corrupt)?;
        if per == 0 || per > (1 << 20) || nblocks != n.div_ceil(per) {
            return Err(Error::Corrupt("index: bad block table".into()));
        }
        let mut lens = Vec::with_capacity(nblocks);
        for _ in 0..nblocks {
            lens.push(r.varint().map_err(corrupt)? as usize);
        }
        let mut blocks = Vec::with_capacity(nblocks);
        for (i, l) in lens.iter().enumerate() {
            let cnt = if i + 1 == nblocks { n - per * i } else { per };
            blocks.push((r.raw(*l).map_err(corrupt)?, cnt));
        }
        let (mut trees_fp, mut trees) = (None, BTreeMap::new());
        let nt = r.varint().map_err(corrupt)? as usize;
        if nt > 0 {
            trees_fp = Some(r.id().map_err(corrupt)?);
            for _ in 0..nt {
                let d = r.str(4096).map_err(corrupt)?;
                trees.insert(d, r.id().map_err(corrupt)?);
            }
        }
        r.finish().map_err(corrupt)?;
        Ok(Some(IndexView {
            written_ns,
            blocks,
            trees_fp,
            trees,
        }))
    }

    /// Visit the entries of block `i` in order. `f` gets the path borrowed from the file.
    ///
    /// Names are NOT validated here (only ordering is): callers that act on a path beyond `lstat` validate it.
    pub fn each_in_block(&self, i: usize, mut f: impl FnMut(&str, IndexEntry) -> Result<()>) -> Result<()> {
        let (bytes, cnt) = self.blocks[i];
        let mut r = Reader::new(bytes);
        let mut prev: &str = "";
        for _ in 0..cnt {
            let (p, e) = read_entry_ref(&mut r, prev)?;
            f(p, e)?;
            prev = p;
        }
        r.finish().map_err(corrupt)
    }

    /// First path of block `i`.
    fn first_path(&self, i: usize) -> Result<&str> {
        let mut r = Reader::new(self.blocks[i].0);
        let b = r.bytes(4096).map_err(corrupt)?;
        std::str::from_utf8(b).map_err(|_| Error::Corrupt("index: utf-8".into()))
    }

    /// Indexes of the blocks that can hold a path in `[lo, hi)` (`hi == None`: unbounded).
    fn blocks_in(&self, lo: &str, hi: Option<&str>) -> Result<Vec<usize>> {
        let mut firsts = Vec::with_capacity(self.blocks.len());
        for i in 0..self.blocks.len() {
            firsts.push(self.first_path(i)?);
        }
        let mut out = Vec::new();
        for i in 0..firsts.len() {
            // block i holds [firsts[i], firsts[i+1]); the last one is open-ended
            let ends_before_lo = firsts.get(i + 1).is_some_and(|next| *next <= lo);
            let starts_after_hi = hi.is_some_and(|h| firsts[i] >= h);
            if !ends_before_lo && !starts_after_hi {
                out.push(i);
            }
        }
        Ok(out)
    }

    /// Blocks that can hold `p` or anything beneath `p/` for some `p` in `only` (all blocks when `only`
    /// is empty or holds the empty path), ascending and without repeats.
    pub fn blocks_for(&self, only: &[String]) -> Result<Vec<usize>> {
        if only.is_empty() || only.iter().any(|f| f.is_empty()) {
            return Ok((0..self.blocks.len()).collect());
        }
        let mut all = Vec::new();
        for f in only {
            let hi = format!("{f}0"); // '0' == '/' + 1: first key after "f/..."
            all.extend(self.blocks_in(f, Some(&hi))?);
        }
        all.sort_unstable();
        all.dedup();
        Ok(all)
    }

    /// Run `work` over every entry of `blocks` on up to `threads` threads; the `Some` results come
    /// back in block order, entry order within a block.
    pub fn par_filter_map<T: Send>(
        &self,
        blocks: &[usize],
        threads: usize,
        work: &(dyn Fn(&mut StatCursor, &str, &IndexEntry) -> Result<Option<T>> + Sync),
    ) -> Result<Vec<T>> {
        let one = |bi: usize| -> Result<Vec<T>> {
            let mut out = Vec::new();
            let mut cur = StatCursor::default();
            self.each_in_block(bi, |p, e| {
                if let Some(t) = work(&mut cur, p, &e)? {
                    out.push(t);
                }
                Ok(())
            })?;
            Ok(out)
        };
        let threads = threads.min(blocks.len()).max(1);
        if threads == 1 {
            let mut out = Vec::new();
            for &b in blocks {
                out.extend(one(b)?);
            }
            return Ok(out);
        }
        let next = AtomicUsize::new(0);
        let mut parts: Vec<(usize, Result<Vec<T>>)> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    sc.spawn(|| {
                        let mut mine = Vec::new();
                        loop {
                            let k = next.fetch_add(1, Ordering::Relaxed);
                            if k >= blocks.len() {
                                break;
                            }
                            let r = one(blocks[k]);
                            let failed = r.is_err();
                            mine.push((k, r));
                            if failed {
                                break;
                            }
                        }
                        mine
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().expect("index scan worker panicked")).collect()
        });
        parts.sort_by_key(|p| p.0);
        let mut out = Vec::new();
        for (_, r) in parts {
            out.extend(r?);
        }
        Ok(out)
    }
}

/// One entry with its path borrowed from the file bytes. `prev` is the previous path of the same block.
fn read_entry_ref<'a>(r: &mut Reader<'a>, prev: &str) -> Result<(&'a str, IndexEntry)> {
    let b = r.bytes(4096).map_err(corrupt)?;
    let p = std::str::from_utf8(b).map_err(|_| Error::Corrupt("index: utf-8".into()))?;
    if p <= prev {
        return Err(Error::Corrupt("index: entries out of order".into()));
    }
    let mode = match r.u8().map_err(corrupt)? {
        1 => EntryMode::File,
        2 => EntryMode::Executable,
        4 => EntryMode::Symlink,
        _ => return Err(Error::Corrupt("index: bad mode".into())),
    };
    let id = r.id().map_err(corrupt)?;
    let size = r.varint().map_err(corrupt)?;
    let mtime_ns = r.varint().map_err(corrupt)?;
    let ctime_ns = r.varint().map_err(corrupt)?;
    Ok((
        p,
        IndexEntry {
            id,
            mode,
            size,
            mtime_ns,
            ctime_ns,
        },
    ))
}
