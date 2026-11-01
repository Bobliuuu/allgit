//! Read one object out of a remote pack with a ranged GET, instead of downloading the pack.

use crate::backend::{Backend, BackendError};
use std::collections::HashMap;
use std::path::Path;
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_store::pack::{block_is_metadata, decode_block, BlockCache, PackIndex};

/// Check the remote pack really ends where its index says: the last 32 bytes (the trailer) must
/// equal the index's pack checksum and nothing may follow. One 33-byte ranged GET. Ranged object
/// reads never touch the trailer, so without this a pack cut short by a half-finished upload,
/// or one with extra bytes, would be accepted.
///
/// `Ok(false)` means the pack file is gone: a compaction or GC removed it after the caller
/// read its index (see [`RACE_ROUNDS`]).
fn check_trailer(b: &dyn Backend, stem: &str, idx: &PackIndex) -> Result<bool, BackendError> {
    let bad = |why: &str| BackendError::Other(format!("pack {stem}: {why}"));
    let start = idx.pack_len.checked_sub(32).ok_or_else(|| bad("index pack length too small"))?;
    match b.get_range(&format!("{stem}.pack"), start, 33)? {
        None => Ok(false),
        Some(t) if t.len() == 32 && t[..] == idx.checksum()[..] => Ok(true),
        Some(t) if t.len() < 32 => Err(bad("pack is shorter than its index says (truncated trailer?)")),
        Some(t) if t.len() > 32 => Err(bad("pack is longer than its index says")),
        Some(_) => Err(bad("pack trailer does not match its index")),
    }
}

/// Fetch `id` from the pack stored at `<stem>.pack` / `<stem>.idx`. Downloads the index
/// (small, 44 bytes per object) and then exactly the object's byte range. The bytes are
/// verified against `id`. `Ok(None)` if the pack, index or object is absent.
pub fn fetch_from_pack(b: &dyn Backend, stem: &str, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
    let Some(idx) = b.get(&format!("{stem}.idx"))? else {
        return Ok(None);
    };
    let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
    let Some((off, len)) = idx.find(id) else { return Ok(None) };
    if !check_trailer(b, stem, &idx)? {
        return Ok(None);
    }
    let Some(bytes) = b.get_range(&format!("{stem}.pack"), off, len as u64)? else {
        return Ok(None);
    };
    if bytes.len() != len as usize {
        return Err(BackendError::Other(format!("pack {stem}: object {id} fails hash verification")));
    }
    let enc = idx
        .decode_entry(id, &bytes)
        .map_err(|e| BackendError::Other(format!("pack {stem}: object {id}: {e}")))?;
    if id_of_encoded(&enc) != *id {
        return Err(BackendError::Other(format!("pack {stem}: object {id} fails hash verification")));
    }
    Ok(Some(enc.into_owned()))
}

/// Key prefix under which a remote keeps packs: `packs/<name>.pack` and `packs/<name>.idx`.
pub const PACK_PREFIX: &str = "packs/";

/// How many times a read re-lists the packs when what it was told about has vanished. Remote
/// compaction (and GC) delete a pack's index and then the pack; the objects are by then in a pack
/// that was uploaded earlier, which a fresh listing shows. One retry covers one compaction that
/// finishes during the read; more cover several in a row.
pub const RACE_ROUNDS: usize = 4;

/// Look for `id` in every pack under [`PACK_PREFIX`]. `Ok(None)` if no pack has it.
/// Cost: one `list`, then per pack one index GET (until found) and one ranged GET. A miss costs
/// one more `list` to rule out a pack set that changed while we looked.
pub fn fetch_from_any_pack(b: &dyn Backend, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
    fetch_from_any_pack_retrying(b, id, RACE_ROUNDS)
}

/// [`fetch_from_any_pack`] with an explicit retry budget (`rounds` = 1: no retry; tests only).
#[doc(hidden)]
pub fn fetch_from_any_pack_retrying(b: &dyn Backend, id: &ObjectId, rounds: usize) -> Result<Option<Vec<u8>>, BackendError> {
    let rounds = rounds.max(1);
    for round in 0..rounds {
        let keys = b.list(PACK_PREFIX)?;
        for key in &keys {
            if let Some(stem) = key.strip_suffix(".idx") {
                if let Some(bytes) = fetch_from_pack(b, stem, id)? {
                    return Ok(Some(bytes));
                }
            }
        }
        if round + 1 == rounds || b.list(PACK_PREFIX)? == keys {
            return Ok(None);
        }
    }
    Ok(None)
}

