//! Shared helpers: deterministic ids, timing with medians, environment capture, tiny JSON writer.
use std::time::Instant;
pub use vcs_bench::Rng;

pub type Id = [u8; 32];

/// Uniform pseudo-random 32-byte ids (stand-in for blake3 object ids, which are uniform).
pub fn random_ids(n: usize, seed: u64) -> Vec<Id> {
    let mut r = Rng::new(seed);
    (0..n)
        .map(|_| {
            let mut id = [0u8; 32];
            r.fill(&mut id);
            id
        })
        .collect()
}
pub fn u64_at(id: &Id, i: usize) -> u64 {
    u64::from_le_bytes(id[i * 8..i * 8 + 8].try_into().unwrap())
}

pub struct Trials {
    pub median: f64,
    pub min: f64,
    pub max: f64,
}
/// Run `f` `trials` times; each returns total nanoseconds for `ops` operations. Reports ns/op.
pub fn trials(n_trials: usize, ops: usize, mut f: impl FnMut()) -> Trials {
    let mut v: Vec<f64> = (0..n_trials)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_nanos() as f64 / ops as f64
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Trials { median: v[v.len() / 2], min: v[0], max: v[v.len() - 1] }
}
pub fn secs(mut f: impl FnMut()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64()
}

pub fn sh(cmd: &str) -> String {
    std::process::Command::new("sh").arg("-c").arg(cmd).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}
/// Machine, load, kernel, CPU, tool versions: recorded with every result.
pub fn env_json() -> String {
    format!(
        "{{\"host\":{:?},\"kernel\":{:?},\"cpu\":{:?},\"cores\":{},\"loadavg\":{:?},\"rustc\":{:?},\"date\":{:?}}}",
        sh("hostname"),
        sh("uname -sr"),
        sh("grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs"),
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        sh("cut -d' ' -f1-3 /proc/loadavg"),
        sh("rustc -V"),
        sh("date -u +%FT%TZ"),
    )
}
/// Drop the page cache (needs root; returns false when it could not).
pub fn drop_caches() -> bool {
    sh("sync; echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null 2>&1 && echo ok") == "ok"
}
pub fn dir_size(p: &std::path::Path) -> u64 {
    let mut t = 0;
    let mut st = vec![p.to_owned()];
    while let Some(d) = st.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata() {
                    if m.is_dir() {
                        st.push(e.path())
                    } else {
                        t += m.len()
                    }
                }
            }
        }
    }
    t
}

/// One result row: key/value pairs serialised as a JSON line.
#[derive(Default)]
pub struct Row(Vec<(String, String)>);
impl Row {
    pub fn new() -> Row {
        Row::default()
    }
    pub fn s(mut self, k: &str, v: &str) -> Row {
        self.0.push((k.into(), format!("{v:?}")));
        self
    }
    pub fn n(mut self, k: &str, v: f64) -> Row {
        self.0.push((k.into(), if v.is_finite() { format!("{v:.4}") } else { "null".into() }));
        self
    }
    pub fn i(mut self, k: &str, v: u64) -> Row {
        self.0.push((k.into(), v.to_string()));
        self
    }
    pub fn json(&self) -> String {
        let b: Vec<String> = self.0.iter().map(|(k, v)| format!("{k:?}:{v}")).collect();
        format!("{{{}}}", b.join(","))
    }
}
pub struct Out {
    f: Option<std::fs::File>,
}
impl Out {
    /// Rows go to stdout and, when `path` is given, to a JSON-lines file.
    pub fn new(path: Option<String>) -> Out {
        Out { f: path.map(|p| std::fs::File::create(p).unwrap()) }
    }
    pub fn row(&mut self, r: Row) {
        use std::io::Write;
        let j = r.json();
        println!("{j}");
        if let Some(f) = &mut self.f {
            writeln!(f, "{j}").unwrap();
        }
    }
}
pub fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}
pub fn arg_or<T: std::str::FromStr>(name: &str, d: T) -> T {
    arg(name).and_then(|s| s.parse().ok()).unwrap_or(d)
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Counting allocator: a binary opts in with `#[global_allocator] static A: Counting = Counting;`.
/// Gives measured (not estimated) heap bytes of a structure and peak build memory.
pub struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(n, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new >= l.size() {
            let n = LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size();
            PEAK.fetch_max(n, Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new, Relaxed);
        }
        System.realloc(p, l, new)
    }
}
pub fn live_bytes() -> usize {
    LIVE.load(Relaxed)
}
pub fn reset_peak() {
    PEAK.store(LIVE.load(Relaxed), Relaxed)
}
pub fn peak_bytes() -> usize {
    PEAK.load(Relaxed)
}
