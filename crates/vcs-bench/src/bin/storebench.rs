//! storebench: reproducible storage benchmark harness (issue #182).
//!
//!   storebench list
//!   storebench gen <workload> <dir> [--scale smoke|small|medium|N] [--seed N]
//!   storebench run [--workloads a,b|all] [--tools git,vcs,walgit] [--ops a,b|all] [--modes warm,cold,partial]
//!                  [--trials N] [--scale S] [--seed N] [--scratch DIR] [--out DIR] [--vcs-bin P] [--git-bin P]
//!                  [--walgit-bin P] [--drop-caches auto|off] [--partial-pct N]
//!
//! See docs/research/storage/benchmark-methodology.md.
use std::fs;
use std::path::PathBuf;
use vcs_bench::harness::{self, Config, Mode, RunSpec, Tool};
use vcs_bench::json::J;
use vcs_bench::workloads::{self, Workload};

fn die(m: &str) -> ! {
    eprintln!("storebench: {m}");
    std::process::exit(2)
}

struct Args(Vec<String>);
impl Args {
    fn opt(&self, name: &str) -> Option<String> {
        self.0.iter().position(|a| a == name).and_then(|i| self.0.get(i + 1)).cloned()
    }
}

fn csv(s: &str) -> Vec<String> {
    s.split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect()
}

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    let a = Args(raw.clone());
    match raw.get(1).map(|s| s.as_str()) {
        Some("list") => {
            println!("workloads:");
            for w in workloads::ALL {
                let p = workloads::plan(w, 1);
                println!("  {:<24} files={:<6} bytes={:<10} zones={:<4} revisions={:<4} branches={}", w.name(), p.files.len(), p.total_bytes(), p.zones.len(), p.revisions, p.branches);
            }
            println!("operations:");
            for o in harness::ops() {
                println!("  {}", o.name);
            }
            println!("tools: git vcs walgit(stub)\nmodes: warm cold partial\nscales: smoke=1 small=8 medium=64 or an integer");
        }
        Some("gen") => {
            let w = raw.get(2).and_then(|s| Workload::parse(s)).unwrap_or_else(|| die("usage: gen <workload> <dir> [--scale S] [--seed N]"));
            let dir = PathBuf::from(raw.get(3).unwrap_or_else(|| die("missing <dir>")));
            let scale = workloads::parse_scale(&a.opt("--scale").unwrap_or("smoke".into())).unwrap_or_else(|| die("bad --scale"));
            let seed: u64 = a.opt("--seed").map(|s| s.parse().unwrap_or_else(|_| die("bad --seed"))).unwrap_or(42);
            let plan = workloads::plan(w, scale);
            fs::create_dir_all(&dir).expect("mkdir");
            let n = workloads::generate(&plan, &dir, seed).expect("generate");
            let zones: Vec<J> = plan.zones.iter().map(|(n, p)| J::obj(vec![("name", J::s(n)), ("path", J::s(p))])).collect();
            let m = J::obj(vec![
                ("workload", J::s(w.name())),
                ("scale", J::n(scale as f64)),
                ("seed", J::n(seed as f64)),
                ("files", J::n(plan.files.len() as f64)),
                ("bytes", J::n(n as f64)),
                ("zones", J::Arr(zones)),
                ("revisions", J::n(plan.revisions as f64)),
                ("edits_per_revision", J::n(plan.edits_per_rev as f64)),
                ("branches", J::n(plan.branches as f64)),
                ("history_zone", plan.hist_zone.as_ref().map(|z| J::s(z)).unwrap_or(J::Null)),
            ]);
            fs::write(dir.join("..storebench-manifest.json"), m.pretty()).expect("manifest");
            println!("generated {} files, {} bytes ({}) in {}; manifest in ..storebench-manifest.json", plan.files.len(), n, w.name(), dir.display());
        }
        Some("run") => run(&a, &raw),
        _ => die("usage: storebench list | gen <workload> <dir> | run [options]  (see docs/research/storage/benchmark-methodology.md)"),
    }
}

