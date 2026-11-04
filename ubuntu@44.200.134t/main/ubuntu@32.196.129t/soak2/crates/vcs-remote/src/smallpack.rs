//! Whole-pack reads for small remote packs, to cut clone request counts and round trips.
//!
//! A clone reads each pack in several dependent waves (index, trailer check, commits, trees,
//! file data), at least one ranged GET per wave per pack. For a pack that is small, the bytes
//! of those waves together are about as many as the pack, and one GET of the whole pack costs
//! one round trip instead of several. [`PackCatalog`](crate::packfetch::PackCatalog) therefore
//! reads every pack of at most [`small_pack_bytes`] with one plain GET, in the same parallel
//! wave as the index reads, and [`BodyBackend`] answers every later ranged read of that pack
//! (trailer check included) from memory.
//!
//! Safety: the body is only kept when its length equals the index's `pack_len` and its last 32
//! bytes equal the index checksum (the same test as the trailer check); anything else is dropped
//! and the normal ranged path reports the real error. Entries are still decoded and hash-checked
//! against their ids exactly as for ranged reads. The memory held is bounded by
//! [`small_pack_total`].

use crate::backend::{Backend, BackendError, Conditional, Versioned};
use std::collections::HashMap;
use std::sync::Arc;

/// Packs up to this many bytes are read whole (`VCS_SMALL_PACK_BYTES` overrides; 0 disables).
pub const DEFAULT_SMALL_PACK_BYTES: u64 = 1 << 20;
/// At most this many bytes of whole packs are read per catalog (`VCS_SMALL_PACK_TOTAL`).
pub const DEFAULT_SMALL_PACK_TOTAL: u64 = 16 << 20;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

pub fn small_pack_bytes() -> u64 {
    env_u64("VCS_SMALL_PACK_BYTES", DEFAULT_SMALL_PACK_BYTES)
}

pub fn small_pack_total() -> u64 {
    env_u64("VCS_SMALL_PACK_TOTAL", DEFAULT_SMALL_PACK_TOTAL)
}

/// Pack bodies by stem (`packs/<name>`).
pub type Bodies = HashMap<String, Vec<u8>>;

/// A backend that serves `get` and `get_range` of `<stem>.pack` from `bodies` and forwards
/// everything else.
pub struct BodyBackend<'a> {
    inner: &'a dyn Backend,
    bodies: Arc<Bodies>,
}

impl<'a> BodyBackend<'a> {
    pub fn new(inner: &'a dyn Backend, bodies: Arc<Bodies>) -> Self {
        Self { inner, bodies }
    }

    fn body(&self, key: &str) -> Option<&Vec<u8>> {
        if self.bodies.is_empty() {
            return None;
        }
        key.strip_suffix(".pack").and_then(|s| self.bodies.get(s))
    }
}

impl Backend for BodyBackend<'_> {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        match self.body(key) {
            Some(b) => Ok(Some(Versioned {
                data: b.clone(),
                tag: String::new(),
            })),
            None => self.inner.get(key),
        }
    }
    fn get_conditional(&self, key: &str, have: &str) -> Result<Conditional, BackendError> {
        self.inner.get_conditional(key, have)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        match self.body(key) {
            Some(d) => {
                let s = (offset as usize).min(d.len());
                let e = s.saturating_add(len as usize).min(d.len());
                Ok(Some(d[s..e].to_vec()))
            }
            None => self.inner.get_range(key, offset, len),
        }
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.exists(key)
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        self.inner.put_if_absent(key, data)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        self.inner.put_if_match(key, data, tag)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.inner.list(prefix)
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        self.inner.delete(key)
    }
    fn supports_gc(&self) -> bool {
        self.inner.supports_gc()
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        self.inner.now_ms()
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        self.inner.modified_ms(key)
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        self.inner.touch(key)
    }
}
