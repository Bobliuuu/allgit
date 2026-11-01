use crate::backend::{validate_key, Backend, BackendError, Versioned};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// One-shot fault injected into the next `put_if_match`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CasFault {
    /// Request lost before reaching the store: nothing applied, caller sees `Unavailable`.
    FailBefore,
    /// Applied, but the response is lost: caller sees `Unavailable`.
    LostAck,
}

#[derive(Default)]
struct Inner {
    objects: BTreeMap<String, (Vec<u8>, u64)>,
    next_version: u64,
    fault: Option<CasFault>,
    requests: u64,
    bytes_read: u64,
    /// Every successful write of any key in order (key, data): lets tests audit history.
    history: Vec<(String, Vec<u8>)>,
    read_log: Vec<String>,
    /// key -> last write/touch time (unix ms of the backend clock).
    mtime: BTreeMap<String, u64>,
    /// Fixed clock for tests; `None` = real time.
    clock: Option<u64>,
    /// Test hook: fail the next `delete` of a key (`Unavailable`), once.
    fail_delete: Option<String>,
    /// Opt-in: claim `supports_gc`, which makes publishers run the GC handshake.
    gc: bool,
}

impl Inner {
    fn now(&self) -> u64 {
        self.clock.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
    }
}

/// In-memory backend with fault injection and request accounting.
#[derive(Default)]
pub struct MemBackend {
    inner: Mutex<Inner>,
}

impl MemBackend {
    pub fn new() -> Self {
        Self::default()
    }
    /// Opt in to garbage collection: `supports_gc` becomes true, so publishers against this
    /// backend run the extra GC handshake (extra steps; the schedule-enumerating model tests
    /// of the plain protocol leave it off) and `remote_gc` accepts it.
    pub fn with_gc(self) -> Self {
        self.inner.lock().unwrap().gc = true;
        self
    }
    pub fn inject_cas_fault(&self, f: CasFault) {
        self.inner.lock().unwrap().fault = Some(f);
    }
    pub fn requests(&self) -> u64 {
        self.inner.lock().unwrap().requests
    }
    pub fn bytes_read(&self) -> u64 {
        self.inner.lock().unwrap().bytes_read
    }
    /// Keys that were read (GET / range) in order.
    pub fn read_log(&self) -> Vec<String> {
        self.inner.lock().unwrap().read_log.clone()
    }
    pub fn history(&self) -> Vec<(String, Vec<u8>)> {
        self.inner.lock().unwrap().history.clone()
    }
    pub fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().objects.keys().cloned().collect()
    }
    /// Test hook: corrupt object `id` wherever it lives. A loose object is replaced by `evil`;
    /// inside a pack the object's bytes are flipped in place (the pack keeps its length).
    #[doc(hidden)]
    pub fn tamper_object(&self, id: &vcs_objects::ObjectId, evil: Vec<u8>) {
        let h = id.to_hex();
        let loose = format!("objects/{}/{}", &h[..2], &h[2..]);
        if self.keys().contains(&loose) {
            return self.tamper(&loose, evil);
        }
        for k in self.keys().into_iter().filter(|k| k.starts_with("packs/") && k.ends_with(".idx")) {
            let idx = self.inner.lock().unwrap().objects[&k].0.clone();
            let idx = vcs_store::pack::PackIndex::parse(&idx, std::path::Path::new(&k)).unwrap();
            if let Some((off, len)) = idx.find(id) {
                let pk = format!("{}.pack", k.trim_end_matches(".idx"));
                let mut data = self.inner.lock().unwrap().objects[&pk].0.clone();
                for b in &mut data[off as usize..off as usize + len as usize] {
                    *b ^= 0x5A;
                }
                return self.tamper(&pk, data);
            }
        }
        panic!("tamper_object: {id} not on the remote");
    }
    /// Pin the backend clock (unix ms) so tests can age objects without sleeping.
    pub fn set_now_ms(&self, ms: u64) {
        self.inner.lock().unwrap().clock = Some(ms);
    }
    /// Advance a pinned clock (pins it at 0 first if it was not pinned).
    pub fn advance_ms(&self, d: u64) {
        let mut g = self.inner.lock().unwrap();
        let n = g.clock.unwrap_or(0) + d;
        g.clock = Some(n);
    }
    /// Make the next `delete(key)` fail with `Unavailable` (nothing is deleted).
    pub fn fail_next_delete(&self, key: &str) {
        self.inner.lock().unwrap().fail_delete = Some(key.to_owned());
    }
    /// Test hook: overwrite raw bytes (simulate tampering / corruption).
    pub fn tamper(&self, key: &str, data: Vec<u8>) {
        let mut g = self.inner.lock().unwrap();
        let v = g.next_version;
        g.next_version += 1;
        g.objects.insert(key.to_owned(), (data, v));
        let n = g.now();
        g.mtime.insert(key.to_owned(), n);
    }
}

