//! Lazily parsed tag table of [`crate::meta::Refs`].
//!
//! `.vcs/refs` is read whole by nearly every command, but most commands never look at tags. The
//! tag section is validated at decode time with a pass that allocates nothing (so a corrupt file
//! is still refused up front), then kept as raw bytes. The `BTreeMap` is built on first access
//! through `Deref`; a mutation (`DerefMut`) drops the raw bytes. An untouched table is written
//! back byte for byte, without being parsed at all.

use crate::error::{Error, Result};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};
use vcs_objects::codec::{DecodeError, Reader, Writer};
use vcs_objects::names::{validate_branch_name, validate_zone_name};
use vcs_objects::ObjectId;

pub type TagMap = BTreeMap<(String, String), ObjectId>;

#[derive(Clone, Default)]
pub struct Tags {
    /// The encoded section (count, then entries) of a strictly ascending, validated table.
    raw: Option<Arc<[u8]>>,
    map: OnceLock<TagMap>,
}

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("refs: {e}"))
}

fn utf8(b: &[u8]) -> Result<&str> {
    std::str::from_utf8(b).map_err(|_| corrupt("utf-8"))
}

impl Tags {
    /// Decode the trailing tag section (varint count + entries). Validates every name without
    /// allocating. A table that is not strictly ascending (never written by this code) is parsed
    /// eagerly, last duplicate wins, as before.
    pub(crate) fn decode_section(section: &[u8]) -> Result<Self> {
        let mut r = Reader::new(section);
        let n = r.count(34).map_err(corrupt)?;
        if n == 0 {
            return Err(corrupt("empty tag list"));
        }
        let mut prev: Option<(&[u8], &[u8])> = None;
        let mut ascending = true;
        for _ in 0..n {
            let a = r.bytes(64).map_err(corrupt)?;
            let t = r.bytes(200).map_err(corrupt)?;
            r.raw(32).map_err(corrupt)?;
            validate_zone_name(utf8(a)?)?;
            validate_branch_name(utf8(t)?)?;
            if prev.is_some_and(|p| p >= (a, t)) {
                ascending = false;
            }
            prev = Some((a, t));
        }
        r.finish().map_err(corrupt)?;
        if ascending {
            return Ok(Tags {
                raw: Some(section.into()),
                map: OnceLock::new(),
            });
        }
        let map = Self::parse(section).map_err(|_| corrupt(DecodeError::Eof))?;
        Ok(Tags::from(map))
    }

    /// Parse a section already validated by `decode_section`.
    fn parse(section: &[u8]) -> std::result::Result<TagMap, DecodeError> {
        let mut r = Reader::new(section);
        let n = r.varint()? as usize;
        let mut v: Vec<((String, String), ObjectId)> = Vec::with_capacity(n);
        for _ in 0..n {
            let a = r.str(64)?;
            let t = r.str(200)?;
            v.push(((a, t), r.id()?));
        }
        // strictly ascending input: bulk build, no rebalancing
        Ok(v.into_iter().collect())
    }

    /// The parsed map (built on first use).
    fn map(&self) -> &TagMap {
        self.map.get_or_init(|| match &self.raw {
            Some(raw) => Self::parse(raw).expect("tag section validated at decode"),
            None => TagMap::new(),
        })
    }

    /// Append the encoding (nothing when empty) to `w`.
    pub(crate) fn encode_into(&self, w: &mut Writer) {
        match (&self.raw, self.map.get()) {
            (Some(raw), _) => {
                w.raw(raw);
            }
            (None, Some(m)) if !m.is_empty() => {
                w.varint(m.len() as u64);
                for ((a, n), id) in m {
                    w.str(a).str(n).id(id);
                }
            }
            _ => {}
        }
    }

    pub fn into_map(self) -> TagMap {
        self.map();
        self.map.into_inner().unwrap_or_default()
    }

    /// True when no entry has been parsed or changed since decode (test hook).
    pub fn is_unparsed(&self) -> bool {
        self.raw.is_some() && self.map.get().is_none()
    }
}

impl From<TagMap> for Tags {
    fn from(m: TagMap) -> Self {
        let cell = OnceLock::new();
        let _ = cell.set(m);
        Tags { raw: None, map: cell }
    }
}