/// Read every pack index, listing again while some index vanished between the LIST and its GET.
fn load_indexes(b: &dyn Backend, rounds: usize) -> Result<Vec<(String, PackIndex)>, BackendError> {
    let mut packs = Vec::new();
    for round in 0..rounds.max(1) {
        packs.clear();
        let mut vanished = false;
        for key in b.list(PACK_PREFIX)? {
            let Some(stem) = key.strip_suffix(".idx") else { continue };
            let Some(idx) = b.get(&key)? else {
                vanished = true;
                continue;
            };
            packs.push((
                stem.to_owned(),
                PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?,
            ));
        }
        if !vanished || round + 1 == rounds.max(1) {
            break;
        }
    }
    Ok(packs)
}

/// Which of `ids` the remote has, without downloading object bodies. Packs are consulted first:
/// one `list` and one index GET per pack, then no request per packed id (a pack is written
/// atomically, so an id in its index is there). The kind byte of a packed object is NOT read
/// (it would cost a request each) and is reported as 0. Ids in no pack are probed loose with one
/// 1-byte ranged GET each, which yields the kind (the first byte of the encoding). `None` means
/// the remote does not have the id.
pub fn probe_kinds(b: &dyn Backend, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Option<u8>)>, BackendError> {
    if ids.is_empty() {
        return Ok(vec![]); // nothing to probe: do not list and index every pack for it
    }
    let packs: Vec<PackIndex> = load_indexes(b, RACE_ROUNDS)?.into_iter().map(|p| p.1).collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if packs.iter().any(|p| p.find(id).is_some()) {
            out.push((*id, Some(0)));
            continue;
        }
        let h = id.to_hex();
        let key = format!("objects/{}/{}", &h[..2], &h[2..]);
        out.push((
            *id,
            match b.get_range(&key, 0, 1)? {
                Some(v) if v.len() == 1 => Some(v[0]),
                _ => None, // absent, or a zero-length (damaged) object file
            },
        ));
    }
    Ok(out)
}

/// Maximum unwanted bytes read between two wanted ranges to keep them in one request.
const COALESCE_GAP: u64 = 64 * 1024;

/// Fetch several objects at once. Packs are listed once and each index read once; wanted
/// ranges that are close together in a pack are merged into one ranged GET; ids not in any
/// pack are read loose, one GET each. Returned bytes are verified against their ids. Errors if
/// any id is absent everywhere. Compare with calling [`fetch_from_any_pack`] per id, which
/// repeats the list and index reads every time.
pub fn fetch_many(b: &dyn Backend, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Vec<u8>)>, BackendError> {
    fetch_many_retrying(b, ids, RACE_ROUNDS)
}

/// [`fetch_many`] with an explicit retry budget (`rounds` = 1: no retry; tests only).
#[doc(hidden)]
pub fn fetch_many_retrying(b: &dyn Backend, ids: &[ObjectId], rounds: usize) -> Result<Vec<(ObjectId, Vec<u8>)>, BackendError> {
    let mut want: Vec<ObjectId> = ids.to_vec();
    want.sort();
    want.dedup();
    let mut out = Vec::with_capacity(want.len());
    let mut remaining: Vec<ObjectId> = want;
    let rounds = rounds.max(1);
    for round in 0..rounds {
        if remaining.is_empty() {
            break;
        }
        let raced = scan_packs(b, &mut remaining, &mut out)?;
        if !raced || round + 1 == rounds {
            break;
        }
    }
    for id in remaining {
        let h = id.to_hex();
        let key = format!("objects/{}/{}", &h[..2], &h[2..]);
        match b.get(&key)? {
            Some(v) => out.push((id, v.data)),
            None => return Err(BackendError::Other(format!("object {id} missing on remote"))),
        }
    }
    Ok(out)
}

