//! Filesystem-monitor interface for `status` (#113) and its journal-backed implementation (#154).
//!
//! A monitor answers one question: *which paths may have changed since token T?* The answer is
//! either a set of paths plus the token to use next time, or [`Changed::FullScan`] ("I cannot
//! vouch for anything"). `status` trusts a `Paths` answer only together with its own saved
//! result for token T (see [`State`]); on every doubt it falls back to the full scan, which is
//! the reference behaviour the property tests compare against.
//!
//! Fall back to a full scan when: no state or the state was computed for another index, zone
//! layout or subscription set; the token is unknown, from another journal epoch or past the end;
//! the backend overflowed or restarted; the monitor does not answer within the barrier timeout
//! (daemon missing or dead); a changed path is not a plain repo-relative path.
//!
//! The journal backend reads `.vcs/fsmon/journal`, written by `vcs watch` (feature `watcher`).
//! Format, one line each: `E <epoch>` (first line), `P <path>` (a path may have changed),
//! `O` (events were lost: everything is in doubt), `X` (watcher failed), `B <nonce>` (barrier).
//! A query drops a probe file into `.vcs/fsmon/probe/` and waits for the daemon to journal its
//! `B` line: inotify delivers events in order, so every event before the probe is then in the
//! journal. Tokens are `<epoch>:<byte offset after the last path line seen>`.

use crate::meta::{frame, unframe};
use crate::repo::ChangeKind;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vcs_objects::codec::{Reader, Writer};

pub const DIR: &str = "fsmon";
pub const JOURNAL: &str = "journal";
pub const PROBE_DIR: &str = "probe";
pub const PID: &str = "pid";
const STATE: &str = "state";

/// What a monitor knows about changes since a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    /// Only these repo-relative paths (and anything beneath them) may differ from the state at
    /// the old token. Duplicates and extra paths are allowed; omissions are not. `new_token`
    /// is valid for the moment of the query.
    Paths { paths: Vec<String>, new_token: String },
    /// No usable answer. `new_token` (if any) marks a moment from which the *next* query is
    /// valid, provided a full scan that starts after this call is stored under it.
    FullScan { new_token: Option<String> },
}

pub trait FsMonitor {
    /// Changes since `token` (`None`: the caller has no saved state).
    fn changed_since(&self, token: Option<&str>) -> Changed;
}

/// Saved `status` result for one monitor token, valid only for the index, zone layout and
/// subscriptions it was computed under (`fp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct State {
    pub fp: [u8; 32],
    pub token: String,
    pub dirty: BTreeMap<String, ChangeKind>,
    pub untracked: Vec<String>,
    pub unsupported: Vec<String>,
}

impl State {
    pub fn load(vcs: &Path) -> Option<State> {
        Self::decode(&fs::read(vcs.join(DIR).join(STATE)).ok()?)
    }
    pub fn remove(vcs: &Path) {
        let _ = fs::remove_file(vcs.join(DIR).join(STATE));
    }
    pub fn store(&self, vcs: &Path) {
        let p = vcs.join(DIR).join(STATE);
        let _ = fs::create_dir_all(vcs.join(DIR));
        let _ = crate::meta::write_cache_atomic(&p, &self.encode());
    }
    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(&self.fp).str(&self.token).varint(self.dirty.len() as u64);
        for (p, k) in &self.dirty {
            w.str(p).u8(if *k == ChangeKind::Deleted { 2 } else { 1 });
        }
        for l in [&self.untracked, &self.unsupported] {
            w.varint(l.len() as u64);
            for p in l {
                w.str(p);
            }
        }
        frame(b"AGFS", w.finish())
    }
    fn decode(b: &[u8]) -> Option<State> {
        let mut r = Reader::new(unframe(b"AGFS", b, "fsmon state").ok()?);
        let mut fp = [0u8; 32];
        fp.copy_from_slice(r.raw(32).ok()?);
        let token = r.str(4096).ok()?;
        let n = r.count(2).ok()?;
        let mut dirty = BTreeMap::new();
        for _ in 0..n {
            let p = r.str(4096).ok()?;
            let k = match r.u8().ok()? {
                1 => ChangeKind::Modified,
                2 => ChangeKind::Deleted,
                _ => return None,
            };
            dirty.insert(p, k);
        }
        let mut lists = [Vec::new(), Vec::new()];
        for l in &mut lists {
            let n = r.count(1).ok()?;
            for _ in 0..n {
                l.push(r.str(4096).ok()?);
            }
        }
        r.finish().ok()?;
        let [untracked, unsupported] = lists;
        Some(State {
            fp,
            token,
            dirty,
            untracked,
            unsupported,
        })
    }
}

