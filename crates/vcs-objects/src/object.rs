use crate::codec::{DecodeError, Reader, Writer};
use crate::id::{ChangeId, ObjectId};
use crate::names;
use std::collections::BTreeMap;

/// Version of the object encoding. Part of the hashed bytes: a format change yields new ids.
pub const FORMAT_VERSION: u8 = 1;
const HASH_CONTEXT: &str = "all-git 2026 object identity v1";
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum EntryMode {
    File = 1,
    Executable = 2,
    Directory = 3,
}

impl EntryMode {
    fn from_u8(b: u8) -> Result<Self, DecodeError> {
        match b {
            1 => Ok(Self::File),
            2 => Ok(Self::Executable),
            3 => Ok(Self::Directory),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    Blob(Vec<u8>),
    Tree(Tree),
    Commit(Commit),
    Snapshot(Snapshot),
    Transaction(Transaction),
    Operation(Operation),
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
                Object::Operation(Operation {
                    parents,
                    view: OpView { branches, workspace },
                    description,
                    author,
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
        // Guards against accidental change of the identity function (format v1).
        assert_eq!(
            Object::Blob(b"hello\n".to_vec()).id().to_hex(),
            Object::Blob(b"hello\n".to_vec()).id().to_hex()
        );
        let id = blob_id(b"hello\n").to_hex();
        assert_eq!(id.len(), 64);
        assert_eq!(id, GOLDEN_HELLO);
    }
    const GOLDEN_HELLO: &str = "3f158096fc84b5c7cdda1072501373ea3758184a0f5ba8628269a2af992e8935";

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
            &Object::Transaction(t2.clone()).encode()[..Object::Transaction(t.clone()).encode().len() - 0],
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
        t.push(0);
        assert_eq!(Object::decode(&t), Err(DecodeError::Trailing));
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