/// One pass over the listed packs, moving what they hold from `remaining` to `out`. Returns true
/// if an index or pack vanished under us (the caller may list again and retry).
fn scan_packs(b: &dyn Backend, remaining: &mut Vec<ObjectId>, out: &mut Vec<(ObjectId, Vec<u8>)>) -> Result<bool, BackendError> {
    let mut raced = false;
    for key in b.list(PACK_PREFIX)? {
        let Some(stem) = key.strip_suffix(".idx") else { continue };
        if remaining.is_empty() {
            break;
        }
        let Some(idx) = b.get(&key)? else {
            raced = true;
            continue;
        };
        let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
        let mut hits: Vec<(u64, u32, ObjectId)> = Vec::new();
        remaining.retain(|id| match idx.find(id) {
            Some((off, len)) => {
                hits.push((off, len, *id));
                false
            }
            None => true,
        });
        hits.sort_by_key(|h| h.0);
        if !hits.is_empty() && !check_trailer(b, stem, &idx)? {
            raced = true;
            remaining.extend(hits.iter().map(|h| h.2));
            continue;
        }
        let pack_key = format!("{stem}.pack");
        let mut blocks = BlockCache::default();
        let mut i = 0;
        while i < hits.len() {
            let start = hits[i].0;
            let mut end = hits[i].0 + hits[i].1 as u64;
            let mut j = i + 1;
            while j < hits.len() && hits[j].0 <= end + COALESCE_GAP {
                end = end.max(hits[j].0 + hits[j].1 as u64);
                j += 1;
            }
            let Some(buf) = b.get_range(&pack_key, start, end - start)? else {
                raced = true;
                remaining.extend(hits[i..].iter().map(|h| h.2));
                break;
            };
            for (off, len, id) in &hits[i..j] {
                let s = (off - start) as usize;
                let e = s + *len as usize;
                let Some(bytes) = buf.get(s..e) else {
                    return Err(BackendError::Other(format!("pack {stem}: short read for {id}")));
                };
                let enc = idx
                    .decode_entry_cached(id, *off, bytes, &mut blocks)
                    .map_err(|e| BackendError::Other(format!("pack {stem}: object {id}: {e}")))?;
                if id_of_encoded(&enc) != *id {
                    return Err(BackendError::Other(format!("pack {stem}: object {id} fails hash verification")));
                }
                out.push((*id, enc.into_owned()));
            }
            i = j;
        }
    }
    Ok(raced)
}

/// First ranged GET when streaming the metadata prefix of a pack, then x4 per request up to the
/// cap. Small first so a pack that is mostly file data costs little over-read.
const PREFIX_FIRST: u64 = 4 * 1024;
const PREFIX_MAX: u64 = 4 * 1024 * 1024;
/// Header of a pack: magic + u32 count; each entry is a u32 length then the encoded object.
const PACK_HEADER: u64 = 12;

/// How much of a pack to read ahead the first time a walk needs something from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadAhead {
    /// Only the object asked for (one ranged GET each).
    None,
    /// The leading commits and trees (packs written by [`crate::Publisher`] put them first),
    /// in a few ranged GETs that grow geometrically and stop at the first file blob.
    Metadata,
    /// The whole pack in one GET.
    Whole,
}

/// Reads objects from a remote that may hold packs, loose objects, or both. The pack list and
/// indexes are fetched once (`1 + packs` requests) and then answer "which pack has it" from
/// memory, so a walk over thousands of objects costs a few large ranged GETs, not LISTs plus
/// one GET per object. A pack the walk never touches is never read. Everything returned is
/// checked against the id that was asked for.
pub struct RemoteSource<'a> {
    b: &'a dyn Backend,
    ahead: ReadAhead,
    packs: Option<Vec<(String, PackIndex)>>,
    read_ahead_done: Vec<bool>,
    /// Pre-read objects keyed by the hash of their bytes, so a lying pack cannot plant data.
    cache: HashMap<ObjectId, Vec<u8>>,
    /// Commits the walk will start from (see [`RemoteSource::with_roots`]).
    roots: Vec<ObjectId>,
    /// The `packs/` listing `packs` was built from, and whether an index vanished while loading.
    listing: Vec<String>,
    vanished: bool,
    rounds: usize,
}

impl<'a> RemoteSource<'a> {
    pub fn new(b: &'a dyn Backend, ahead: ReadAhead) -> Self {
        Self {
            b,
            ahead,
            packs: None,
            read_ahead_done: vec![],
            cache: HashMap::new(),
            roots: vec![],
            listing: vec![],
            vanished: false,
            rounds: RACE_ROUNDS,
        }
    }

    /// Retry budget for packs that vanish mid-read (1 = none; tests only).
    #[doc(hidden)]
    pub fn with_race_rounds(mut self, rounds: usize) -> Self {
        self.rounds = rounds.max(1);
        self
    }

