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

/// Fingerprint binding a saved state to the exact index, zone layout and subscriptions.
pub(crate) fn fingerprint(index: &[u8], zones: &[u8], workspace: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for part in [index, zones, workspace] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part);
    }
    *h.finalize().as_bytes()
}
