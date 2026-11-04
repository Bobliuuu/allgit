mod dataset;
mod delta;
mod expac;
mod expe;
mod expf;
mod expg;
mod layout;
mod json;
mod packlab;
mod synth;

use json::Out;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(|s| s == "synth").unwrap_or(false) {
        // lab synth <zones> <commits> <seed>  > stream.fi
        synth::synth(a[2].parse().unwrap(), a[3].parse().unwrap(), a[4].parse().unwrap());
        return;
    }
    if a.get(1).map(|s| s == "fixture").unwrap_or(false) {
        // lab fixture <workload|all> <outdir> <scale> [seed]: git repos for the storebench workloads (same generators, same edits)
        fixture(&a[2], &a[3], a[4].parse().unwrap(), a.get(5).and_then(|s| s.parse().ok()).unwrap_or(42));
        return;
    }
    if a.len() < 5 {
        eprintln!("usage: lab <exp: a|c|e|f|g|all> <name> <repo.git> <out.jsonl> [max_commits]");
        std::process::exit(2);
    }
    let (exp, name, repo, outp) = (&a[1], &a[2], &a[3], &a[4]);
    let maxc = a.get(5).and_then(|s| s.parse().ok());
    let t0 = std::time::Instant::now();
    let ds = dataset::Dataset::load(name, repo, maxc);
    eprintln!("loaded {} objects, {} MiB raw, {} commits, {} zones in {:.1}s", ds.objs.len(), ds.total_bytes() >> 20, ds.ncommits, ds.zones.len(), t0.elapsed().as_secs_f64());
    let out = Out { path: outp.clone(), dataset: name.clone() };
    let sh = |c: &str| String::from_utf8_lossy(&std::process::Command::new("sh").args(["-c", c]).output().map(|o| o.stdout).unwrap_or_default()).trim().to_string();
    out.emit(
        "meta",
        vec![
            ("exp_arg", json::J::s(exp.clone())),
            ("kernel", json::J::s(sh("uname -sr"))),
            ("cpu", json::J::s(sh("lscpu | grep 'Model name' | sed 's/.*: *//'"))),
            ("cores", json::J::s(sh("nproc"))),
            ("load_at_start", json::J::s(sh("uptime | sed 's/.*load average: //'"))),
            ("rustc", json::J::s(sh("rustc --version"))),
            ("git", json::J::s(sh("git --version"))),
            ("lab_threads", json::J::U(packlab::threads() as u64)),
            ("objects", json::J::U(ds.objs.len() as u64)),
            ("repo_head", json::J::s(sh(&format!("git -C {repo} rev-parse HEAD")))),
            ("date", json::J::s(sh("date -u +%FT%TZ"))),
        ],
    );
    let run = |e: &str| exp == "all" || exp == e;
    if run("a") {
        expac::run_a(&ds, repo, &out);
    }
    if run("c") {
        expac::run_c(&ds, &out);
    }
    if exp == "c4" {
        expac::run_c4(&ds, &out);
    }
    if run("e") {
        expe::run_e(&ds, &out);
    }
    if run("f") {
        expf::run_f(&ds, &out);
    }
    if run("g") {
        expg::run_g(&ds, &out);
    }
}

fn fixture(which: &str, outdir: &str, scale: usize, seed: u64) {
    use std::process::Command;
    use vcs_bench::workloads::{self, Workload, ALL};
    let ws: Vec<Workload> = if which == "all" { ALL.to_vec() } else { vec![Workload::parse(which).expect("workload")] };
    for w in ws {
        let plan = workloads::plan(w, scale);
        let work = format!("{outdir}/{}.work", w.name());
        let bare = format!("{outdir}/{}.git", w.name());
        let _ = Command::new("rm").args(["-rf", &work, &bare]).status();
        std::fs::create_dir_all(&work).unwrap();
        workloads::generate(&plan, std::path::Path::new(&work), seed).unwrap();
        let g = |args: &[&str], step: u64| {
            let d = format!("{}", 1_600_000_000 + step * 600);
            let st = Command::new("git").args(["-C", &work]).args(args).env("GIT_AUTHOR_DATE", format!("{d} +0000")).env("GIT_COMMITTER_DATE", format!("{d} +0000")).env("GIT_AUTHOR_NAME", "lab").env("GIT_AUTHOR_EMAIL", "lab@example.com").env("GIT_COMMITTER_NAME", "lab").env("GIT_COMMITTER_EMAIL", "lab@example.com").status().unwrap();
            assert!(st.success(), "git {args:?}");
        };
        g(&["init", "-q", "-b", "main"], 0);
        g(&["add", "-A"], 0);
        g(&["commit", "-q", "-m", "base"], 0);
        for rev in 1..=plan.revisions {
            workloads::evolve(&plan, std::path::Path::new(&work), seed, rev as u64, plan.edits_per_rev).unwrap();
            g(&["add", "-A"], rev as u64);
            g(&["commit", "-q", "-m", &format!("rev {rev}")], rev as u64);
        }
        assert!(Command::new("git").args(["clone", "-q", "--bare", &work, &bare]).status().unwrap().success());
        let _ = Command::new("rm").args(["-rf", &work]).status();
        println!("{} -> {bare} ({} revisions, scale {scale})", w.name(), plan.revisions);
    }
}
