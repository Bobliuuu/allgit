//! Large-file paths: streaming add with constant memory, parallel verification against a manifest,
//! parallel checkout into a preallocated file, and byte-range reads that touch only the chunks that
//! overlap the range. Everything here works on manifests (`ChunkedBlob`) and plain blob chunks, so
//! it applies to both chunk layouts; only `put_file` creates the large layout.

use crate::{ObjectStore, StoreError};
use std::fs::File;
use std::io;
use vcs_objects::chunk::{large_candidates, large_next_cut, par_map, par_map_with, Cand, LARGE_WARM};
use vcs_objects::{blob_id, ChunkRef, ChunkedBlob, Object, ObjectId, LARGE_THRESHOLD};

/// Bytes read per round by [`put_file`]; peak memory is about this plus one maximal chunk.
pub const WINDOW: usize = 64 << 20;
/// Work unit for parallel verification and checkout: consecutive chunks totalling at least this much.
const GROUP_BYTES: u64 = 4 << 20;
/// Threads that write one file in parallel.
const WRITERS: usize = 4;

fn ioerr(path: &str, source: io::Error) -> StoreError {
    StoreError::Io { path: path.into(), source }
}

#[cfg(unix)]
pub fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}
#[cfg(unix)]
pub fn write_all_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}
#[cfg(windows)]
pub fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}
#[cfg(windows)]
pub fn write_all_at(f: &File, mut buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Reserve `len` bytes of disk for `f` (so parallel writers do not fragment it) and set its length.
/// Falls back to a sparse `set_len` where `fallocate` is unavailable or unsupported.
pub fn preallocate(f: &File, len: u64) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: valid fd; fallocate only affects the file's allocation.
        let r = unsafe { libc::fallocate(f.as_raw_fd(), 0, 0, len as libc::off_t) };
        if r == 0 {
            return Ok(());
        }
    }
    f.set_len(len)
}

/// Read `[off, off+buf.len())` of `f` into `buf` with several threads.
fn par_read(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    const PART: usize = 8 << 20;
    if buf.len() <= PART {
        return read_exact_at(f, buf, off);
    }
    let parts: Vec<(usize, &mut [u8])> = buf.chunks_mut(PART).enumerate().collect();
    let slots: Vec<std::sync::Mutex<Option<&mut [u8]>>> = parts.into_iter().map(|(_, s)| std::sync::Mutex::new(Some(s))).collect();
    let res = par_map(slots.len(), |i| {
        let s = slots[i].lock().unwrap().take().expect("each part is read once");
        read_exact_at(f, s, off + (i * PART) as u64)
    });
    res.into_iter().collect()
}