/// A repo-relative path a monitor may hand us: no empty, `.`/`..` or `.vcs` components.
pub(crate) fn plain_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\\')
        && p.split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(vcs_objects::names::META_DIR))
}

/// Reads the journal written by `vcs watch`.
pub struct JournalMonitor {
    dir: PathBuf,
    timeout: Duration,
}

static NONCE: AtomicU64 = AtomicU64::new(0);

impl JournalMonitor {
    /// A monitor for the repository metadata directory `vcs`, if a journal exists.
    pub fn open(vcs: &Path) -> Option<JournalMonitor> {
        let dir = vcs.join(DIR);
        dir.join(JOURNAL).is_file().then_some(JournalMonitor {
            dir,
            timeout: Duration::from_millis(400),
        })
    }

    /// Is a watcher journaling for the repository whose metadata directory is `vcs`?
    pub fn daemon_running(vcs: &Path) -> bool {
        Self::open(vcs).is_some_and(|m| m.daemon_alive())
    }

    fn daemon_alive(&self) -> bool {
        let Ok(pid) = fs::read_to_string(self.dir.join(PID)) else {
            return false;
        };
        let pid = pid.trim();
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        // Without /proc (non-Linux) liveness is judged by the barrier timeout alone.
        !Path::new("/proc/self").exists() || Path::new("/proc").join(pid).exists()
    }
}

struct Scan {
    paths: BTreeSet<String>,
    lost: bool,
    /// Offset after the last non-barrier line.
    last_event_end: usize,
    barrier: bool,
}

/// Parse complete lines of `buf` (which starts at journal offset `base`) looking for `nonce`.
fn scan_lines(buf: &[u8], base: usize, nonce: &str, s: &mut Scan) -> usize {
    let mut pos = 0;
    while let Some(nl) = buf[pos..].iter().position(|b| *b == b'\n') {
        let line = &buf[pos..pos + nl];
        let end = base + pos + nl + 1;
        match line.first() {
            Some(b'B') => {
                if line.get(2..) == Some(nonce.as_bytes()) {
                    s.barrier = true;
                    return pos + nl + 1;
                }
            }
            Some(b'P') if line.len() > 2 => match std::str::from_utf8(&line[2..]) {
                Ok(p) => {
                    s.paths.insert(p.to_owned());
                    s.last_event_end = end;
                }
                Err(_) => {
                    s.lost = true;
                    s.last_event_end = end;
                }
            },
            _ => {
                // `O`, `X` or anything unknown: everything is in doubt.
                s.lost = true;
                s.last_event_end = end;
            }
        }
        pos += nl + 1;
    }
    pos
}