    /// Tell the source which commits the walk starts from. A pack written by [`crate::Publisher`]
    /// stores every object after its dependencies, so a root commit is the LAST object of its
    /// closure's metadata: the end of the root's index entry bounds the metadata the walk can
    /// need from that pack. Metadata read-ahead then fetches exactly `[header, end of last
    /// root)` instead of guessing with 4 KiB, 16 KiB, ... requests that overshoot into file
    /// data. Purely a request-shaping hint: anything the bounded read does not cover is still
    /// fetched by an ordinary ranged GET, and every byte is verified against its id as before.
    pub fn with_roots(mut self, roots: Vec<ObjectId>) -> Self {
        self.roots = roots;
        self
    }

    fn packs(&mut self) -> Result<&[(String, PackIndex)], BackendError> {
        if self.packs.is_none() {
            let mut v = Vec::new();
            self.listing = self.b.list(PACK_PREFIX)?;
            self.vanished = false;
            for key in &self.listing {
                let Some(stem) = key.strip_suffix(".idx") else { continue };
                let Some(idx) = self.b.get(key)? else {
                    self.vanished = true;
                    continue;
                };
                let idx = PackIndex::parse(&idx.data, Path::new(stem)).map_err(|e| BackendError::Other(e.to_string()))?;
                v.push((stem.to_owned(), idx));
            }
            self.read_ahead_done = vec![false; v.len()];
            self.packs = Some(v);
        }
        Ok(self.packs.as_deref().unwrap_or(&[]))
    }

    fn read_ahead(&mut self, i: usize, wanted: &ObjectId) -> Result<(), BackendError> {
        let (stem, pack_len, version) = {
            let (s, idx) = &self.packs.as_ref().expect("packs loaded")[i];
            (s.clone(), idx.pack_len, idx.version())
        };
        let key = format!("{stem}.pack");
        let end = pack_len.saturating_sub(32);
        if self.ahead == ReadAhead::Whole {
            if let Some(v) = self.b.get(&key)? {
                let to = (end as usize).min(v.data.len());
                absorb(&mut self.cache, &v.data, PACK_HEADER as usize, to, false, version, &mut Vec::new());
            }
            return Ok(());
        }
        // Known bound. A pack written by `Publisher` holds its commits first and then its trees,
        // each group dependencies-first. So the walk's root commits end the commit group, and the
        // trees those commits (and the trees below them) name end at the last of them: read
        // exactly up to the furthest of those ends, then stop. No guessed over-read into file data.
        // In metadata mode the walk only asks for commits and trees, so the id that first touched
        // this pack is itself a root of the metadata needed from it (a parent commit in an older pack).
        let mut starts = self.roots.clone();
        starts.push(*wanted);
        let mut bound = self.packs.as_ref().expect("packs loaded")[i]
            .1
            .known_end(&starts)
            .map(|e| e.min(end));
        if bound.is_some() {
            let mut pos = PACK_HEADER;
            let mut pending: Vec<ObjectId> = Vec::new();
            while let Some(target) = bound.filter(|t| *t > pos) {
                let Some(buf) = self.b.get_range(&key, pos, (target - pos).min(PREFIX_MAX))? else {
                    break;
                };
                let mut fresh = Vec::new();
                let (used, stop) = absorb(&mut self.cache, &buf, 0, buf.len(), true, version, &mut fresh);
                pos += used as u64;
                if stop || used == 0 {
                    break;
                }
                // metadata the parsed objects name but we have not read yet, if this pack holds it
                let idx = &self.packs.as_ref().expect("packs loaded")[i].1;
                for id in &fresh {
                    if let Some(bytes) = self.cache.get(id) {
                        pending.extend(metadata_deps(bytes));
                    }
                }
                pending.retain(|d| !self.cache.contains_key(d));
                let need = idx.known_end(&pending).map(|e| e.min(end));
                bound = match (bound, need) {
                    (Some(b), Some(n)) => Some(b.max(n)),
                    (b, n) => b.or(n),
                };
            }
            return Ok(());
        }
        let (mut pos, mut want) = (PACK_HEADER, PREFIX_FIRST);
        while pos < end {
            let Some(buf) = self.b.get_range(&key, pos, want.min(end - pos))? else {
                break;
            };
            let (used, stop) = absorb(&mut self.cache, &buf, 0, buf.len(), true, version, &mut Vec::new());
            pos += used as u64;
            // stop: first non-metadata entry; used == 0: one entry larger than the request
            if stop || used == 0 {
                break;
            }
            want = (want * 4).min(PREFIX_MAX);
        }
        Ok(())
    }

