//! Changed-path filters: for a commit and each of its parents, a small Bloom filter holding every
//! file path that differs from that parent, plus all the directories above those files. "Does this
//! commit change `a/b`?" then costs one hash lookup instead of reading trees; a miss is certain
//! (the entries are equal), a hit is only "maybe" and is settled by comparing the real entries.
//!
//! One file per zone, `.vcs/path-filters/<hash of zone id>`, a pure cache like the commit-graph:
//! `blake3`-checked, discarded when anything is wrong, never needed for a correct answer, built
//! lazily from the commits a deep path-limited walk passes. Commits with very large changes (more
//! than [`MAX_PATHS`] paths, e.g. an import commit) and root commits get no filter ("unknown").
//!
//! File: `"AGPF" || version:u8 || count:u32 || count * id:32 (sorted) || count * offset:u32 ||
//! data || blake3(all preceding)`. Data per commit: `nparents:u8`, then per parent
//! `len:u32` (`u32::MAX` = unknown) and `len` filter bytes. Offsets are into `data`.

use crate::cgraph::cg_name;
use crate::error::Result;
use crate::meta::write_cache_atomic;
use crate::repo::Repo;
use crate::treeops;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use vcs_objects::{Commit, ObjectId};

pub(crate) const PF_DIR: &str = "path-filters";
const MAGIC: &[u8; 4] = b"AGPF";
const VERSION: u8 = 1;
/// More changed paths than this: no filter (it would be big and always say "maybe" anyway).
pub(crate) const MAX_PATHS: usize = 512;
const HASHES: u64 = 7;
const BITS_PER_PATH: usize = 12;
const UNKNOWN: u32 = u32::MAX;

/// A filter for one (commit, parent) pair: `None` = unknown (anything may have changed).
pub(crate) type Filter = Option<Vec<u8>>;

fn hash_pair(path: &str) -> (u64, u64) {
    let h = blake3::hash(path.as_bytes());
    let b = h.as_bytes();
    (
        u64::from_le_bytes(b[..8].try_into().unwrap()),
        u64::from_le_bytes(b[8..16].try_into().unwrap()) | 1,
    )
}

fn bit_positions(path: &str, nbits: u64) -> impl Iterator<Item = u64> {
    let (h1, h2) = hash_pair(path);
    (0..HASHES).map(move |i| h1.wrapping_add(i.wrapping_mul(h2)) % nbits)
}

/// Is `path` possibly in the filter? Always yes for an unknown filter.
pub(crate) fn may_contain(f: &Filter, path: &str) -> bool {
    match f {
        None => true,
        Some(b) if b.is_empty() => false,
        Some(b) => bit_positions(path, b.len() as u64 * 8).all(|p| b[(p / 8) as usize] & (1 << (p % 8)) != 0),
    }
}

/// Filter of a change touching `files` (full zone-relative paths). `None` if too many.
pub(crate) fn build_filter<'a>(files: impl Iterator<Item = &'a String>) -> Filter {
    let mut paths: BTreeSet<&str> = BTreeSet::new();
    for f in files {
        let mut end = f.len();
        loop {
            paths.insert(&f[..end]);
            match f[..end].rfind('/') {
                Some(i) => end = i,
                None => break,
            }
        }
        if paths.len() > MAX_PATHS {
            return None;
        }
    }
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let bytes = (paths.len() * BITS_PER_PATH).div_ceil(8).max(8);
    let mut b = vec![0u8; bytes];
    for p in paths {
        for pos in bit_positions(p, bytes as u64 * 8) {
            b[(pos / 8) as usize] |= 1 << (pos % 8);
        }
    }
    Some(b)
}

/// The filters of a zone: what was read from the file plus what was computed this run.
#[derive(Default)]
pub(crate) struct PathBlooms {
    raw: Vec<u8>,
    count: usize,
    added: BTreeMap<ObjectId, Vec<Filter>>,
}

impl PathBlooms {
    /// Parse and verify a file; anything wrong gives an empty set.
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> PathBlooms {
        Self::parse(bytes).unwrap_or_default()
    }

    fn parse(bytes: Vec<u8>) -> Option<PathBlooms> {
        if bytes.len() < 4 + 1 + 4 + 32 || &bytes[..4] != MAGIC || bytes[4] != VERSION {
            return None;
        }
        let (body, sum) = bytes.split_at(bytes.len() - 32);
        if blake3::hash(body).as_bytes() != sum {
            return None;
        }
        let count = u32::from_le_bytes(body[5..9].try_into().ok()?) as usize;
        let table = 9usize.checked_add(count.checked_mul(36)?)?;
        if table > body.len() {
            return None;
        }
        let data_len = body.len() - table;
        // offsets must point inside the data and every record must be well formed
        for i in 0..count {
            let off = u32::from_le_bytes(body[9 + count * 32 + i * 4..][..4].try_into().ok()?) as usize;
            if off >= data_len {
                return None;
            }
            let mut p = table + off;
            let np = *body.get(p)? as usize;
            p += 1;
            for _ in 0..np {
                let l = u32::from_le_bytes(body.get(p..p + 4)?.try_into().ok()?);
                p += 4;
                if l != UNKNOWN {
                    p = p.checked_add(l as usize)?;
                }
            }
            if p > body.len() {
                return None;
            }
        }
        Some(PathBlooms {
            raw: bytes,
            count,
            added: BTreeMap::new(),
        })
    }

