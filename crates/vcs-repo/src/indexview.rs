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
use crate::index5::BLOCK5;
use crate::worktree_fs::StatCursor;
use std::collections::BTreeMap;

/// A per-entry job for the parallel block scan.
type StatWork<'a, T> = dyn Fn(&mut StatCursor, &str, &IndexEntry) -> Result<Option<T>> + Sync + 'a;
use std::sync::atomic::{AtomicUsize, Ordering};
use vcs_objects::codec::Reader;
use vcs_objects::{EntryMode, ObjectId};

pub(crate) struct IndexView<'a> {
    pub written_ns: u64,
    blocks: Vec<(&'a [u8], usize)>,
    /// `AGI5`: the image whose mapped base is walked in blocks of `BLOCK5` records, merged with the log.
    v5: Option<&'a crate::indeximage::IndexImage>,
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
            v5: None,
            trees_fp,
            trees,
        }))
    }

    /// A view over an `AGI5` image (base plus log); `None` for the older layouts.
    pub fn of_image(img: &'a crate::indeximage::IndexImage) -> Result<Option<Self>> {
        if !img.is_v5() {
            return Ok(None);
        }
        let (trees_fp, trees) = match img.trees()? {
            Some((fp, t)) => (Some(*fp), t.clone()),
            None => (None, BTreeMap::new()),
        };
        Ok(Some(IndexView {
            written_ns: img.written_ns,
            blocks: Vec::new(),
            v5: Some(img),
            trees_fp,
            trees,
        }))
    }

    fn nblocks(&self) -> usize {
        match self.v5 {
            Some(img) => img.base5().map_or(1, |b| b.n.div_ceil(BLOCK5).max(1)),
            None => self.blocks.len(),
        }
    }

    /// Visit the entries of block `i` in order. `f` gets the path borrowed from the file.
    ///
    /// Names are NOT validated here (only ordering is): callers that act on a path beyond `lstat` validate it.
    pub fn each_in_block(&self, i: usize, mut f: impl FnMut(&str, IndexEntry) -> Result<()>) -> Result<()> {
        if let Some(img) = self.v5 {
            return Self::each_v5(img, i, &mut f);
        }
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

    fn each_v5(img: &crate::indeximage::IndexImage, i: usize, f: &mut dyn FnMut(&str, IndexEntry) -> Result<()>) -> Result<()> {
        let b = img.base5().expect("v5 view has a base");
        let nb = b.n.div_ceil(BLOCK5).max(1);
        let (a, z) = ((i * BLOCK5).min(b.n), ((i + 1) * BLOCK5).min(b.n));
        let lo = if i == 0 { None } else { Some(b.path(a)?) };
        let hi = if i + 1 < nb { Some(b.path(z)?) } else { None };
        let mut ov = img.overlay_range(lo, hi).peekable();
        let mut prev: &str = "";
        for ri in a..z {
            let (p, e) = b.get_at(ri)?;
            if p <= prev {
                return Err(Error::Corrupt("index: entries out of order".into()));
            }
            prev = p;
            while let Some((k, v)) = ov.next_if(|(k, _)| k.as_str() < p) {
                if let Some(v) = v {
                    f(k, v.clone())?;
                }
            }
            match ov.next_if(|(k, _)| k.as_str() == p) {
                Some((_, Some(v))) => f(p, v.clone())?,
                Some((_, None)) => {}
                None => f(p, e)?,
            }
        }
        for (k, v) in ov {
            if let Some(v) = v {
                f(k, v.clone())?;
            }
        }
        Ok(())
    }

    /// First path of block `i`.
    fn first_path(&self, i: usize) -> Result<&str> {
        if let Some(img) = self.v5 {
            let b = img.base5().expect("v5 view has a base");
            return if b.n == 0 { Ok("") } else { b.path((i * BLOCK5).min(b.n - 1)) };
        }
        let mut r = Reader::new(self.blocks[i].0);
        let b = r.bytes(4096).map_err(corrupt)?;
        std::str::from_utf8(b).map_err(|_| Error::Corrupt("index: utf-8".into()))
    }

    /// Indexes of the blocks that can hold a path in `[lo, hi)` (`hi == None`: unbounded).
    fn blocks_in(&self, lo: &str, hi: Option<&str>) -> Result<Vec<usize>> {
        let mut firsts = Vec::with_capacity(self.nblocks());
        for i in 0..self.nblocks() {
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
            return Ok((0..self.nblocks()).collect());
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

    /// Visit every entry on up to `threads` block-parallel workers, one `T` (made by `init`) per
    /// block, returned in block order. Paths are checked as the full decoder checks them (order and
    /// `validate_path_after`); nothing is collected per entry.
    pub fn par_scan<T: Send>(
        &self,
        threads: usize,
        init: &(dyn Fn() -> T + Sync),
        visit: &(dyn Fn(&mut T, &str, &IndexEntry) + Sync),
    ) -> Result<Vec<T>> {
        let blocks: Vec<usize> = (0..self.nblocks()).collect();
        let one = |bi: usize| -> Result<T> {
            let mut t = init();
            let mut prev = String::new();
            self.each_in_block(bi, |p, e| {
                vcs_objects::names::validate_path_after(&prev, p)?;
                visit(&mut t, p, &e);
                prev.clear();
                prev.push_str(p);
                Ok(())
            })?;
            Ok(t)
        };
        let threads = threads.min(blocks.len()).max(1);
        if threads == 1 {
            return blocks.iter().map(|&b| one(b)).collect();
        }
        let next = AtomicUsize::new(0);
        let mut parts: Vec<(usize, Result<T>)> = std::thread::scope(|sc| {
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
        parts.into_iter().map(|(_, r)| r).collect()
    }

    /// Run `work` over every entry of `blocks` on up to `threads` threads; the `Some` results come
    /// back in block order, entry order within a block.
    pub fn par_filter_map<T: Send>(&self, blocks: &[usize], threads: usize, work: &StatWork<'_, T>) -> Result<Vec<T>> {
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
