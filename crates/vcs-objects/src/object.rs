use crate::codec::{DecodeError, Reader, Writer};
use crate::id::{ChangeId, ObjectId};
use crate::names;
use std::collections::BTreeMap;

/// Version of the object encoding. Part of the hashed bytes: a format change yields new ids.
/// v3 added `EntryMode::Symlink` (tree entry mode 4); every id changed with the bump.
/// v2 added `Kind::Chunked` (chunk manifests for large blobs); every id changed with the bump.
pub const FORMAT_VERSION: u8 = 3;
const HASH_CONTEXT: &str = "all-git 2026 object identity v3";
/// Files of at least this many bytes are stored as a chunk manifest plus chunk blobs; smaller
/// ones as one blob. Fixed by the format: changing it (or the chunk parameters) is a version bump.
pub const CHUNK_THRESHOLD: usize = 1 << 20;
/// Chunking parameters fixed by format v2 and v3.
pub const CHUNK_MIN: usize = 4 << 10;
pub const CHUNK_AVG: usize = 16 << 10;
pub const CHUNK_MAX: usize = 64 << 10;
const MAX_MESSAGE: usize = 1 << 20;
const MAX_PARENTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Blob = 1,
    Tree = 2,
    Commit = 3,
    Snapshot = 4,
    Transaction = 5,
    Operation = 6,
    Chunked = 7,
    Tag = 8,
}

impl Kind {
    fn from_u8(b: u8) -> Result<Self, DecodeError> {
        Ok(match b {
            1 => Kind::Blob,
            2 => Kind::Tree,
            3 => Kind::Commit,
            4 => Kind::Snapshot,
            5 => Kind::Transaction,
            6 => Kind::Operation,
            7 => Kind::Chunked,
            8 => Kind::Tag,
            _ => return Err(DecodeError::Kind(b)),
        })
    }
}

/// Identity = BLAKE3-derive_key(context)( kind || version || payload ).
/// Domain separation: context string + kind byte; kinds can never collide.
pub fn id_of_parts(kind: Kind, payload: &[u8]) -> ObjectId {
    let mut h = blake3::Hasher::new_derive_key(HASH_CONTEXT);
    h.update(&[kind as u8, FORMAT_VERSION]);
    h.update(payload);
    ObjectId::from_bytes(*h.finalize().as_bytes())
}

/// Identity of an encoded object (`kind||version||payload`).
pub fn id_of_encoded(encoded: &[u8]) -> ObjectId {
    let mut h = blake3::Hasher::new_derive_key(HASH_CONTEXT);
    h.update(encoded);
    ObjectId::from_bytes(*h.finalize().as_bytes())
}

/// Blob id without building an `Object` (avoids copying large content).
pub fn blob_id(data: &[u8]) -> ObjectId {
    id_of_parts(Kind::Blob, data)
}

/// Id a file with these bytes has in a tree: a plain blob below [`CHUNK_THRESHOLD`], otherwise
/// the id of its chunk manifest. One canonical id per content.
pub fn content_id(data: &[u8]) -> ObjectId {
    match ChunkedBlob::plan(data) {
        Some(m) => Object::Chunked(m).id(),
        None => blob_id(data),
    }
}

/// One chunk of a [`ChunkedBlob`]: the id of a plain Blob and its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    pub id: ObjectId,
    pub len: u32,
}

/// Manifest of a large file: ordered chunks whose concatenation is the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkedBlob {
    pub total_len: u64,
    pub chunks: Vec<ChunkRef>,
}