fn run(a: &Args, raw: &[String]) {
    let wl = a.opt("--workloads").unwrap_or("small_source".into());
    let workloads: Vec<Workload> = if wl == "all" { workloads::ALL.to_vec() } else { csv(&wl).iter().map(|s| Workload::parse(s).unwrap_or_else(|| die(&format!("unknown workload {s}")))).collect() };
    let tools: Vec<Tool> = csv(&a.opt("--tools").unwrap_or("git,vcs".into())).iter().map(|s| Tool::parse(s).unwrap_or_else(|| die(&format!("unknown tool {s}")))).collect();
    let modes: Vec<Mode> = csv(&a.opt("--modes").unwrap_or("warm,cold,partial".into())).iter().map(|s| Mode::parse(s).unwrap_or_else(|| die(&format!("unknown mode {s}")))).collect();
    let ops = match a.opt("--ops") {
        Some(o) if o != "all" => csv(&o),
        _ => vec![],
    };
    let known: Vec<String> = harness::ops().into_iter().map(|o| o.name).collect();
    for o in &ops {
        if !known.contains(o) {
            die(&format!("unknown op {o} (see `storebench list`)"));
        }
    }
    let scratch = PathBuf::from(a.opt("--scratch").unwrap_or("/tmp/storebench".into()));
    let out_dir = PathBuf::from(a.opt("--out").unwrap_or("storebench-out".into()));
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
    let vcs_bin = a.opt("--vcs-bin").unwrap_or_else(|| exe_dir.join("vcs").display().to_string());
    if !std::path::Path::new(&vcs_bin).is_file() && tools.contains(&Tool::Vcs) {
        die(&format!("vcs binary not found at {vcs_bin}; build with `cargo build --release -p vcs-cli` or pass --vcs-bin"));
    }
    let cfg = Config {
        git_bin: a.opt("--git-bin").unwrap_or("git".into()),
        vcs_bin,
        walgit_bin: a.opt("--walgit-bin"),
        scratch: scratch.clone(),
        seed: a.opt("--seed").map(|s| s.parse().unwrap_or_else(|_| die("bad --seed"))).unwrap_or(42),
        trials: a.opt("--trials").map(|s| s.parse().unwrap_or_else(|_| die("bad --trials"))).unwrap_or(5),
        scale: workloads::parse_scale(&a.opt("--scale").unwrap_or("smoke".into())).unwrap_or_else(|| die("bad --scale")),
        drop_caches: a.opt("--drop-caches").unwrap_or("auto".into()),
        partial_pct: a.opt("--partial-pct").map(|s| s.parse().unwrap_or_else(|_| die("bad --partial-pct"))).unwrap_or(50),
    };
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).expect("scratch");
    fs::create_dir_all(&out_dir).expect("out dir");
    let scratch = scratch.canonicalize().expect("scratch");
    let cfg = Config { scratch: scratch.clone(), ..cfg };
    let load0 = fs::read_to_string("/proc/loadavg").unwrap_or_default().trim().to_string();
    let spec = RunSpec { workloads, tools, ops, modes };
    let mut log = |m: &str| eprintln!("{m}");
    let out = harness::run(&cfg, &spec, &mut log).unwrap_or_else(|e| die(&e));
    let cmdline = raw.iter().map(|s| if s.contains(' ') { format!("'{s}'") } else { s.clone() }).collect::<Vec<_>>().join(" ");
    let meta = harness::metadata(&cfg, &cmdline, &out.cache, &load0);
    fs::write(out_dir.join("results.json"), harness::results_json(meta, &out, cfg.seed).pretty()).expect("write json");
    fs::write(out_dir.join("trials.csv"), harness::trials_csv(&out)).expect("write csv");
    fs::write(out_dir.join("summary.csv"), harness::summary_csv(&out, cfg.seed)).expect("write csv");
    let failed = out.recs.iter().filter(|r| !r.ok).count();
    eprintln!("done: {} trials ({} failed), {} skipped cells; wrote {}/{{results.json,trials.csv,summary.csv}}", out.recs.len(), failed, out.skips.len(), out_dir.display());
    let _ = fs::remove_dir_all(&scratch);
}