impl FsMonitor for JournalMonitor {
    fn changed_since(&self, token: Option<&str>) -> Changed {
        let none = Changed::FullScan { new_token: None };
        if !self.daemon_alive() {
            return none;
        }
        let jp = self.dir.join(JOURNAL);
        let Ok(mut f) = fs::File::open(&jp) else { return none };
        let mut head = Vec::new();
        // The header line: `E <epoch>`.
        let mut byte = [0u8; 1];
        while head.len() < 64 {
            match f.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => break,
                Ok(1) => head.push(byte[0]),
                _ => return none,
            }
        }
        let header_end = head.len() + 1;
        let Some(epoch) = head
            .strip_prefix(b"E ")
            .and_then(|e| std::str::from_utf8(e).ok())
            .map(str::to_owned)
        else {
            return none;
        };
        // Where to start reading, and whether the old token is usable at all.
        let mut token_ok = false;
        let mut start = header_end;
        if let Some((e, off)) = token.and_then(|t| t.split_once(':')) {
            if e == epoch {
                if let Ok(off) = off.parse::<usize>() {
                    if off >= header_end && off as u64 <= fs::metadata(&jp).map_or(0, |m| m.len()) {
                        start = off;
                        token_ok = true;
                    }
                }
            }
        }
        // Barrier.
        let nonce = format!("{}-{}-{}", std::process::id(), NONCE.fetch_add(1, Ordering::Relaxed), now_nanos());
        let probe_dir = self.dir.join(PROBE_DIR);
        let probe = probe_dir.join(&nonce);
        if fs::create_dir_all(&probe_dir).is_err() || fs::write(&probe, b"").is_err() {
            return none;
        }
        let mut s = Scan {
            paths: BTreeSet::new(),
            lost: false,
            last_event_end: start,
            barrier: false,
        };
        let deadline = Instant::now() + self.timeout;
        let mut pos = start;
        let mut buf = Vec::new();
        loop {
            use std::io::Seek;
            if f.seek(std::io::SeekFrom::Start(pos as u64)).is_ok() {
                buf.clear();
                if f.read_to_end(&mut buf).is_ok() {
                    pos += scan_lines(&buf, pos, &nonce, &mut s);
                }
            }
            if s.barrier || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_micros(300));
        }
        let _ = fs::remove_file(&probe);
        if !s.barrier {
            return none;
        }
        // A header rewritten under us (rotation) shows up as an epoch change: re-check.
        let still = fs::read(&jp).ok().is_some_and(|b| b.starts_with(format!("E {epoch}\n").as_bytes()));
        if !still {
            return none;
        }
        let new_token = format!("{epoch}:{}", s.last_event_end);
        if !token_ok || s.lost {
            return Changed::FullScan {
                new_token: Some(new_token),
            };
        }
        Changed::Paths {
            paths: s.paths.into_iter().collect(),
            new_token,
        }
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Test monitor: events are whatever the test pushes, with controllable misbehaviour. Tokens are
/// `<generation>:<events seen>`; bumping the generation (a "backend restart") or calling
/// [`MockMonitor::overflow`] makes the next answer a full scan.
#[doc(hidden)]
#[derive(Default)]
pub struct MockMonitor {
    inner: Mutex<MockInner>,
}

#[derive(Default)]
struct MockInner {
    gen: u64,
    log: Vec<String>,
    lost_until: usize,
    dead: bool,
}

impl MockMonitor {
    pub fn new() -> MockMonitor {
        MockMonitor::default()
    }
    /// Record that `path` may have changed (call it more than once for duplicates).
    pub fn event(&self, path: &str) {
        self.inner.lock().unwrap().log.push(path.to_owned());
    }
    /// Events were dropped: every query that spans this point must be a full scan.
    pub fn overflow(&self) {
        let mut g = self.inner.lock().unwrap();
        g.lost_until = g.log.len() + 1;
        g.log.push(String::new());
    }
    /// Backend restart: old tokens become invalid.
    pub fn restart(&self) {
        let mut g = self.inner.lock().unwrap();
        g.gen += 1;
        g.log.clear();
        g.lost_until = 0;
    }
    /// Daemon gone: no answers at all.
    pub fn set_dead(&self, dead: bool) {
        self.inner.lock().unwrap().dead = dead;
    }
}

impl FsMonitor for MockMonitor {
    fn changed_since(&self, token: Option<&str>) -> Changed {
        let g = self.inner.lock().unwrap();
        if g.dead {
            return Changed::FullScan { new_token: None };
        }
        let now = format!("{}:{}", g.gen, g.log.len());
        let from = token.and_then(|t| {
            let (gen, n) = t.split_once(':')?;
            (gen.parse::<u64>().ok()? == g.gen)
                .then(|| n.parse::<usize>().ok())
                .flatten()
                .filter(|n| *n <= g.log.len())
        });
        let Some(from) = from else {
            return Changed::FullScan { new_token: Some(now) };
        };
        let span = &g.log[from..];
        if span.iter().any(|p| p.is_empty()) {
            return Changed::FullScan { new_token: Some(now) };
        }
        Changed::Paths {
            paths: span.to_vec(),
            new_token: now,
        }
    }
}

/// Digest of the index bytes, the part of a fingerprint that costs time (7 MB at 100k files).
pub(crate) fn index_digest(index: &[u8]) -> [u8; 32] {
    *blake3::hash(index).as_bytes()
}

/// Fingerprint binding a saved state to the exact index, zone layout and subscriptions.
pub(crate) fn fingerprint(index: &[u8], zones: &[u8], workspace: &[u8]) -> [u8; 32] {
    fingerprint_with(&index_digest(index), index.len(), zones, workspace)
}

/// [`fingerprint`] from the index's digest and length, so a remembered digest skips hashing the file.
pub(crate) fn fingerprint_with(digest: &[u8; 32], index_len: usize, zones: &[u8], workspace: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&(index_len as u64).to_le_bytes());
    h.update(digest);
    for part in [zones, workspace] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    *h.finalize().as_bytes()
}