impl ChunkedBlob {
    /// Deterministic manifest for `data`, or `None` when it is below [`CHUNK_THRESHOLD`].
    pub fn plan(data: &[u8]) -> Option<ChunkedBlob> {
        if data.len() < CHUNK_THRESHOLD {
            return None;
        }
        let mut start = 0;
        let chunks = crate::chunk::chunk_ends(data, crate::chunk::ChunkParams::new(CHUNK_MIN, CHUNK_AVG, CHUNK_MAX))
            .into_iter()
            .map(|end| {
                let r = ChunkRef {
                    id: blob_id(&data[start..end]),
                    len: (end - start) as u32,
                };
                start = end;
                r
            })
            .collect();
        Some(ChunkedBlob {
            total_len: data.len() as u64,
            chunks,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum EntryMode {
    File = 1,
    Executable = 2,
    Directory = 3,
    /// Symbolic link: the entry id names a Blob holding the link target bytes.
    Symlink = 4,
}

impl EntryMode {
    fn from_u8(b: u8) -> Result<Self, DecodeError> {
        match b {
            1 => Ok(Self::File),
            2 => Ok(Self::Executable),
            3 => Ok(Self::Directory),
            4 => Ok(Self::Symlink),
            _ => Err(DecodeError::Invalid("tree entry mode")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub mode: EntryMode,
    pub id: ObjectId,
}

/// Directory object. Invariant: entries strictly sorted by name bytes, names valid.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self, DecodeError> {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for w in entries.windows(2) {
            if w[0].name == w[1].name {
                return Err(DecodeError::Invalid("duplicate tree entry"));
            }
        }
        for e in &entries {
            names::validate_component(&e.name).map_err(|_| DecodeError::Invalid("tree entry name"))?;
        }
        Ok(Self { entries })
    }
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since Unix epoch.
    pub timestamp: u64,
}

impl Signature {
    fn encode(&self, w: &mut Writer) {
        w.str(&self.name).str(&self.email).varint(self.timestamp);
    }
    fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(Self {
            name: r.str(256)?,
            email: r.str(256)?,
            timestamp: r.varint()?,
        })
    }
}

/// A version of one zone's content. `parents` are commits of the **same zone**
/// (invariant checked by graph validation, not by the codec).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub zone: String,
    pub parents: Vec<ObjectId>,
    /// Tree rooted at the zone's path (paths inside are zone-relative).
    pub root: ObjectId,
    pub change_id: ChangeId,
    pub author: Signature,
    pub message: String,
}

/// Immutable pin of a consistent set of zone versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub zones: BTreeMap<String, ObjectId>,
    pub author: Signature,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefUpdate {
    pub zone: String,
    pub branch: String,
    pub old: Option<ObjectId>,
    pub new: ObjectId,
}

/// Immutable record of one atomic publication that advanced one or more zone branches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    /// Sorted by (zone, branch); at most one update per (zone, branch).
    pub updates: Vec<RefUpdate>,
    pub author: Signature,
    pub message: String,
    /// Lock operations applied atomically with the updates (docs/locks.md). Encoded only when
    /// non-empty, so transactions without locks keep their original bytes and ids.
    pub lock_ops: Vec<LockOp>,
}

/// One lock operation carried inside a [`Transaction`]. `path` is relative to the zone root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOp {
    /// Take (or idempotently re-take / extend) the lock for the transaction's principal.
    Acquire {
        zone: String,
        path: String,
        lease_ms: u64,
        reason: String,
    },
    /// Extend a held lock; only the holder, only with the current token.
    Renew {
        zone: String,
        path: String,
        token: u64,
        lease_ms: u64,
    },
    /// Drop a lock; the holder with the current token, or anyone once expired.
    Release { zone: String, path: String, token: u64 },
    /// Administrative removal at any time (unrestricted until authn/authz exists).
    Break { zone: String, path: String },
    /// Names the fencing token the publisher believes it holds. Rejected if not current.
    Present { zone: String, path: String, token: u64 },
}

impl LockOp {
    pub fn zone(&self) -> &str {
        match self {
            LockOp::Acquire { zone, .. }
            | LockOp::Renew { zone, .. }
            | LockOp::Release { zone, .. }
            | LockOp::Break { zone, .. }
            | LockOp::Present { zone, .. } => zone,
        }
    }
    pub fn path(&self) -> &str {
        match self {
            LockOp::Acquire { path, .. }
            | LockOp::Renew { path, .. }
            | LockOp::Release { path, .. }
            | LockOp::Break { path, .. }
            | LockOp::Present { path, .. } => path,
        }
    }
    fn encode(&self, w: &mut Writer) {
        match self {
            LockOp::Acquire {
                zone,
                path,
                lease_ms,
                reason,
            } => {
                w.u8(1).str(zone).str(path).varint(*lease_ms).str(reason);
            }
            LockOp::Renew {
                zone,
                path,
                token,
                lease_ms,
            } => {
                w.u8(2).str(zone).str(path).varint(*token).varint(*lease_ms);
            }
            LockOp::Release { zone, path, token } => {
                w.u8(3).str(zone).str(path).varint(*token);
            }
            LockOp::Break { zone, path } => {
                w.u8(4).str(zone).str(path);
            }
            LockOp::Present { zone, path, token } => {
                w.u8(5).str(zone).str(path).varint(*token);
            }
        }
    }
    fn decode(r: &mut Reader) -> Result<Self, DecodeError> {
        let tag = r.u8()?;
        let zone = r.str(64)?;
        names::validate_zone_name(&zone).map_err(|_| DecodeError::Invalid("zone name"))?;
        let path = r.str(4096)?;
        names::validate_path(&path).map_err(|_| DecodeError::Invalid("lock path"))?;
        Ok(match tag {
            1 => LockOp::Acquire {
                zone,
                path,
                lease_ms: r.varint()?,
                reason: r.str(1024)?,
            },
            2 => LockOp::Renew {
                zone,
                path,
                token: r.varint()?,
                lease_ms: r.varint()?,
            },
            3 => LockOp::Release {
                zone,
                path,
                token: r.varint()?,
            },
            4 => LockOp::Break { zone, path },
            5 => LockOp::Present {
                zone,
                path,
                token: r.varint()?,
            },
            _ => return Err(DecodeError::Invalid("lock op tag")),
        })
    }
}