    /// The verified encoded bytes of `id`: from read-ahead, else one ranged GET from the pack
    /// that indexes it, else one GET of the loose object. `Ok(None)` if the remote lacks it.
    /// If a pack or index vanished since the listing (a compaction replaced it), the pack list is
    /// read again and the lookup retried; the objects are in the pack that replaced it.
    pub fn fetch(&mut self, id: &ObjectId) -> Result<Option<Vec<u8>>, BackendError> {
        for round in 0..self.rounds {
            let last = round + 1 == self.rounds;
            match self.fetch_once(id)? {
                Fetched::Found(v) => return Ok(Some(v)),
                Fetched::Raced if last => {
                    return Err(BackendError::Other(format!(
                        "object {id}: pack file missing (the remote's packs kept changing; try again)"
                    )))
                }
                Fetched::Raced => {}
                Fetched::Absent => {
                    // Not anywhere. Before saying so, rule out a pack set that changed under us.
                    if last || (!self.vanished && self.b.list(PACK_PREFIX)? == self.listing) {
                        return Ok(None);
                    }
                }
            }
            self.packs = None;
        }
        Ok(None)
    }

    fn fetch_once(&mut self, id: &ObjectId) -> Result<Fetched, BackendError> {
        if let Some(v) = self.cache.remove(id) {
            return Ok(Fetched::Found(v));
        }
        let hit = self
            .packs()?
            .iter()
            .enumerate()
            .find_map(|(i, (stem, idx))| idx.find(id).map(|(o, l)| (i, format!("{stem}.pack"), o, l, idx.version())));
        if let Some((i, key, off, len, version)) = hit {
            if self.ahead != ReadAhead::None && !self.read_ahead_done[i] {
                self.read_ahead_done[i] = true;
                self.read_ahead(i, id)?;
                if let Some(v) = self.cache.remove(id) {
                    return Ok(Fetched::Found(v));
                }
            }
            let Some(bytes) = self.b.get_range(&key, off, len as u64)? else {
                return Ok(Fetched::Raced);
            };
            if bytes.len() != len as usize {
                return Err(BackendError::Other(format!("object {id} fails hash verification")));
            }
            if version >= 3 {
                // a block holds many objects: keep them all, the walk will ask for its neighbours
                let blk = decode_block(&bytes).map_err(|e| BackendError::Other(format!("pack {key}: object {id}: {e}")))?;
                for (_, enc) in blk.iter() {
                    self.cache.insert(id_of_encoded(enc), enc.to_vec());
                }
                return match self.cache.remove(id) {
                    Some(v) => Ok(Fetched::Found(v)),
                    None => Err(BackendError::Other(format!("pack {key}: object {id} fails hash verification"))),
                };
            }
            let enc = self.packs.as_ref().expect("packs loaded")[i]
                .1
                .decode_entry(id, &bytes)
                .map_err(|e| BackendError::Other(format!("pack {key}: object {id}: {e}")))?;
            if id_of_encoded(&enc) != *id {
                return Err(BackendError::Other(format!("object {id} fails hash verification")));
            }
            return Ok(Fetched::Found(enc.into_owned()));
        }
        let h = id.to_hex();
        Ok(match self.b.get(&format!("objects/{}/{}", &h[..2], &h[2..]))? {
            Some(v) => Fetched::Found(v.data),
            None => Fetched::Absent,
        })
    }
}

enum Fetched {
    Found(Vec<u8>),
    /// A pack the index named is gone.
    Raced,
    Absent,
}

/// Parse `[u32 len][entry]` frames (entry decoded per pack `version`) from `buf[from..to]` into
/// `cache`. With `meta_only`, stop at the first object that is not a tree or commit. Returns
/// (bytes consumed from `from`, stopped). An entry that fails to decode ends the scan; the
/// normal ranged read then reports the error.
fn absorb(
    cache: &mut HashMap<ObjectId, Vec<u8>>,
    buf: &[u8],
    from: usize,
    to: usize,
    meta_only: bool,
    version: u8,
    fresh: &mut Vec<ObjectId>,
) -> (usize, bool) {
    let mut p = from;
    while p + 4 <= to {
        let len = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap()) as usize;
        let Some(raw) = p.checked_add(4 + len).filter(|e| *e <= to).map(|e| &buf[p + 4..e]) else {
            break;
        };
        if version >= 3 {
            if meta_only && !block_is_metadata(raw) {
                return (p - from, true);
            }
            let Ok(blk) = decode_block(raw) else { break };
            for (_, enc) in blk.iter() {
                let id = id_of_encoded(enc);
                cache.insert(id, enc.to_vec());
                fresh.push(id);
            }
            p += 4 + len;
            continue;
        }
        let Ok(obj) = vcs_store::pack::decode_payload(version, raw) else {
            break;
        };
        let obj = &obj[..];
        // kinds: 2 = tree, 3 = commit (vcs_objects::Kind)
        if meta_only && !matches!(obj.first(), Some(2 | 3)) {
            return (p - from, true);
        }
        let id = id_of_encoded(obj);
        cache.insert(id, obj.to_vec());
        fresh.push(id);
        p += 4 + len;
    }
    (p - from, false)
}

