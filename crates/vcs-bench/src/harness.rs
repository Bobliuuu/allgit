//! Operation runner for the storage benchmark harness (issue #182): fixtures, operations, cache modes,
//! randomized repeated trials, metadata. CLI benchmarks only: this module shells out to the `git` binary and
//! to `vcs`; core crates never do.

use crate::json::J;
use crate::stats::summarize;
use crate::workloads::{self, Plan};
use crate::Rng;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Git,
    Vcs,
    Walgit,
}
impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Git => "git",
            Tool::Vcs => "vcs",
            Tool::Walgit => "walgit",
        }
    }
    pub fn parse(s: &str) -> Option<Tool> {
        match s {
            "git" => Some(Tool::Git),
            "vcs" | "allgit" => Some(Tool::Vcs),
            "walgit" => Some(Tool::Walgit),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Warm,
    Cold,
    Partial,
}
impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Warm => "warm",
            Mode::Cold => "cold",
            Mode::Partial => "partial",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "warm" => Some(Mode::Warm),
            "cold" => Some(Mode::Cold),
            "partial" => Some(Mode::Partial),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub git_bin: String,
    pub vcs_bin: String,
    pub walgit_bin: Option<String>,
    pub scratch: PathBuf,
    pub seed: u64,
    pub trials: usize,
    pub scale: usize,
    /// "auto" | "off"
    pub drop_caches: String,
    /// Percentage of fixture files re-read in `partial` mode.
    pub partial_pct: u8,
}

#[derive(Clone, Debug)]
pub struct Cmd {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

fn cmd(argv: &[&str], cwd: &Path) -> Cmd {
    Cmd {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.to_path_buf(),
        env: vec![],
    }
}

#[derive(Clone, Debug)]
pub enum Step {
    Run(Cmd),
    /// Run, then SIGKILL its process group after `ms` (if still running).
    RunKilled(Cmd, u64),
    Edit {
        dir: PathBuf,
        pct: usize,
        tag: u64,
        prefix: Option<String>,
    },
    Mkdir(PathBuf),
}

pub struct TrialPlan {
    pub prep: Vec<Step>,
    pub timed: Vec<Cmd>,
    pub parallel: bool,
    pub cleanup: Vec<PathBuf>,
    pub note: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Generated files only, in `<root>/work`, no repository.
    Src,
    /// Full history, `<root>/{work,remote,peer}`.
    Repo,
}

#[derive(Clone, Debug, Default)]
pub struct FixtureInfo {
    pub hist_ids: Vec<String>,
}

pub struct OpCx<'a> {
    pub tool: Tool,
    pub root: &'a Path,
    pub plan: &'a Plan,
    pub cfg: &'a Config,
    pub info: &'a FixtureInfo,
    pub trial_seed: u64,
}
impl OpCx<'_> {
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }
    fn zone_arg(&self) -> String {
        self.plan.hist_zone.clone().unwrap_or_else(|| "root".into())
    }
    fn bin(&self) -> String {
        match self.tool {
            Tool::Git => self.cfg.git_bin.clone(),
            _ => self.cfg.vcs_bin.clone(),
        }
    }
    /// Tool-specific command: `git` args for Git, `vcs` args for Vcs.
    fn tc(&self, git: &[&str], vcs: &[&str], cwd: &Path) -> Cmd {
        let (args,) = match self.tool {
            Tool::Git => (git,),
            _ => (vcs,),
        };
        let mut argv = vec![self.bin()];
        argv.extend(args.iter().map(|s| s.to_string()));
        Cmd {
            argv,
            cwd: cwd.to_path_buf(),
            env: vec![],
        }
    }
    fn init_cmds(&self, dir: &Path) -> Vec<Cmd> {
        match self.tool {
            Tool::Git => vec![self.tc(&["init", "-q", "-b", "main"], &[], dir)],
            _ => {
                let mut v = vec![self.tc(&[], &["init"], dir)];
                for (n, p) in &self.plan.zones {
                    v.push(self.tc(&[], &["zone", "create", n, "--path", p], dir));
                }
                v
            }
        }
    }
    fn add_all(&self, dir: &Path) -> Cmd {
        self.tc(&["add", "-A"], &["add", "."], dir)
    }
    fn commit(&self, dir: &Path, m: &str) -> Cmd {
        self.tc(&["commit", "-q", "-m", m], &["commit", "-m", m], dir)
    }
    fn edit(&self, dir: &Path, pct: usize, tag: u64) -> Step {
        Step::Edit {
            dir: dir.to_path_buf(),
            pct,
            tag,
            prefix: None,
        }
    }
}

type OpFn = fn(&OpCx) -> Result<TrialPlan, String>;

pub struct OpDef {
    pub name: String,
    pub fixture: Fix,
    /// Needs a private copy of the fixture per trial (state changes that `cleanup` cannot undo).
    pub mutates: bool,
    /// Empty = all modes.
    pub modes: Vec<Mode>,
    pub f: OpFn,
}

fn tp(prep: Vec<Step>, timed: Vec<Cmd>) -> Result<TrialPlan, String> {
    Ok(TrialPlan {
        prep,
        timed,
        parallel: false,
        cleanup: vec![],
        note: String::new(),
    })
}

fn od(name: &str, fixture: Fix, mutates: bool, f: OpFn) -> OpDef {
    OpDef {
        name: name.into(),
        fixture,
        mutates,
        modes: vec![],
        f,
    }
}

fn rm_repo_dirs(cx: &OpCx) -> Vec<PathBuf> {
    vec![cx.work().join(".git"), cx.work().join(".vcs")]
}

fn sparse_k(cx: &OpCx, pct: usize) -> Result<Vec<(String, String)>, String> {
    let z = cx.plan.zones.len();
    let k = (z * pct + 50) / 100;
    if k < 1 {
        return Err(format!("workload has {z} zones: {pct}% selects none"));
    }
    Ok(cx.plan.zones.iter().take(k).cloned().collect())
}