/// Local state captured by an [`Operation`]: every local branch head and the workspace
/// subscriptions (zone -> checked-out branch). Remote-tracking refs are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct OpView {
    /// (zone, branch) -> commit. Sorted by key (BTreeMap order == canonical order).
    pub branches: BTreeMap<(String, String), ObjectId>,
    /// zone -> checked-out branch, for subscribed zones.
    pub workspace: BTreeMap<String, String>,
    /// (zone, tag name) -> tag target. `None` in operations recorded before tags were part of the
    /// view (restoring those leaves tags alone); `Some` otherwise, even when empty.
    pub tags: Option<BTreeMap<(String, String), ObjectId>>,
}

/// One entry of the operation log (distinct from the commit DAG): the local view after a
/// mutating repository operation. `parents` are the previous operation(s), normally one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    pub parents: Vec<ObjectId>,
    pub view: OpView,
    pub description: String,
    pub author: Signature,
}

/// Maximum length of an annotated tag's opaque signature field.
pub const MAX_TAG_SIGNATURE: usize = 4096;

/// Annotated tag: a named, messaged pointer at one commit of one zone. `signature` is an opaque
/// byte string (empty = unsigned); the object model stores it but does not verify it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatedTag {
    pub zone: String,
    pub name: String,
    pub target: ObjectId,
    pub tagger: Signature,
    pub message: String,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    Blob(Vec<u8>),
    Tree(Tree),
    Commit(Commit),
    Snapshot(Snapshot),
    Transaction(Transaction),
    Operation(Operation),
    Chunked(ChunkedBlob),
    Tag(AnnotatedTag),
}

impl Object {
    pub fn kind(&self) -> Kind {
        match self {
            Object::Blob(_) => Kind::Blob,
            Object::Tree(_) => Kind::Tree,
            Object::Commit(_) => Kind::Commit,
            Object::Snapshot(_) => Kind::Snapshot,
            Object::Transaction(_) => Kind::Transaction,
            Object::Operation(_) => Kind::Operation,
            Object::Chunked(_) => Kind::Chunked,
            Object::Tag(_) => Kind::Tag,
        }
    }

