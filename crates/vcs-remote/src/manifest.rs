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

    pub fn encode(&self) -> Vec<u8> {
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
}
