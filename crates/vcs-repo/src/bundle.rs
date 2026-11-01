//! Offline transport bundles: `vcs bundle create|verify`, `vcs clone <bundle>`, `vcs fetch <bundle>`.
//! Format, limits and the safety argument: docs/bundle.md.
//!
//! A bundle is one file: a fixed preface, a small text header (zone definitions, branch heads,
//! prerequisite commits), a pack of objects in the existing `VCSPACK1` format, and a BLAKE3
//! trailer over everything before it. Reading never trusts a length before checking it against
//! the real file size, and nothing is written to a repository until the whole file has been
//! verified.

use crate::error::{io, msg, Error, Result};
use crate::gc::children;
use crate::meta::{write_atomic, Refs, Workspace, Zones, DEFAULT_BRANCH, ROOT_ZONE};
use crate::repo::Repo;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use vcs_objects::names::{validate_branch_name, validate_path, validate_zone_name};
use vcs_objects::{id_of_encoded, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::pack::{build_pack_fmt, decode_payload_max, PackFormat, PACK_MAGIC, PACK_MAGIC_V2};
use vcs_store::ObjectStore;

pub const MAGIC: &[u8; 8] = b"VCSBNDL\n";
pub const VERSION: u32 = 1;
/// Largest header accepted (bytes).
pub const MAX_HEADER: usize = 1 << 20;
/// Largest single encoded object accepted (bytes).
pub const MAX_OBJECT: usize = 64 << 20;
const PACK_V1: &str = "vcspack1";
const PACK_V2: &str = "vcspack2";
const PREFACE: u64 = 8 + 4 + 4;
const TRAILER: u64 = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleZone {
    pub id: String,
    pub path: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleHead {
    pub zone: String,
    pub branch: String,
    pub id: ObjectId,
}

/// What a bundle says about itself (everything here has been checked).
#[derive(Clone, Debug)]
pub struct BundleInfo {
    pub version: u32,
    pub pack_format: String,
    pub objects: usize,
    pub zones: Vec<BundleZone>,
    pub heads: Vec<BundleHead>,
    /// Commits the receiving repository must already have.
    pub prerequisites: Vec<ObjectId>,
    pub bytes: u64,
}

/// Result of [`Repo::bundle_fetch`].
#[derive(Debug)]
pub struct BundleFetchReport {
    /// (zone, branch, old remote-tracking head, new head)
    pub updated: Vec<(String, String, Option<ObjectId>, ObjectId)>,
    pub objects: usize,
}

fn bad<T>(why: impl std::fmt::Display) -> Result<T> {
    Err(Error::Corrupt(format!("bundle: {why}")))
}

// ---------- header ----------

struct Header {
    info: BundleInfo,
    pack_start: u64,
    pack_len: u64,
}

fn encode_header(info: &BundleInfo, pack_len: u64) -> String {
    let mut s = String::new();
    s.push_str(&format!("pack-format\t{}\n", info.pack_format));
    s.push_str(&format!("pack-bytes\t{pack_len}\n"));
    s.push_str(&format!("objects\t{}\n", info.objects));
    for z in &info.zones {
        s.push_str(&format!("zone\t{}\t{}\t{}\n", z.id, z.path, z.label.as_deref().unwrap_or("")));
    }
    for h in &info.heads {
        s.push_str(&format!("head\t{}\t{}\t{}\n", h.zone, h.branch, h.id.to_hex()));
    }
    for p in &info.prerequisites {
        s.push_str(&format!("prereq\t{}\n", p.to_hex()));
    }
    s
}

fn parse_header(text: &str) -> Result<(BundleInfo, u64)> {
    let (mut fmt, mut pack_bytes, mut objects) = (None, None, None);
    let (mut zones, mut heads, mut prereqs) = (Vec::<BundleZone>::new(), Vec::<BundleHead>::new(), Vec::<ObjectId>::new());
    let num = |v: &str| v.parse::<u64>().ok().filter(|n| n.to_string() == v);
    for line in text.split_terminator('\n') {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["pack-format", v] if fmt.is_none() => fmt = Some((*v).to_owned()),
            ["pack-bytes", v] if pack_bytes.is_none() => pack_bytes = num(v),
            ["objects", v] if objects.is_none() => objects = num(v),
            ["zone", id, path, label] => {
                validate_zone_name(id)?;
                if *id == ROOT_ZONE {
                    if !path.is_empty() {
                        return bad("zone root must have an empty path");
                    }
                } else {
                    validate_path(path)?;
                }
                if !label.is_empty() {
                    validate_zone_name(label)?;
                }
                if zones.iter().any(|z| z.id == *id) {
                    return bad(format!("zone {id} listed twice"));
                }
                zones.push(BundleZone {
                    id: (*id).into(),
                    path: (*path).into(),
                    label: (!label.is_empty()).then(|| (*label).into()),
                });
            }
            ["head", zone, branch, hex] => {
                validate_branch_name(branch)?;
                let id = ObjectId::from_hex(hex).or_else(|_| bad("bad head id"))?;
                if !zones.iter().any(|z| z.id == *zone) {
                    return bad(format!("head {zone}/{branch} names an undefined zone"));
                }
                if heads.iter().any(|h| h.zone == *zone && h.branch == *branch) {
                    return bad(format!("head {zone}/{branch} listed twice"));
                }
                heads.push(BundleHead {
                    zone: (*zone).into(),
                    branch: (*branch).into(),
                    id,
                });
            }
            ["prereq", hex] => {
                let id = ObjectId::from_hex(hex).or_else(|_| bad("bad prerequisite id"))?;
                if !prereqs.contains(&id) {
                    prereqs.push(id);
                }
            }
            [k, ..] if k.starts_with("x-") => {} // reserved for extensions; ignored
            _ => return bad(format!("unrecognised header line {:?}", line.chars().take(40).collect::<String>())),
        }
    }
    let (Some(pack_format), Some(pack_bytes), Some(objects)) = (fmt, pack_bytes, objects) else {
        return bad("header lacks pack-format, pack-bytes or objects");
    };
    if pack_format != PACK_V1 && pack_format != PACK_V2 {
        return bad(format!("unsupported pack format {pack_format:?}"));
    }
    Ok((
        BundleInfo {
            version: VERSION,
            pack_format,
            objects: usize::try_from(objects).or_else(|_| bad("object count"))?,
            zones,
            heads,
            prerequisites: prereqs,
            bytes: 0,
        },
        pack_bytes,
    ))
}

/// True if `path` is a regular file that starts with the bundle magic.
pub fn is_bundle_file(path: &Path) -> bool {
    let mut b = [0u8; 8];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut b))
        .map(|()| &b == MAGIC)
        .unwrap_or(false)
}