/// Commits and trees a commit or tree names (parents, root tree, subdirectories). Used only to
/// size read-ahead; an object that does not decode names nothing.
fn metadata_deps(encoded: &[u8]) -> Vec<ObjectId> {
    match vcs_objects::Object::decode(encoded) {
        Ok(vcs_objects::Object::Commit(c)) => c.parents.iter().copied().chain(std::iter::once(c.root)).collect(),
        Ok(vcs_objects::Object::Tree(t)) => t
            .entries()
            .iter()
            .filter(|e| e.mode == vcs_objects::EntryMode::Directory)
            .map(|e| e.id)
            .collect(),
        _ => vec![],
    }
}

/// Upload a local pack (and its index) as `packs/<name>`. The pack goes first, so a reader
/// never sees an index without its pack. Create-only: an existing name is left unchanged.
pub fn upload_pack(b: &dyn Backend, name: &str, pack_path: &Path) -> Result<(), BackendError> {
    let other = |e: std::io::Error| BackendError::Other(e.to_string());
    let pack = std::fs::read(pack_path).map_err(other)?;
    let idx = std::fs::read(pack_path.with_extension("idx")).map_err(other)?;
    // Remote readers fetch single entries by range and cannot resolve delta bases: v4 packs stay local.
    if idx.starts_with(vcs_store::pack::IDX_MAGIC_V4) {
        return Err(BackendError::Other(
            "v4 (delta) packs are local-only; repack as v2 before uploading".into(),
        ));
    }
    b.put_if_absent(&format!("{PACK_PREFIX}{name}.pack"), &pack)?;
    b.put_if_absent(&format!("{PACK_PREFIX}{name}.idx"), &idx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemBackend;
    use vcs_objects::Object;
    use vcs_store::pack::write_pack;

    /// Incompressible bytes (xorshift), so pack size reflects object count in range tests.
    fn noise(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn reads_one_object_by_range_not_whole_pack() {
        let d = tempfile::tempdir().unwrap();
        let objs: Vec<_> = (0..50)
            .map(|i| {
                let enc = Object::Blob(noise(i as u64, 10_000)).encode();
                (id_of_encoded(&enc), enc)
            })
            .collect();
        let want = objs[17].clone();
        let pack = write_pack(&d.path().join("pack"), d.path(), objs).unwrap();
        let idx_path = pack.pack_path.with_extension("idx");
        let b = MemBackend::new();
        let pack_bytes = std::fs::read(&pack.pack_path).unwrap();
        b.put_if_absent("p/x.pack", &pack_bytes).unwrap();
        b.put_if_absent("p/x.idx", &std::fs::read(&idx_path).unwrap()).unwrap();
        let before = b.bytes_read();
        let got = fetch_from_pack(&b, "p/x", &want.0).unwrap().unwrap();
        assert_eq!(got, want.1);
        let used = b.bytes_read() - before;
        assert!(
            used < pack_bytes.len() as u64 / 10,
            "read {used} of {} pack bytes",
            pack_bytes.len()
        );
        assert_eq!(fetch_from_pack(&b, "p/x", &ObjectId::from_bytes([9; 32])).unwrap(), None);
        assert_eq!(fetch_from_pack(&b, "p/none", &want.0).unwrap(), None);
        // corrupt the pack on the remote: the ranged bytes must fail verification
        let mut bad = pack_bytes.clone();
        for x in bad.iter_mut().skip(12) {
            *x ^= 0xFF;
        }
        b.tamper("p/x.pack", bad);
        assert!(fetch_from_pack(&b, "p/x", &want.0).is_err());
    }
}
