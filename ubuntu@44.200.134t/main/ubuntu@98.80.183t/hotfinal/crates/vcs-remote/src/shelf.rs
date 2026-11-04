//! Shelves: private, named bundles of work-in-progress stored on the remote under
//! `shelves/<principal>/<name>/{meta,pack,idx}` WITHOUT touching the manifest (so the CAS publish
//! protocol is not involved). Design and safety argument: docs/shelves.md.
//!
//! * `pack` + `idx` are an ordinary pack (v2) holding the objects of the shelf that the shared
//!   object store does not already have. They are written with create-only PUTs first.
//! * `meta` is written LAST (create-only). A shelf exists exactly when its `meta` exists, so a
//!   crashed `shelve` leaves debris but never a half-visible shelf.
//! * The shelf depends on `bases`: commits it was built on that live in the shared store. While
//!   the shelf exists (and has not expired) remote GC treats `bases` as roots. To close the race
//!   with a concurrent sweep, [`upload`] runs the same Dekker-style handshake as a publisher
//!   (touch, tombstone check, existence check, hold-time fence) before it writes `meta`.
//! * Everything read back is hash-verified before the caller may store it ([`download`]).

use crate::backend::{Backend, BackendError};
use crate::packfetch::PACK_PREFIX;
use crate::protocol::{object_key, pack_tomb_key, tomb_key, DEFAULT_HOLD_MS};
use std::collections::BTreeSet;
use std::path::Path;
use vcs_objects::names::{validate_branch_name, validate_zone_name};
use vcs_objects::{id_of_encoded, ObjectId};
use vcs_store::pack::{decode_payload_max, PackIndex};

pub const SHELF_PREFIX: &str = "shelves/";
const FORMAT: &str = "vcs-shelf 1";
/// Largest `meta` accepted (bytes).
pub const MAX_META: usize = 1 << 20;
/// Largest single decoded object accepted from a shelf pack (bytes).
pub const MAX_OBJECT: u64 = 64 << 20;
/// Largest pack accepted on download (bytes).
pub const MAX_PACK: u64 = 4 << 30;

#[derive(Debug, thiserror::Error)]
pub enum ShelfError {
    #[error("shelf {0} already exists (or a crashed `vcs shelve` left debris: `vcs shelve --delete` it first)")]
    Exists(String),
    #[error("no such shelf {0}")]
    NotFound(String),
    #[error("invalid shelf: {0}")]
    Invalid(String),
    #[error("a base commit of the shelf was swept or is being swept by a running gc: {0}; retry")]
    Gc(String),
    #[error(transparent)]
    Backend(#[from] BackendError),
}

type Result<T> = std::result::Result<T, ShelfError>;

fn bad<T>(why: impl std::fmt::Display) -> Result<T> {
    Err(ShelfError::Invalid(why.to_string()))
}

/// What a shelf holds for one zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShelfZone {
    pub zone: String,
    pub branch: String,
    /// Zone head the work was based on (None: the branch had no commits).
    pub base: Option<ObjectId>,
    /// The ephemeral commit holding the work (parent: `base`).
    pub commit: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShelfMeta {
    pub principal: String,
    pub name: String,
    pub message: String,
    /// Display only: `Name <email>` of the shelver.
    pub author: String,
    /// Backend clock when available, else the client's (unix ms).
    pub created_ms: u64,
    pub expires_ms: Option<u64>,
    pub zones: Vec<ShelfZone>,
    /// Commits in the SHARED store that the shelf needs (remote GC roots while it lives).
    pub bases: Vec<ObjectId>,
    pub objects: u32,
    pub pack_bytes: u64,
    /// Hex of the pack checksum (the pack's last 32 bytes).
    pub pack_checksum: String,
}

impl ShelfMeta {
    pub fn id(&self) -> String {
        format!("{}/{}", self.principal, self.name)
    }
    pub fn expired(&self, now_ms: u64) -> bool {
        self.expires_ms.is_some_and(|e| e <= now_ms)
    }
}

/// A principal as a key component: 1..=128 of `[A-Za-z0-9._@+=-]`, not starting with a dot.
pub fn validate_principal(p: &str) -> Result<()> {
    let ok = !p.is_empty() && p.len() <= 128 && !p.starts_with('.') && p.chars().all(|c| c.is_ascii_alphanumeric() || "._@+=-".contains(c));
    if ok {
        Ok(())
    } else {
        bad(format!(
            "principal {p:?} is not usable in a shelf key (1..=128 of A-Z a-z 0-9 . _ @ + = -)"
        ))
    }
}

/// A shelf name: 1..=64 of `[A-Za-z0-9._-]`, not starting with a dot.
pub fn validate_name(n: &str) -> Result<()> {
    let ok = !n.is_empty() && n.len() <= 64 && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if ok {
        Ok(())
    } else {
        bad(format!(
            "shelf name {n:?} must be 1..=64 of A-Z a-z 0-9 . _ - and not start with a dot"
        ))
    }
}

pub fn meta_key(principal: &str, name: &str) -> String {
    format!("{SHELF_PREFIX}{principal}/{name}/meta")
}
pub fn pack_key(principal: &str, name: &str) -> String {
    format!("{SHELF_PREFIX}{principal}/{name}/pack")
}
pub fn idx_key(principal: &str, name: &str) -> String {
    format!("{SHELF_PREFIX}{principal}/{name}/idx")
}

/// Split a `shelves/<principal>/<name>/<file>` key; `file` must be `meta`, `pack` or `idx`.
pub fn parse_key(key: &str) -> Option<(&str, &str, &str)> {
    let rest = key.strip_prefix(SHELF_PREFIX)?;
    let mut it = rest.split('/');
    let (p, n, f) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || !matches!(f, "meta" | "pack" | "idx") {
        return None;
    }
    if validate_principal(p).is_err() || validate_name(n).is_err() {
        return None;
    }
    Some((p, n, f))
}

/// The owner (principal component) of any key under `shelves/`, valid or not.
pub fn owner_of(key: &str) -> Option<&str> {
    key.strip_prefix(SHELF_PREFIX)?.split('/').next().filter(|s| !s.is_empty())
}

fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(4096).collect()
}