fn sparse_clone(cx: &OpCx, pct: usize) -> Result<TrialPlan, String> {
    let sel = sparse_k(cx, pct)?;
    let dst = cx.root.join("sparse_dst");
    let mut t = match cx.tool {
        Tool::Git => {
            let url = format!("file://{}", cx.remote().display());
            let mut v = vec![cx.tc(&["clone", "-q", "--filter=blob:none", "--sparse", &url, "sparse_dst"], &[], cx.root)];
            let mut a: Vec<String> = ["sparse-checkout", "set"].iter().map(|s| s.to_string()).collect();
            a.extend(sel.iter().map(|(_, p)| p.clone()));
            let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
            v.push(cx.tc(&refs, &[], &dst));
            v
        }
        _ => {
            let r = cx.remote().display().to_string();
            let mut a: Vec<String> = vec!["clone".into(), r, "sparse_dst".into()];
            for (n, _) in &sel {
                a.push("--zone".into());
                a.push(n.clone());
            }
            let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
            vec![cx.tc(&[], &refs, cx.root)]
        }
    };
    let timed = std::mem::take(&mut t);
    Ok(TrialPlan {
        prep: vec![],
        timed,
        parallel: false,
        cleanup: vec![dst],
        note: format!("{} of {} zones", sel.len(), cx.plan.zones.len()),
    })
}

fn peer_publish_steps(cx: &OpCx, tag: u64) -> Vec<Step> {
    let peer = cx.root.join("peer");
    vec![
        cx.edit(&peer, 1, tag),
        Step::Run(cx.add_all(&peer)),
        Step::Run(cx.commit(&peer, "peer")),
        Step::Run(cx.tc(&["push", "-q", "origin", "HEAD:main"], &["publish"], &peer)),
    ]
}

