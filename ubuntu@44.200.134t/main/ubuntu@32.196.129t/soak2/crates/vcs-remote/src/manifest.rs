//! The authoritative remote state: one small object replaced by compare-and-swap.
//! Its successful CAS is the **linearization point** of every publish.

use std::collections::BTreeMap;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_branch_name, validate_zone_name};
use vcs_objects::ObjectId;

pub const KEY: &str = "manifest";
/// Applied-transaction ids retained for idempotency. A retry older than this window cannot
/// be recognized as already-applied (it is then rejected as Diverged, never double-applied,
/// because its `old` heads no longer match).
pub const MAX_APPLIED: usize = 4096;
/// Size of the v1-v4 encoding above which [`Manifest::encode`] switches to the compact v5 layout.
pub const COMPACT_THRESHOLD: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub generation: u64,
    pub branches: BTreeMap<(String, String), ObjectId>,
    pub snapshots: BTreeMap<String, ObjectId>,
    pub applied: Vec<ObjectId>,
    /// Zone name -> owned path prefix (non-root zones). Needed so a clone knows the layout.
    pub zones: BTreeMap<String, String>,
    /// Zone id -> display name, only for zones renamed after they got commits (the id is the
    /// name embedded in commits and keys everything above). Absent entry: the id is the name.
    pub labels: BTreeMap<String, String>,
    /// (zone, zone-relative path) -> lock. Empty unless locks are used (docs/locks.md).
    pub locks: BTreeMap<(String, String), Lock>,
    /// Fencing counter; never reused.
    pub next_token: u64,
    /// High-water mark of validator time (unix ms); never decreases.
    pub time_hw: u64,
    /// (zone, tag name) -> a commit (lightweight tag) or an annotated tag object. Empty unless
    /// tags were published; only then is the v4 format used.
    pub tags: BTreeMap<(String, String), ObjectId>,
}

/// One lock record. `holder` is the validator-trusted principal, not the self-asserted author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    pub holder: String,
    pub token: u64,
    pub acquired_at: u64,
    pub expires_at: u64,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
#[error("malformed remote manifest: {0}")]
pub struct ManifestError(pub String);

/// Keys must arrive strictly ascending (the encoder writes map order), so one manifest has
/// exactly one encoding and a duplicate key cannot silently overwrite an earlier one.
fn ascending<K: Ord>(prev: Option<&K>, next: &K) -> Result<(), ManifestError> {
    if prev.is_some_and(|p| p >= next) {
        return Err(ManifestError("keys not strictly ascending".into()));
    }
    Ok(())
}

impl Manifest {
    pub fn head(&self, zone: &str, branch: &str) -> Option<ObjectId> {
        self.branches.get(&(zone.to_owned(), branch.to_owned())).copied()
    }

    /// True once any lock state exists. Only then is the v2 format used, so manifests of
    /// repositories that never lock stay byte-identical to v1 (old readers reject v2).
    pub fn has_lock_state(&self) -> bool {
        !self.locks.is_empty() || self.next_token != 0 || self.time_hw != 0
    }

    /// Format version: 4 once tags exist (it carries everything v3 does), else 3 once lock state exists, else 2 if zone labels exist, else 1 (so
    /// repositories using neither stay byte-identical to v1; older readers reject newer versions).
    fn version(&self) -> u8 {
        if !self.tags.is_empty() {
            4
        } else if self.has_lock_state() {
            3
        } else if !self.labels.is_empty() {
            2
        } else {
            1
        }
    }

    /// Canonical bytes. Manifests whose v1-v4 encoding exceeds [`COMPACT_THRESHOLD`] use the
    /// compact v5 layout (zone-grouped, prefix-compressed names); smaller ones keep their v1-v4
    /// bytes so repositories that never grow large stay readable by older binaries.
    pub fn encode(&self) -> Vec<u8> {
        let legacy = self.encode_legacy();
        if legacy.len() > COMPACT_THRESHOLD {
            self.encode_compact()
        } else {
            legacy
        }
    }