impl ShelfMeta {
    pub fn encode(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(FORMAT);
        s.push('\n');
        s.push_str(&format!("principal\t{}\nname\t{}\n", self.principal, self.name));
        s.push_str(&format!(
            "message\t{}\nauthor\t{}\n",
            one_line(&self.message),
            one_line(&self.author)
        ));
        s.push_str(&format!("created-ms\t{}\n", self.created_ms));
        if let Some(e) = self.expires_ms {
            s.push_str(&format!("expires-ms\t{e}\n"));
        }
        s.push_str(&format!(
            "objects\t{}\npack-bytes\t{}\npack-checksum\t{}\n",
            self.objects, self.pack_bytes, self.pack_checksum
        ));
        for z in &self.zones {
            let base = z.base.map(|b| b.to_hex()).unwrap_or_else(|| "-".into());
            s.push_str(&format!("zone\t{}\t{}\t{}\t{}\n", z.zone, z.branch, base, z.commit.to_hex()));
        }
        for b in &self.bases {
            s.push_str(&format!("base\t{}\n", b.to_hex()));
        }
        s.into_bytes()
    }

    /// Strict parser: unknown or repeated lines, bad ids and bad names are errors.
    pub fn parse(bytes: &[u8]) -> Result<ShelfMeta> {
        if bytes.len() > MAX_META {
            return bad("meta too large");
        }
        let text = std::str::from_utf8(bytes).or_else(|_| bad("meta is not utf-8"))?;
        let mut lines = text.split_terminator('\n');
        if lines.next() != Some(FORMAT) {
            return bad("not a vcs-shelf 1 meta");
        }
        let num = |v: &str| v.parse::<u64>().ok().filter(|n| n.to_string() == v);
        let id = |v: &str| ObjectId::from_hex(v).or_else(|_| bad(format!("bad object id {v:?}")));
        let (mut principal, mut name, mut message, mut author) = (None, None, None, None);
        let (mut created, mut expires, mut objects, mut pack_bytes, mut sum) = (None, None, None, None, None);
        let (mut zones, mut bases) = (Vec::<ShelfZone>::new(), Vec::<ObjectId>::new());
        for line in lines {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["principal", v] if principal.is_none() => principal = Some((*v).to_owned()),
                ["name", v] if name.is_none() => name = Some((*v).to_owned()),
                ["message", v] if message.is_none() => message = Some((*v).to_owned()),
                ["author", v] if author.is_none() => author = Some((*v).to_owned()),
                ["created-ms", v] if created.is_none() => created = num(v),
                ["expires-ms", v] if expires.is_none() => {
                    expires = Some(num(v).ok_or_else(|| ShelfError::Invalid("bad expires-ms".into()))?)
                }
                ["objects", v] if objects.is_none() => objects = num(v).and_then(|n| u32::try_from(n).ok()),
                ["pack-bytes", v] if pack_bytes.is_none() => pack_bytes = num(v),
                ["pack-checksum", v] if sum.is_none() => {
                    if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                        return bad("bad pack-checksum");
                    }
                    sum = Some((*v).to_owned())
                }
                ["zone", z, b, base, commit] => {
                    validate_zone_name(z).or_else(bad)?;
                    validate_branch_name(b).or_else(bad)?;
                    if zones.iter().any(|x| x.zone == *z) {
                        return bad(format!("zone {z} listed twice"));
                    }
                    zones.push(ShelfZone {
                        zone: (*z).into(),
                        branch: (*b).into(),
                        base: if *base == "-" { None } else { Some(id(base)?) },
                        commit: id(commit)?,
                    });
                }
                ["base", h] => {
                    let i = id(h)?;
                    if !bases.contains(&i) {
                        bases.push(i);
                    }
                }
                _ => return bad(format!("unrecognised line {:?}", line.chars().take(40).collect::<String>())),
            }
        }
        let (
            Some(principal),
            Some(name),
            Some(message),
            Some(author),
            Some(created_ms),
            Some(objects),
            Some(pack_bytes),
            Some(pack_checksum),
        ) = (principal, name, message, author, created, objects, pack_bytes, sum)
        else {
            return bad("meta lacks a required line");
        };
        validate_principal(&principal)?;
        validate_name(&name)?;
        if zones.is_empty() {
            return bad("a shelf holds at least one zone");
        }
        Ok(ShelfMeta {
            principal,
            name,
            message,
            author,
            created_ms,
            expires_ms: expires,
            zones,
            bases,
            objects,
            pack_bytes,
            pack_checksum,
        })
    }
}

