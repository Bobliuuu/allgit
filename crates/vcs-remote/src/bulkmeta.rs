//! Bulk metadata fetch for a full lazy clone: stream the commit and tree prefix of every remote pack through all
//! cores into packs of the local store, instead of walking the history one object at a time.
//!
//! The walk of `protocol::fetch_heads_impl` is a single-threaded post-order DFS. On a deep history its stack holds
//! every commit down the chain and its read-ahead cache every object of the metadata prefix (21 GiB at 10M commits),
//! and each object pays decode, two hash-set probes, a `contains` and a store `put` on one thread.
//!
//! Here a reader thread reads each pack's metadata prefix in large sequential ranges (a pack written by `Publisher`
//! holds commits, then trees, dependencies first, then file data), worker threads hash and strictly decode every
//! object and check that what it names (parents, root, subdirectories) is itself fetched, and a writer thread
//! appends the verified objects to local packs of bounded size. Memory is the set of fetched ids (about 64 B per
//! object) plus a few queued chunks.
//!
//! What it does NOT assume: that a pack's metadata is all in its prefix, or that every pack is a `Publisher` pack.
//! Anything a prefix does not deliver is reported back as a hole and the caller walks it the old way. Every object is
//! hashed against its id and decoded exactly as the walk does, so a lying pack cannot plant data.
use crate::backend::Backend;
use crate::packfetch::{Loaded, PackCatalog, PACK_HEADER};
use crate::protocol::PublishError;
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_store::pack::{block_is_metadata, decode_block, decode_payload};
use vcs_store::LocalStore;

/// Bytes read from a remote pack per request.
const CHUNK: u64 = 16 << 20;
/// Objects (and bytes) per local pack written by the bulk fetch.
const SEG_OBJECTS: usize = 400_000;
const SEG_BYTES: usize = 1 << 30;
/// File objects remembered for the caller's existence probe (see `LAZY_VERIFY_BUDGET`).
const FILE_BUDGET: usize = 128;

/// Hasher for ids, which are uniform already: the 8 bytes after the first (the first picks the shard).
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() >= 16 {
            self.0 = u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes"));
        }
    }
}

type Shard = HashSet<ObjectId, BuildHasherDefault<IdHasher>>;

/// The ids fetched so far, sharded so the workers rarely meet on a lock.
struct Known {
    shards: Vec<Mutex<Shard>>,
}

impl Known {
    fn new() -> Self {
        Self {
            shards: (0..256).map(|_| Mutex::new(Shard::default())).collect(),
        }
    }
    fn shard(&self, id: &ObjectId) -> &Mutex<Shard> {
        &self.shards[id.as_bytes()[0] as usize]
    }
    /// True if `id` was not known before.
    fn insert(&self, id: ObjectId) -> bool {
        self.shard(&id).lock().unwrap().insert(id)
    }
    fn contains(&self, id: &ObjectId) -> bool {
        self.shard(id).lock().unwrap().contains(id)
    }
}

/// What the workers share.
struct Ctx {
    known: Known,
    files: Mutex<Vec<ObjectId>>,
    stop_batch: AtomicUsize,
}

/// Per-worker state: a direct-mapped memory of dependencies already confirmed fetched (consecutive root trees name
/// mostly the same subtrees), the dependencies not yet confirmed, and the verified objects waiting for the writer.
struct Worker {
    recent: Vec<Option<ObjectId>>,
    unresolved: Vec<ObjectId>,
    compact_at: usize,
    out: Vec<(ObjectId, Vec<u8>)>,
    out_bytes: usize,
}

impl Worker {
    fn new() -> Self {
        Self {
            recent: vec![None; 1 << 14],
            unresolved: Vec::new(),
            compact_at: 1 << 20,
            out: Vec::new(),
            out_bytes: 0,
        }
    }
    fn dep(&mut self, ctx: &Ctx, d: &ObjectId) {
        let b = d.as_bytes();
        let slot = (u16::from_le_bytes([b[8], b[9]]) as usize) & ((1 << 14) - 1);
        if self.recent[slot].as_ref() == Some(d) {
            return;
        }
        if ctx.known.contains(d) {
            self.recent[slot] = Some(*d);
        } else {
            self.unresolved.push(*d);
            // Dependencies in a pack that is read later pile up (many small incremental packs): keep the list distinct.
            if self.unresolved.len() >= self.compact_at {
                self.unresolved.sort_unstable();
                self.unresolved.dedup();
                self.unresolved.retain(|u| !ctx.known.contains(u));
                self.compact_at = (self.unresolved.len() * 2).max(1 << 20);
            }
        }
    }
}

fn bad(id: &ObjectId, e: impl std::fmt::Display) -> PublishError {
    PublishError::Remote(format!("object {id}: {e}"))
}

