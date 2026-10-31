//! The authoritative remote state: one small object replaced by compare-and-swap.
//! Its successful CAS is the **linearization point** of every publish.

use std::collections::BTreeMap;
use vcs_objects::codec::{Reader, Writer};
use vcs_objects::names::{validate_zone_name, validate_branch_name};
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
}

#[derive(Debug, thiserror::Error)]
#[error("malformed remote manifest: {0}")]
pub struct ManifestError(pub String);

impl Manifest {
    pub fn head(&self, zone: &str, branch: &str) -> Option<ObjectId> {
        self.branches.get(&(zone.to_owned(), branch.to_owned())).copied()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(b"AGMF").u8(1).varint(self.generation).varint(self.branches.len() as u64);
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
        w.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        let e = |x: vcs_objects::DecodeError| ManifestError(x.to_string());
        let mut r = Reader::new(bytes);
        if r.raw(4).map_err(e)? != b"AGMF" {
            return Err(ManifestError("bad magic".into()));
        }
        let v = r.u8().map_err(e)?;
        if v != 1 {
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
            m.branches.insert((a, b), r.id().map_err(e)?);
        }
        for _ in 0..r.count(33).map_err(e)? {
            let n = r.str(200).map_err(e)?;
            validate_branch_name(&n).map_err(|x| ManifestError(x.to_string()))?;
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
            m.zones.insert(n, p);
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
        assert!(Manifest::decode(b"junk").is_err());
        let mut bad = m.encode();
        bad.push(0);
        assert!(Manifest::decode(&bad).is_err());
    }
}