impl Deref for Tags {
    type Target = TagMap;
    fn deref(&self) -> &TagMap {
        self.map()
    }
}

impl DerefMut for Tags {
    fn deref_mut(&mut self) -> &mut TagMap {
        self.map();
        self.raw = None;
        self.map.get_mut().expect("initialized")
    }
}

impl<'a> IntoIterator for &'a Tags {
    type Item = (&'a (String, String), &'a ObjectId);
    type IntoIter = std::collections::btree_map::Iter<'a, (String, String), ObjectId>;
    fn into_iter(self) -> Self::IntoIter {
        self.map().iter()
    }
}

impl PartialEq for Tags {
    fn eq(&self, o: &Self) -> bool {
        match (&self.raw, &o.raw) {
            (Some(a), Some(b)) if a == b => true,
            _ => self.map() == o.map(),
        }
    }
}
impl PartialEq<TagMap> for Tags {
    fn eq(&self, o: &TagMap) -> bool {
        self.map() == o
    }
}

impl PartialEq<Tags> for TagMap {
    fn eq(&self, o: &Tags) -> bool {
        o.map() == self
    }
}

impl std::fmt::Debug for Tags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.map().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use crate::meta::Refs;
    use vcs_objects::ObjectId;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes([n; 32])
    }

    fn sample(n: usize) -> Refs {
        let mut r = Refs::default();
        r.branches.insert(("root".into(), "main".into()), id(1));
        for i in 0..n {
            r.tags
                .insert((if i % 3 == 0 { "z" } else { "root" }.into(), format!("v{i}")), id((i % 250) as u8));
        }
        r
    }

    #[test]
    fn untouched_tags_are_not_parsed_and_roundtrip_byte_for_byte() {
        let bytes = sample(500).encode();
        let r = Refs::decode(&bytes).unwrap();
        assert!(r.tags.is_unparsed());
        assert_eq!(r.encode(), bytes, "raw section re-emitted unchanged");
        assert!(r.tags.is_unparsed(), "encode does not parse");
        assert_eq!(r.tags.len(), 500);
        assert!(!r.tags.is_unparsed());
        assert_eq!(r.encode(), bytes, "parsed but unchanged also identical");
    }

    #[test]
    fn mutation_drops_raw_bytes_and_matches_eager_build() {
        let bytes = sample(100).encode();
        let mut r = Refs::decode(&bytes).unwrap();
        r.tags.insert(("root".into(), "new".into()), id(9));
        r.tags.remove(&("root".into(), "v1".into()));
        let again = Refs::decode(&r.encode()).unwrap();
        assert_eq!(again.tags, r.tags);
        assert_eq!(again.tags.len(), 100);
        assert_eq!(again.tags.get(&("root".to_owned(), "new".to_owned())), Some(&id(9)));
        r.tags.clear();
        let none = Refs::decode(&r.encode()).unwrap();
        assert!(none.tags.is_empty());
    }

    #[test]
    fn corrupt_tag_sections_are_refused_up_front() {
        let good = sample(3).encode();
        // bad tag name inside the section
        let mut bad = good.clone();
        let pos = bad.windows(2).position(|w| w == b"v1").unwrap();
        bad[pos] = b'/';
        bad[pos + 1] = b'/';
        assert!(Refs::decode(&bad).is_err());
        // truncated
        assert!(Refs::decode(&good[..good.len() - 5]).is_err());
        // trailing junk
        let mut junk = good.clone();
        junk.push(0);
        assert!(Refs::decode(&junk).is_err());
    }

    #[test]
    fn unsorted_or_duplicate_sections_still_decode_last_wins() {
        use vcs_objects::codec::Writer;
        let mut w = Writer::new();
        w.varint(0).varint(0).varint(2);
        w.str("root").str("b").id(&id(1));
        w.str("root").str("a").id(&id(2));
        let mut bytes = b"AGRF".to_vec();
        bytes.push(1);
        bytes.extend(w.finish());
        let r = Refs::decode(&bytes).unwrap();
        assert!(!r.tags.is_unparsed());
        assert_eq!(r.tags.len(), 2);
    }
}