    /// The v1-v4 layout (the version is chosen by which features the manifest uses).
    pub fn encode_legacy(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(b"AGMF")
            .u8(self.version())
            .varint(self.generation)
            .varint(self.branches.len() as u64);
        for ((a, b), id) in &self.branches {
            w.str(a).str(b).id(id);
        }
        w.varint(self.snapshots.len() as u64);
        for (n, id) in &self.snapshots {
            w.str(n).id(id);
        }
        w.varint(self.applied.len() as u64);
        for id in &self.applied {
            w.id(id);
        }
        w.varint(self.zones.len() as u64);
        for (n, p) in &self.zones {
            w.str(n).str(p);
        }
        if self.version() >= 2 {
            w.varint(self.labels.len() as u64);
            for (i, l) in &self.labels {
                w.str(i).str(l);
            }
        }
        if self.version() >= 3 {
            w.varint(self.next_token).varint(self.time_hw).varint(self.locks.len() as u64);
            for ((z, p), l) in &self.locks {
                w.str(z)
                    .str(p)
                    .str(&l.holder)
                    .varint(l.token)
                    .varint(l.acquired_at)
                    .varint(l.expires_at)
                    .str(&l.reason);
            }
        }
        if self.version() == 4 {
            w.varint(self.tags.len() as u64);
            for ((z, n), id) in &self.tags {
                w.str(z).str(n).id(id);
            }
        }
        w.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
        let mut r = Reader::new(bytes);
        if r.raw(4).map_err(e)? != b"AGMF" {
            return Err(ManifestError("bad magic".into()));
        }
        let v = r.u8().map_err(e)?;
        if v == 5 {
            return Self::decode_compact(bytes);
        }
        if !(1..=4).contains(&v) {
            return Err(ManifestError(format!("unsupported version {v}")));
        }
        let mut m = Manifest {
            generation: r.varint().map_err(e)?,
            ..Default::default()
        };
        for _ in 0..r.count(34).map_err(e)? {
            let a = r.str(64).map_err(e)?;
            let b = r.str(200).map_err(e)?;
            validate_zone_name(&a).map_err(|x| ManifestError(x.to_string()))?;
            validate_branch_name(&b).map_err(|x| ManifestError(x.to_string()))?;
            ascending(m.branches.last_key_value().map(|(k, _)| k), &(a.clone(), b.clone()))?;
            m.branches.insert((a, b), r.id().map_err(e)?);
        }
        for _ in 0..r.count(33).map_err(e)? {
            let n = r.str(200).map_err(e)?;
            validate_branch_name(&n).map_err(|x| ManifestError(x.to_string()))?;
            ascending(m.snapshots.last_key_value().map(|(k, _)| k), &n)?;
            m.snapshots.insert(n, r.id().map_err(e)?);
        }
        let n = r.count(32).map_err(e)?;
        if n > MAX_APPLIED {
            return Err(ManifestError("applied list too long".into()));
        }
        for _ in 0..n {
            m.applied.push(r.id().map_err(e)?);
        }
        for _ in 0..r.count(2).map_err(e)? {
            let n = r.str(64).map_err(e)?;
            let p = r.str(4096).map_err(e)?;
            validate_zone_name(&n).map_err(|x| ManifestError(x.to_string()))?;
            vcs_objects::names::validate_path(&p).map_err(|x| ManifestError(x.to_string()))?;
            ascending(m.zones.last_key_value().map(|(k, _)| k), &n)?;
            m.zones.insert(n, p);
        }
        if v >= 2 {
            for _ in 0..r.count(2).map_err(e)? {
                let i = r.str(64).map_err(e)?;
                let l = r.str(64).map_err(e)?;
                validate_zone_name(&i).map_err(|x| ManifestError(x.to_string()))?;
                validate_zone_name(&l).map_err(|x| ManifestError(x.to_string()))?;
                ascending(m.labels.last_key_value().map(|(k, _)| k), &i)?;
                m.labels.insert(i, l);
            }
            // the encoder writes v2 only when labels exist; an empty v2 would re-encode as v1
            if v == 2 && m.labels.is_empty() {
                return Err(ManifestError("v2 manifest without labels".into()));
            }
        }
        if v >= 3 {
            m.next_token = r.varint().map_err(e)?;
            m.time_hw = r.varint().map_err(e)?;
            for _ in 0..r.count(7).map_err(e)? {
                let z = r.str(64).map_err(e)?;
                let p = r.str(4096).map_err(e)?;
                validate_zone_name(&z).map_err(|x| ManifestError(x.to_string()))?;
                vcs_objects::names::validate_path(&p).map_err(|x| ManifestError(x.to_string()))?;
                let l = Lock {
                    holder: r.str(512).map_err(e)?,
                    token: r.varint().map_err(e)?,
                    acquired_at: r.varint().map_err(e)?,
                    expires_at: r.varint().map_err(e)?,
                    reason: r.str(1024).map_err(e)?,
                };
                if l.token >= m.next_token {
                    return Err(ManifestError("lock token not below next_token".into()));
                }
                ascending(m.locks.last_key_value().map(|(k, _)| k), &(z.clone(), p.clone()))?;
                m.locks.insert((z, p), l);
            }
            if v == 3 && !m.has_lock_state() {
                return Err(ManifestError("v3 manifest without lock state".into()));
            }
        }
        if v == 4 {
            for _ in 0..r.count(34).map_err(e)? {
                let z = r.str(64).map_err(e)?;
                let n = r.str(200).map_err(e)?;
                validate_zone_name(&z).map_err(|x| ManifestError(x.to_string()))?;
                validate_branch_name(&n).map_err(|x| ManifestError(x.to_string()))?;
                ascending(m.tags.last_key_value().map(|(k, _)| k), &(z.clone(), n.clone()))?;
                m.tags.insert((z, n), r.id().map_err(e)?);
            }
            // the encoder writes v4 only when tags exist; an empty v4 would re-encode as an older version
            if m.tags.is_empty() {
                return Err(ManifestError("v4 manifest without tags".into()));
            }
        }
        r.finish().map_err(e)?;
        Ok(m)
    }
}

