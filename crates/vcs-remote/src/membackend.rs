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
    /// Test hook: overwrite raw bytes (simulate tampering / corruption).
    pub fn tamper(&self, key: &str, data: Vec<u8>) {
        let mut g = self.inner.lock().unwrap();
        let v = g.next_version;
        g.next_version += 1;
        g.objects.insert(key.to_owned(), (data, v));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conforms() {
        crate::backend::conformance(&MemBackend::new());
    }
}