/// Stream the file once and compare BLAKE3 of all but the last 32 bytes with those bytes.
fn check_trailer(f: &mut fs::File, len: u64, path: &Path) -> Result<()> {
    io(path, f.seek(SeekFrom::Start(0)))?;
    let mut h = blake3::Hasher::new();
    let mut left = len - TRAILER;
    let mut buf = vec![0u8; 64 << 10];
    while left > 0 {
        let n = left.min(buf.len() as u64) as usize;
        io(path, f.read_exact(&mut buf[..n]))?;
        h.update(&buf[..n]);
        left -= n as u64;
    }
    let mut t = [0u8; 32];
    io(path, f.read_exact(&mut t))?;
    if h.finalize().as_bytes() != &t {
        return bad("hash mismatch (file is corrupt or truncated)");
    }
    Ok(())
}

fn read_header(f: &mut fs::File, path: &Path) -> Result<Header> {
    let len = io(path, f.metadata())?.len();
    io(path, f.seek(SeekFrom::Start(0)))?;
    let mut magic = [0u8; 8];
    if len < 8 || io(path, f.read_exact(&mut magic)).is_err() || &magic != MAGIC {
        return bad("not a bundle (bad magic)");
    }
    if len < PREFACE + TRAILER {
        return bad("file too short");
    }
    check_trailer(f, len, path)?;
    io(path, f.seek(SeekFrom::Start(8)))?;
    let mut w = [0u8; 8];
    io(path, f.read_exact(&mut w))?;
    let version = u32::from_le_bytes(w[..4].try_into().unwrap());
    if version != VERSION {
        return bad(format!("unsupported bundle version {version} (this build reads {VERSION})"));
    }
    let hlen = u32::from_le_bytes(w[4..].try_into().unwrap()) as u64;
    if hlen > MAX_HEADER as u64 || PREFACE + hlen + TRAILER > len {
        return bad("header length out of range");
    }
    let mut hb = vec![0u8; hlen as usize];
    io(path, f.read_exact(&mut hb))?;
    let text = String::from_utf8(hb).or_else(|_| bad("header is not utf-8"))?;
    let (mut info, pack_len) = parse_header(&text)?;
    let pack_start = PREFACE + hlen;
    if pack_start + pack_len + TRAILER != len {
        return bad("pack length does not match the file size");
    }
    info.bytes = len;
    Ok(Header {
        info,
        pack_start,
        pack_len,
    })
}