    fn lookup_raw(&self, id: &ObjectId) -> Option<Vec<Filter>> {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let at = 9 + mid * 32;
            match self.raw[at..at + 32].cmp(id.as_bytes().as_slice()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let table = 9 + self.count * 36;
                    let off = u32::from_le_bytes(self.raw[9 + self.count * 32 + mid * 4..][..4].try_into().unwrap()) as usize;
                    let mut p = table + off;
                    let np = self.raw[p] as usize;
                    p += 1;
                    let mut out = Vec::with_capacity(np);
                    for _ in 0..np {
                        let l = u32::from_le_bytes(self.raw[p..p + 4].try_into().unwrap());
                        p += 4;
                        if l == UNKNOWN {
                            out.push(None);
                        } else {
                            out.push(Some(self.raw[p..p + l as usize].to_vec()));
                            p += l as usize;
                        }
                    }
                    return Some(out);
                }
            }
        }
        None
    }

    pub(crate) fn get(&self, id: &ObjectId) -> Option<Vec<Filter>> {
        self.added.get(id).cloned().or_else(|| self.lookup_raw(id))
    }

    pub(crate) fn insert(&mut self, id: ObjectId, f: Vec<Filter>) {
        self.added.insert(id, f);
    }

    /// Were any filters read from disk?
    pub(crate) fn has_file(&self) -> bool {
        self.count > 0
    }

    pub(crate) fn pending(&self) -> usize {
        self.added.len()
    }

    /// Every commit id and its filters (file plus new), sorted by id.
    pub(crate) fn all(&self) -> BTreeMap<ObjectId, Vec<Filter>> {
        let mut m = BTreeMap::new();
        for i in 0..self.count {
            let at = 9 + i * 32;
            let id = ObjectId::from_bytes(self.raw[at..at + 32].try_into().unwrap());
            if let Some(f) = self.lookup_raw(&id) {
                m.insert(id, f);
            }
        }
        for (k, v) in &self.added {
            m.insert(*k, v.clone());
        }
        m
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let all = self.all();
        let mut ids = Vec::with_capacity(all.len() * 32);
        let mut offs = Vec::with_capacity(all.len() * 4);
        let mut data = Vec::new();
        for (id, fs) in &all {
            ids.extend_from_slice(id.as_bytes());
            offs.extend_from_slice(&(data.len() as u32).to_le_bytes());
            data.push(fs.len() as u8);
            for f in fs {
                match f {
                    None => data.extend_from_slice(&UNKNOWN.to_le_bytes()),
                    Some(b) => {
                        data.extend_from_slice(&(b.len() as u32).to_le_bytes());
                        data.extend_from_slice(b);
                    }
                }
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&(all.len() as u32).to_le_bytes());
        out.extend_from_slice(&ids);
        out.extend_from_slice(&offs);
        out.extend_from_slice(&data);
        let sum = blake3::hash(&out);
        out.extend_from_slice(sum.as_bytes());
        out
    }
}

impl Repo {
    pub(crate) fn pf_path(&self, zone: &str) -> std::path::PathBuf {
        self.common.join(PF_DIR).join(cg_name(zone))
    }

    pub(crate) fn load_path_blooms(&self, zone: &str) -> PathBlooms {
        fs::read(self.pf_path(zone)).map(PathBlooms::from_bytes).unwrap_or_default()
    }

    pub(crate) fn save_path_blooms(&self, zone: &str, b: &PathBlooms) {
        let _ = fs::create_dir_all(self.common.join(PF_DIR));
        let _ = write_cache_atomic(&self.pf_path(zone), &b.encode()); // best effort: it is only a cache
    }

    /// The filters of `c` (one per parent) computed from the trees; `None` for a root commit.
    pub(crate) fn compute_filters(&self, c: &Commit) -> Result<Option<Vec<Filter>>> {
        if c.parents.is_empty() {
            return Ok(None);
        }
        let mut out = Vec::with_capacity(c.parents.len());
        for p in &c.parents {
            let pr = self.commit_obj(p)?.root;
            let d = treeops::diff_trees_files(&self.store, Some(pr), Some(c.root), "")?;
            let f = if d.old.len() + d.new.len() > 2 * MAX_PATHS {
                None
            } else {
                build_filter(d.old.keys().chain(d.new.keys()))
            };
            out.push(f);
        }
        Ok(Some(out))
    }

    /// Check every file under `.vcs/path-filters` against the real commits; one message per problem.
    pub(crate) fn fsck_path_filters(&self, problems: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(self.common.join(PF_DIR)) else {
            return;
        };
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.contains('.') {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let b = PathBlooms::from_bytes(bytes);
            if b.count == 0 && b.added.is_empty() {
                problems.push(format!(
                    "path-filters {name}: unreadable or empty (it is only a cache and is rebuilt on next use)"
                ));
                continue;
            }
            for (id, f) in b.all() {
                match self.commit_obj(&id) {
                    Ok(c) if cg_name(&c.zone) == name => match self.compute_filters(&c) {
                        Ok(Some(want)) if want == f => {}
                        Ok(None) => problems.push(format!("path-filters {name}: {id} is a root commit")),
                        Ok(Some(_)) => problems.push(format!("path-filters {name}: wrong filter for {id}")),
                        Err(e) => problems.push(format!("path-filters {name}: cannot check {id}: {e}")),
                    },
                    Ok(_) => problems.push(format!("path-filters {name}: {id} belongs to another zone")),
                    Err(_) => problems.push(format!("path-filters {name}: {id} is not a readable commit")),
                }
            }
        }
    }
}