const DIGEST_CACHE: &str = "ixdigest";

/// Identity of the index file in place (inode, length, mtime, ctime); a rewrite is a rename of a new
/// file, so any change to the index changes it.
fn index_identity(vcs: &Path) -> Option<[u8; 32]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(vcs.join("index")).ok()?;
        let mut k = [0u8; 32];
        k[..8].copy_from_slice(&m.ino().to_le_bytes());
        k[8..16].copy_from_slice(&m.size().to_le_bytes());
        k[16..24].copy_from_slice(&((m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128) as u64).to_le_bytes());
        k[24..].copy_from_slice(&((m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128) as u64).to_le_bytes());
        Some(k)
    }
    #[cfg(not(unix))]
    {
        let _ = vcs;
        None
    }
}

/// Remember `digest` as the digest of the index file now in place (best effort).
pub(crate) fn remember_index_digest(vcs: &Path, digest: &[u8; 32]) {
    let Some(id) = index_identity(vcs) else { return };
    let dir = vcs.join(DIR);
    if !dir.is_dir() {
        return;
    }
    let mut b = id.to_vec();
    b.extend_from_slice(digest);
    let _ = crate::meta::write_cache_nosync(&dir.join(DIGEST_CACHE), &b);
}

/// Digest of `index` (the bytes just read from `vcs/index`): remembered per file identity, hashed
/// only when the file is not the one last seen. The identity is taken before and after the read of
/// the cache, and the bytes' length must match, so a file replaced meanwhile is never trusted.
pub(crate) fn index_digest_cached(vcs: &Path, index: &[u8]) -> [u8; 32] {
    let Some(id) = index_identity(vcs) else {
        return index_digest(index);
    };
    if let Ok(c) = fs::read(vcs.join(DIR).join(DIGEST_CACHE)) {
        if c.len() == 64 && c[..32] == id && u64::from_le_bytes(id[8..16].try_into().expect("8 bytes")) == index.len() as u64 {
            let mut d = [0u8; 32];
            d.copy_from_slice(&c[32..]);
            if index_identity(vcs) == Some(id) {
                return d;
            }
        }
    }
    let d = index_digest(index);
    if index_identity(vcs) == Some(id) {
        let mut b = id.to_vec();
        b.extend_from_slice(&d);
        let dir = vcs.join(DIR);
        if dir.is_dir() {
            let _ = crate::meta::write_cache_nosync(&dir.join(DIGEST_CACHE), &b);
        }
    }
    d
}

/// Index files at least this big (about 14,000 entries) are worth a watcher: below it a full
/// `status` scan is a few tens of milliseconds.
const AUTOWATCH_MIN_INDEX: u64 = 1_000_000;
/// After a start attempt, wait this long before trying again (a watcher that cannot start, e.g.
/// out of inotify watches, must not be respawned by every command).
const AUTOWATCH_RETRY_SECS: u64 = 600;
/// A watcher started by a command stops after this long without a query.
pub const AUTOWATCH_IDLE_SECS: u64 = 1800;

impl crate::repo::Repo {
    /// Start a detached `<exe> watch --idle-exit N` for this repository when it is big enough that
    /// `status` benefits, none is running, and none was tried in the last ten minutes. Never fails
    /// and never waits: the command that called this still does its own (full) scan this time; the
    /// next one finds the journal. `VCS_NO_FSMONITOR` and `VCS_NO_AUTOWATCH` turn it off.
    pub fn autostart_watcher(&self, exe: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            if std::env::var_os("VCS_NO_FSMONITOR").is_some() || std::env::var_os("VCS_NO_AUTOWATCH").is_some() {
                return;
            }
            let big = fs::metadata(self.vcs.join("index")).is_ok_and(|m| m.len() >= AUTOWATCH_MIN_INDEX);
            if !big || JournalMonitor::daemon_running(&self.vcs) {
                return;
            }
            let dir = self.vcs.join(DIR);
            let stamp = dir.join("autowatch");
            let recent = fs::metadata(&stamp)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|e| e.as_secs() < AUTOWATCH_RETRY_SECS);
            if recent {
                return;
            }
            let _ = fs::create_dir_all(&dir);
            if fs::write(&stamp, b"").is_err() {
                return;
            }
            let _ = std::process::Command::new(exe)
                .arg("watch")
                .arg("--idle-exit")
                .arg(AUTOWATCH_IDLE_SECS.to_string())
                .current_dir(&self.worktree)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn();
        }
        #[cfg(not(unix))]
        let _ = exe;
    }
}