// ---------- pack scan ----------

struct Node {
    children: Vec<ObjectId>,
    /// (zone, parents) for a commit
    commit: Option<(String, Vec<ObjectId>)>,
}

/// Read every object of the pack, verifying framing, the pack checksum, canonical decoding and
/// that no id repeats. `visit` sees each object right after it passed. Allocation is bounded by
/// the largest single object, which is itself bounded by the bytes actually left in the file.
fn scan_pack(f: &mut fs::File, path: &Path, h: &Header, mut visit: impl FnMut(ObjectId, Object, &[u8]) -> Result<()>) -> Result<()> {
    io(path, f.seek(SeekFrom::Start(h.pack_start)))?;
    let mut r = BufReader::new(Read::take(&mut *f, h.pack_len));
    let mut hasher = blake3::Hasher::new();
    let mut head = [0u8; 12];
    let (want_magic, version) = if h.info.pack_format == PACK_V2 {
        (PACK_MAGIC_V2, 2u8)
    } else {
        (PACK_MAGIC, 1u8)
    };
    if h.pack_len < 12 + 32 || r.read_exact(&mut head).is_err() || &head[..8] != want_magic {
        return bad("pack header");
    }
    hasher.update(&head);
    let count = u32::from_le_bytes(head[8..].try_into().unwrap()) as usize;
    if count != h.info.objects {
        return bad("object count in the pack differs from the header");
    }
    let mut pos = 12u64;
    let body_end = h.pack_len - 32;
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let mut lb = [0u8; 4];
        if pos + 4 > body_end || r.read_exact(&mut lb).is_err() {
            return bad("pack truncated");
        }
        hasher.update(&lb);
        pos += 4;
        let n = u32::from_le_bytes(lb) as u64;
        if n == 0 || n > MAX_OBJECT as u64 + 1 || pos + n > body_end {
            return bad("object length out of range");
        }
        let mut buf = vec![0u8; n as usize];
        if r.read_exact(&mut buf).is_err() {
            return bad("pack truncated");
        }
        hasher.update(&buf);
        pos += n;
        let buf = decode_payload_max(version, &buf, MAX_OBJECT as u64).or_else(|e| bad(format!("entry does not decode: {e}")))?;
        let obj = Object::decode(&buf).or_else(|e| bad(format!("object does not decode: {e}")))?;
        let id = id_of_encoded(&buf);
        if !seen.insert(id) {
            return bad(format!("object {id} appears twice"));
        }
        visit(id, obj, &buf)?;
    }
    if pos != body_end {
        return bad("trailing bytes in the pack");
    }
    let mut sum = [0u8; 32];
    if r.read_exact(&mut sum).is_err() || hasher.finalize().as_bytes() != &sum {
        return bad("pack checksum mismatch");
    }
    Ok(())
}