/// Bytes of the shelf pack and index, plus the numbers the meta records about them.
pub struct ShelfPack {
    pub pack: Vec<u8>,
    pub idx: Vec<u8>,
}

/// Units (`(keys to touch, tombstone key)`) guarding the given base commits in the shared store.
fn guard_units(b: &dyn Backend, ids: &[ObjectId]) -> Result<Vec<(Vec<String>, String)>> {
    let mut units: Vec<(Vec<String>, String)> = Vec::new();
    let mut packs: Option<Vec<(String, PackIndex)>> = None;
    for id in ids {
        let loose = object_key(id);
        if b.exists(&loose)? {
            units.push((vec![loose], tomb_key(id)));
            continue;
        }
        if packs.is_none() {
            let mut v = Vec::new();
            for k in b.list(PACK_PREFIX)? {
                let Some(stem) = k.strip_suffix(".idx") else { continue };
                let Some(x) = b.get(&k)? else { continue };
                if let Ok(i) = PackIndex::parse(&x.data, Path::new(stem)) {
                    v.push((stem.to_owned(), i));
                }
            }
            packs = Some(v);
        }
        match packs.as_ref().unwrap().iter().find(|(_, i)| i.find(id).is_some()) {
            Some((stem, _)) => units.push((vec![format!("{stem}.idx"), format!("{stem}.pack")], pack_tomb_key(stem))),
            None => return Err(ShelfError::Gc(format!("base commit {id} is not on the remote"))),
        }
    }
    Ok(units)
}

/// The GC handshake for the shelf's bases (see module docs). A no-op on backends that cannot be
/// garbage collected (no sweeper can exist there).
fn guard_bases(b: &dyn Backend, bases: &[ObjectId], hold_ms: u64) -> Result<()> {
    if bases.is_empty() || !b.supports_gc() {
        return Ok(());
    }
    let start = b.now_ms()?;
    let units = guard_units(b, bases)?;
    for (keys, _) in &units {
        for k in keys {
            if !b.touch(k)? {
                return Err(ShelfError::Gc(format!("{k} vanished")));
            }
        }
    }
    for (_, tomb) in &units {
        if b.exists(tomb)? {
            return Err(ShelfError::Gc(format!("{tomb} exists")));
        }
    }
    for (keys, _) in &units {
        for k in keys {
            if !b.exists(k)? {
                return Err(ShelfError::Gc(format!("{k} vanished")));
            }
        }
    }
    if b.now_ms()?.saturating_sub(start) > hold_ms {
        return Err(ShelfError::Gc("the shelf took longer than the publisher hold time".into()));
    }
    Ok(())
}

