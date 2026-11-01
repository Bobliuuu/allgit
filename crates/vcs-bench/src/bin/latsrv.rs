//! `latsrv`: vcs-server over a local directory with a fixed injected latency per backend call,
//! standing in for an object store that is far away (EMULATED: not real S3). Every backend call
//! is appended to a trace file (`op<TAB>start_us<TAB>end_us<TAB>key`).
//!
//! usage: latsrv --root DIR --tokens FILE --listen ADDR --latency-ms N [--trace FILE]

use std::sync::Arc;
use std::time::Duration;
use vcs_remote::timeline::TimelineBackend;
use vcs_remote::{Backend, FsBackend};
use vcs_server::{Config, Server, TokenTable};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let need = |k: &str| get(k).unwrap_or_else(|| panic!("missing {k}"));
    let fs: Arc<dyn Backend> = Arc::new(FsBackend::open(need("--root")).unwrap());
    let ms: u64 = get("--latency-ms").map(|v| v.parse().unwrap()).unwrap_or(0);
    let mut tl = TimelineBackend::new(fs, Duration::from_millis(ms));
    if let Some(t) = get("--trace") {
        tl = tl.with_trace_file(std::path::Path::new(&t)).unwrap();
    }
    let tokens = TokenTable::parse(&std::fs::read_to_string(need("--tokens")).unwrap()).unwrap();
    let srv = Server::start(Arc::new(tl), Config::new(tokens), &need("--listen")).unwrap();
    eprintln!(
        "latsrv listening on {} (latency {ms} ms per backend call, emulated)",
        srv.local_addr()
    );
    srv.join();
}