pub(crate) fn state_exists(vcs: &Path) -> bool {
    vcs.join(DIR).join(STATE).is_file()
}

// ---- rebinding a saved state across index rewrites ----
//
// A saved [`State`] is bound to the exact index (fingerprint). Commands that rewrite the index
// while changing only known entries (a patched `add`/`restore`/`rm`, or `commit` storing cache
// trees) append `old new <touched paths>` to `.vcs/fsmon/rebind`. `status` may then follow the
// chain from the state's fingerprint to the current one and treat the touched paths as changed
// (they are re-checked), instead of throwing the state away. Any gap in the chain, an index
// written by something that did not log, or a damaged line means the chain does not reach the
// current fingerprint and `status` does a full scan, as before.

const REBIND: &str = "rebind";
const REBIND_MAX_BYTES: u64 = 256 << 10;
const REBIND_MAX_PATHS: usize = 20_000;

fn hex(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn log_rebind(vcs: &Path, old: &[u8; 32], new: &[u8; 32], touched: &[String]) {
    if touched.len() > REBIND_MAX_PATHS || touched.iter().any(|p| p.contains(['\t', '\n', '\r'])) {
        return;
    }
    let path = vcs.join(DIR).join(REBIND);
    let mut line = format!("{} {}", hex(old), hex(new));
    for t in touched {
        line.push('\t');
        line.push_str(t);
    }
    line.push('\n');
    let big = fs::metadata(&path).is_ok_and(|m| m.len() > REBIND_MAX_BYTES);
    let f = fs::OpenOptions::new()
        .create(true)
        .append(!big)
        .write(true)
        .truncate(big)
        .open(&path);
    if let Ok(mut f) = f {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// The paths touched by the logged rewrites that lead from fingerprint `from` to `to`, or `None`
/// when the log does not connect them.
pub(crate) fn follow_rebinds(vcs: &Path, from: &[u8; 32], to: &[u8; 32]) -> Option<Vec<String>> {
    let text = fs::read_to_string(vcs.join(DIR).join(REBIND)).ok()?;
    let mut cur = hex(from);
    let want = hex(to);
    let mut out = Vec::new();
    for _ in 0..512 {
        if cur == want {
            return Some(out);
        }
        let line = text
            .lines()
            .rev()
            .find(|l| l.starts_with(&cur) && l.as_bytes().get(64) == Some(&b' '))?;
        let mut parts = line.split('\t');
        let head = parts.next()?;
        let next = head.get(65..129)?;
        out.extend(parts.map(str::to_owned));
        cur = next.to_owned();
    }
    None
}

#[cfg(test)]
mod digest_cache_tests {
    use super::*;

    #[test]
    fn remembered_digest_matches_the_bytes_and_follows_rewrites() {
        let d = tempfile::tempdir().unwrap();
        let vcs = d.path();
        fs::create_dir_all(vcs.join(DIR)).unwrap();
        let (a, b) = (vec![1u8; 4096], vec![2u8; 4096]); // same length, different content
        fs::write(vcs.join("index"), &a).unwrap();
        assert_eq!(index_digest_cached(vcs, &a), index_digest(&a));
        assert_eq!(index_digest_cached(vcs, &a), index_digest(&a)); // served from the cache
                                                                    // a rewrite (new file renamed over the old) of the same length is not mistaken for the old one
        let tmp = vcs.join("index.tmp");
        fs::write(&tmp, &b).unwrap();
        fs::rename(&tmp, vcs.join("index")).unwrap();
        assert_eq!(index_digest_cached(vcs, &b), index_digest(&b));
        // a digest remembered by the writer is the one served
        remember_index_digest(vcs, &index_digest(&b));
        assert_eq!(index_digest_cached(vcs, &b), index_digest(&b));
        // fingerprints with and without the cache agree
        assert_eq!(
            fingerprint_with(&index_digest_cached(vcs, &b), b.len(), b"z", b"w"),
            fingerprint(&b, b"z", b"w")
        );
        assert_ne!(fingerprint(&a, b"z", b"w"), fingerprint(&b, b"z", b"w"));
    }
}
