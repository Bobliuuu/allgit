//! Process measurement helpers (Linux /proc based; degrade to zero elsewhere).
use std::fs;

fn status_kb(key: &str) -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}
pub fn rss_kb() -> u64 {
    status_kb("VmRSS:")
}
pub fn hwm_kb() -> u64 {
    status_kb("VmHWM:")
}
/// Reset the peak-RSS watermark (Linux >= 4.0). Best effort.
pub fn hwm_reset() {
    let _ = fs::write("/proc/self/clear_refs", "5");
}
/// Run `f` and return (result, peak RSS growth in KiB during f).
pub fn with_peak<T>(f: impl FnOnce() -> T) -> (T, u64) {
    hwm_reset();
    let before = rss_kb();
    let r = f();
    let peak = hwm_kb();
    (r, peak.saturating_sub(before))
}
pub fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        0.0
    } else {
        v[v.len() / 2]
    }
}
pub fn pct(v: &mut Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() {
        return 0.0;
    }
    v[(((v.len() - 1) as f64) * p).round() as usize]
}
pub fn machine_meta() -> serde_json::Value {
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("model name")).map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string()))
        .unwrap_or_default();
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    let load = fs::read_to_string("/proc/loadavg").unwrap_or_default().trim().to_string();
    let mem = fs::read_to_string("/proc/meminfo").ok().and_then(|s| s.lines().next().map(|l| l.to_string())).unwrap_or_default();
    let nproc = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    serde_json::json!({
        "cpu": cpu, "kernel": kernel, "loadavg_at_start": load, "memtotal": mem, "nproc": nproc,
        "zstd": zstd::zstd_safe::version_string(),
        "crates": {"zstd":"0.14","lz4_flex":"0.14","brotli":"9","fastcdc":"5","blake3":"1"},
        "args": std::env::args().collect::<Vec<_>>(),
    })
}