/// Store the file `f` of `len` bytes as content: for `len >= LARGE_THRESHOLD` the large-file layout,
/// read in [`WINDOW`]-sized rounds (boundary search, hashing and chunk stores run on all cores) so memory
/// stays constant; smaller files go through `put_content`. The id equals `content_id` of the file's bytes.
pub fn put_file(store: &dyn ObjectStore, f: &File, len: u64) -> Result<ObjectId, StoreError> {
    if len < LARGE_THRESHOLD as u64 {
        let mut v = vec![0u8; len as usize];
        read_exact_at(f, &mut v, 0).map_err(|e| ioerr("<file>", e))?;
        return store.put_content(&v);
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut buf_off = 0u64; // absolute offset of buf[0]
    let mut scanned = 0u64; // all positions below this are searched for candidates
    let mut start = 0u64; // start of the next chunk to emit
    let mut cands: Vec<Cand> = Vec::new();
    let mut chunks: Vec<ChunkRef> = Vec::new();
    loop {
        let eof = scanned == len;
        let mut cuts: Vec<(u64, u64)> = Vec::new();
        while start < len {
            match large_next_cut(start, &cands, scanned, eof, len) {
                Some(c) => {
                    cuts.push((start, c));
                    start = c;
                }
                None => break,
            }
        }
        if !cuts.is_empty() {
            let b = &buf;
            let done = par_map(cuts.len(), |i| {
                let (s, e) = cuts[i];
                let data = &b[(s - buf_off) as usize..(e - buf_off) as usize];
                let id = blob_id(data);
                store.put_blob_prehashed(&id, data)?;
                Ok::<_, StoreError>(ChunkRef {
                    id,
                    len: data.len() as u32,
                })
            });
            for d in done {
                chunks.push(d?);
            }
        }
        if start >= len {
            break;
        }
        // keep only what the next chunk needs, plus the hash warm-up bytes before it
        let keep_from = start.saturating_sub(LARGE_WARM as u64).max(buf_off);
        buf.drain(..(keep_from - buf_off) as usize);
        buf_off = keep_from;
        cands.drain(..cands.partition_point(|c| c.pos < start));
        let take = (len - scanned).min(WINDOW as u64) as usize;
        let old = buf.len();
        debug_assert_eq!(buf_off + old as u64, scanned);
        buf.resize(old + take, 0);
        par_read(f, &mut buf[old..], scanned).map_err(|e| ioerr("<file>", e))?;
        // candidate search over the new region, in parallel segments
        const SEG: usize = 4 << 20;
        let nseg = take.div_ceil(SEG);
        let b = &buf;
        let parts = par_map(nseg, |k| {
            let (s, e) = (old + k * SEG, (old + (k + 1) * SEG).min(old + take));
            let mut v = Vec::new();
            large_candidates(&b[s.saturating_sub(LARGE_WARM)..s], &b[s..e], buf_off + s as u64, &mut v);
            v
        });
        for p in parts {
            cands.extend(p);
        }
        scanned += take as u64;
    }
    let m = ChunkedBlob {
        total_len: len,
        chunks,
        large: true,
    };
    store.put(&Object::Chunked(m))
}

/// Store `data` in the v3 small-chunk layout whatever its size (what releases before the large-file layout
/// wrote for files of 1 MiB and up). Returns the manifest id, or the blob id below 1 MiB.
pub fn put_legacy_content(store: &dyn ObjectStore, data: &[u8]) -> Result<ObjectId, StoreError> {
    match ChunkedBlob::plan(data) {
        None => store.put_blob(data),
        Some(m) => {
            let offs = m.offsets();
            for (c, &s) in m.chunks.iter().zip(&offs) {
                store.put_blob_prehashed(&c.id, &data[s as usize..s as usize + c.len as usize])?;
            }
            store.put(&Object::Chunked(m))
        }
    }
}

/// Most bytes the incremental path re-reads and re-chunks before giving up in favour of a full pass.
const INCREMENTAL_MAX: u64 = 256 << 20;

/// Store the file `f` (`len` bytes) given the manifest `old` (id `old_id`) of an earlier version of the
/// same length in the large layout: hash its chunks in parallel, and re-chunk only the stretches whose
/// chunks changed, up to the first old boundary the new cuts land on again (cut decisions depend only on
/// the bytes before the cut, so everything after that boundary is unchanged). Returns the file's content
/// id, which is `old_id` when nothing changed and is always the id `put_file` would give. `None` means
/// the incremental path does not apply (different length, legacy layout, large rewrite): use `put_file`.
pub fn put_file_incremental(
    store: &dyn ObjectStore,
    f: &File,
    len: u64,
    old_id: &ObjectId,
    old: &ChunkedBlob,
) -> Result<Option<ObjectId>, StoreError> {
    if !old.large || old.total_len != len {
        return Ok(None);
    }
    let Some(changed) = changed_chunks(old, f).map_err(|e| ioerr("<file>", e))? else {
        return Ok(None);
    };
    if changed.is_empty() {
        return Ok(Some(*old_id));
    }
    let offs = old.offsets();
    let n = old.chunks.len();
    let bound = |k: usize| if k + 1 == n { len } else { offs[k + 1] }; // end of old chunk k
    let ends: Vec<u64> = (0..n).map(bound).collect();
    let changed_bytes: u64 = changed.iter().map(|&i| old.chunks[i].len as u64).sum();
    if changed_bytes > INCREMENTAL_MAX / 4 {
        return Ok(None);
    }
    let mut out: Vec<ChunkRef> = Vec::with_capacity(n + 2);
    let mut i = 0;
    let mut ci = 0; // cursor into `changed`
    while i < n {
        if ci >= changed.len() || changed[ci] != i {
            out.push(old.chunks[i]);
            i += 1;
            continue;
        }
        // first changed chunk of a run: re-chunk from its start until the cuts rejoin an old boundary at or
        // past the end of the last changed chunk of the run
        let mut j = i;
        while ci < changed.len() && changed[ci] == j {
            ci += 1;
            j += 1;
        }
        let s = offs[i];
        let min_end = ends[j - 1];
        let mut buf: Vec<u8> = Vec::new();
        let buf_off = s.saturating_sub(LARGE_WARM as u64);
        let mut cands: Vec<Cand> = Vec::new();
        let (mut known, mut start) = (s, s);
        let mut cuts: Vec<u64> = Vec::new();
        let rejoin = 'outer: loop {
            loop {
                match large_next_cut(start, &cands, known, known == len, len) {
                    Some(c) => {
                        cuts.push(c);
                        start = c;
                        if c >= min_end {
                            if let Ok(k) = ends.binary_search(&c) {
                                break 'outer Some(k);
                            }
                        }
                        if c >= len {
                            break 'outer None;
                        }
                    }
                    None => break,
                }
            }
            if known - s > INCREMENTAL_MAX {
                return Ok(None);
            }
            let take = (len - known).min(8 << 20) as usize;
            let old_len = buf.len();
            if old_len == 0 {
                // include the warm-up bytes before `s`
                buf.resize((s - buf_off) as usize, 0);
                read_exact_at(f, &mut buf, buf_off).map_err(|e| ioerr("<file>", e))?;
            }
            let old_len = buf.len();
            buf.resize(old_len + take, 0);
            read_exact_at(f, &mut buf[old_len..], known).map_err(|e| ioerr("<file>", e))?;
            large_candidates(
                &buf[old_len.saturating_sub(LARGE_WARM)..old_len],
                &buf[old_len..],
                known,
                &mut cands,
            );
            known += take as u64;
        };
        // store the new chunks of this run
        let refs = par_map(cuts.len(), |k| {
            let a = if k == 0 { s } else { cuts[k - 1] };
            let data = &buf[(a - buf_off) as usize..(cuts[k] - buf_off) as usize];
            let id = blob_id(data);
            store.put_blob_prehashed(&id, data)?;
            Ok::<_, StoreError>(ChunkRef {
                id,
                len: data.len() as u32,
            })
        });
        for r in refs {
            out.push(r?);
        }
        // continue after the old chunk the new cuts rejoined (or the end of the file)
        i = match rejoin {
            Some(k) => k + 1,
            None => n,
        };
        // changed chunks swallowed by the re-chunked stretch are done
        while ci < changed.len() && changed[ci] < i {
            ci += 1;
        }
    }
    let m = ChunkedBlob {
        total_len: len,
        chunks: out,
        large: true,
    };
    store.put(&Object::Chunked(m)).map(Some)
}