/// Verify and take one encoded object.
fn take(ctx: &Ctx, w: &mut Worker, enc: &[u8], tx: &mpsc::SyncSender<Vec<(ObjectId, Vec<u8>)>>) -> Result<(), PublishError> {
    let id = id_of_encoded(enc);
    let obj = Object::decode(enc).map_err(|e| bad(&id, e))?;
    for d in crate::protocol::deps_of(&obj, true) {
        w.dep(ctx, &d);
    }
    if let Object::Tree(t) = &obj {
        let mut files = ctx.files.lock().unwrap();
        if files.len() < FILE_BUDGET {
            files.extend(
                t.entries()
                    .iter()
                    .filter(|e| e.mode != vcs_objects::EntryMode::Directory)
                    .map(|e| e.id),
            );
            files.truncate(FILE_BUDGET);
        }
    }
    if ctx.known.insert(id) {
        w.out_bytes += enc.len();
        w.out.push((id, enc.to_vec()));
        if w.out.len() >= 4096 || w.out_bytes >= (8 << 20) {
            w.out_bytes = 0;
            tx.send(std::mem::take(&mut w.out))
                .map_err(|_| PublishError::Remote("bulk fetch writer stopped".into()))?;
        }
    }
    Ok(())
}

/// Process the frames of one chunk. Returns false at the first object that is not a commit or a tree (the end of
/// the metadata prefix).
fn process(
    ctx: &Ctx,
    w: &mut Worker,
    buf: &[u8],
    version: u8,
    tx: &mpsc::SyncSender<Vec<(ObjectId, Vec<u8>)>>,
) -> Result<bool, PublishError> {
    let mut p = 0;
    while p + 4 <= buf.len() {
        let len = u32::from_le_bytes(buf[p..p + 4].try_into().expect("4 bytes")) as usize;
        let Some(raw) = p.checked_add(4 + len).filter(|e| *e <= buf.len()).map(|e| &buf[p + 4..e]) else {
            break;
        };
        if version >= 3 {
            if !block_is_metadata(raw) {
                return Ok(false);
            }
            let Ok(blk) = decode_block(raw) else { return Ok(false) };
            for (_, enc) in blk.iter() {
                take(ctx, w, enc, tx)?;
            }
        } else {
            let Ok(obj) = decode_payload(version, raw) else { return Ok(false) };
            if !matches!(obj.first(), Some(2 | 3)) {
                return Ok(false);
            }
            take(ctx, w, &obj, tx)?;
        }
        p += 4 + len;
    }
    Ok(true)
}

/// Length of the complete `[u32 len][entry]` frames at the start of `buf`.
fn whole_frames(buf: &[u8]) -> usize {
    let mut p = 0;
    while p + 4 <= buf.len() {
        let len = u32::from_le_bytes(buf[p..p + 4].try_into().expect("4 bytes")) as usize;
        match p.checked_add(4 + len) {
            Some(e) if e <= buf.len() => p = e,
            _ => break,
        }
    }
    p
}

/// What a bulk fetch left for the caller.
pub struct BulkMeta {
    /// Ids that fetched objects name but no pack prefix delivered (or roots that were not delivered).
    pub holes: Vec<ObjectId>,
    /// A few file ids named by fetched trees, for the caller's existence probe on the remote.
    pub files: Vec<ObjectId>,
    pub objects: usize,
}