/// Longest common prefix of `a` and `b` in bytes, cut back to a char boundary.
fn shared_prefix(a: &str, b: &str) -> usize {
    let mut n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while !a.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// `s` as (bytes shared with `prev`, remaining suffix).
fn put_pname(w: &mut Writer, prev: &str, s: &str) {
    let k = shared_prefix(prev, s);
    w.varint(k as u64).str(&s[k..]);
}

fn get_pname(r: &mut Reader, prev: &str, max: usize) -> Result<String, ManifestError> {
    let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
    let k = r.varint().map_err(e)? as usize;
    let suffix = r.str(max).map_err(e)?;
    if k > prev.len() || !prev.is_char_boundary(k) || k + suffix.len() > max {
        return Err(ManifestError("bad prefix-compressed name".into()));
    }
    let full = format!("{}{}", &prev[..k], suffix);
    if shared_prefix(prev, &full) != k {
        return Err(ManifestError("non-canonical prefix length".into()));
    }
    Ok(full)
}

impl Manifest {
    /// Compact layout (v5). Same content as v1-v4, but entries sharing a zone are grouped under
    /// one copy of the zone name and sorted names are stored as (shared prefix length, suffix)
    /// against their predecessor. Object ids (32 incompressible bytes each) are unchanged.
    pub fn encode_compact(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(b"AGMF")
            .u8(5)
            .varint(self.generation)
            .varint(self.next_token)
            .varint(self.time_hw);
        fn group<'a, V>(w: &mut Writer, m: &'a BTreeMap<(String, String), V>, mut put: impl FnMut(&mut Writer, &str, &'a V)) {
            let mut groups: Vec<(&str, Vec<(&str, &V)>)> = Vec::new();
            for ((z, n), v) in m {
                match groups.last_mut() {
                    Some((gz, items)) if *gz == z => items.push((n, v)),
                    _ => groups.push((z, vec![(n, v)])),
                }
            }
            w.varint(groups.len() as u64);
            let mut pz = "";
            for (z, items) in groups {
                put_pname(w, pz, z);
                pz = z;
                w.varint(items.len() as u64);
                let mut pn = "";
                for (n, v) in items {
                    put_pname(w, pn, n);
                    pn = n;
                    put(w, n, v);
                }
            }
        }
        group(&mut w, &self.branches, |w, _, id| {
            w.id(id);
        });
        w.varint(self.snapshots.len() as u64);
        let mut prev = "";
        for (n, id) in &self.snapshots {
            put_pname(&mut w, prev, n);
            prev = n;
            w.id(id);
        }
        w.varint(self.applied.len() as u64);
        for id in &self.applied {
            w.id(id);
        }
        w.varint(self.zones.len() as u64);
        let (mut pn, mut pp) = ("", "");
        for (n, p) in &self.zones {
            put_pname(&mut w, pn, n);
            put_pname(&mut w, pp, p);
            (pn, pp) = (n, p);
        }
        w.varint(self.labels.len() as u64);
        let mut prev = "";
        for (i, l) in &self.labels {
            put_pname(&mut w, prev, i);
            prev = i;
            w.str(l);
        }
        group(&mut w, &self.locks, |w, _, l| {
            w.str(&l.holder)
                .varint(l.token)
                .varint(l.acquired_at)
                .varint(l.expires_at)
                .str(&l.reason);
        });
        group(&mut w, &self.tags, |w, _, id| {
            w.id(id);
        });
        w.finish()
    }

    fn decode_compact(bytes: &[u8]) -> Result<Self, ManifestError> {
        let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
        let bad = |x: vcs_objects::names::NameError| ManifestError(x.to_string());
        let mut r = Reader::new(bytes);
        r.raw(5).map_err(e)?; // magic and version, checked by the caller
        let mut m = Manifest {
            generation: r.varint().map_err(e)?,
            next_token: r.varint().map_err(e)?,
            time_hw: r.varint().map_err(e)?,
            ..Default::default()
        };
        // Reads a zone-grouped map; `item` reads the value (and validates the name) of one entry.
        fn group<V>(
            r: &mut Reader,
            out: &mut BTreeMap<(String, String), V>,
            mut item: impl FnMut(&mut Reader, &str, &str) -> Result<V, ManifestError>,
        ) -> Result<(), ManifestError> {
            let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
            let mut pz = String::new();
            let mut first = true;
            for _ in 0..r.count(4).map_err(e)? {
                let z = get_pname(r, &pz, 64)?;
                if !first && z <= pz {
                    return Err(ManifestError("keys not strictly ascending".into()));
                }
                first = false;
                pz = z.clone();
                let n = r.count(33).map_err(e)?;
                if n == 0 {
                    return Err(ManifestError("empty zone group".into()));
                }
                let mut pn = String::new();
                for i in 0..n {
                    let name = get_pname(r, &pn, 4096)?;
                    if i > 0 && name <= pn {
                        return Err(ManifestError("keys not strictly ascending".into()));
                    }
                    pn = name.clone();
                    let v = item(r, &z, &name)?;
                    out.insert((z.clone(), name), v);
                }
            }
            Ok(())
        }
        let ids = |r: &mut Reader, z: &str, n: &str| -> Result<ObjectId, ManifestError> {
            validate_zone_name(z).map_err(|x| ManifestError(x.to_string()))?;
            validate_branch_name(n).map_err(|x| ManifestError(x.to_string()))?;
            r.id().map_err(|x| ManifestError(x.to_string()))
        };
        group(&mut r, &mut m.branches, ids)?;
        let mut prev = String::new();
        for i in 0..r.count(33).map_err(e)? {
            let n = get_pname(&mut r, &prev, 200)?;
            validate_branch_name(&n).map_err(bad)?;
            if i > 0 && n <= prev {
                return Err(ManifestError("keys not strictly ascending".into()));
            }
            prev = n.clone();
            m.snapshots.insert(n, r.id().map_err(e)?);
        }
        let n = r.count(32).map_err(e)?;
        if n > MAX_APPLIED {
            return Err(ManifestError("applied list too long".into()));
        }
        for _ in 0..n {
            m.applied.push(r.id().map_err(e)?);
        }
        let (mut pn, mut pp) = (String::new(), String::new());
        for i in 0..r.count(2).map_err(e)? {
            let n = get_pname(&mut r, &pn, 64)?;
            let p = get_pname(&mut r, &pp, 4096)?;
            validate_zone_name(&n).map_err(bad)?;
            vcs_objects::names::validate_path(&p).map_err(bad)?;
            if i > 0 && n <= pn {
                return Err(ManifestError("keys not strictly ascending".into()));
            }
            (pn, pp) = (n.clone(), p.clone());
            m.zones.insert(n, p);
        }
        let mut prev = String::new();
        for i in 0..r.count(2).map_err(e)? {
            let id = get_pname(&mut r, &prev, 64)?;
            let l = r.str(64).map_err(e)?;
            validate_zone_name(&id).map_err(bad)?;
            validate_zone_name(&l).map_err(bad)?;
            if i > 0 && id <= prev {
                return Err(ManifestError("keys not strictly ascending".into()));
            }
            prev = id.clone();
            m.labels.insert(id, l);
        }
        let next_token = m.next_token;
        group(&mut r, &mut m.locks, |r, z, p| {
            validate_zone_name(z).map_err(|x| ManifestError(x.to_string()))?;
            vcs_objects::names::validate_path(p).map_err(|x| ManifestError(x.to_string()))?;
            let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
            let l = Lock {
                holder: r.str(512).map_err(e)?,
                token: r.varint().map_err(e)?,
                acquired_at: r.varint().map_err(e)?,
                expires_at: r.varint().map_err(e)?,
                reason: r.str(1024).map_err(e)?,
            };
            if l.token >= next_token {
                return Err(ManifestError("lock token not below next_token".into()));
            }
            Ok(l)
        })?;
        group(&mut r, &mut m.tags, ids)?;
        r.finish().map_err(e)?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_reject_garbage() {
        let mut m = Manifest {
            generation: 7,
            ..Default::default()
        };
        m.branches.insert(("a".into(), "main".into()), ObjectId::from_bytes([1; 32]));
        m.applied.push(ObjectId::from_bytes([2; 32]));
        assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
        m.labels.insert("engine".into(), "core".into());
        assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
        m.labels.clear();
        assert_eq!(m.encode()[4], 1, "no labels and no locks keeps the v1 encoding");
        assert!(Manifest::decode(b"junk").is_err());
        let mut bad = m.encode();
        bad.push(0);
        assert!(Manifest::decode(&bad).is_err());
    }

    #[test]
    fn v3_roundtrip_only_with_lock_state() {
        let mut m = Manifest {
            next_token: 2,
            time_hw: 99,
            ..Default::default()
        };
        m.locks.insert(
            ("z".into(), "a/b.bin".into()),
            Lock {
                holder: "alice".into(),
                token: 1,
                acquired_at: 10,
                expires_at: 50,
                reason: "editing".into(),
            },
        );
        let b = m.encode();
        assert_eq!(b[4], 3);
        assert_eq!(Manifest::decode(&b).unwrap(), m);
        let mut bad = m.clone();
        bad.locks.get_mut(&("z".into(), "a/b.bin".into())).unwrap().token = 2;
        assert!(Manifest::decode(&bad.encode()).is_err(), "token must be below next_token");
    }

    #[test]
    fn tags_use_v4_and_tag_free_manifests_keep_their_bytes() {
        let mut m = Manifest {
            generation: 3,
            ..Default::default()
        };
        m.branches.insert(("a".into(), "main".into()), ObjectId::from_bytes([1; 32]));
        let before = m.encode();
        assert_eq!(before[4], 1);
        m.tags.insert(("a".into(), "v1".into()), ObjectId::from_bytes([5; 32]));
        let b = m.encode();
        assert_eq!(b[4], 4);
        assert_eq!(Manifest::decode(&b).unwrap(), m);
        m.tags.clear();
        assert_eq!(m.encode(), before, "tag-free manifest is byte-identical to before tags existed");
        // tags and locks together
        m.tags.insert(("a".into(), "v1".into()), ObjectId::from_bytes([5; 32]));
        m.next_token = 2;
        m.locks.insert(
            ("a".into(), "f".into()),
            Lock {
                holder: "h".into(),
                token: 1,
                acquired_at: 1,
                expires_at: 2,
                reason: String::new(),
            },
        );
        assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
    }

    fn big(zones: usize, branches: usize) -> Manifest {
        let mut m = Manifest {
            generation: 9,
            next_token: 5,
            time_hw: 77,
            ..Default::default()
        };
        for z in 0..zones {
            let zn = format!("zone-{z:05}");
            for b in 0..branches {
                m.branches.insert(
                    (zn.clone(), format!("feature/topic-{b}")),
                    ObjectId::from_bytes([(z + b) as u8; 32]),
                );
            }
            m.zones.insert(zn.clone(), format!("services/{zn}/src"));
            if z % 7 == 0 {
                m.tags.insert((zn.clone(), "v1.0".into()), ObjectId::from_bytes([3; 32]));
                m.labels.insert(zn.clone(), format!("renamed-{z}"));
            }
            if z % 11 == 0 {
                m.locks.insert(
                    (zn.clone(), "art/héllo.bin".into()),
                    Lock {
                        holder: "alice".into(),
                        token: 1 + (z % 4) as u64,
                        acquired_at: 5,
                        expires_at: 9,
                        reason: "x".into(),
                    },
                );
            }
        }
        m.snapshots.insert("nightly-1".into(), ObjectId::from_bytes([8; 32]));
        m.snapshots.insert("nightly-2".into(), ObjectId::from_bytes([9; 32]));
        for i in 0..50u8 {
            m.applied.push(ObjectId::from_bytes([i; 32]));
        }
        m
    }

    #[test]
    fn compact_v5_roundtrips_is_smaller_and_small_manifests_stay_legacy() {
        let small = big(3, 2);
        assert!(small.encode_legacy().len() < COMPACT_THRESHOLD);
        assert_eq!(small.encode(), small.encode_legacy(), "small manifests keep their v1-v4 bytes");
        let m = big(2000, 5);
        let legacy = m.encode_legacy();
        let compact = m.encode_compact();
        assert!(legacy.len() > COMPACT_THRESHOLD);
        assert_eq!(m.encode(), compact);
        assert_eq!(compact[4], 5);
        assert!(compact.len() < legacy.len(), "{} < {}", compact.len(), legacy.len());
        assert_eq!(Manifest::decode(&compact).unwrap(), m);
        assert_eq!(Manifest::decode(&legacy).unwrap(), m, "legacy bytes are still read");
        // empty and single-entry manifests also survive the compact layout
        for m in [Manifest::default(), big(1, 1)] {
            assert_eq!(Manifest::decode(&m.encode_compact()).unwrap(), m);
        }
    }

    #[test]
    fn compact_v5_rejects_non_canonical_and_corrupt_input() {
        let m = big(40, 3);
        let good = m.encode_compact();
        assert_eq!(Manifest::decode(&good).unwrap(), m);
        for i in 5..good.len() {
            let mut b = good.clone();
            b[i] ^= 0x55;
            // never panics; if it still decodes it must re-encode to exactly these bytes
            if let Ok(d) = Manifest::decode(&b) {
                assert_eq!(d.encode_compact(), b, "byte {i}: accepted a non-canonical encoding");
            }
        }
        for n in 0..good.len() {
            assert!(Manifest::decode(&good[..n]).is_err(), "truncated at {n}");
        }
        let mut t = good.clone();
        t.push(0);
        assert!(Manifest::decode(&t).is_err());
        // a lock token at or above next_token is refused here too
        let mut bad = m.clone();
        bad.next_token = 1;
        assert!(Manifest::decode(&bad.encode_compact()).is_err());
    }
}