/// Store a shelf. `pack`/`idx` first (create-only), the base guard, then `meta` last.
pub fn upload(b: &dyn Backend, meta: &ShelfMeta, pack: &ShelfPack) -> Result<()> {
    upload_with_hold(b, meta, pack, DEFAULT_HOLD_MS, &mut || {})
}

/// [`upload`] with an explicit hold time and a hook that runs right before `meta` is written
/// (tests interleave a sweeper there).
#[doc(hidden)]
pub fn upload_with_hold(b: &dyn Backend, meta: &ShelfMeta, pack: &ShelfPack, hold_ms: u64, before_meta: &mut dyn FnMut()) -> Result<()> {
    validate_principal(&meta.principal)?;
    validate_name(&meta.name)?;
    let (mk, pk, ik) = (
        meta_key(&meta.principal, &meta.name),
        pack_key(&meta.principal, &meta.name),
        idx_key(&meta.principal, &meta.name),
    );
    if b.exists(&mk)? {
        return Err(ShelfError::Exists(meta.id()));
    }
    if !b.put_if_absent(&pk, &pack.pack)? {
        return Err(ShelfError::Exists(meta.id()));
    }
    let res = (|| {
        if !b.put_if_absent(&ik, &pack.idx)? {
            return Err(ShelfError::Exists(meta.id()));
        }
        guard_bases(b, &meta.bases, hold_ms)?;
        before_meta();
        if !b.put_if_absent(&mk, &meta.encode())? {
            return Err(ShelfError::Exists(meta.id()));
        }
        Ok(())
    })();
    if res.is_err() {
        // Only debris of ours (meta was never written): remove it so the name stays usable.
        if !b.exists(&mk).unwrap_or(true) {
            let _ = b.delete(&ik);
            let _ = b.delete(&pk);
        }
    }
    res
}

/// Read and parse one shelf's meta, checking that it matches the key it was found under.
pub fn load_meta(b: &dyn Backend, principal: &str, name: &str) -> Result<ShelfMeta> {
    validate_principal(principal)?;
    validate_name(name)?;
    let v = b
        .get(&meta_key(principal, name))?
        .ok_or_else(|| ShelfError::NotFound(format!("{principal}/{name}")))?;
    let m = ShelfMeta::parse(&v.data)?;
    if m.principal != principal || m.name != name {
        return bad("meta does not match the key it is stored under");
    }
    Ok(m)
}

/// One entry of [`list`].
pub struct Listing {
    pub principal: String,
    pub name: String,
    pub meta: std::result::Result<ShelfMeta, String>,
}

/// Every shelf visible to the caller (the server filters its listing), optionally only `owner`'s.
pub fn list(b: &dyn Backend, owner: Option<&str>) -> Result<Vec<Listing>> {
    let prefix = match owner {
        Some(o) => format!("{SHELF_PREFIX}{o}/"),
        None => SHELF_PREFIX.to_owned(),
    };
    let mut out = Vec::new();
    for k in b.list(&prefix)? {
        let Some((p, n, "meta")) = parse_key(&k) else { continue };
        out.push(Listing {
            principal: p.into(),
            name: n.into(),
            meta: load_meta(b, p, n).map_err(|e| e.to_string()),
        });
    }
    Ok(out)
}

/// **DESTRUCTIVE.** Remove a shelf: `meta` first (the shelf stops existing and stops being a GC
/// root), then `idx` and `pack`. Idempotent.
pub fn delete(b: &dyn Backend, principal: &str, name: &str) -> Result<()> {
    validate_principal(principal)?;
    validate_name(name)?;
    b.delete(&meta_key(principal, name))?;
    b.delete(&idx_key(principal, name))?;
    b.delete(&pack_key(principal, name))?;
    Ok(())
}

/// Roots a live shelf pins in the shared store: its `bases`. Expired shelves pin nothing.
/// An unparsable meta is an error (fail closed: GC must not guess).
pub fn roots(b: &dyn Backend, now_ms: u64) -> Result<Vec<ObjectId>> {
    let mut r: BTreeSet<ObjectId> = BTreeSet::new();
    for k in b.list(SHELF_PREFIX)? {
        let Some((p, n, "meta")) = parse_key(&k) else { continue };
        let Some(v) = b.get(&k)? else { continue };
        let m = ShelfMeta::parse(&v.data).map_err(|e| ShelfError::Invalid(format!("{k}: {e}")))?;
        if m.principal != p || m.name != n {
            return bad(format!("{k}: meta does not match its key"));
        }
        if !m.expired(now_ms) {
            r.extend(m.bases);
        }
    }
    Ok(r.into_iter().collect())
}