/// Stream the metadata of every pack of the remote into `local`. `None` when the packs do not cover `roots` (the
/// caller then walks the history as before; nothing was written). `local` must not hold any of the objects (a
/// clone): they are written as they come, not deduplicated against the store.
pub fn fetch_metadata_bulk(
    backend: &dyn Backend,
    local: &LocalStore,
    cat: &Arc<PackCatalog>,
    roots: &[ObjectId],
) -> Result<Option<BulkMeta>, PublishError> {
    let loaded: Arc<Loaded> = cat.get(backend)?;
    let packs = loaded.packs();
    if roots.iter().any(|r| !packs.iter().any(|(_, idx)| idx.find(r).is_some())) {
        return Ok(None);
    }
    let io = loaded.view(backend);
    let ctx = Ctx {
        known: Known::new(),
        files: Mutex::new(Vec::new()),
        stop_batch: AtomicUsize::new(usize::MAX),
    };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 64);
    let mut objects = 0usize;
    let mut unresolved: Vec<ObjectId> = Vec::new();
    for (stem, idx) in packs {
        let key = format!("{stem}.pack");
        let (version, end) = (idx.version(), idx.pack_len.saturating_sub(32));
        ctx.stop_batch.store(usize::MAX, Ordering::SeqCst);
        let (chunk_tx, chunk_rx) = mpsc::sync_channel::<(usize, Vec<u8>)>(threads * 2);
        let chunk_rx = Mutex::new(chunk_rx);
        let (obj_tx, obj_rx) = mpsc::sync_channel::<Vec<(ObjectId, Vec<u8>)>>(threads * 2);
        let result: Result<(Vec<ObjectId>, usize), PublishError> = std::thread::scope(|sc| {
            // writer: verified objects -> local packs of bounded size
            let writer = sc.spawn(move || -> Result<usize, PublishError> {
                let mut seg: Vec<(ObjectId, Vec<u8>)> = Vec::new();
                let (mut bytes, mut total) = (0usize, 0usize);
                let flush = |seg: &mut Vec<(ObjectId, Vec<u8>)>, bytes: &mut usize| -> Result<(), PublishError> {
                    if seg.is_empty() {
                        return Ok(());
                    }
                    let n = seg.len();
                    local.add_streamed_pack(n, seg.iter().map(|(id, b)| Ok((*id, std::borrow::Cow::Borrowed(b.as_slice())))))?;
                    seg.clear();
                    *bytes = 0;
                    Ok(())
                };
                for batch in obj_rx {
                    for (id, b) in batch {
                        bytes += b.len();
                        total += 1;
                        seg.push((id, b));
                    }
                    if seg.len() >= SEG_OBJECTS || bytes >= SEG_BYTES {
                        flush(&mut seg, &mut bytes)?;
                    }
                }
                flush(&mut seg, &mut bytes)?;
                Ok(total)
            });
            // workers
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    let (ctx, chunk_rx, obj_tx) = (&ctx, &chunk_rx, obj_tx.clone());
                    sc.spawn(move || -> Result<Vec<ObjectId>, PublishError> {
                        let mut w = Worker::new();
                        let mut failed: Option<PublishError> = None;
                        loop {
                            let next = chunk_rx.lock().unwrap().recv();
                            let Ok((no, buf)) = next else { break };
                            if failed.is_some() || no > ctx.stop_batch.load(Ordering::SeqCst) {
                                continue; // drain so the reader never blocks on a dead pipeline
                            }
                            match process(ctx, &mut w, &buf, version, &obj_tx) {
                                Ok(true) => {}
                                Ok(false) => {
                                    ctx.stop_batch.fetch_min(no, Ordering::SeqCst);
                                }
                                Err(e) => failed = Some(e),
                            }
                        }
                        if let Some(e) = failed {
                            return Err(e);
                        }
                        if !w.out.is_empty() {
                            obj_tx
                                .send(std::mem::take(&mut w.out))
                                .map_err(|_| PublishError::Remote("bulk fetch writer stopped".into()))?;
                        }
                        Ok(w.unresolved)
                    })
                })
                .collect();
            drop(obj_tx);
            // reader (this thread): sequential ranged reads of the metadata prefix
            let mut read_err: Option<PublishError> = None;
            let (mut pos, mut want, mut no) = (PACK_HEADER, CHUNK, 0usize);
            while pos < end && no <= ctx.stop_batch.load(Ordering::SeqCst) {
                let n = want.min(end - pos);
                let buf = match io.get_range(&key, pos, n) {
                    Ok(Some(b)) if b.len() as u64 == n => b,
                    Ok(_) => {
                        read_err = Some(PublishError::Remote(format!("pack {key} changed while it was read (try again)")));
                        break;
                    }
                    Err(e) => {
                        read_err = Some(e.into());
                        break;
                    }
                };
                let used = whole_frames(&buf);
                if used == 0 {
                    if n == end - pos {
                        break; // a damaged tail: the holes it leaves are reported below
                    }
                    want = want.saturating_mul(2); // one entry larger than the request
                    continue;
                }
                want = CHUNK;
                let mut buf = buf;
                buf.truncate(used);
                pos += used as u64;
                if chunk_tx.send((no, buf)).is_err() {
                    break;
                }
                no += 1;
            }
            drop(chunk_tx);
            let mut left = Vec::new();
            let mut err = read_err;
            for w in workers {
                match w.join().expect("bulk fetch worker panicked") {
                    Ok(u) => left.extend(u),
                    Err(e) => err = err.or(Some(e)),
                }
            }
            // a failed writer is the root cause of the workers' "writer stopped"
            let wrote = writer.join().expect("bulk fetch writer panicked")?;
            if let Some(e) = err {
                return Err(e);
            }
            Ok((left, wrote))
        });
        let (left, wrote) = result?;
        objects += wrote;
        unresolved.extend(left);
    }
    // A dependency that was in flight on another worker when it was looked up is known by now.
    unresolved.sort();
    unresolved.dedup();
    let mut holes: Vec<ObjectId> = unresolved.into_iter().filter(|d| !ctx.known.contains(d)).collect();
    holes.extend(roots.iter().filter(|r| !ctx.known.contains(r)).copied());
    holes.sort();
    holes.dedup();
    let files = std::mem::take(&mut *ctx.files.lock().unwrap());
    Ok(Some(BulkMeta { holes, files, objects }))
}