pub fn ops() -> Vec<OpDef> {
    let mut v: Vec<OpDef> = vec![];
    v.push(od("init", Fix::Src, false, |cx| {
        let d = cx.root.join("empty");
        let c = cx.init_cmds(&d);
        // timed: init only (zone creation is part of vcs setup, included in the sequence)
        Ok(TrialPlan {
            prep: vec![Step::Mkdir(d.clone())],
            timed: c,
            parallel: false,
            cleanup: vec![d],
            note: "empty dir; vcs includes zone creation".into(),
        })
    }));
    v.push(od("add_initial", Fix::Src, false, |cx| {
        let w = cx.work();
        let prep = cx.init_cmds(&w).into_iter().map(Step::Run).collect();
        Ok(TrialPlan {
            prep,
            timed: vec![cx.add_all(&w)],
            parallel: false,
            cleanup: rm_repo_dirs(cx),
            note: String::new(),
        })
    }));
    v.push(od("commit_initial", Fix::Src, false, |cx| {
        let w = cx.work();
        let mut prep: Vec<Step> = cx.init_cmds(&w).into_iter().map(Step::Run).collect();
        prep.push(Step::Run(cx.add_all(&w)));
        Ok(TrialPlan {
            prep,
            timed: vec![cx.commit(&w, "initial")],
            parallel: false,
            cleanup: rm_repo_dirs(cx),
            note: String::new(),
        })
    }));
    v.push(od("status", Fix::Repo, false, |cx| {
        tp(vec![], vec![cx.tc(&["status", "--porcelain"], &["status"], &cx.work())])
    }));
    v.push(od("status_dirty", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(vec![cx.edit(&w, 1, 1)], vec![cx.tc(&["status", "--porcelain"], &["status"], &w)])
    }));
    v.push(od("diff", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(vec![cx.edit(&w, 1, 2)], vec![cx.tc(&["diff", "--stat"], &["diff", "--stat"], &w)])
    }));
    v.push(od("add_incremental", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(vec![cx.edit(&w, 1, 3)], vec![cx.add_all(&w)])
    }));
    v.push(od("commit", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(vec![cx.edit(&w, 1, 4), Step::Run(cx.add_all(&w))], vec![cx.commit(&w, "bench")])
    }));
    v.push(od("log", Fix::Repo, false, |cx| {
        let w = cx.work();
        let z = cx.zone_arg();
        let mut c = cx.tc(&["log", "--oneline"], &["log", "--oneline", "-n", "1000000", "--zone", &z], &w);
        if cx.tool == Tool::Git {
            c.argv.push("--no-color".into());
        }
        tp(vec![], vec![c])
    }));
    v.push(od("blame", Fix::Repo, false, |cx| {
        let w = cx.work();
        let hot = cx.plan.hot_file().to_string();
        tp(vec![], vec![cx.tc(&["blame", "--", &hot], &["blame", &hot], &w)])
    }));
    v.push(od("lookup", Fix::Repo, false, |cx| {
        if cx.info.hist_ids.is_empty() {
            return Err("no history ids".into());
        }
        let id = cx.info.hist_ids[(cx.trial_seed as usize) % cx.info.hist_ids.len()].clone();
        let z = cx.zone_arg();
        let w = cx.work();
        Ok(TrialPlan {
            prep: vec![],
            timed: vec![cx.tc(&["show", "--stat", "--format=%H", &id], &["show", &id, "--zone", &z], &w)],
            parallel: false,
            cleanup: vec![],
            note: format!("random commit {}", &id[..id.len().min(12)]),
        })
    }));
    v.push(od("tree_traversal", Fix::Repo, false, |cx| match cx.tool {
        Tool::Git => tp(vec![], vec![cx.tc(&["ls-tree", "-r", "HEAD"], &[], &cx.work())]),
        _ => Err("vcs CLI has no tree listing command (native API only)".into()),
    }));
    v.push(od("merge_base", Fix::Repo, false, |cx| match cx.tool {
        Tool::Git => tp(vec![], vec![cx.tc(&["merge-base", "main", "alt"], &[], &cx.work())]),
        _ => Err("vcs CLI has no merge-base command (native API only)".into()),
    }));
    v.push(od("switch", Fix::Repo, true, |cx| {
        let z = cx.zone_arg();
        tp(
            vec![],
            vec![cx.tc(&["switch", "-q", "alt"], &["switch", "alt", "--zone", &z], &cx.work())],
        )
    }));
    v.push(od("checkout_files", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(vec![cx.edit(&w, 10, 5)], vec![cx.tc(&["restore", "."], &["restore", "."], &w)])
    }));
    v.push(od("clone", Fix::Repo, false, |cx| {
        let c = match cx.tool {
            Tool::Git => cx.tc(
                &["clone", "-q", &format!("file://{}", cx.remote().display()), "clone_dst"],
                &[],
                cx.root,
            ),
            _ => cx.tc(&[], &["clone", &cx.remote().display().to_string(), "clone_dst"], cx.root),
        };
        Ok(TrialPlan {
            prep: vec![],
            timed: vec![c],
            parallel: false,
            cleanup: vec![cx.root.join("clone_dst")],
            note: String::new(),
        })
    }));
    for pct in [1usize, 5, 10, 50] {
        let f: OpFn = match pct {
            1 => |cx| sparse_clone(cx, 1),
            5 => |cx| sparse_clone(cx, 5),
            10 => |cx| sparse_clone(cx, 10),
            _ => |cx| sparse_clone(cx, 50),
        };
        v.push(od(&format!("sparse_clone_{pct}pct"), Fix::Repo, false, f));
    }
    v.push(od("fetch", Fix::Repo, true, |cx| {
        let prep = peer_publish_steps(cx, 6);
        tp(prep, vec![cx.tc(&["fetch", "-q", "origin"], &["fetch"], &cx.work())])
    }));
    v.push(od("publish", Fix::Repo, true, |cx| {
        let w = cx.work();
        tp(
            vec![cx.edit(&w, 1, 7), Step::Run(cx.add_all(&w)), Step::Run(cx.commit(&w, "bench"))],
            vec![cx.tc(&["push", "-q", "origin", "HEAD:main"], &["publish"], &w)],
        )
    }));
    v.push(od("sync", Fix::Repo, true, |cx| {
        let prep = peer_publish_steps(cx, 8);
        tp(prep, vec![cx.tc(&["pull", "-q", "--ff-only"], &["sync"], &cx.work())])
    }));
    v.push(od("publish_concurrent", Fix::Repo, true, |cx| {
        let n = 4;
        if cx.plan.zones.len() < n {
            return Err("needs >= 4 zones".into());
        }
        let mut prep = vec![];
        let mut timed = vec![];
        for i in 0..n {
            let d = cx.root.join(format!("cp{i}"));
            let (zn, zp) = cx.plan.zones[i].clone();
            match cx.tool {
                Tool::Git => prep.push(Step::Run(cx.tc(
                    &["clone", "-q", &format!("file://{}", cx.remote().display()), &format!("cp{i}")],
                    &[],
                    cx.root,
                ))),
                _ => prep.push(Step::Run(cx.tc(
                    &[],
                    &["clone", &cx.remote().display().to_string(), &format!("cp{i}")],
                    cx.root,
                ))),
            }
            prep.push(Step::Edit {
                dir: d.clone(),
                pct: 1,
                tag: 100 + i as u64,
                prefix: Some(format!("{zp}/")),
            });
            prep.push(Step::Run(cx.add_all(&d)));
            prep.push(Step::Run(cx.commit(&d, "cp")));
            let refspec = format!("HEAD:refs/heads/peer{i}");
            timed.push(cx.tc(&["push", "-q", "origin", &refspec], &["publish", "--zone", &zn], &d));
        }
        Ok(TrialPlan {
            prep,
            timed,
            parallel: true,
            cleanup: vec![],
            note: "4 publishers at once; git pushes to per-peer branches, vcs publishes one zone each".into(),
        })
    }));
    v.push(od("repack", Fix::Repo, true, |cx| {
        tp(vec![], vec![cx.tc(&["repack", "-a", "-d", "-q"], &["repack"], &cx.work())])
    }));
    v.push(od("gc", Fix::Repo, true, |cx| {
        let mut p = tp(vec![], vec![cx.tc(&["gc", "-q"], &["gc", "--dry-run"], &cx.work())])?;
        p.note = "vcs gc is --dry-run only (destructive gc not implemented)".into();
        Ok(p)
    }));
    let mut cold = od("cold_restart", Fix::Repo, false, |cx| {
        let w = cx.work();
        let z = cx.zone_arg();
        tp(
            vec![],
            vec![
                cx.tc(&["log", "-1", "--oneline"], &["log", "-n", "1", "--oneline", "--zone", &z], &w),
                cx.tc(&["status", "--porcelain"], &["status"], &w),
            ],
        )
    });
    cold.modes = vec![Mode::Cold];
    v.push(cold);
    v.push(od("recovery", Fix::Repo, true, |cx| {
        let w = cx.work();
        let ms = 2 + cx.trial_seed % 25;
        // kill an `add && commit` pipeline mid-flight, then time the recovery path (stale lock removal, fsck, status)
        let script = match cx.tool {
            Tool::Git => format!("{0} add -A && {0} commit -q -m crash", cx.bin()),
            _ => format!("{0} add . && {0} commit -m crash", cx.bin()),
        };
        let kill = cmd(&["sh", "-c", &script], &w);
        let rec = match cx.tool {
            Tool::Git => format!(
                "rm -f .git/index.lock; {0} fsck --no-progress >/dev/null 2>&1; {0} status --porcelain >/dev/null",
                cx.bin()
            ),
            _ => format!("{0} fsck >/dev/null && {0} status >/dev/null", cx.bin()),
        };
        Ok(TrialPlan {
            prep: vec![cx.edit(&w, 5, 9), Step::RunKilled(kill, ms)],
            timed: vec![cmd(&["sh", "-c", &rec], &w)],
            parallel: false,
            cleanup: vec![],
            note: format!("add+commit SIGKILLed after {ms} ms (may already have finished)"),
        })
    }));
    v
}

// ---------------------------------------------------------------------------------------------------------
// process helpers

fn base_env(c: &mut Command, cfg: &Config, tool: Tool) {
    let home = cfg.scratch.join("home");
    let _ = fs::create_dir_all(&home);
    c.env("HOME", &home).env("XDG_CONFIG_HOME", home.join("xdg"));
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c.env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "gc.auto")
        .env("GIT_CONFIG_VALUE_0", "0");
    c.env("GIT_CONFIG_KEY_1", "maintenance.auto").env("GIT_CONFIG_VALUE_1", "false");
    c.env("GIT_AUTHOR_NAME", "bench").env("GIT_AUTHOR_EMAIL", "bench@example.invalid");
    c.env("GIT_COMMITTER_NAME", "bench")
        .env("GIT_COMMITTER_EMAIL", "bench@example.invalid");
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_PAGER", "cat").env("LC_ALL", "C");
    c.env("VCS_AUTHOR_NAME", "bench").env("VCS_AUTHOR_EMAIL", "bench@example.invalid");
    let _ = tool;
}