/// Ids (`principal/name`) of shelves whose expiry has passed at `now_ms`.
pub fn expired(b: &dyn Backend, now_ms: u64) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for l in list(b, None)? {
        if let Ok(m) = l.meta {
            if m.expired(now_ms) {
                out.push((l.principal, l.name));
            }
        }
    }
    Ok(out)
}

/// Download and fully verify a shelf's objects: index structure, pack length, trailer and
/// checksum, the object count, and every object's hash against its id. Returns
/// `(id, encoded bytes)`. Nothing is stored anywhere.
pub fn download(b: &dyn Backend, meta: &ShelfMeta) -> Result<Vec<(ObjectId, Vec<u8>)>> {
    let (p, n) = (&meta.principal, &meta.name);
    let idx_bytes = b
        .get(&idx_key(p, n))?
        .ok_or_else(|| ShelfError::Invalid("shelf index is missing".into()))?
        .data;
    let label = format!("{p}/{n}");
    let idx = PackIndex::parse(&idx_bytes, Path::new(&label)).or_else(|e| bad(format!("index: {e}")))?;
    if idx.pack_len != meta.pack_bytes || idx.pack_len > MAX_PACK || idx.len() != meta.objects as usize {
        return bad("index disagrees with meta (length or object count)");
    }
    if hex(idx.checksum()) != meta.pack_checksum {
        return bad("index checksum disagrees with meta");
    }
    let pack = b
        .get(&pack_key(p, n))?
        .ok_or_else(|| ShelfError::Invalid("shelf pack is missing".into()))?
        .data;
    if pack.len() as u64 != idx.pack_len || pack.len() < 32 {
        return bad("pack length differs from its index");
    }
    let body = &pack[..pack.len() - 32];
    if blake3::hash(body).as_bytes() != idx.checksum() || pack[pack.len() - 32..] != idx.checksum()[..] {
        return bad("pack checksum mismatch");
    }
    let mut out = Vec::with_capacity(idx.len());
    for (id, off, len) in idx.iter() {
        let (off, len) = (off as usize, len as usize);
        let end = off.checked_add(len).filter(|e| *e <= body.len());
        let Some(end) = end else {
            return bad("index entry outside the pack");
        };
        let enc = decode_payload_max(idx.version(), &pack[off..end], MAX_OBJECT).or_else(|e| bad(format!("object {id}: {e}")))?;
        if id_of_encoded(&enc) != id {
            return bad(format!("object {id} fails hash verification"));
        }
        out.push((id, enc.into_owned()));
    }
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Hex of a pack checksum, as stored in `meta`.
pub fn checksum_hex(idx: &PackIndex) -> String {
    hex(idx.checksum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ShelfMeta {
        ShelfMeta {
            principal: "alice@x.io".into(),
            name: "wip-1".into(),
            message: "half done\tthing".into(),
            author: "Alice <a@x.io>".into(),
            created_ms: 5,
            expires_ms: Some(99),
            zones: vec![ShelfZone {
                zone: "root".into(),
                branch: "main".into(),
                base: None,
                commit: ObjectId::from_hex(&"ab".repeat(32)).unwrap(),
            }],
            bases: vec![ObjectId::from_hex(&"cd".repeat(32)).unwrap()],
            objects: 3,
            pack_bytes: 100,
            pack_checksum: "0".repeat(64),
        }
    }

    #[test]
    fn meta_round_trips_and_is_strict() {
        let mut m = sample();
        let back = ShelfMeta::parse(&m.encode()).unwrap();
        m.message = "half done thing".into(); // control characters are flattened
        assert_eq!(back, m);
        assert!(ShelfMeta::parse(b"junk").is_err());
        let mut t = String::from_utf8(m.encode()).unwrap();
        t.push_str("surprise\t1\n");
        assert!(ShelfMeta::parse(t.as_bytes()).is_err());
        assert!(ShelfMeta::parse(&m.encode().into_iter().take(40).collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn keys_and_names() {
        assert_eq!(parse_key("shelves/a@b/n1/meta"), Some(("a@b", "n1", "meta")));
        assert_eq!(parse_key("shelves/a/n/other"), None);
        assert_eq!(parse_key("shelves/a/n/meta/x"), None);
        assert_eq!(parse_key("shelves/../n/meta"), None);
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_principal("a/b").is_err());
        assert_eq!(owner_of("shelves/bob/x/pack"), Some("bob"));
    }
}