/// Consecutive chunk ranges `[first, last)` of at least [`GROUP_BYTES`] each (the last may be smaller).
fn groups(m: &ChunkedBlob, lo: usize, hi: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut first, mut bytes) = (lo, 0u64);
    for i in lo..hi {
        bytes += m.chunks[i].len as u64;
        if bytes >= GROUP_BYTES {
            out.push((first, i + 1));
            first = i + 1;
            bytes = 0;
        }
    }
    if first < hi {
        out.push((first, hi));
    }
    out
}

/// True if the first `m.total_len` bytes of `f` hash chunk by chunk to the ids in `m`, and `f` is that
/// long. Reads each byte once, in parallel; stops early on the first mismatch. This is the status fast
/// path: no boundary search, no store writes. (It cannot tell a non-canonical manifest from a canonical
/// one, which only matters for hostile manifests.)
pub fn file_matches(m: &ChunkedBlob, f: &File) -> io::Result<bool> {
    if f.metadata()?.len() != m.total_len {
        return Ok(false);
    }
    let offs = m.offsets();
    let g = groups(m, 0, m.chunks.len());
    let bad = std::sync::atomic::AtomicBool::new(false);
    let res = par_map(g.len(), |k| -> io::Result<()> {
        if bad.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        let (a, b) = g[k];
        let base = offs[a];
        let end = if b == m.chunks.len() { m.total_len } else { offs[b] };
        let mut buf = vec![0u8; (end - base) as usize];
        read_exact_at(f, &mut buf, base)?;
        for i in a..b {
            let s = (offs[i] - base) as usize;
            if blob_id(&buf[s..s + m.chunks[i].len as usize]) != m.chunks[i].id {
                bad.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
        Ok(())
    });
    for r in res {
        r?;
    }
    Ok(!bad.load(std::sync::atomic::Ordering::Relaxed))
}

/// Chunk indices whose bytes in `f` differ from the manifest (reads and hashes everything in parallel).
/// `None` when `f` has a different length (then no chunk correspondence exists).
pub fn changed_chunks(m: &ChunkedBlob, f: &File) -> io::Result<Option<Vec<usize>>> {
    if f.metadata()?.len() != m.total_len {
        return Ok(None);
    }
    let offs = m.offsets();
    let g = groups(m, 0, m.chunks.len());
    let res = par_map(g.len(), |k| -> io::Result<Vec<usize>> {
        let (a, b) = g[k];
        let base = offs[a];
        let end = if b == m.chunks.len() { m.total_len } else { offs[b] };
        let mut buf = vec![0u8; (end - base) as usize];
        read_exact_at(f, &mut buf, base)?;
        Ok((a..b)
            .filter(|&i| {
                let s = (offs[i] - base) as usize;
                blob_id(&buf[s..s + m.chunks[i].len as usize]) != m.chunks[i].id
            })
            .collect())
    });
    let mut out = Vec::new();
    for r in res {
        out.extend(r?);
    }
    Ok(Some(out))
}

fn fetch_chunk<S: ObjectStore + ?Sized>(store: &S, manifest: &ObjectId, c: &ChunkRef) -> Result<Vec<u8>, StoreError> {
    let b = store.get_blob(&c.id)?;
    if b.len() != c.len as usize {
        return Err(StoreError::Corrupt {
            id: *manifest,
            reason: format!("chunk {} has length {}, manifest says {}", c.id, b.len(), c.len),
        });
    }
    Ok(b)
}

/// Bytes `[off, off+len)` of the file described by `m` (clamped to its length), fetching only the
/// chunks that overlap the range, in parallel when there are several. `id` is the manifest's id (for errors).
pub fn read_manifest_range<S: ObjectStore + ?Sized>(
    store: &S,
    id: &ObjectId,
    m: &ChunkedBlob,
    off: u64,
    len: u64,
) -> Result<Vec<u8>, StoreError> {
    let offs = m.offsets();
    let (first, last) = m.chunk_range(&offs, off, len);
    if first == last {
        return Ok(Vec::new());
    }
    let want_end = off.saturating_add(len).min(m.total_len);
    let slice_of = |i: usize, b: &[u8], out: &mut Vec<u8>| {
        let (cs, ce) = (offs[i], offs[i] + m.chunks[i].len as u64);
        let (s, e) = (off.max(cs), want_end.min(ce));
        out.extend_from_slice(&b[(s - cs) as usize..(e - cs) as usize]);
    };
    // the reservation is bounded by what was asked for and what the manifest claims, never by one alone
    let mut out = Vec::with_capacity((want_end - off).min(16 << 20) as usize);
    if last - first < 4 {
        for i in first..last {
            let b = fetch_chunk(store, id, &m.chunks[i])?;
            slice_of(i, &b, &mut out);
        }
        return Ok(out);
    }
    let g = groups(m, first, last);
    let parts = par_map(g.len(), |k| -> Result<Vec<u8>, StoreError> {
        let mut v = Vec::new();
        for i in g[k].0..g[k].1 {
            let b = fetch_chunk(store, id, &m.chunks[i])?;
            slice_of(i, &b, &mut v);
        }
        Ok(v)
    });
    for p in parts {
        out.extend_from_slice(&p?);
    }
    Ok(out)
}

/// Write the whole file described by `m` into `f` (already created, empty): preallocate, then fetch,
/// verify and `pwrite` groups of chunks at their offsets in parallel. Memory use is a few groups, not
/// the file. The caller makes the result visible atomically (write to a temporary name, then rename).
pub fn write_manifest_to<S: ObjectStore + ?Sized>(store: &S, id: &ObjectId, m: &ChunkedBlob, f: &File) -> Result<(), StoreError> {
    preallocate(f, m.total_len).map_err(|e| ioerr("<file>", e))?;
    let offs = m.offsets();
    let g = groups(m, 0, m.chunks.len());
    // Writers to one file serialise on its inode lock and the waiters spin: more than a few threads only burn CPU
    // (measured: 16 threads 4.6 s system time for a 1 GiB checkout, 4 threads 0.9 s at the same wall time).
    let res = par_map_with(WRITERS, g.len(), |k| -> Result<(), StoreError> {
        let (a, b) = g[k];
        // one pwrite per chunk straight from the verified chunk: no group buffer, no second copy
        for i in a..b {
            let chunk = fetch_chunk(store, id, &m.chunks[i])?;
            write_all_at(f, &chunk, offs[i]).map_err(|e| ioerr("<file>", e))?;
        }
        Ok(())
    });
    res.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryStore, ObjectStore};
    use std::io::Write;

    fn rnd(n: usize, mut x: u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }
    fn file_of(d: &tempfile::TempDir, name: &str, data: &[u8]) -> File {
        let p = d.path().join(name);
        std::fs::File::create(&p).unwrap().write_all(data).unwrap();
        File::open(p).unwrap()
    }

    #[test]
    fn streaming_put_file_equals_in_memory_content_id_and_reads_back() {
        let d = tempfile::tempdir().unwrap();
        for (n, seed) in [(16usize << 20, 1u64), ((16 << 20) + 12_345, 2), (150 << 20, 3)] {
            let mut data = rnd(n, seed);
            if n > 100 << 20 {
                data[30 << 20..45 << 20].fill(0); // 15 MiB of zeros: maximum-size chunks
            }
            let s = MemoryStore::new();
            let f = file_of(&d, "a", &data);
            let id = put_file(&s, &f, n as u64).unwrap();
            assert_eq!(id, vcs_objects::content_id(&data), "size {n}");
            assert_eq!(s.get_content(&id).unwrap(), data);
            assert_eq!(s.get_content_range(&id, 15 << 20, 1000).unwrap(), data[15 << 20..(15 << 20) + 1000]);
        }
    }

    #[test]
    fn incremental_put_equals_full_put_for_edits_anywhere() {
        let d = tempfile::tempdir().unwrap();
        let n = 64 << 20;
        let base = rnd(n, 5);
        let s = MemoryStore::new();
        let id = put_file(&s, &file_of(&d, "v1", &base), n as u64).unwrap();
        let Object::Chunked(old) = s.get(&id).unwrap() else { panic!() };
        // unchanged -> same id, nothing written
        let f = file_of(&d, "same", &base);
        assert_eq!(put_file_incremental(&s, &f, n as u64, &id, &old).unwrap(), Some(id));
        let edits: Vec<(&str, Box<dyn Fn(&mut Vec<u8>)>)> = vec![
            ("first byte", Box::new(|v| v[0] ^= 1)),
            ("last byte", Box::new(|v| *v.last_mut().unwrap() ^= 1)),
            ("middle byte", Box::new(|v| v[33 << 20] ^= 1)),
            (
                "3 MiB overwrite",
                Box::new(|v| v[20 << 20..23 << 20].copy_from_slice(&rnd(3 << 20, 8))),
            ),
            (
                "two far edits",
                Box::new(|v| {
                    v[5 << 20] ^= 1;
                    v[50 << 20] ^= 1;
                }),
            ),
            ("zeroed 20 MiB", Box::new(|v| v[10 << 20..30 << 20].fill(0))),
            (
                "adjacent chunks",
                Box::new(|v| v[8 << 20..14 << 20].copy_from_slice(&rnd(6 << 20, 9))),
            ),
        ];
        for (name, edit) in edits {
            let mut v = base.clone();
            edit(&mut v);
            let f = file_of(&d, "v2", &v);
            let got = put_file_incremental(&s, &f, n as u64, &id, &old).unwrap();
            let want = vcs_objects::content_id(&v);
            if let Some(g) = got {
                assert_eq!(g, want, "{name}");
                assert_eq!(s.get_content(&g).unwrap(), v, "{name}");
            } else {
                assert_eq!(put_file(&s, &f, n as u64).unwrap(), want, "{name} (fallback)");
            }
        }
        // a different length never takes the incremental path
        let mut longer = base.clone();
        longer.push(1);
        let f = file_of(&d, "longer", &longer);
        assert_eq!(put_file_incremental(&s, &f, longer.len() as u64, &id, &old).unwrap(), None);
    }

    #[test]
    fn file_matches_and_changed_chunks_see_a_one_byte_change() {
        let d = tempfile::tempdir().unwrap();
        let data = rnd(20 << 20, 12);
        let m = ChunkedBlob::plan_large(&data).unwrap();
        assert!(file_matches(&m, &file_of(&d, "a", &data)).unwrap());
        let mut e = data.clone();
        e[9 << 20] ^= 1;
        let f = file_of(&d, "b", &e);
        assert!(!file_matches(&m, &f).unwrap());
        let ch = changed_chunks(&m, &f).unwrap().unwrap();
        assert_eq!(ch.len(), 1);
        let (first, _) = m.chunk_range(&m.offsets(), 9 << 20, 1);
        assert_eq!(ch[0], first);
        assert!(!file_matches(&m, &file_of(&d, "c", &data[..data.len() - 1])).unwrap());
    }
}