fn build(c: &Cmd, cfg: &Config, tool: Tool) -> Command {
    let mut p = Command::new(&c.argv[0]);
    p.args(&c.argv[1..])
        .current_dir(&c.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    base_env(&mut p, cfg, tool);
    for (k, v) in &c.env {
        p.env(k, v);
    }
    p
}

/// Runs to completion; returns (exit ok, code, stderr tail).
fn run_cmd(c: &Cmd, cfg: &Config, tool: Tool) -> (bool, i32, String) {
    match build(c, cfg, tool).output() {
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr);
            let tail: String = e.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
            (o.status.success(), o.status.code().unwrap_or(-1), tail)
        }
        Err(e) => (false, -2, format!("spawn {}: {e}", c.argv[0])),
    }
}

fn run_killed(c: &Cmd, cfg: &Config, tool: Tool, ms: u64) {
    use std::os::unix::process::CommandExt;
    let mut p = build(c, cfg, tool);
    p.process_group(0);
    if let Ok(mut ch) = p.spawn() {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        if let Ok(None) = ch.try_wait() {
            let _ = Command::new("kill").args(["-9", "--", &format!("-{}", ch.id())]).status();
        }
        let _ = ch.wait();
    }
}

fn child_cpu_ms() -> (f64, f64) {
    // /proc/self/stat fields 16,17 (cutime, cstime) in clock ticks (assume 100 Hz). Linux only.
    let Ok(s) = fs::read_to_string("/proc/self/stat") else {
        return (f64::NAN, f64::NAN);
    };
    let Some(rest) = s.rsplit_once(')').map(|x| x.1) else {
        return (f64::NAN, f64::NAN);
    };
    let t: Vec<&str> = rest.split_whitespace().collect();
    let g = |i: usize| t.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(f64::NAN) * 10.0;
    (g(13), g(14))
}

pub fn sh_out(argv: &[&str]) -> String {
    Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let _ = fs::remove_dir_all(dst);
    if let Some(p) = dst.parent() {
        let _ = fs::create_dir_all(p);
    }
    let st = Command::new("cp").arg("-a").arg(src).arg(dst).status().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("cp -a {} {} failed", src.display(), dst.display()))
    }
}

// ---------------------------------------------------------------------------------------------------------
// cache control

pub struct CacheCtl {
    pub available: bool,
    pub method: String,
}

pub fn probe_cache(cfg: &Config) -> CacheCtl {
    if cfg.drop_caches == "off" {
        return CacheCtl {
            available: false,
            method: "disabled by --drop-caches off".into(),
        };
    }
    let r = drop_caches_raw();
    match r {
        Ok(m) => CacheCtl {
            available: true,
            method: m,
        },
        Err(e) => CacheCtl {
            available: false,
            method: e,
        },
    }
}

fn drop_caches_raw() -> Result<String, String> {
    let _ = Command::new("sync").status();
    if !Path::new("/proc/sys/vm/drop_caches").exists() {
        return Err("no /proc/sys/vm/drop_caches (not Linux); macOS `sudo purge` not attempted".into());
    }
    let root = sh_out(&["id", "-u"]) == "0";
    let st = if root {
        Command::new("sh").args(["-c", "echo 3 > /proc/sys/vm/drop_caches"]).status()
    } else {
        Command::new("sudo")
            .args(["-n", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"])
            .stderr(Stdio::null())
            .status()
    };
    match st {
        Ok(s) if s.success() => Ok(if root {
            "sync; echo 3 > /proc/sys/vm/drop_caches (root)".into()
        } else {
            "sync; sudo -n sh -c 'echo 3 > /proc/sys/vm/drop_caches'".into()
        }),
        _ => Err("drop_caches not permitted (sudo -n failed)".into()),
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        return Some(PathBuf::from(bin));
    }
    std::env::var("PATH")
        .ok()?
        .split(':')
        .map(|d| Path::new(d).join(bin))
        .find(|p| p.is_file())
}

fn read_discard(p: &Path) {
    if let Ok(mut f) = fs::File::open(p) {
        let mut buf = vec![0u8; 1 << 16];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
        }
    }
}

/// Read (and discard) every file under `dir` for which `pick(relpath)` holds.
fn read_tree(dir: &Path, base: &Path, pick: &dyn Fn(&str) -> bool) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            read_tree(&p, base, pick);
        } else if ft.is_file() {
            let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
            if pick(&rel) {
                read_discard(&p);
            }
        }
    }
}