struct Scan {
    nodes: BTreeMap<ObjectId, Node>,
}

fn scan_nodes(f: &mut fs::File, path: &Path, h: &Header) -> Result<Scan> {
    let mut nodes = BTreeMap::new();
    scan_pack(f, path, h, |id, obj, _| {
        let mut ch = Vec::new();
        children(&obj, &mut ch);
        let commit = match &obj {
            Object::Commit(c) => Some((c.zone.clone(), c.parents.clone())),
            _ => None,
        };
        nodes.insert(id, Node { children: ch, commit });
        Ok(())
    })?;
    Ok(Scan { nodes })
}

/// Check heads and connectivity. `outside(id)` says whether an id that is not in the bundle is
/// acceptable (it is a prerequisite's history, i.e. present in the receiving store).
fn check_graph(info: &BundleInfo, scan: &Scan, outside: &dyn Fn(&ObjectId) -> bool) -> Result<()> {
    let mut commit_zone: BTreeMap<ObjectId, &str> = BTreeMap::new();
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    for hd in &info.heads {
        if !scan.nodes.contains_key(&hd.id) {
            if !info.prerequisites.contains(&hd.id) {
                return bad(format!(
                    "head {}/{} is neither in the bundle nor a prerequisite",
                    hd.zone, hd.branch
                ));
            }
            continue;
        }
        // (id, zone context: set only while walking commit parents)
        let mut stack: Vec<(ObjectId, Option<&str>)> = vec![(hd.id, Some(hd.zone.as_str()))];
        while let Some((id, zone)) = stack.pop() {
            let Some(n) = scan.nodes.get(&id) else {
                if !outside(&id) {
                    return bad(format!(
                        "object {id} is referenced but neither in the bundle nor available as a prerequisite"
                    ));
                }
                continue;
            };
            match (&n.commit, zone) {
                (Some((cz, _)), Some(z)) => {
                    if cz != z {
                        return bad(format!("commit {id} belongs to zone {cz}, not {z}"));
                    }
                    if let Some(prev) = commit_zone.insert(id, z) {
                        if prev != z {
                            return bad(format!("commit {id} is reachable from zones {prev} and {z}"));
                        }
                        continue;
                    }
                }
                (Some(_), None) => return bad(format!("commit {id} appears where a tree or blob is expected")),
                (None, Some(_)) => return bad(format!("head chain reaches non-commit {id}")),
                (None, None) => {}
            }
            if zone.is_none() && !seen.insert(id) {
                continue;
            }
            match &n.commit {
                Some((_, parents)) => {
                    // children() lists parents then the root tree
                    for c in &n.children {
                        let is_parent = parents.contains(c);
                        stack.push((*c, is_parent.then_some(zone).flatten()));
                    }
                }
                None => stack.extend(n.children.iter().map(|c| (*c, None))),
            }
        }
    }
    Ok(())
}

/// Verify a bundle file without a repository: hashes, framing, object decoding, and that every
/// head's history is complete inside the bundle apart from what the prerequisites supply.
pub fn verify_file(path: &Path) -> Result<BundleInfo> {
    let mut f = io(path, fs::File::open(path))?;
    let h = read_header(&mut f, path)?;
    let scan = scan_nodes(&mut f, path, &h)?;
    let has_prereqs = !h.info.prerequisites.is_empty();
    check_graph(&h.info, &scan, &|_| has_prereqs)?;
    Ok(h.info)
}

// ---------- create ----------

