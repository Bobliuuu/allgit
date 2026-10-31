use crate::backend::{validate_key, Backend, BackendError, Versioned};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Directory-backed authoritative backend (a shared filesystem or a local "remote").
/// Create-only uses `hard_link` (atomic, fails if target exists). CAS is serialized by an
/// `O_EXCL` lock file; the version tag is the BLAKE3 of the content (so ABA with identical
/// content is harmless: the swap would be a no-op change of state).
pub struct FsBackend {
    root: PathBuf,
}

fn err(e: std::io::Error) -> BackendError {
    BackendError::Other(e.to_string())
}

fn tag_of(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

impl FsBackend {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BackendError> {
        let root = root.into();
        fs::create_dir_all(root.join(".tmp")).map_err(err)?;
        Ok(Self { root })
    }
    fn path(&self, key: &str) -> Result<PathBuf, BackendError> {
        validate_key(key)?;
        Ok(self.root.join(key))
    }
    fn tmp_file(&self, data: &[u8]) -> Result<PathBuf, BackendError> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = self.root.join(".tmp").join(format!(
            "{}.{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut f = fs::File::create(&p).map_err(err)?;
        f.write_all(data).map_err(err)?;
        f.sync_all().map_err(err)?;
        Ok(p)
    }
    fn lock(&self) -> Result<LockFile, BackendError> {
        let p = self.root.join(".cas.lock");
        let start = Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(_) => return Ok(LockFile(p)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > Duration::from_secs(10) {
                        return Err(BackendError::Unavailable("CAS lock busy (stale .cas.lock?)".into()));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(err(e)),
            }
        }
    }
}

struct LockFile(PathBuf);
impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for e in fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        if e.file_type()?.is_dir() {
            walk(&p, base, out)?;
        } else if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

impl Backend for FsBackend {
    fn get(&self, key: &str) -> Result<Option<Versioned>, BackendError> {
        match fs::read(self.path(key)?) {
            Ok(data) => Ok(Some(Versioned { tag: tag_of(&data), data })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(err(e)),
        }
    }
    fn get_range(&self, key: &str, offset: u64, len: u64) -> Result<Option<Vec<u8>>, BackendError> {
        let mut f = match fs::File::open(self.path(key)?) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(err(e)),
        };
        f.seek(SeekFrom::Start(offset)).map_err(err)?;
        let mut buf = Vec::new();
        f.take(len).read_to_end(&mut buf).map_err(err)?;
        Ok(Some(buf))
    }
    fn exists(&self, key: &str) -> Result<bool, BackendError> {
        Ok(self.path(key)?.is_file())
    }
    fn put_if_absent(&self, key: &str, data: &[u8]) -> Result<bool, BackendError> {
        let dest = self.path(key)?;
        fs::create_dir_all(dest.parent().expect("key has parent")).map_err(err)?;
        let tmp = self.tmp_file(data)?;
        let r = fs::hard_link(&tmp, &dest);
        let _ = fs::remove_file(&tmp);
        match r {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(err(e)),
        }
    }
    fn put_if_match(&self, key: &str, data: &[u8], tag: &str) -> Result<Option<String>, BackendError> {
        let dest = self.path(key)?;
        let _l = self.lock()?;
        match fs::read(&dest) {
            Ok(cur) if tag_of(&cur) == tag => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(err(e)),
        }
        let tmp = self.tmp_file(data)?;
        fs::rename(&tmp, &dest).map_err(err)?;
        Ok(Some(tag_of(data)))
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let mut v = Vec::new();
        walk(&self.root, &self.root, &mut v).map_err(err)?;
        v.retain(|k| k.starts_with(prefix));
        v.sort();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conforms() {
        let d = tempfile::tempdir().unwrap();
        crate::backend::conformance(&FsBackend::open(d.path().join("r")).unwrap());
    }
    #[test]
    fn concurrent_cas_exactly_one_winner() {
        let d = tempfile::tempdir().unwrap();
        let b = std::sync::Arc::new(FsBackend::open(d.path().join("r")).unwrap());
        b.put_if_absent("m", b"0").unwrap();
        let tag = b.get("m").unwrap().unwrap().tag;
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let (b, tag) = (b.clone(), tag.clone());
                std::thread::spawn(move || b.put_if_match("m", format!("w{i}").as_bytes(), &tag).unwrap().is_some())
            })
            .collect();
        let wins = hs.into_iter().map(|h| h.join().unwrap()).filter(|w| *w).count();
        assert_eq!(wins, 1);
    }
}