fn set_cache(mode: Mode, root: &Path, cfg: &Config, seed: u64) {
    match mode {
        Mode::Warm => read_tree(root, root, &|_| true),
        Mode::Cold | Mode::Partial => {
            let _ = drop_caches_raw();
            for b in [&cfg.git_bin, &cfg.vcs_bin] {
                if let Some(p) = which(b) {
                    read_discard(&p);
                }
            }
            if mode == Mode::Partial {
                read_tree(root, root, &|rel| {
                    let mut h = blake3::Hasher::new();
                    h.update(rel.as_bytes());
                    h.update(&seed.to_le_bytes());
                    (h.finalize().as_bytes()[0] as u32 * 100 / 256) < cfg.partial_pct as u32
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// fixtures

fn must(c: &Cmd, cfg: &Config, tool: Tool) -> Result<(), String> {
    let (ok, code, err) = run_cmd(c, cfg, tool);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "fixture command failed (exit {code}): {:?} in {}: {err}",
            c.argv,
            c.cwd.display()
        ))
    }
}

fn dated(mut c: Cmd, step: u64) -> Cmd {
    let ts = 1_700_000_000 + 60 * step;
    c.env.push(("VCS_TIMESTAMP".into(), ts.to_string()));
    c.env.push(("GIT_AUTHOR_DATE".into(), format!("@{ts} +0000")));
    c.env.push(("GIT_COMMITTER_DATE".into(), format!("@{ts} +0000")));
    c
}

pub fn build_src_fixture(plan: &Plan, root: &Path, seed: u64) -> Result<(), String> {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root.join("work")).map_err(|e| e.to_string())?;
    workloads::generate(plan, &root.join("work"), seed).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build `<root>/{work,remote,peer}` with the workload's full history for `tool`.
pub fn build_repo_fixture(cfg: &Config, tool: Tool, plan: &Plan, src_root: &Path, root: &Path) -> Result<FixtureInfo, String> {
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let work = root.join("work");
    copy_tree(&src_root.join("work"), &work)?;
    let info0 = FixtureInfo::default();
    let cx = OpCx {
        tool,
        root,
        plan,
        cfg,
        info: &info0,
        trial_seed: 0,
    };
    let remote = root.join("remote");
    let mut step = 0u64;
    let run = |c: Cmd, step: u64| must(&dated(c, step), cfg, tool);
    for c in cx.init_cmds(&work) {
        run(c, step)?;
    }
    run(cx.add_all(&work), step)?;
    run(cx.commit(&work, "base"), step)?;
    let z = cx.zone_arg();
    for rev in 1..=plan.revisions {
        step += 1;
        workloads::evolve(plan, &work, cfg.seed, rev as u64, plan.edits_per_rev).map_err(|e| e.to_string())?;
        run(cx.add_all(&work), step)?;
        run(cx.commit(&work, &format!("rev {rev}")), step)?;
    }
    // remote + publish
    match tool {
        Tool::Git => {
            run(cx.tc(&["init", "-q", "--bare", "-b", "main", "remote"], &[], root), step)?;
            run(cx.tc(&["config", "uploadpack.allowFilter", "true"], &[], &remote), step)?;
            run(cx.tc(&["config", "uploadpack.allowAnySHA1InWant", "true"], &[], &remote), step)?;
            run(
                cx.tc(&["remote", "add", "origin", &format!("file://{}", remote.display())], &[], &work),
                step,
            )?;
            run(cx.tc(&["push", "-q", "origin", "main"], &[], &work), step)?;
            run(cx.tc(&["branch", "-q", "--set-upstream-to=origin/main", "main"], &[], &work), step)?;
        }
        _ => {
            fs::create_dir_all(&remote).map_err(|e| e.to_string())?;
            run(cx.tc(&[], &["remote", "set", &remote.display().to_string()], &work), step)?;
            run(cx.tc(&[], &["publish"], &work), step)?;
        }
    }
    // branches (many_branches) and the `alt` branch used by switch / merge-base
    let on = |b: &str| cx.tc(&["switch", "-q", b], &["switch", b, "--zone", &z], &work);
    for b in 0..plan.branches {
        step += 1;
        let name = format!("b{b:03}");
        run(cx.tc(&["branch", &name], &["branch", &name, "--zone", &z], &work), step)?;
        run(on(&name), step)?;
        workloads::evolve_one(plan, &work, cfg.seed, 10_000 + b as u64).map_err(|e| e.to_string())?;
        run(cx.add_all(&work), step)?;
        run(cx.commit(&work, &format!("branch {b}")), step)?;
        run(on("main"), step)?;
    }
    step += 1;
    run(cx.tc(&["branch", "alt"], &["branch", "alt", "--zone", &z], &work), step)?;
    run(on("alt"), step)?;
    workloads::evolve_pct(plan, &work, cfg.seed, 77_777, 10).map_err(|e| e.to_string())?;
    run(cx.add_all(&work), step)?;
    run(cx.commit(&work, "alt change"), step)?;
    run(on("main"), step)?;
    // peer clone
    match tool {
        Tool::Git => run(
            cx.tc(&["clone", "-q", &format!("file://{}", remote.display()), "peer"], &[], root),
            step,
        )?,
        _ => run(cx.tc(&[], &["clone", &remote.display().to_string(), "peer"], root), step)?,
    }
    // history ids (newest first) for random lookups
    let out = match tool {
        Tool::Git => sh_cmd(&[&cfg.git_bin, "log", "--format=%H"], &work, cfg, tool),
        _ => sh_cmd(&[&cfg.vcs_bin, "log", "--oneline", "-n", "1000000", "--zone", &z], &work, cfg, tool),
    };
    let hist_ids: Vec<String> = out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect();
    if hist_ids.is_empty() {
        return Err("fixture produced no history ids".into());
    }
    Ok(FixtureInfo { hist_ids })
}

fn sh_cmd(argv: &[&str], cwd: &Path, cfg: &Config, tool: Tool) -> String {
    let c = Cmd {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.to_path_buf(),
        env: vec![],
    };
    let mut p = build(&c, cfg, tool);
    p.stdout(Stdio::piped());
    p.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

pub fn dir_size(p: &Path) -> u64 {
    let mut t = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    t += dir_size(&e.path());
                } else if let Ok(m) = e.metadata() {
                    t += m.len();
                }
            }
        }
    }
    t
}

// ---------------------------------------------------------------------------------------------------------
// results

#[derive(Clone, Debug)]
pub struct Rec {
    pub workload: String,
    pub tool: Tool,
    pub op: String,
    pub mode: Mode,
    pub trial: usize,
    pub order: usize,
    pub wall_ms: f64,
    pub cpu_user_ms: f64,
    pub cpu_sys_ms: f64,
    pub ok: bool,
    pub exit_codes: Vec<i32>,
    pub note: String,
    pub stderr_tail: String,
}

#[derive(Clone, Debug)]
pub struct Skip {
    pub workload: String,
    pub tool: Tool,
    pub op: String,
    pub mode: Option<Mode>,
    pub reason: String,
}

pub struct RunSpec {
    pub workloads: Vec<workloads::Workload>,
    pub tools: Vec<Tool>,
    pub ops: Vec<String>,
    pub modes: Vec<Mode>,
}

pub struct RunOutput {
    pub recs: Vec<Rec>,
    pub skips: Vec<Skip>,
    pub datasets: Vec<J>,
    pub cache: CacheCtl,
}

pub fn walgit_probe(cfg: &Config) -> (bool, String) {
    let cand = cfg.walgit_bin.clone().or_else(|| std::env::var("WALGIT_BIN").ok());
    match cand {
        Some(b) if Path::new(&b).is_file() => (true, format!("{b}: {}", sh_out(&[&b, "--version"]))),
        Some(b) => (false, format!("{b} not found")),
        None => (false, "no --walgit-bin / WALGIT_BIN given".into()),
    }
}