fn select_heads(repo: &Repo, selectors: &[String]) -> Result<Vec<(String, String, ObjectId)>> {
    let defs = repo.zones()?;
    let refs = repo.refs()?;
    let mut out: BTreeMap<(String, String), ObjectId> = BTreeMap::new();
    if selectors.is_empty() {
        out.extend(refs.branches.iter().map(|(k, v)| (k.clone(), *v)));
    }
    for s in selectors {
        if let Some((z, b)) = s.split_once('/') {
            if let Some(zone) = defs.resolve(z) {
                match refs.head(&zone, b) {
                    Some(id) => {
                        out.insert((zone, b.to_owned()), id);
                        continue;
                    }
                    None => return msg(format!("no such branch {z}/{b}")),
                }
            }
        }
        if let Some(zone) = defs.resolve(s) {
            let bs = refs.branches_of(&zone);
            if bs.is_empty() {
                return msg(format!("zone {s:?} has no branches to bundle"));
            }
            out.extend(bs.into_iter().map(|(b, id)| ((zone.clone(), b), id)));
        } else if let Some(id) = refs.head(ROOT_ZONE, s) {
            out.insert((ROOT_ZONE.to_owned(), s.clone()), id);
        } else {
            return msg(format!("{s:?} is neither a zone nor a branch (use zone/branch)"));
        }
    }
    if out.is_empty() {
        return msg("nothing to bundle: no branches have commits");
    }
    Ok(out.into_iter().map(|((z, b), id)| (z, b, id)).collect())
}

impl Repo {
    /// Write a bundle of the selected branches (`zone`, `zone/branch`, or a root-zone branch; none
    /// selects every branch). With `since`, objects reachable from that revision are left out and
    /// the commit becomes a prerequisite of the bundle (in every selected zone where it resolves).
    pub fn bundle_create(&self, file: &Path, selectors: &[String], since: Option<&str>) -> Result<BundleInfo> {
        let heads = select_heads(self, selectors)?;
        let defs = self.zones()?;
        let mut prereqs: Vec<ObjectId> = Vec::new();
        if let Some(rev) = since {
            let zones: BTreeSet<&str> = heads.iter().map(|h| h.0.as_str()).collect();
            for z in zones {
                if let Ok(c) = self.resolve_rev(z, rev) {
                    if !prereqs.contains(&c) {
                        prereqs.push(c);
                    }
                }
            }
            if prereqs.is_empty() {
                return msg(format!("--since {rev:?} does not resolve to a commit in any selected zone"));
            }
        }
        // Everything the receiver is promised to have.
        let mut have: BTreeSet<ObjectId> = BTreeSet::new();
        let mut stack = prereqs.clone();
        while let Some(id) = stack.pop() {
            if have.insert(id) {
                children(&self.store.get(&id)?, &mut stack);
            }
        }
        let mut objs: BTreeMap<ObjectId, Vec<u8>> = BTreeMap::new();
        let mut stack: Vec<ObjectId> = heads.iter().map(|h| h.2).collect();
        while let Some(id) = stack.pop() {
            if have.contains(&id) || objs.contains_key(&id) {
                continue;
            }
            let o = self.store.get(&id).map_err(|e| match e {
                vcs_store::StoreError::NotFound(i) => Error::Msg(format!(
                    "cannot bundle: object {i} is not in this repository (a sparse clone must fetch it first)"
                )),
                e => e.into(),
            })?;
            children(&o, &mut stack);
            objs.insert(id, o.encode());
        }
        let used_zones: BTreeSet<&str> = heads.iter().map(|h| h.0.as_str()).collect();
        let info = BundleInfo {
            version: VERSION,
            pack_format: PACK_V2.into(),
            objects: objs.len(),
            zones: used_zones
                .iter()
                .map(|z| BundleZone {
                    id: (*z).into(),
                    path: defs.path_of(z).unwrap_or("").into(),
                    label: (defs.label(z) != *z).then(|| defs.label(z).to_owned()),
                })
                .collect(),
            heads: heads
                .into_iter()
                .map(|(zone, branch, id)| BundleHead { zone, branch, id })
                .collect(),
            prerequisites: prereqs,
            bytes: 0,
        };
        let pack = build_pack_fmt(objs.into_iter().collect(), PackFormat::V2).pack;
        let header = encode_header(&info, pack.len() as u64);
        let tmp = PathBuf::from(format!("{}.tmp{}", file.display(), std::process::id()));
        let mut hasher = blake3::Hasher::new();
        let res = (|| -> Result<()> {
            let mut out = io(&tmp, fs::File::create(&tmp))?;
            let mut put = |b: &[u8]| -> Result<()> {
                hasher.update(b);
                io(&tmp, out.write_all(b))
            };
            put(MAGIC)?;
            put(&VERSION.to_le_bytes())?;
            put(&(header.len() as u32).to_le_bytes())?;
            put(header.as_bytes())?;
            put(&pack)?;
            let sum = *hasher.finalize().as_bytes();
            io(&tmp, out.write_all(&sum))?;
            io(&tmp, out.sync_all())?;
            io(file, fs::rename(&tmp, file))
        })();
        if res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        res?;
        let mut info = info;
        info.bytes = PREFACE + header.len() as u64 + pack.len() as u64 + TRAILER;
        Ok(info)
    }