    /// Canonical bytes: `kind || version || payload`.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.kind() as u8).u8(FORMAT_VERSION);
        match self {
            Object::Blob(b) => {
                w.raw(b);
            }
            Object::Chunked(m) => {
                w.varint(m.total_len).varint(m.chunks.len() as u64);
                for c in &m.chunks {
                    w.id(&c.id).varint(c.len as u64);
                }
            }
            Object::Tree(t) => {
                w.varint(t.entries.len() as u64);
                for e in &t.entries {
                    w.str(&e.name).u8(e.mode as u8).id(&e.id);
                }
            }
            Object::Commit(c) => {
                w.str(&c.zone).varint(c.parents.len() as u64);
                for p in &c.parents {
                    w.id(p);
                }
                w.id(&c.root).raw(c.change_id.as_bytes());
                c.author.encode(&mut w);
                w.str(&c.message);
            }
            Object::Snapshot(s) => {
                w.varint(s.zones.len() as u64);
                for (a, id) in &s.zones {
                    w.str(a).id(id);
                }
                s.author.encode(&mut w);
                w.str(&s.message);
            }
            Object::Transaction(t) => {
                w.varint(t.updates.len() as u64);
                for u in &t.updates {
                    w.str(&u.zone).str(&u.branch);
                    match &u.old {
                        None => {
                            w.u8(0);
                        }
                        Some(o) => {
                            w.u8(1).id(o);
                        }
                    }
                    w.id(&u.new);
                }
                t.author.encode(&mut w);
                w.str(&t.message);
                if !t.lock_ops.is_empty() {
                    w.varint(t.lock_ops.len() as u64);
                    for op in &t.lock_ops {
                        op.encode(&mut w);
                    }
                }
            }
            Object::Operation(o) => {
                w.varint(o.parents.len() as u64);
                for p in &o.parents {
                    w.id(p);
                }
                w.varint(o.view.branches.len() as u64);
                for ((a, b), id) in &o.view.branches {
                    w.str(a).str(b).id(id);
                }
                w.varint(o.view.workspace.len() as u64);
                for (a, b) in &o.view.workspace {
                    w.str(a).str(b);
                }
                o.author.encode(&mut w);
                w.str(&o.description);
                // appended last so operations without a tag view keep their exact bytes
                if let Some(tags) = &o.view.tags {
                    w.varint(tags.len() as u64);
                    for ((z, n), id) in tags {
                        w.str(z).str(n).id(id);
                    }
                }
            }
            Object::Tag(t) => {
                w.str(&t.zone).str(&t.name).id(&t.target);
                t.tagger.encode(&mut w);
                w.str(&t.message).bytes(&t.signature);
            }
        }
        w.finish()
    }

    pub fn id(&self) -> ObjectId {
        id_of_encoded(&self.encode())
    }

    /// Strict decode of untrusted bytes. Rejects non-canonical forms.
    pub fn decode(bytes: &[u8]) -> Result<Object, DecodeError> {
        let mut r = Reader::new(bytes);
        let kind = Kind::from_u8(r.u8()?)?;
        let ver = r.u8()?;
        if ver != FORMAT_VERSION {
            return Err(DecodeError::Version(ver));
        }
        let obj = match kind {
            Kind::Blob => {
                let n = r.remaining();
                Object::Blob(r.raw(n)?.to_vec())
            }
            Kind::Chunked => {
                let total_len = r.varint()?;
                let n = r.count(32 + 1)?;
                if n < 2 || total_len < CHUNK_THRESHOLD as u64 {
                    return Err(DecodeError::Invalid("chunked blob below threshold"));
                }
                let mut chunks = Vec::with_capacity(n);
                let mut sum = 0u64;
                for _ in 0..n {
                    let id = r.id()?;
                    let len = r.varint()?;
                    if len == 0 || len > CHUNK_MAX as u64 {
                        return Err(DecodeError::Invalid("chunk length"));
                    }
                    sum += len;
                    chunks.push(ChunkRef { id, len: len as u32 });
                }
                if sum != total_len {
                    return Err(DecodeError::Invalid("chunk lengths do not sum to total"));
                }
                Object::Chunked(ChunkedBlob { total_len, chunks })
            }
            Kind::Tree => {
                let n = r.count(1 + 1 + 1 + 32)?;
                let mut entries: Vec<TreeEntry> = Vec::with_capacity(n);
                for _ in 0..n {
                    let name = r.str(names::MAX_NAME)?;
                    let mode = EntryMode::from_u8(r.u8()?)?;
                    let id = r.id()?;
                    entries.push(TreeEntry { name, mode, id });
                }
                for w in entries.windows(2) {
                    if w[0].name >= w[1].name {
                        return Err(DecodeError::Invalid("tree entries not strictly sorted"));
                    }
                }
                for e in &entries {
                    names::validate_component(&e.name).map_err(|_| DecodeError::Invalid("tree entry name"))?;
                }
                Object::Tree(Tree { entries })
            }
            Kind::Commit => {
                let zone = r.str(64)?;
                names::validate_zone_name(&zone).map_err(|_| DecodeError::Invalid("zone name"))?;
                let np = r.count(32)?;
                if np > MAX_PARENTS {
                    return Err(DecodeError::Limit("too many parents"));
                }
                let mut parents = Vec::with_capacity(np);
                for _ in 0..np {
                    parents.push(r.id()?);
                }
                let root = r.id()?;
                let mut cid = [0u8; 16];
                cid.copy_from_slice(r.raw(16)?);
                let author = Signature::decode(&mut r)?;
                let message = r.str(MAX_MESSAGE)?;
                Object::Commit(Commit {
                    zone,
                    parents,
                    root,
                    change_id: ChangeId::from_bytes(cid),
                    author,
                    message,
                })
            }
            Kind::Snapshot => {
                let n = r.count(2 + 32)?;
                let mut zones = BTreeMap::new();
                let mut prev: Option<String> = None;
                for _ in 0..n {
                    let a = r.str(64)?;
                    names::validate_zone_name(&a).map_err(|_| DecodeError::Invalid("zone name"))?;
                    if prev.as_ref().is_some_and(|p| *p >= a) {
                        return Err(DecodeError::Invalid("snapshot zones not strictly sorted"));
                    }
                    prev = Some(a.clone());
                    zones.insert(a, r.id()?);
                }
                let author = Signature::decode(&mut r)?;
                let message = r.str(MAX_MESSAGE)?;
                Object::Snapshot(Snapshot { zones, author, message })
            }
            Kind::Transaction => {
                let n = r.count(4 + 32)?;
                let mut updates: Vec<RefUpdate> = Vec::with_capacity(n);
                for _ in 0..n {
                    let zone = r.str(64)?;
                    names::validate_zone_name(&zone).map_err(|_| DecodeError::Invalid("zone name"))?;
                    let branch = r.str(200)?;
                    names::validate_branch_name(&branch).map_err(|_| DecodeError::Invalid("branch name"))?;
                    let old = match r.u8()? {
                        0 => None,
                        1 => Some(r.id()?),
                        _ => return Err(DecodeError::Invalid("option tag")),
                    };
                    let new = r.id()?;
                    updates.push(RefUpdate { zone, branch, old, new });
                }
                for w in updates.windows(2) {
                    if (&w[0].zone, &w[0].branch) >= (&w[1].zone, &w[1].branch) {
                        return Err(DecodeError::Invalid("transaction updates not strictly sorted"));
                    }
                }
                let author = Signature::decode(&mut r)?;
                let message = r.str(MAX_MESSAGE)?;
                let mut lock_ops = Vec::new();
                if r.remaining() > 0 {
                    let n = r.count(4)?;
                    if n == 0 {
                        return Err(DecodeError::Invalid("empty lock op list"));
                    }
                    for _ in 0..n {
                        lock_ops.push(LockOp::decode(&mut r)?);
                    }
                }
                Object::Transaction(Transaction {
                    updates,
                    author,
                    message,
                    lock_ops,
                })
            }
            Kind::Operation => {
                let np = r.count(32)?;
                if np > MAX_PARENTS {
                    return Err(DecodeError::Limit("too many parents"));
                }
                let mut parents = Vec::with_capacity(np);
                for _ in 0..np {
                    parents.push(r.id()?);
                }
                let nb = r.count(2 + 2 + 32)?;
                let mut branches = BTreeMap::new();
                let mut prev: Option<(String, String)> = None;
                for _ in 0..nb {
                    let a = r.str(64)?;
                    names::validate_zone_name(&a).map_err(|_| DecodeError::Invalid("zone name"))?;
                    let b = r.str(200)?;
                    names::validate_branch_name(&b).map_err(|_| DecodeError::Invalid("branch name"))?;
                    let key = (a, b);
                    if prev.as_ref().is_some_and(|p| *p >= key) {
                        return Err(DecodeError::Invalid("operation branches not strictly sorted"));
                    }
                    prev = Some(key.clone());
                    branches.insert(key, r.id()?);
                }
                let nw = r.count(2 + 2)?;
                let mut workspace = BTreeMap::new();
                let mut prev: Option<String> = None;
                for _ in 0..nw {
                    let a = r.str(64)?;
                    names::validate_zone_name(&a).map_err(|_| DecodeError::Invalid("zone name"))?;
                    let b = r.str(200)?;
                    names::validate_branch_name(&b).map_err(|_| DecodeError::Invalid("branch name"))?;
                    if prev.as_ref().is_some_and(|p| *p >= a) {
                        return Err(DecodeError::Invalid("operation workspace not strictly sorted"));
                    }
                    prev = Some(a.clone());
                    workspace.insert(a, b);
                }
                let author = Signature::decode(&mut r)?;
                let description = r.str(MAX_MESSAGE)?;
                let tags = if r.remaining() > 0 {
                    let nt = r.count(2 + 2 + 32)?;
                    let mut tags = BTreeMap::new();
                    let mut prev: Option<(String, String)> = None;
                    for _ in 0..nt {
                        let z = r.str(64)?;
                        names::validate_zone_name(&z).map_err(|_| DecodeError::Invalid("zone name"))?;
                        let n = r.str(200)?;
                        names::validate_branch_name(&n).map_err(|_| DecodeError::Invalid("tag name"))?;
                        let key = (z, n);
                        if prev.as_ref().is_some_and(|p| *p >= key) {
                            return Err(DecodeError::Invalid("operation tags not strictly sorted"));
                        }
                        prev = Some(key.clone());
                        tags.insert(key, r.id()?);
                    }
                    Some(tags)
                } else {
                    None
                };
                Object::Operation(Operation {
                    parents,
                    view: OpView { branches, workspace, tags },
                    description,
                    author,
                })
            }
            Kind::Tag => {
                let zone = r.str(64)?;
                names::validate_zone_name(&zone).map_err(|_| DecodeError::Invalid("zone name"))?;
                let name = r.str(200)?;
                names::validate_branch_name(&name).map_err(|_| DecodeError::Invalid("tag name"))?;
                let target = r.id()?;
                let tagger = Signature::decode(&mut r)?;
                let message = r.str(MAX_MESSAGE)?;
                let signature = r.bytes(MAX_TAG_SIGNATURE)?.to_vec();
                Object::Tag(AnnotatedTag {
                    zone,
                    name,
                    target,
                    tagger,
                    message,
                    signature,
                })
            }
        };
        r.finish()?;
        Ok(obj)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sig() -> Signature {
        Signature {
            name: "A".into(),
            email: "a@x".into(),
            timestamp: 1_700_000_000,
        }
    }
    fn oid(n: u8) -> ObjectId {
        ObjectId::from_bytes([n; 32])
    }

    #[test]
    fn annotated_tag_roundtrips_and_rejects_bad_names() {
        let t = Object::Tag(AnnotatedTag {
            zone: "engine".into(),
            name: "v1.0".into(),
            target: oid(7),
            tagger: sig(),
            message: "release\n".into(),
            signature: vec![1, 2, 3],
        });
        assert_eq!(Object::decode(&t.encode()).unwrap(), t);
        assert_eq!(t.kind(), Kind::Tag);
        let Object::Tag(mut bad) = t else { unreachable!() };
        bad.name = "a..b".into();
        assert!(Object::decode(&Object::Tag(bad).encode()).is_err());
    }

    #[test]
    fn blob_id_matches_object_id() {
        let d = b"hello\n";
        assert_eq!(blob_id(d), Object::Blob(d.to_vec()).id());
    }

    #[test]
    fn kinds_are_domain_separated() {
        // Same payload bytes, different kind => different id.
        assert_ne!(id_of_parts(Kind::Blob, b""), id_of_parts(Kind::Tree, b""));
    }

    #[test]
    fn tree_order_independent_of_input_order() {
        let e = |n: &str| TreeEntry {
            name: n.into(),
            mode: EntryMode::File,
            id: oid(1),
        };
        let a = Tree::new(vec![e("b"), e("a"), e("c")]).unwrap();
        let b = Tree::new(vec![e("c"), e("b"), e("a")]).unwrap();
        assert_eq!(Object::Tree(a).id(), Object::Tree(b).id());
    }

    #[test]
    fn tree_rejects_dupes_and_bad_names() {
        let e = |n: &str| TreeEntry {
            name: n.into(),
            mode: EntryMode::File,
            id: oid(1),
        };
        assert!(Tree::new(vec![e("a"), e("a")]).is_err());
        for bad in ["", "..", "a/b", ".vcs", ".VCS"] {
            assert!(Tree::new(vec![e(bad)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn decode_rejects_unsorted_tree() {
        // hand-build tree with entries b, a
        let mut w = Writer::new();
        w.u8(Kind::Tree as u8).u8(FORMAT_VERSION).varint(2);
        w.str("b").u8(1).id(&oid(1)).str("a").u8(1).id(&oid(1));
        assert!(Object::decode(&w.finish()).is_err());
    }

    #[test]
    fn decode_rejects_trailing_and_bad_version_and_kind() {
        let mut enc = Object::Blob(b"x".to_vec()).encode();
        assert!(Object::decode(&enc).is_ok());
        let mut bad = enc.clone();
        bad[1] = 99;
        assert_eq!(Object::decode(&bad), Err(DecodeError::Version(99)));
        bad = enc.clone();
        bad[0] = 77;
        assert_eq!(Object::decode(&bad), Err(DecodeError::Kind(77)));
        let t = Object::Tree(Tree::default());
        enc = t.encode();
        enc.push(0);
        assert_eq!(Object::decode(&enc), Err(DecodeError::Trailing));
    }

    #[test]
    fn golden_blob_id_is_stable() {
        // Guards against accidental change of the identity function (format v2).
        assert_eq!(
            Object::Blob(b"hello\n".to_vec()).id().to_hex(),
            Object::Blob(b"hello\n".to_vec()).id().to_hex()
        );
        let id = blob_id(b"hello\n").to_hex();
        assert_eq!(id.len(), 64);
        assert_eq!(id, GOLDEN_HELLO);
    }
    // Deliberately changed when FORMAT_VERSION went 2 -> 3 (symlink entry mode); the v2 value was
    // de6e9755b08af02d38d3d998f15520ff076c051b4b84753e99b098b728bc724a.
    const GOLDEN_HELLO: &str = "31378e608099a5eaeef6479254797e97d56e268011cf52f84a92f77b59b34fac";

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

    #[test]
    fn content_id_is_blob_below_threshold_and_manifest_at_or_above() {
        let small = rnd(CHUNK_THRESHOLD - 1, 1);
        assert_eq!(content_id(&small), blob_id(&small));
        assert!(ChunkedBlob::plan(&small).is_none());
        let big = rnd(CHUNK_THRESHOLD, 1);
        let m = ChunkedBlob::plan(&big).unwrap();
        assert_ne!(content_id(&big), blob_id(&big));
        assert_eq!(content_id(&big), Object::Chunked(m.clone()).id());
        assert_eq!(m.total_len as usize, big.len());
        assert_eq!(m.chunks.iter().map(|c| c.len as usize).sum::<usize>(), big.len());
        assert!(m.chunks.iter().all(|c| c.len as usize <= CHUNK_MAX && c.len > 0));
    }

    #[test]
    fn chunked_roundtrip_and_golden_id() {
        let big = rnd(CHUNK_THRESHOLD + 12345, 42);
        let o = Object::Chunked(ChunkedBlob::plan(&big).unwrap());
        assert_eq!(Object::decode(&o.encode()).unwrap(), o);
        // Pins the chunker parameters and manifest encoding (format v3): changing either changes this.
        assert_eq!(o.id().to_hex(), GOLDEN_CHUNKED);
    }
    // Also changed by the v3 bump (the version byte is hashed); the v2 value was
    // c07e3acd5be026d050469aaf80ec54325a99efc06977a8f9f9d39b6a21133fc4.
    const GOLDEN_CHUNKED: &str = "1085d7e6e3566bdda2ac7b8a241eb903b16f25b399ae44c7604779ec8d935686";

    #[test]
    fn chunked_decode_rejects_malformed() {
        let big = rnd(CHUNK_THRESHOLD, 5);
        let m = ChunkedBlob::plan(&big).unwrap();
        let enc = Object::Chunked(m.clone()).encode();
        // truncation at every length near the end and start
        for cut in [1usize, 2, 3, 10, enc.len() / 2, enc.len() - 1] {
            assert!(Object::decode(&enc[..cut]).is_err(), "cut {cut}");
        }
        let mut t = enc.clone();
        t.push(0);
        assert_eq!(Object::decode(&t), Err(DecodeError::Trailing));
        let mut bad = m.clone();
        bad.total_len += 1;
        assert!(Object::decode(&Object::Chunked(bad).encode()).is_err(), "sum mismatch");
        let mut bad = m.clone();
        bad.chunks[0].len = 0;
        assert!(Object::decode(&Object::Chunked(bad).encode()).is_err(), "empty chunk");
        let mut bad = m.clone();
        bad.chunks[0].len = CHUNK_MAX as u32 + 1;
        assert!(Object::decode(&Object::Chunked(bad).encode()).is_err(), "oversize chunk");
        let one = ChunkedBlob {
            total_len: 10,
            chunks: vec![ChunkRef { id: oid(1), len: 10 }],
        };
        assert!(Object::decode(&Object::Chunked(one).encode()).is_err(), "below threshold");
        // an old-format object is refused
        let mut old = enc;
        old[1] = 1;
        assert_eq!(Object::decode(&old), Err(DecodeError::Version(1)));
    }

    #[test]
    fn symlink_entry_roundtrips_and_v2_is_rejected() {
        let t = Object::Tree(
            Tree::new(vec![
                TreeEntry {
                    name: "f".into(),
                    mode: EntryMode::File,
                    id: oid(1),
                },
                TreeEntry {
                    name: "l".into(),
                    mode: EntryMode::Symlink,
                    id: oid(2),
                },
            ])
            .unwrap(),
        );
        let enc = t.encode();
        assert_eq!(Object::decode(&enc).unwrap(), t);
        let mut v1 = enc.clone();
        v1[1] = 2;
        assert_eq!(Object::decode(&v1), Err(DecodeError::Version(2)));
        // mode byte 5 is still unknown
        let mut bad = enc;
        let pos = bad.iter().rposition(|b| *b == 4).unwrap();
        bad[pos] = 5;
        assert!(Object::decode(&bad).is_err());
    }

    #[test]
    fn commit_and_snapshot_and_tx_roundtrip() {
        let c = Commit {
            zone: "engine".into(),
            parents: vec![oid(1), oid(2)],
            root: oid(3),
            change_id: ChangeId::from_bytes([9; 16]),
            author: sig(),
            message: "msg\nbody".into(),
        };
        let mut zones = BTreeMap::new();
        zones.insert("a".to_string(), oid(1));
        zones.insert("b".to_string(), oid(2));
        let s = Snapshot {
            zones,
            author: sig(),
            message: "s".into(),
        };
        let t = Transaction {
            updates: vec![
                RefUpdate {
                    zone: "a".into(),
                    branch: "main".into(),
                    old: None,
                    new: oid(1),
                },
                RefUpdate {
                    zone: "b".into(),
                    branch: "main".into(),
                    old: Some(oid(5)),
                    new: oid(2),
                },
            ],
            author: sig(),
            message: "t".into(),
            lock_ops: vec![],
        };
        let mut t2 = t.clone();
        t2.lock_ops = vec![
            LockOp::Acquire {
                zone: "a".into(),
                path: "x/y.bin".into(),
                lease_ms: 5,
                reason: "r".into(),
            },
            LockOp::Renew {
                zone: "a".into(),
                path: "x/y.bin".into(),
                token: 3,
                lease_ms: 9,
            },
            LockOp::Release {
                zone: "a".into(),
                path: "x/y.bin".into(),
                token: 3,
            },
            LockOp::Break {
                zone: "a".into(),
                path: "x/y.bin".into(),
            },
            LockOp::Present {
                zone: "a".into(),
                path: "x/y.bin".into(),
                token: 3,
            },
        ];
        assert_ne!(Object::Transaction(t.clone()).encode(), Object::Transaction(t2.clone()).encode());
        assert_eq!(
            &Object::Transaction(t2.clone()).encode()[..Object::Transaction(t.clone()).encode().len()],
            &Object::Transaction(t.clone()).encode()[..],
            "lock ops are a pure suffix: lock-free transactions keep their bytes"
        );
        for o in [
            Object::Commit(c),
            Object::Snapshot(s),
            Object::Transaction(t),
            Object::Transaction(t2),
        ] {
            let enc = o.encode();
            assert_eq!(Object::decode(&enc).unwrap(), o);
            assert_eq!(id_of_encoded(&enc), o.id());
        }
    }

    fn op_strategy() -> impl Strategy<Value = Operation> {
        (
            proptest::collection::vec(any::<[u8; 32]>(), 0..4),
            proptest::collection::btree_map(("[a-z][a-z0-9]{0,6}", "[a-z][a-z0-9]{0,6}"), any::<[u8; 32]>(), 0..6),
            proptest::collection::btree_map("[a-z][a-z0-9]{0,6}", "[a-z][a-z0-9]{0,6}", 0..4),
            ".{0,40}",
            any::<u64>(),
        )
            .prop_map(|(ps, bs, ws, d, ts)| Operation {
                parents: ps.into_iter().map(ObjectId::from_bytes).collect(),
                view: OpView {
                    branches: bs.into_iter().map(|(k, v)| (k, ObjectId::from_bytes(v))).collect(),
                    workspace: ws,
                    tags: None,
                },
                description: d,
                author: Signature {
                    name: "A".into(),
                    email: "a@x".into(),
                    timestamp: ts,
                },
            })
    }

    #[test]
    fn operation_roundtrip_and_strictness() {
        let mut v = OpView::default();
        v.branches.insert(("a".into(), "main".into()), oid(1));
        v.branches.insert(("b".into(), "main".into()), oid(2));
        v.workspace.insert("a".into(), "main".into());
        let op = Object::Operation(Operation {
            parents: vec![oid(7)],
            view: v,
            description: "commit: x".into(),
            author: sig(),
        });
        let enc = op.encode();
        assert_eq!(enc[0], 6);
        assert_eq!(Object::decode(&enc).unwrap(), op);
        let mut t = enc.clone();
        t.extend([0, 0]);
        assert_eq!(Object::decode(&t), Err(DecodeError::Trailing));
        // a view without a tag section (every operation recorded before tags) keeps its bytes;
        // with one (even empty) it decodes back as Some
        let Object::Operation(mut o) = op.clone() else { unreachable!() };
        assert_eq!(o.view.tags, None);
        o.view.tags = Some(Default::default());
        let with_empty = Object::Operation(o.clone()).encode();
        assert_eq!(with_empty.len(), enc.len() + 1);
        assert_eq!(Object::decode(&with_empty).unwrap(), Object::Operation(o.clone()));
        o.view.tags = Some([(("a".to_owned(), "v1".to_owned()), oid(3))].into());
        let enc_t = Object::Operation(o.clone()).encode();
        assert_eq!(Object::decode(&enc_t).unwrap(), Object::Operation(o));
        // unsorted branches are rejected
        let mut w = Writer::new();
        w.u8(Kind::Operation as u8).u8(FORMAT_VERSION).varint(0).varint(2);
        w.str("b").str("main").id(&oid(1)).str("a").str("main").id(&oid(2));
        w.varint(0);
        sig().encode(&mut w);
        w.str("d");
        assert!(Object::decode(&w.finish()).is_err());
        // duplicate workspace zone rejected
        let mut w = Writer::new();
        w.u8(Kind::Operation as u8).u8(FORMAT_VERSION).varint(0).varint(0).varint(2);
        w.str("a").str("main").str("a").str("main");
        sig().encode(&mut w);
        w.str("d");
        assert!(Object::decode(&w.finish()).is_err());
    }

    proptest! {
        #[test]
        fn operation_roundtrip_prop(op in op_strategy()) {
            let o = Object::Operation(op);
            let enc = o.encode();
            prop_assert_eq!(Object::decode(&enc).unwrap(), o.clone());
            prop_assert_eq!(id_of_encoded(&enc), o.id());
        }
        #[test]
        fn blob_roundtrip(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let o = Object::Blob(data);
            prop_assert_eq!(Object::decode(&o.encode()).unwrap(), o);
        }
        #[test]
        fn tree_roundtrip_and_order_independent(names in proptest::collection::btree_set("[a-z0-9][a-z0-9._-]{0,11}", 0..20), seed in any::<u64>()) {
            let mut v: Vec<TreeEntry> = names.iter().map(|n| TreeEntry{name:n.clone(), mode:EntryMode::File, id: oid(1)}).collect();
            let t1 = Tree::new(v.clone()).unwrap();
            // deterministic shuffle
            let mut s = seed | 1;
            for i in (1..v.len()).rev() { s ^= s << 13; s ^= s >> 7; s ^= s << 17; v.swap(i, (s % (i as u64 + 1)) as usize); }
            let t2 = Tree::new(v).unwrap();
            prop_assert_eq!(&t1, &t2);
            let o = Object::Tree(t1);
            prop_assert_eq!(Object::decode(&o.encode()).unwrap(), o.clone());
            prop_assert_eq!(o.id(), Object::Tree(t2).id());
        }
        #[test]
        fn decode_arbitrary_never_panics(data in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = Object::decode(&data);
        }
        #[test]
        fn decode_then_encode_is_identity(data in proptest::collection::vec(any::<u8>(), 0..256)) {
            // canonical: anything that decodes re-encodes to the same bytes
            if let Ok(o) = Object::decode(&data) { prop_assert_eq!(o.encode(), data); }
        }
    }
}