pub fn run(cfg: &Config, spec: &RunSpec, log: &mut dyn FnMut(&str)) -> Result<RunOutput, String> {
    let cache = probe_cache(cfg);
    log(&format!("cache control: available={} ({})", cache.available, cache.method));
    let all_ops = ops();
    let sel: Vec<&OpDef> = all_ops
        .iter()
        .filter(|o| spec.ops.is_empty() || spec.ops.iter().any(|n| n == &o.name))
        .collect();
    if sel.is_empty() {
        return Err("no operations selected".into());
    }
    let mut out = RunOutput {
        recs: vec![],
        skips: vec![],
        datasets: vec![],
        cache,
    };
    let (walgit_ready, walgit_msg) = walgit_probe(cfg);
    let mut order = 0usize;
    for w in &spec.workloads {
        let plan = workloads::plan(*w, cfg.scale);
        let wdir = cfg.scratch.join(w.name());
        let src_root = wdir.join("src");
        log(&format!(
            "== {} (scale {}): {} files, {} bytes",
            w.name(),
            cfg.scale,
            plan.files.len(),
            plan.total_bytes()
        ));
        build_src_fixture(&plan, &src_root, cfg.seed)?;
        out.datasets.push(J::obj(vec![
            ("workload", J::s(w.name())),
            ("scale", J::n(cfg.scale as f64)),
            ("files", J::n(plan.files.len() as f64)),
            ("bytes", J::n(plan.total_bytes() as f64)),
            ("zones", J::n(plan.zones.len() as f64)),
            ("revisions", J::n(plan.revisions as f64)),
            ("branches", J::n(plan.branches as f64)),
        ]));
        let mut fixtures: BTreeMap<Tool, (PathBuf, FixtureInfo)> = BTreeMap::new();
        let mut cells: Vec<(Tool, usize, Mode, usize)> = vec![];
        for &tool in &spec.tools {
            if tool == Tool::Walgit {
                out.skips.push(Skip {
                    workload: w.name().into(),
                    tool,
                    op: "*".into(),
                    mode: None,
                    reason: if walgit_ready {
                        format!("walgit found ({walgit_msg}) but the runner is a stub: see docs/research/storage/benchmark-methodology.md")
                    } else {
                        format!("walgit runner stub, binary unavailable: {walgit_msg}")
                    },
                });
                continue;
            }
            let needs_repo = sel.iter().any(|o| o.fixture == Fix::Repo);
            if needs_repo {
                let root = wdir.join(tool.name());
                log(&format!("building {} fixture ...", tool.name()));
                let t0 = Instant::now();
                match build_repo_fixture(cfg, tool, &plan, &src_root, &root) {
                    Ok(info) => {
                        log(&format!(
                            "  built in {:.1}s, {} history ids, {} bytes on disk",
                            t0.elapsed().as_secs_f64(),
                            info.hist_ids.len(),
                            dir_size(&root)
                        ));
                        fixtures.insert(tool, (root, info));
                    }
                    Err(e) => {
                        log(&format!("  FIXTURE FAILED: {e}"));
                        out.skips.push(Skip {
                            workload: w.name().into(),
                            tool,
                            op: "*".into(),
                            mode: None,
                            reason: format!("fixture build failed: {e}"),
                        });
                        continue;
                    }
                }
            }
            for (oi, op) in sel.iter().enumerate() {
                let (base, info) = match op.fixture {
                    Fix::Src => (src_root.clone(), FixtureInfo::default()),
                    Fix::Repo => match fixtures.get(&tool) {
                        Some((r, i)) => (r.clone(), i.clone()),
                        None => continue,
                    },
                };
                let cx = OpCx {
                    tool,
                    root: &base,
                    plan: &plan,
                    cfg,
                    info: &info,
                    trial_seed: 0,
                };
                if let Err(reason) = (op.f)(&cx) {
                    out.skips.push(Skip {
                        workload: w.name().into(),
                        tool,
                        op: op.name.clone(),
                        mode: None,
                        reason,
                    });
                    continue;
                }
                let modes: Vec<Mode> = if op.modes.is_empty() {
                    spec.modes.clone()
                } else {
                    op.modes.iter().copied().filter(|m| spec.modes.contains(m)).collect()
                };
                for m in modes {
                    if m != Mode::Warm && !out.cache.available {
                        out.skips.push(Skip {
                            workload: w.name().into(),
                            tool,
                            op: op.name.clone(),
                            mode: Some(m),
                            reason: format!("cache drop unavailable: {}", out.cache.method),
                        });
                        continue;
                    }
                    for t in 0..cfg.trials {
                        cells.push((tool, oi, m, t));
                    }
                }
            }
        }
        // randomized order across the whole cell
        let mut rng = Rng::new(cfg.seed ^ 0x0DE5_0000 ^ (*w as u64));
        for i in (1..cells.len()).rev() {
            cells.swap(i, (rng.next_u64() as usize) % (i + 1));
        }
        log(&format!("running {} trials in randomized order", cells.len()));
        for (tool, oi, mode, t) in cells {
            let op = sel[oi];
            let (base, info) = match op.fixture {
                Fix::Src => (src_root.clone(), FixtureInfo::default()),
                Fix::Repo => {
                    let (r, i) = fixtures.get(&tool).unwrap();
                    (r.clone(), i.clone())
                }
            };
            order += 1;
            let trial_seed = Rng::new(cfg.seed ^ (order as u64).wrapping_mul(0x9E37_79B9)).next_u64() >> 8;
            let (root, copied) = if op.mutates {
                let r = cfg.scratch.join("run").join(format!("t{order}"));
                copy_tree(&base, &r)?;
                (r, true)
            } else {
                (base.clone(), false)
            };
            let cx = OpCx {
                tool,
                root: &root,
                plan: &plan,
                cfg,
                info: &info,
                trial_seed,
            };
            let tpn = (op.f)(&cx).map_err(|e| format!("op {} plan failed on trial: {e}", op.name))?;
            let mut rec = Rec {
                workload: w.name().into(),
                tool,
                op: op.name.clone(),
                mode,
                trial: t,
                order,
                wall_ms: f64::NAN,
                cpu_user_ms: f64::NAN,
                cpu_sys_ms: f64::NAN,
                ok: false,
                exit_codes: vec![],
                note: tpn.note.clone(),
                stderr_tail: String::new(),
            };
            let mut prep_ok = true;
            // a copied fixture still points at the base fixture's remote: re-point work and peer at the copy
            let mut steps: Vec<Step> = vec![];
            if copied && op.fixture == Fix::Repo {
                let url = root.join("remote");
                for d in ["work", "peer"] {
                    let dir = root.join(d);
                    steps.push(Step::Run(match tool {
                        Tool::Git => cx.tc(&["remote", "set-url", "origin", &format!("file://{}", url.display())], &[], &dir),
                        _ => cx.tc(&[], &["remote", "set", &url.display().to_string()], &dir),
                    }));
                }
            }
            steps.extend(tpn.prep.iter().cloned());
            for s in &steps {
                match s {
                    Step::Run(c) => {
                        let (ok, code, err) = run_cmd(c, cfg, tool);
                        if !ok {
                            prep_ok = false;
                            rec.stderr_tail = format!("prep {:?} exit {code}: {err}", c.argv);
                        }
                    }
                    Step::RunKilled(c, ms) => run_killed(c, cfg, tool, *ms),
                    Step::Edit { dir, pct, tag, prefix } => {
                        let r = match prefix {
                            Some(p) => workloads::evolve_in(&plan, dir, cfg.seed, *tag, (plan.files_in(p).len() * pct / 100).max(1), p),
                            None => workloads::evolve_pct(&plan, dir, cfg.seed, *tag, *pct),
                        };
                        if let Err(e) = r {
                            prep_ok = false;
                            rec.stderr_tail = format!("edit: {e}");
                        }
                    }
                    Step::Mkdir(d) => {
                        let _ = fs::remove_dir_all(d);
                        let _ = fs::create_dir_all(d);
                    }
                }
                if !prep_ok {
                    break;
                }
            }
            if prep_ok {
                set_cache(mode, &root, cfg, cfg.seed ^ order as u64);
                let (u0, s0) = child_cpu_ms();
                let t0 = Instant::now();
                if tpn.parallel {
                    let mut ch = vec![];
                    for c in &tpn.timed {
                        let mut p = build(c, cfg, tool);
                        match p.spawn() {
                            Ok(k) => ch.push(Some(k)),
                            Err(_) => {
                                ch.push(None);
                                rec.exit_codes.push(-2)
                            }
                        }
                    }
                    for k in ch.into_iter().flatten() {
                        let o = k.wait_with_output();
                        match o {
                            Ok(o) => {
                                rec.exit_codes.push(o.status.code().unwrap_or(-1));
                                if !o.status.success() && rec.stderr_tail.is_empty() {
                                    rec.stderr_tail = String::from_utf8_lossy(&o.stderr).chars().take(300).collect();
                                }
                            }
                            Err(_) => rec.exit_codes.push(-2),
                        }
                    }
                } else {
                    for c in &tpn.timed {
                        let (ok, code, err) = run_cmd(c, cfg, tool);
                        rec.exit_codes.push(code);
                        if !ok {
                            rec.stderr_tail = err;
                            break;
                        }
                    }
                }
                rec.wall_ms = t0.elapsed().as_secs_f64() * 1e3;
                let (u1, s1) = child_cpu_ms();
                rec.cpu_user_ms = u1 - u0;
                rec.cpu_sys_ms = s1 - s0;
                rec.ok = !rec.exit_codes.is_empty() && rec.exit_codes.iter().all(|c| *c == 0);
                if tpn.parallel {
                    let good = rec.exit_codes.iter().filter(|c| **c == 0).count();
                    rec.note = format!("{}; {good}/{} publishers succeeded", rec.note, rec.exit_codes.len());
                }
            }
            for c in &tpn.cleanup {
                let _ = fs::remove_dir_all(c);
            }
            if copied {
                let _ = fs::remove_dir_all(&root);
            }
            if !rec.ok {
                log(&format!(
                    "  FAIL {} {} {} {}: {}",
                    w.name(),
                    tool.name(),
                    op.name,
                    mode.name(),
                    rec.stderr_tail.replace('\n', " ")
                ));
            }
            out.recs.push(rec);
        }
        // fixtures are large: delete after each workload
        let _ = fs::remove_dir_all(&wdir);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------
// output

type Key = (String, Tool, String, Mode);

pub struct SummaryRow {
    pub key: Key,
    pub trials: usize,
    pub failed: usize,
    pub wall: Option<crate::stats::Summary>,
    pub cpu: Option<crate::stats::Summary>,
}

pub fn summarize_all(recs: &[Rec], seed: u64) -> Vec<SummaryRow> {
    let mut m: BTreeMap<Key, Vec<&Rec>> = BTreeMap::new();
    for r in recs {
        m.entry((r.workload.clone(), r.tool, r.op.clone(), r.mode)).or_default().push(r);
    }
    m.into_iter()
        .map(|(key, rs)| {
            let ok: Vec<&&Rec> = rs.iter().filter(|r| r.ok).collect();
            let wall: Vec<f64> = ok.iter().map(|r| r.wall_ms).collect();
            let cpu: Vec<f64> = ok.iter().map(|r| r.cpu_user_ms + r.cpu_sys_ms).filter(|x| x.is_finite()).collect();
            SummaryRow {
                trials: rs.len(),
                failed: rs.len() - ok.len(),
                wall: summarize(&wall, seed),
                cpu: summarize(&cpu, seed ^ 1),
                key,
            }
        })
        .collect()
}

fn sj(s: &Option<crate::stats::Summary>) -> J {
    match s {
        None => J::Null,
        Some(s) => J::obj(vec![
            ("n", J::n(s.n as f64)),
            ("median", J::n(s.median)),
            ("mean", J::n(s.mean)),
            ("stdev", J::n(s.stdev)),
            ("min", J::n(s.min)),
            ("max", J::n(s.max)),
            ("ci95_lo", J::n(s.ci95_lo)),
            ("ci95_hi", J::n(s.ci95_hi)),
            ("ci_reliable", J::Bool(s.ci_reliable)),
        ]),
    }
}

pub fn results_json(meta: J, out: &RunOutput, seed: u64) -> J {
    let sums = summarize_all(&out.recs, seed);
    J::obj(vec![
        ("schema", J::s("allgit-storebench/1")),
        ("meta", meta),
        ("datasets", J::Arr(out.datasets.clone())),
        (
            "summary",
            J::Arr(
                sums.iter()
                    .map(|s| {
                        J::obj(vec![
                            ("workload", J::s(&s.key.0)),
                            ("tool", J::s(s.key.1.name())),
                            ("op", J::s(&s.key.2)),
                            ("mode", J::s(s.key.3.name())),
                            ("trials", J::n(s.trials as f64)),
                            ("failed", J::n(s.failed as f64)),
                            ("wall_ms", sj(&s.wall)),
                            ("cpu_ms", sj(&s.cpu)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "trials",
            J::Arr(
                out.recs
                    .iter()
                    .map(|r| {
                        J::obj(vec![
                            ("order", J::n(r.order as f64)),
                            ("workload", J::s(&r.workload)),
                            ("tool", J::s(r.tool.name())),
                            ("op", J::s(&r.op)),
                            ("mode", J::s(r.mode.name())),
                            ("trial", J::n(r.trial as f64)),
                            ("wall_ms", J::n(r.wall_ms)),
                            ("cpu_user_ms", J::n(r.cpu_user_ms)),
                            ("cpu_sys_ms", J::n(r.cpu_sys_ms)),
                            ("ok", J::Bool(r.ok)),
                            ("exit_codes", J::Arr(r.exit_codes.iter().map(|c| J::n(*c as f64)).collect())),
                            ("note", J::s(&r.note)),
                            ("stderr_tail", J::s(&r.stderr_tail)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "skipped",
            J::Arr(
                out.skips
                    .iter()
                    .map(|s| {
                        J::obj(vec![
                            ("workload", J::s(&s.workload)),
                            ("tool", J::s(s.tool.name())),
                            ("op", J::s(&s.op)),
                            ("mode", s.mode.map(|m| J::s(m.name())).unwrap_or(J::Null)),
                            ("reason", J::s(&s.reason)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn csv_esc(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\"").replace('\n', " "))
    } else {
        s.to_string()
    }
}

pub fn trials_csv(out: &RunOutput) -> String {
    let mut s = String::from("order,workload,tool,op,mode,trial,wall_ms,cpu_user_ms,cpu_sys_ms,ok,exit_codes,note\n");
    for r in &out.recs {
        let codes: Vec<String> = r.exit_codes.iter().map(|c| c.to_string()).collect();
        s.push_str(&format!(
            "{},{},{},{},{},{},{:.3},{:.1},{:.1},{},{},{}\n",
            r.order,
            r.workload,
            r.tool.name(),
            r.op,
            r.mode.name(),
            r.trial,
            r.wall_ms,
            r.cpu_user_ms,
            r.cpu_sys_ms,
            r.ok,
            codes.join(" "),
            csv_esc(&r.note)
        ));
    }
    s
}

pub fn summary_csv(out: &RunOutput, seed: u64) -> String {
    let mut s = String::from(
        "workload,tool,op,mode,trials,failed,median_ms,ci95_lo_ms,ci95_hi_ms,ci_reliable,mean_ms,stdev_ms,min_ms,max_ms,median_cpu_ms\n",
    );
    for r in summarize_all(&out.recs, seed) {
        let (a, b, c, d, e, f, g, h) = match &r.wall {
            Some(w) => (
                w.median,
                w.ci95_lo,
                w.ci95_hi,
                w.ci_reliable.to_string(),
                w.mean,
                w.stdev,
                w.min,
                w.max,
            ),
            None => (f64::NAN, f64::NAN, f64::NAN, "false".into(), f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        };
        let cpu = r.cpu.as_ref().map(|c| c.median).unwrap_or(f64::NAN);
        s.push_str(&format!(
            "{},{},{},{},{},{},{:.3},{:.3},{:.3},{},{:.3},{:.3},{:.3},{:.3},{:.1}\n",
            r.key.0,
            r.key.1.name(),
            r.key.2,
            r.key.3.name(),
            r.trials,
            r.failed,
            a,
            b,
            c,
            d,
            e,
            f,
            g,
            h,
            cpu
        ));
    }
    s
}

/// Hardware / software metadata for the result file.
pub fn metadata(cfg: &Config, cmdline: &str, cache: &CacheCtl, load_start: &str) -> J {
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_else(|| sh_out(&["sysctl", "-n", "machdep.cpu.brand_string"]));
    let mem = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("MemTotal")).map(|l| l.to_string()))
        .unwrap_or_default();
    let (walgit_ready, walgit_msg) = walgit_probe(cfg);
    let rev = std::env::var("ALLGIT_REV").unwrap_or_else(|_| sh_out(&["git", "rev-parse", "HEAD"]));
    let scratch = cfg.scratch.display().to_string();
    J::obj(vec![
        ("date_utc", J::s(sh_out(&["date", "-u", "+%FT%TZ"]))),
        ("command_line", J::s(cmdline)),
        ("seed", J::n(cfg.seed as f64)),
        ("trials", J::n(cfg.trials as f64)),
        ("scale", J::n(cfg.scale as f64)),
        ("host", J::s(sh_out(&["uname", "-n"]))),
        ("kernel", J::s(sh_out(&["uname", "-sr"]))),
        ("cpu_model", J::s(cpu)),
        ("cpu_cores", J::s(if Path::new("/proc/cpuinfo").exists() { sh_out(&["nproc"]) } else { sh_out(&["sysctl", "-n", "hw.ncpu"]) })),
        ("mem", J::s(mem)),
        ("scratch_fs", J::s(sh_out(&["df", "-T", &scratch]).lines().last().unwrap_or("").to_string())),
        ("loadavg_start", J::s(load_start)),
        ("loadavg_end", J::s(fs::read_to_string("/proc/loadavg").unwrap_or_default().trim())),
        ("transparent_hugepage", J::s(fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled").unwrap_or_default().trim())),
        ("git_version", J::s(sh_out(&[&cfg.git_bin, "--version"]))),
        ("vcs_version", J::s(sh_out(&[&cfg.vcs_bin, "--version"]))),
        ("rustc", J::s(sh_out(&["rustc", "--version"]))),
        ("allgit_rev", J::s(rev)),
        ("walgit", J::s(format!("ready={walgit_ready}: {walgit_msg}"))),
        ("partial_pct", J::n(cfg.partial_pct as f64)),
        ("cache_drop_available", J::Bool(cache.available)),
        ("cache_drop_method", J::s(&cache.method)),
        ("cache_modes", J::s("warm = every fixture file read once before the timed command; cold = sync + drop_caches then re-read the tool binaries only; partial = cold + a seeded fraction (partial_pct) of fixture files re-read")),
        ("timing", J::s("wall clock of the timed command sequence, spawn to exit, process start-up included; cpu = delta of children utime+stime from /proc/self/stat (10 ms ticks)")),
        ("ci", J::s("95% percentile bootstrap of the median, 2000 resamples; ci_reliable only when n >= 5")),
    ])
}