    // ---------- apply ----------

    /// Verify `file` completely against this repository and compute what applying it changes.
    /// Writes nothing.
    fn bundle_prepare(&self, file: &Path) -> Result<Prepared> {
        let mut f = io(file, fs::File::open(file))?;
        let h = read_header(&mut f, file)?;
        let scan = scan_nodes(&mut f, file, &h)?;
        for p in &h.info.prerequisites {
            match self.store.get(p) {
                Ok(Object::Commit(_)) => {}
                Ok(_) => return bad(format!("prerequisite {p} is not a commit here")),
                Err(vcs_store::StoreError::NotFound(_)) => {
                    return msg(format!(
                        "this bundle needs commit {} which this repository does not have (it is an incremental bundle; fetch an earlier one first)",
                        p.short()
                    ))
                }
                Err(e) => return Err(e.into()),
            }
        }
        let outside_err = std::cell::Cell::new(None);
        check_graph(&h.info, &scan, &|id| match self.store.contains(id) {
            Ok(b) => b,
            Err(e) => {
                outside_err.set(Some(e.to_string()));
                false
            }
        })?;
        if let Some(e) = outside_err.take() {
            return msg(e);
        }
        // Zone definitions: same checks as `fetch`, done on a copy so a bad one changes nothing.
        let mut defs = self.zones()?;
        let mut zones_changed = false;
        for z in &h.info.zones {
            if z.id == ROOT_ZONE {
                continue;
            }
            match defs.path_of(&z.id) {
                None => {
                    defs.create(&z.id, &z.path)?;
                    zones_changed = true;
                }
                Some(q) if q == z.path => {}
                Some(q) => return msg(format!("zone {:?} is at {q:?} here but {:?} in the bundle", z.id, z.path)),
            }
            if let Some(l) = &z.label {
                if defs.label(&z.id) == z.id && defs.set_label(&z.id, l).is_ok() {
                    zones_changed = true;
                }
            }
        }
        let rr = self.remote_refs()?;
        let mut updates: Vec<RefUpdate> = h
            .info
            .heads
            .iter()
            .filter(|hd| rr.head(&hd.zone, &hd.branch) != Some(hd.id))
            .map(|hd| RefUpdate {
                zone: hd.zone.clone(),
                branch: hd.branch.clone(),
                old: rr.head(&hd.zone, &hd.branch),
                new: hd.id,
            })
            .collect();
        updates.sort();
        Ok(Prepared {
            header: h,
            defs: zones_changed.then_some(defs),
            updates,
            rr,
        })
    }