impl Backend for MemBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        let r = g.objects.get(key).map(|(d, v)| Versioned {
            data: d.clone(),
            tag: v.to_string(),
        });
        if let Some(v) = &r {
            g.bytes_read += v.data.len() as u64;
            g.read_log.push(key.to_owned());
        }
        Ok(r)
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        let r = g.objects.get(key).map(|(d, _)| {
            let s = (offset as usize).min(d.len());
            let e = s.saturating_add(len as usize).min(d.len());
            d[s..e].to_vec()
        });
        if let Some(v) = &r {
            g.bytes_read += v.len() as u64;
            g.read_log.push(key.to_owned());
        }
        Ok(r)
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        Ok(g.objects.contains_key(key))
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        // The manifest create-only write is the CAS for an empty repository: faults apply too.
        let fault = if key == crate::manifest::KEY { g.fault.take() } else { None };
        if fault == Some(CasFault::FailBefore) {
            return Err(BackendError::Unavailable("injected: request lost".into()));
        }
        let created = !g.objects.contains_key(key);
        if created {
            let v = g.next_version;
            g.next_version += 1;
            g.objects.insert(key.to_owned(), (data.to_vec(), v));
            let n = g.now();
            g.mtime.insert(key.to_owned(), n);
            g.history.push((key.to_owned(), data.to_vec()));
        }
        if fault == Some(CasFault::LostAck) {
            return Err(BackendError::Unavailable("injected: response lost".into()));
        }
        Ok(created)
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        let fault = g.fault.take();
        if fault == Some(CasFault::FailBefore) {
            return Err(BackendError::Unavailable("injected: request lost".into()));
        }
        let ok = g.objects.get(key).is_some_and(|(_, v)| v.to_string() == tag);
        let res = if ok {
            let v = g.next_version;
            g.next_version += 1;
            g.objects.insert(key.to_owned(), (data.to_vec(), v));
            let n = g.now();
            g.mtime.insert(key.to_owned(), n);
            g.history.push((key.to_owned(), data.to_vec()));
            Some(v.to_string())
        } else {
            None
        };
        if fault == Some(CasFault::LostAck) {
            return Err(BackendError::Unavailable("injected: response lost".into()));
        }
        Ok(res)
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        Ok(g.objects.keys().filter(|k| k.starts_with(prefix)).cloned().collect())
    }
    fn delete(&self, key: &str) -> Result<(), BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        if g.fail_delete.as_deref() == Some(key) {
            g.fail_delete = None;
            return Err(BackendError::Unavailable("injected: delete lost".into()));
        }
        g.objects.remove(key);
        g.mtime.remove(key);
        Ok(())
    }
    fn supports_gc(&self) -> bool {
        self.inner.lock().unwrap().gc
    }
    fn now_ms(&self) -> Result<u64, BackendError> {
        Ok(self.inner.lock().unwrap().now())
    }
    fn modified_ms(&self, key: &str) -> Result<Option<u64>, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        Ok(g.mtime.get(key).copied())
    }
    fn touch(&self, key: &str) -> Result<bool, BackendError> {
        validate_key(key)?;
        let mut g = self.inner.lock().unwrap();
        g.requests += 1;
        if !g.objects.contains_key(key) {
            return Ok(false);
        }
        let n = g.now();
        g.mtime.insert(key.to_owned(), n);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conforms() {
        crate::backend::conformance(&MemBackend::new());
        crate::backend::conformance_gc(&MemBackend::new().with_gc());
    }
}