    /// The commit sequence shared by fetch and clone. Objects first, then the transaction record,
    /// then the zone file, and last the single `remote-refs` write that makes every zone's new
    /// head visible at once. Caller holds the repository lock.
    fn bundle_commit(&self, file: &Path, p: &Prepared, author: Signature) -> Result<()> {
        let mut f = io(file, fs::File::open(file))?;
        scan_pack(&mut f, file, &p.header, |_, obj, _| {
            self.store.put(&obj)?;
            Ok(())
        })?;
        self.store.sync()?;
        if !p.updates.is_empty() {
            self.store.put(&Object::Transaction(Transaction {
                updates: p.updates.clone(),
                author,
                message: format!("bundle {}", file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
                lock_ops: vec![],
            }))?;
            self.store.sync()?;
        }
        if let Some(d) = &p.defs {
            write_atomic(&self.common.join("zones"), &d.encode())?;
        }
        if !p.updates.is_empty() {
            let mut rr = p.rr.clone();
            rr.apply(&p.updates)?;
            write_atomic(&self.common.join("remote-refs"), &rr.encode())?;
        }
        Ok(())
    }

    /// `vcs fetch <bundle>`: import the bundle's objects and set the remote-tracking heads of
    /// every zone in it, all or none. Local branches are untouched.
    pub fn bundle_fetch(&self, file: &Path, author: Signature) -> Result<BundleFetchReport> {
        let _l = self.lock()?;
        let p = self.bundle_prepare(file)?;
        self.bundle_commit(file, &p, author)?;
        Ok(BundleFetchReport {
            updated: p.updates.iter().map(|u| (u.zone.clone(), u.branch.clone(), u.old, u.new)).collect(),
            objects: p.header.info.objects,
        })
    }

    /// Prerequisite commits of `file` that this repository lacks (for `bundle verify` in a repo).
    pub fn bundle_missing_prerequisites(&self, file: &Path, info: &BundleInfo) -> Result<Vec<ObjectId>> {
        let _ = file;
        let mut v = Vec::new();
        for p in &info.prerequisites {
            if !self.store.contains(p)? {
                v.push(*p);
            }
        }
        Ok(v)
    }

    /// `vcs clone <bundle> <dest>`: a new repository whose branches are the bundle's heads, with
    /// each zone checked out on `main` (else its first branch). Bundles with prerequisites cannot
    /// be cloned (they need an existing repository: use `fetch`).
    pub fn clone_from_bundle(file: &Path, dest: &Path, author: Signature) -> Result<Repo> {
        let info = verify_file(file)?;
        if !info.prerequisites.is_empty() {
            return msg(
                "this is an incremental bundle (it has prerequisites); fetch it into a repository that has them instead of cloning",
            );
        }
        if dest.exists() && fs::read_dir(dest).map(|mut d| d.next().is_some()).unwrap_or(true) {
            return msg(format!("destination {} exists and is not empty", dest.display()));
        }
        let created = !dest.exists();
        let repo = Repo::init(dest)?;
        let r = (|| -> Result<()> {
            {
                let _l = repo.lock()?;
                let p = repo.bundle_prepare(file)?;
                repo.bundle_commit(file, &p, author)?;
                let mut local = Refs::default();
                local.apply(&p.updates)?;
                write_atomic(&repo.common.join("refs"), &local.encode())?;
                repo.save_ws(&Workspace::default())?;
            }
            let mut pick: BTreeMap<&str, &str> = BTreeMap::new();
            for h in &info.heads {
                let e = pick.entry(h.zone.as_str()).or_insert(h.branch.as_str());
                if h.branch == DEFAULT_BRANCH {
                    *e = DEFAULT_BRANCH;
                }
            }
            for (z, b) in pick {
                repo.zone_add(z, Some(b))?;
            }
            Ok(())
        })();
        match r {
            Ok(()) => Ok(repo),
            Err(e) => {
                drop(repo);
                if created {
                    let _ = fs::remove_dir_all(dest);
                }
                Err(e)
            }
        }
    }
}

struct Prepared {
    header: Header,
    defs: Option<Zones>,
    updates: Vec<RefUpdate>,
    rr: Refs,
}
