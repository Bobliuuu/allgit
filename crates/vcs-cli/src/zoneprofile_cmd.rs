//! `vcs profile ...`, `vcs pull|sync --auto`, `vcs clone --auto/--auto-profile`: the local usage
//! profile and the zone predictor that picks what to download. Ranking logic lives in
//! `vcs_repo::zoneprofile` (pure, tested); this module gathers inputs, prints the plan and
//! applies it. Contract and limits: docs/zone-profile.md.

use crate::json::J;
use crate::{autozone, need_remote, open, remote, Cmd, ZoneCmd};
use clap::{Args, Subcommand};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use vcs_remote::Backend;
use vcs_repo::meta::Zones;
use vcs_repo::zoneprofile::{self as zp, Budget, Graph, Hints, Kind, Params, Profile};
use vcs_repo::{Error, Repo, SyncAction, ROOT_ZONE};

pub const HINTS_FILE: &str = ".vcs-profile-hints.toml";
/// New zones a `--auto` run adds when neither a flag nor the hints say otherwise.
pub const DEFAULT_MAX_ZONES: usize = 8;
/// Newest commits per zone the co-change miner reads, and how far apart (seconds) two commits with one author and message may be.
const COCHANGE_COMMITS: usize = 300;
const COCHANGE_WINDOW_S: u64 = 120;
const COCHANGE_MAX_GROUP: usize = 8;

#[derive(Args, Clone, Default)]
pub struct BudgetArgs {
    /// Add at most this many zones (default: the hints' max_zones, else 8). Zones already
    /// subscribed and the root zone are always kept and do not count
    #[arg(long, value_name = "N")]
    pub budget_zones: Option<usize>,
    /// Add at most this many stored bytes of file contents (e.g. 200M, 2G; default: no limit)
    #[arg(long, value_name = "SIZE")]
    pub budget_bytes: Option<String>,
}

/// Flags of `vcs sync` and `vcs pull`.
#[derive(Args, Clone, Default)]
pub struct SyncAuto {
    /// Choose which zones to download from your local usage profile (see `vcs profile`); the
    /// other zones get commits and trees only and are listed, fetchable with `vcs profile fetch`
    #[arg(long)]
    pub auto: bool,
    #[command(flatten)]
    pub budget: BudgetArgs,
    /// Seed the prediction with a profile or hints file (repeatable; with --auto)
    #[arg(long = "auto-profile", value_name = "FILE")]
    pub auto_profile: Vec<PathBuf>,
    /// With --auto: show why each zone was chosen or skipped
    #[arg(long)]
    pub explain: bool,
    /// With --auto: print the plan, download no file contents, change nothing but zone metadata
    #[arg(long)]
    pub dry_run: bool,
    /// Print the number of remote requests and bytes transferred
    #[arg(long)]
    pub stats: bool,
}

/// Extra flags of `vcs clone`.
#[derive(Args, Clone, Default)]
pub struct CloneAuto {
    /// Choose the zones to download from hints (`.vcs-profile-hints.toml`) and `--auto-profile`
    /// files; `--explain` and `--dry-run` work as for `--for`
    #[arg(long)]
    pub auto: bool,
    /// A profile (exported `.vcs/profile`) or hints file to predict from; implies --auto. Repeatable
    #[arg(long = "auto-profile", value_name = "FILE")]
    pub auto_profile: Vec<PathBuf>,
    #[command(flatten)]
    pub budget: BudgetArgs,
}
impl CloneAuto {
    pub fn active(&self) -> bool {
        self.auto || !self.auto_profile.is_empty()
    }
}

#[derive(Subcommand, Clone)]
pub enum ProfileCmd {
    /// Show what is recorded: counts per zone and where the file is
    Show,
    /// Start recording again after `off`
    On,
    /// Stop recording and erase what was recorded (hints still work)
    Off,
    /// Erase recorded events, keep recording
    Clear,
    /// Run a command and record it as build activity in the current zone (and in any zone whose
    /// directory the command line names). The command's exit status is passed through
    Record {
        /// Also record these paths as used (repeatable)
        #[arg(long = "path")]
        paths: Vec<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        cmd: Vec<String>,
    },
    /// Record that you read these paths (a hint for editors and build tools)
    Note { paths: Vec<String> },
    /// Rank the zones for this repository from the profile (offline) and print the plan
    Rank {
        #[command(flatten)]
        budget: BudgetArgs,
        /// Show why each zone is ranked where it is
        #[arg(long)]
        explain: bool,
    },
    /// List zones that are not subscribed, with how to fetch them
    Skipped,
    /// Download the contents of zones now (commits and trees too if missing) and subscribe
    Fetch {
        zones: Vec<String>,
        /// Also the zones they depend on
        #[arg(long)]
        with_deps: bool,
        /// Print the number of remote requests and bytes transferred
        #[arg(long)]
        stats: bool,
    },
}

// ------------------------------------------------------------------ --json

static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
thread_local! {
    static DOC: std::cell::RefCell<Vec<(&'static str, J)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `--json`: every command here then writes exactly one document (see [`flush_json`]).
pub fn set_json(on: bool) {
    JSON.store(on, std::sync::atomic::Ordering::SeqCst);
}
fn json_on() -> bool {
    JSON.load(std::sync::atomic::Ordering::SeqCst)
}
fn put(key: &'static str, v: J) {
    DOC.with(|d| d.borrow_mut().push((key, v)));
}
fn flush_json(command: &'static str) {
    if !json_on() {
        return;
    }
    let mut fields = vec![
        ("version", J::Num(u64::from(crate::json::SCHEMA_VERSION))),
        ("command", J::Str(command.into())),
    ];
    DOC.with(|d| fields.append(&mut d.borrow_mut()));
    println!("{}", J::Obj(fields).render());
}
fn say(m: &str) {
    if json_on() {
        put("message", jstr(m));
    } else {
        println!("{m}");
    }
}
fn jstr(s: impl Into<String>) -> J {
    J::Str(s.into())
}

// ------------------------------------------------------------------ helpers

fn now_ms() -> u64 {
    std::env::var("VCS_PROFILE_NOW_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        })
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn io_err(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Zone and privacy-trimmed directory of a repo-relative path.
fn hit(zones: &Zones, rel: &str, is_dir: bool) -> (String, String) {
    (zones.owner(rel).to_owned(), zp::privacy_dir(rel, is_dir))
}

fn load_hints(repo: &Repo, extra: &[PathBuf]) -> Result<(Hints, Vec<PathBuf>, Profile), Error> {
    let mut hints = Hints::default();
    let mut used = Vec::new();
    let mut seed = Profile::default();
    for p in [
        repo.worktree().join(HINTS_FILE),
        repo.worktree().join(".vcs").join("profile-hints.toml"),
    ] {
        if let Ok(t) = std::fs::read_to_string(&p) {
            hints.merge(Hints::parse(&t)?);
            used.push(p);
        }
    }
    for p in extra {
        let t = std::fs::read_to_string(p).map_err(|e| io_err(p, e))?;
        if t.starts_with("vcs-profile") {
            seed.import(&Profile::parse(&t)?);
        } else {
            hints.merge(Hints::parse(&t).map_err(|e| Error::Msg(format!("{}: {e}", p.display())))?);
        }
        used.push(p.clone());
    }
    Ok((hints, used, seed))
}

/// Map hint zone names (display names or ids) to ids; report unknown ones.
fn normalize(h: &Hints, zones: &Zones, notes: &mut Vec<String>) -> Hints {
    let mut r = |n: &String| -> Option<String> {
        let z = zones.resolve(n);
        if z.is_none() {
            notes.push(format!("hints name zone {n:?}, which does not exist (ignored)"));
        }
        z
    };
    let map = |v: &Vec<String>, f: &mut dyn FnMut(&String) -> Option<String>| v.iter().filter_map(f).collect::<Vec<_>>();
    let mut out = h.clone();
    out.roles = h.roles.iter().map(|(k, v)| (k.clone(), map(v, &mut r))).collect();
    out.pin = map(&h.pin, &mut r);
    out.exclude = map(&h.exclude, &mut r);
    out.deps = h.deps.iter().filter_map(|(k, v)| r(k).map(|k| (k, map(v, &mut r)))).collect();
    out
}

struct Opts {
    budget_zones: Option<usize>,
    budget_bytes: Option<u64>,
    explain: bool,
    dry_run: bool,
    extra: Vec<PathBuf>,
}

fn budget_of(b: &BudgetArgs) -> Result<(Option<usize>, Option<u64>), Error> {
    let bytes = match &b.budget_bytes {
        Some(s) => Some(zp::parse_bytes(s).ok_or_else(|| Error::Msg(format!("--budget-bytes: cannot read {s:?} (use e.g. 500M, 2G)")))?),
        None => None,
    };
    if b.budget_zones == Some(0) {
        return Err(Error::Msg("--budget-zones must be at least 1".into()));
    }
    Ok((b.budget_zones, bytes))
}

struct Computed {
    ranked: Vec<zp::Ranked>,
    plan: zp::Plan,
    notes: Vec<String>,
    events: usize,
    disabled: bool,
    role: Option<String>,
    files: BTreeMap<String, u64>,
    /// zones subscribed before this plan was applied
    already: BTreeSet<String>,
}

/// Rank and plan from what is in `repo` now (all zone heads fetched). `backend` is used only to
/// read manifests and sizes.
fn compute(repo: &Repo, backend: Option<&dyn Backend>, o: &Opts) -> Result<Computed, Error> {
    let zones = repo.zones()?;
    let (hints_raw, _used, seed) = load_hints(repo, &o.extra)?;
    let mut notes = Vec::new();
    let hints = normalize(&hints_raw, &zones, &mut notes);
    let mut profile = repo.profile_load()?;
    if !profile.disabled && !seed.events.is_empty() {
        let n = profile.import(&seed);
        if !o.dry_run {
            profile.save(&repo.profile_file())?;
        }
        notes.push(format!("imported {n} event(s) from --auto-profile file(s)"));
    }
    let names: Vec<String> = zones.names().map(str::to_owned).collect();
    let mut params = Params {
        now_ms: now_ms(),
        ..Params::default()
    };
    if let Some(h) = hints.half_life_days {
        params.half_life_days = h;
    }
    // Read manifests only of zones that can matter: the ones the profile or hints point at, then
    // the zones those depend on, `max_depth` rounds. Reading all of them costs one request each.
    let mut need: BTreeSet<String> = zp::rank(&profile, &hints, &Graph::default(), &names, &params)
        .into_iter()
        .filter(|r| r.score > 0.0 || r.pinned)
        .map(|r| r.zone)
        .collect();
    let mut inv = autozone::inventory(repo, backend, Some(&need), false)?;
    for _ in 0..params.max_depth {
        let more: Vec<String> = inv
            .deps
            .edges
            .keys()
            .filter(|(f, t)| need.contains(f) && !need.contains(t))
            .map(|(_, t)| t.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        need.extend(more);
        inv = autozone::inventory(repo, backend, Some(&need), false)?;
    }
    let sizes = autozone::inventory(repo, backend, Some(&BTreeSet::new()), true)?;
    inv.bytes = sizes.bytes;
    inv.files = sizes.files;
    notes.extend(inv.notes.iter().cloned());
    let mut graph = Graph::default();
    for (from, to) in inv.deps.edges.keys() {
        graph.add(from, to);
    }
    // co-change from the commits every zone already has locally (no request): zones committed together by one author with one message
    graph.cochange = zp::mine_cochange(&repo.cochange_records(COCHANGE_COMMITS)?, COCHANGE_WINDOW_S, COCHANGE_MAX_GROUP);
    let ranked = zp::rank(&profile, &hints, &graph, &names, &params);
    let ws = repo.workspace()?;
    let mut always: BTreeSet<String> = ws.subs.keys().cloned().collect();
    always.insert(ROOT_ZONE.to_owned());
    let budget = Budget {
        max_zones: Some(o.budget_zones.or(hints.max_zones).unwrap_or(DEFAULT_MAX_ZONES)),
        max_bytes: o.budget_bytes.or(hints.max_bytes),
    };
    let plan = zp::plan(&ranked, &inv.bytes, &always, &budget, params.min_score);
    if profile.events.is_empty() && hints_raw.role.is_none() && hints_raw.pin.is_empty() {
        notes.push("no usage profile and no hints yet: only the root zone and already subscribed zones are chosen. Work normally (commands are recorded), pass --auto-profile FILE, or add .vcs-profile-hints.toml".into());
    }
    Ok(Computed {
        ranked,
        plan,
        notes,
        events: profile.events.len(),
        disabled: profile.disabled,
        role: hints.role.clone(),
        files: inv.files,
        already: ws.subs.keys().cloned().collect(),
    })
}

fn plan_json(repo: &Repo, c: &Computed, applied: bool) -> Result<J, Error> {
    let zones = repo.zones()?;
    let by: BTreeMap<&str, &zp::Ranked> = c.ranked.iter().map(|r| (r.zone.as_str(), r)).collect();
    let item = |ch: &zp::Choice, hot: bool| {
        let mut f = vec![
            ("zone", jstr(ch.zone.clone())),
            ("label", jstr(zones.label(&ch.zone))),
            ("score", J::Float(ch.score)),
            ("bytes", J::Num(ch.bytes)),
            ("why", jstr(ch.why.clone())),
            ("already_subscribed", J::Bool(c.already.contains(&ch.zone))),
            (
                "reasons",
                J::Arr(
                    by.get(ch.zone.as_str())
                        .map(|r| r.reasons.iter().cloned().map(J::Str).collect())
                        .unwrap_or_default(),
                ),
            ),
        ];
        if !hot {
            f.push(("files", J::Num(c.files.get(&ch.zone).copied().unwrap_or(0))));
        }
        J::Obj(f)
    };
    Ok(J::Obj(vec![
        ("applied", J::Bool(applied)),
        ("events", J::Num(c.events as u64)),
        ("recording", J::Bool(!c.disabled)),
        ("role", c.role.clone().map_or(J::Null, J::Str)),
        ("zones_known", J::Num(c.ranked.len() as u64)),
        ("hot", J::Arr(c.plan.hot.iter().map(|h| item(h, true)).collect())),
        ("skipped", J::Arr(c.plan.skipped.iter().map(|h| item(h, false)).collect())),
        ("notes", J::Arr(c.notes.iter().cloned().map(J::Str).collect())),
    ]))
}

fn print_plan(repo: &Repo, c: &Computed, explain: bool, applied: bool) -> Result<(), Error> {
    if json_on() {
        put("plan", plan_json(repo, c, applied)?);
        return Ok(());
    }
    let zones = repo.zones()?;
    let by: BTreeMap<&str, &zp::Ranked> = c.ranked.iter().map(|r| (r.zone.as_str(), r)).collect();
    println!(
        "profile: {} event(s){}{}; {} zone(s) known",
        c.events,
        if c.disabled { ", recording is off" } else { "" },
        c.role.as_ref().map(|r| format!(", role {r}")).unwrap_or_default(),
        c.ranked.len()
    );
    let label = |z: &str| zones.label(z).to_owned();
    let hot_new: Vec<&zp::Choice> = c.plan.hot.iter().filter(|h| !c.already.contains(&h.zone)).collect();
    println!(
        "hot zones ({} {}, contents downloaded first, in this order):",
        hot_new.len(),
        if applied { "added" } else { "to add" }
    );
    for h in &c.plan.hot {
        let sub = if c.already.contains(&h.zone) { " (already subscribed)" } else { "" };
        println!("  {:<24} {:>9}  {}{}", label(&h.zone), human(h.bytes), h.why, sub);
        if explain {
            for r in by.get(h.zone.as_str()).map(|r| r.reasons.as_slice()).unwrap_or(&[]) {
                println!("      - {r}");
            }
        }
    }
    let skipped = &c.plan.skipped;
    println!(
        "skipped zones ({}): commits and trees only, file contents not downloaded",
        skipped.len()
    );
    for s in skipped {
        let files = c.files.get(&s.zone).copied().unwrap_or(0);
        println!(
            "  {:<24} {:>9} {:>8} file(s)  score {:.3}  {}",
            label(&s.zone),
            human(s.bytes),
            files,
            s.score,
            s.why
        );
        if explain {
            for r in by.get(s.zone.as_str()).map(|r| r.reasons.as_slice()).unwrap_or(&[]) {
                println!("      - {r}");
            }
        }
    }
    if !skipped.is_empty() {
        println!(
            "  get one with `vcs profile fetch <zone>` (or all: `vcs profile fetch {}`)",
            skipped.iter().map(|s| label(&s.zone)).collect::<Vec<_>>().join(" ")
        );
    }
    let total: u64 = hot_new.iter().map(|h| h.bytes).sum();
    let skipped_b: u64 = skipped.iter().map(|s| s.bytes).sum();
    println!(
        "bytes of file contents: {} {} now, {} left on the remote (stored sizes, before download compression effects)",
        human(total),
        if applied { "downloaded" } else { "to download" },
        human(skipped_b)
    );
    for n in &c.notes {
        println!("note: {n}");
    }
    Ok(())
}

/// Fetch commits and trees of every remote zone not subscribed (so planning can read manifests
/// and sizes, and skipped zones can be inspected offline). Returns the unsubscribed zone names.
fn fetch_metadata(repo: &Repo, backend: &dyn Backend) -> Result<Vec<String>, Error> {
    repo.fetch(backend, None)?; // subscribed zones; also adopts new zone definitions
    let ws = repo.workspace()?;
    let rest: Vec<String> = repo
        .zones()?
        .names()
        .filter(|z| !ws.subs.contains_key(*z))
        .map(str::to_owned)
        .collect();
    if !rest.is_empty() {
        repo.fetch(backend, Some(&rest))?;
    }
    Ok(rest)
}

/// Subscribe to the hot zones, hottest first. A zone that fails is reported and moved to the
/// skipped list (never dropped silently).
fn apply(repo: &Repo, backend: &dyn Backend, c: &mut Computed) -> Result<(), Error> {
    let rr = repo.remote_refs()?;
    let hot = std::mem::take(&mut c.plan.hot);
    let subscribed = repo.workspace()?.subs;
    let names: Vec<String> = hot
        .iter()
        .filter(|h| !subscribed.contains_key(&h.zone) && rr.head(&h.zone, vcs_repo::DEFAULT_BRANCH).is_some())
        .map(|h| h.zone.clone())
        .collect();
    // One download for all chosen zones; the order of `hot` (hottest first) is kept.
    let mut results: BTreeMap<String, Result<(), Error>> = repo.zones_add_fetching(backend, &names).into_iter().collect();
    for h in hot {
        if subscribed.contains_key(&h.zone) {
            c.plan.hot.push(h);
        } else if rr.head(&h.zone, vcs_repo::DEFAULT_BRANCH).is_none() {
            c.plan.skipped.push(zp::Choice {
                why: "no `main` branch on the remote".into(),
                ..h
            });
        } else {
            match results.remove(&h.zone) {
                Some(Ok(())) => c.plan.hot.push(h),
                Some(Err(e)) => c.plan.skipped.push(zp::Choice {
                    why: format!("could not add: {e}"),
                    ..h
                }),
                None => unreachable!("one result per zone"),
            }
        }
    }
    Ok(())
}

/// Prints the transfer since the previous stage (with --stats) so the cost of planning can be read apart from contents.
struct Stages<'a> {
    on: bool,
    ct: &'a vcs_remote::CountingBackend<'a>,
    last: (u64, u64),
}
impl<'a> Stages<'a> {
    fn new(on: bool, ct: &'a vcs_remote::CountingBackend<'a>) -> Self {
        Stages { on, ct, last: (0, 0) }
    }
    fn mark(&mut self, what: &str) {
        let s = self.ct.stats();
        if self.on && json_on() {
            DOC.with(|d| {
                let mut d = d.borrow_mut();
                if !d.iter().any(|(k, _)| *k == "stages") {
                    d.push(("stages", J::Arr(vec![])));
                }
                if let Some((_, J::Arr(v))) = d.iter_mut().find(|(k, _)| *k == "stages") {
                    v.push(J::Obj(vec![
                        ("stage", jstr(what)),
                        ("requests", J::Num(s.requests - self.last.0)),
                        ("bytes_down", J::Num(s.bytes_down - self.last.1)),
                    ]));
                }
            });
        } else if self.on {
            println!(
                "  stage {what}: requests {}, downloaded {} bytes",
                s.requests - self.last.0,
                s.bytes_down - self.last.1
            );
        }
        self.last = (s.requests, s.bytes_down);
    }
}

fn stats_line(label: &str, ct: &vcs_remote::CountingBackend) {
    let s = ct.stats();
    if json_on() {
        put(
            "transfer",
            J::Obj(vec![
                ("label", jstr(label)),
                ("requests", J::Num(s.requests)),
                ("bytes_down", J::Num(s.bytes_down)),
            ]),
        );
        return;
    }
    println!("{label}: requests {}, downloaded {} bytes", s.requests, s.bytes_down);
}

fn print_sync(rep: &vcs_repo::SyncReport) {
    if json_on() {
        let v = rep
            .actions
            .iter()
            .map(|(a, b, act)| {
                let (name, to) = match act {
                    SyncAction::UpToDate => ("up-to-date", None),
                    SyncAction::FastForwarded { to, .. } => ("fast-forwarded", Some(to.to_string())),
                    SyncAction::Ahead => ("ahead", None),
                    SyncAction::Diverged => ("diverged", None),
                    SyncAction::Dirty => ("dirty", None),
                    SyncAction::NoRemote => ("no-remote", None),
                };
                J::Obj(vec![
                    ("zone", jstr(a.clone())),
                    ("branch", jstr(b.clone())),
                    ("action", jstr(name)),
                    ("to", to.map_or(J::Null, J::Str)),
                ])
            })
            .collect();
        put("sync", J::Arr(v));
        return;
    }
    for (a, b, act) in &rep.actions {
        let what = match act {
            SyncAction::UpToDate => "up to date".to_owned(),
            SyncAction::FastForwarded { from, to } => format!(
                "fast-forwarded {} -> {}",
                from.map(|i| i.short()).unwrap_or_else(|| "(none)".into()),
                to.short()
            ),
            SyncAction::Ahead => "ahead of remote (run `vcs publish`)".into(),
            SyncAction::Diverged => {
                let r = if a == ROOT_ZONE { String::new() } else { format!(" --zone {a}") };
                format!("diverged; merge with `vcs merge --remote {b}{r}`")
            }
            SyncAction::Dirty => "dirty (left untouched; commit or discard changes first)".into(),
            SyncAction::NoRemote => "not on the remote yet (run `vcs publish`)".into(),
        };
        println!("{a}/{b}: {what}");
    }
}

// ------------------------------------------------------------------ entry points

/// `vcs sync` / `vcs pull` with the flags in `a` (`--auto` or not).
pub fn sync(a: &SyncAuto) -> Result<(), Error> {
    sync_inner(a)?;
    flush_json(if a.auto { "sync --auto" } else { "sync" });
    Ok(())
}

fn sync_inner(a: &SyncAuto) -> Result<(), Error> {
    let (repo, _) = open()?;
    let backend = remote::open_memo(&need_remote(&repo)?, false)?;
    let ct = vcs_remote::CountingBackend::new(&backend);
    if !a.auto {
        for (f, n) in [(a.explain, "--explain"), (a.dry_run, "--dry-run")] {
            if f {
                return Err(Error::Msg(format!("{n} needs --auto")));
            }
        }
        let rep = repo.sync(&ct)?;
        print_sync(&rep);
        if a.stats {
            stats_line("transfer", &ct);
        }
        return Ok(());
    }
    let (bz, bb) = budget_of(&a.budget)?;
    let o = Opts {
        budget_zones: bz,
        budget_bytes: bb,
        explain: a.explain,
        dry_run: a.dry_run,
        extra: a.auto_profile.clone(),
    };
    let mut st = Stages::new(a.stats, &ct);
    mark_auto(&repo);
    let idx = vcs_remote::IdxCacheBackend::new(&ct, repo.profile_state_dir().join("remote-idx"));
    let ct2: &dyn Backend = &idx;
    fetch_metadata(&repo, ct2)?;
    st.mark("metadata (commits and trees of all zones)");
    let mut c = compute(&repo, Some(ct2), &o)?;
    st.mark("planning (manifests, sizes)");
    if !o.dry_run {
        apply(&repo, ct2, &mut c)?;
        st.mark("contents of newly chosen zones");
    }
    print_plan(&repo, &c, o.explain, !o.dry_run)?;
    if o.dry_run {
        if a.stats {
            stats_line("transfer (metadata only, dry run)", &ct);
        }
        return Ok(());
    }
    let rep = repo.sync(ct2)?;
    st.mark("sync of subscribed zones");
    print_sync(&rep);
    if a.stats {
        stats_line("transfer (planning, metadata and contents included)", &ct);
    }
    Ok(())
}

/// Hook for `run`: `vcs clone --auto / --auto-profile`.
pub fn intercept(cmd: &Cmd) -> Option<Result<(), Error>> {
    match cmd {
        Cmd::Clone {
            url,
            dir,
            stats,
            sel,
            auto,
            ..
        } if auto.active() => {
            if sel.active() {
                return Some(Err(Error::Msg(
                    "--auto/--auto-profile cannot be combined with --for/--task/--for-owner; use one".into(),
                )));
            }
            Some(clone_auto(url, dir, *stats, sel.explain, sel.dry_run, auto))
        }
        _ => None,
    }
}

fn clone_auto(url: &str, dir: &Path, stats: bool, explain: bool, dry_run: bool, a: &CloneAuto) -> Result<(), Error> {
    clone_auto_inner(url, dir, stats, explain, dry_run, a)?;
    flush_json("clone --auto");
    Ok(())
}

fn clone_auto_inner(url: &str, dir: &Path, stats: bool, explain: bool, dry_run: bool, a: &CloneAuto) -> Result<(), Error> {
    let cwd = std::env::current_dir().map_err(|e| io_err(Path::new("."), e))?;
    let url = remote::normalize_url(url, &cwd);
    let (bz, bb) = budget_of(&a.budget)?;
    let o = Opts {
        budget_zones: bz,
        budget_bytes: bb,
        explain,
        dry_run,
        extra: a.auto_profile.clone(),
    };
    // A clone reads packs under 1 MiB whole, which would download every zone when the remote is
    // small; a selective clone wants ranged reads. An explicit setting wins.
    if std::env::var_os("VCS_SMALL_PACK_BYTES").is_none() {
        std::env::set_var("VCS_SMALL_PACK_BYTES", "0");
    }
    let backend = remote::open_memo(&url, true)?;
    let ct = vcs_remote::CountingBackend::new(&backend);
    let tmp;
    let dest: PathBuf = if dry_run {
        tmp = tempfile::tempdir().map_err(|e| io_err(Path::new("tempdir"), e))?;
        tmp.path().join("plan")
    } else {
        dir.to_path_buf()
    };
    let mut st = Stages::new(stats, &ct);
    let repo = Repo::clone_from(&ct, &url, &dest, Some(&[ROOT_ZONE.to_owned()]))?;
    st.mark("clone of the root zone");
    if !dry_run {
        mark_auto(&repo);
    }
    let idx = vcs_remote::IdxCacheBackend::new(&ct, repo.profile_state_dir().join("remote-idx"));
    let ct2: &dyn Backend = &idx;
    fetch_metadata(&repo, ct2)?;
    st.mark("metadata (commits and trees of all zones)");
    let mut c = compute(&repo, Some(ct2), &o)?;
    st.mark("planning (manifests, sizes)");
    if !dry_run {
        apply(&repo, ct2, &mut c)?;
        st.mark("contents of chosen zones");
    }
    if !dry_run {
        if json_on() {
            put("cloned_into", jstr(dir.display().to_string()));
        } else {
            println!("Cloned {url} into {} (zones chosen from your profile and hints)", dir.display());
        }
    }
    print_plan(&repo, &c, explain, !dry_run)?;
    if stats {
        stats_line("clone total transfer (planning included)", &ct);
    }
    Ok(())
}

// ------------------------------------------------------------------ vcs profile

pub fn run(c: &ProfileCmd) -> Result<(), Error> {
    run_inner(c)?;
    flush_json("profile");
    Ok(())
}

fn run_inner(c: &ProfileCmd) -> Result<(), Error> {
    let (repo, cwd) = open()?;
    match c {
        ProfileCmd::Show => {
            let p = repo.profile_load()?;
            let zones = repo.zones()?;
            let mut per: BTreeMap<&str, (u32, u32, u32)> = BTreeMap::new();
            for e in &p.events {
                let t = per.entry(e.zone.as_str()).or_default();
                match e.kind {
                    Kind::Read => t.0 += 1,
                    Kind::Edit => t.1 += 1,
                    Kind::Build => t.2 += 1,
                }
            }
            let (h, used, _) = load_hints(&repo, &[])?;
            if json_on() {
                put("file", jstr(repo.profile_file().display().to_string()));
                put("recording", J::Bool(!p.disabled));
                put("events", J::Num(p.events.len() as u64));
                put(
                    "zones",
                    J::Arr(
                        per.iter()
                            .map(|(z, (r, e, b))| {
                                J::Obj(vec![
                                    ("zone", jstr(*z)),
                                    ("label", jstr(zones.label(z))),
                                    ("read", J::Num(u64::from(*r))),
                                    ("edit", J::Num(u64::from(*e))),
                                    ("build", J::Num(u64::from(*b))),
                                ])
                            })
                            .collect(),
                    ),
                );
                put("hints", J::Arr(used.iter().map(|u| jstr(u.display().to_string())).collect()));
                put("role", h.role.map_or(J::Null, J::Str));
                return Ok(());
            }
            println!("profile file: {} (local only, never uploaded)", repo.profile_file().display());
            println!(
                "recording: {}",
                if p.disabled {
                    "off (`vcs profile on` to resume)"
                } else {
                    "on (`vcs profile off` to stop and erase)"
                }
            );
            println!("{} event(s)", p.events.len());
            for (z, (r, e, b)) in per {
                println!("  {:<24} read {r:>4}  edit {e:>4}  build {b:>4}", zones.label(z));
            }
            for u in used {
                println!("hints: {}", u.display());
            }
            if let Some(r) = h.role {
                println!("role: {r}");
            }
        }
        ProfileCmd::On => {
            repo.profile_set_enabled(true)?;
            say("profile recording is on");
        }
        ProfileCmd::Off => {
            repo.profile_set_enabled(false)?;
            say("profile recording is off and recorded events were erased; `--auto` now uses hints only");
        }
        ProfileCmd::Clear => {
            repo.profile_clear()?;
            say("profile events erased");
        }
        ProfileCmd::Note { paths } => {
            let zones = repo.zones()?;
            let mut hits = Vec::new();
            for p in paths {
                let rel = repo.rel_path(&cwd, p)?;
                let is_dir = cwd.join(p).is_dir();
                hits.push(hit(&zones, &rel, is_dir));
            }
            let n = repo.profile_record(Kind::Read, now_ms(), &hits)?;
            say(&format!("recorded {n} event(s)"));
        }
        ProfileCmd::Record { paths, cmd } => {
            let zones = repo.zones()?;
            let mut hits = Vec::new();
            let here = repo.rel_path(&cwd, ".")?;
            hits.push(hit(&zones, &here, true));
            for p in paths.iter().chain(cmd.iter().skip(1)) {
                // a word of the command line that names an existing path inside the repository
                if p.starts_with('-') && !paths.contains(p) {
                    continue;
                }
                let full = cwd.join(p);
                if full.exists() {
                    if let Ok(rel) = repo.rel_path(&cwd, p) {
                        hits.push(hit(&zones, &rel, full.is_dir()));
                    }
                }
            }
            repo.profile_record(Kind::Build, now_ms(), &hits)?;
            let status = std::process::Command::new(&cmd[0])
                .args(&cmd[1..])
                .status()
                .map_err(|e| Error::Msg(format!("cannot run {}: {e}", cmd[0])))?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        ProfileCmd::Rank { budget, explain } => {
            let (bz, bb) = budget_of(budget)?;
            let o = Opts {
                budget_zones: bz,
                budget_bytes: bb,
                explain: *explain,
                dry_run: true,
                extra: vec![],
            };
            let c = compute(&repo, None, &o)?;
            print_plan(&repo, &c, *explain, false)?;
        }
        ProfileCmd::Skipped => {
            let ws = repo.workspace()?;
            let zones = repo.zones()?;
            let rr = repo.remote_refs()?;
            let mut n = 0;
            let mut rows = Vec::new();
            for z in zones.names().filter(|z| !ws.subs.contains_key(*z)) {
                let meta = rr
                    .head(z, vcs_repo::DEFAULT_BRANCH)
                    .is_some_and(|h| repo.store().contains(&h).unwrap_or(false));
                if json_on() {
                    rows.push(J::Obj(vec![
                        ("zone", jstr(z)),
                        ("label", jstr(zones.label(z))),
                        ("metadata_local", J::Bool(meta)),
                    ]));
                    continue;
                }
                println!(
                    "{:<24} {}",
                    zones.label(z),
                    if meta {
                        "commits and trees local; contents on demand"
                    } else {
                        "nothing local; fetch to see it"
                    }
                );
                n += 1;
            }
            if json_on() {
                put("skipped", J::Arr(rows));
            } else if n == 0 {
                println!("every zone is subscribed");
            } else {
                println!("get one with `vcs profile fetch <zone>`");
            }
        }
        ProfileCmd::Fetch {
            zones: names,
            with_deps,
            stats,
        } => {
            if names.is_empty() {
                return Err(Error::Msg("give at least one zone".into()));
            }
            let backend = remote::open_memo(&need_remote(&repo)?, false)?;
            let ct = vcs_remote::CountingBackend::new(&backend);
            let mut defs = repo.zones()?;
            if names.iter().any(|n| defs.resolve(n).is_none()) {
                repo.fetch(&ct, Some(&[ROOT_ZONE.to_owned()]))?; // learn the remote's zone definitions
                defs = repo.zones()?;
            }
            let mut want: Vec<String> = Vec::new();
            for n in names {
                let z = defs.resolve(n).ok_or_else(|| Error::Msg(format!("no such zone {n:?}")))?;
                if !want.contains(&z) {
                    want.push(z);
                }
            }
            if *with_deps {
                repo.fetch(&ct, Some(&want))?;
                let inv = autozone::inventory(&repo, Some(&ct), None, true)?;
                let mut i = 0;
                while i < want.len() {
                    for (f, t) in inv.deps.edges.keys() {
                        if *f == want[i] && !want.contains(t) {
                            want.push(t.clone());
                        }
                    }
                    i += 1;
                }
            }
            let mut hits = Vec::new();
            let mut rows = Vec::new();
            for z in &want {
                if repo.workspace()?.subs.contains_key(z) {
                    rows.push((z.clone(), "already-subscribed"));
                    say(&format!("{}: already subscribed", defs.label(z)));
                    continue;
                }
                repo.fetch(&ct, Some(std::slice::from_ref(z)))?;
                repo.zone_add_fetching(&ct, z, None)?;
                rows.push((z.clone(), "fetched"));
                say(&format!("{}: fetched and subscribed", defs.label(z)));
                hits.push((z.clone(), String::new()));
            }
            if json_on() {
                DOC.with(|d| d.borrow_mut().retain(|(k, _)| *k != "message"));
                put(
                    "zones",
                    J::Arr(
                        rows.into_iter()
                            .map(|(z, st)| J::Obj(vec![("zone", jstr(z)), ("status", jstr(st))]))
                            .collect(),
                    ),
                );
            }
            let _ = repo.profile_record(Kind::Read, now_ms(), &hits);
            if *stats {
                stats_line("transfer", &ct);
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ fetch on access

/// File in the common dir that says this repository was set up or synced with `--auto`: only then is an
/// unsubscribed zone "skipped" (a deliberate `clone --zone` is left alone).
const AUTO_MARKER: &str = "profile-auto";

fn mark_auto(repo: &Repo) {
    let _ = std::fs::write(repo.profile_state_dir().join(AUTO_MARKER), b"1\n");
}

fn autofetch_off() -> bool {
    crate::config::Config::load()
        .ok()
        .and_then(|c| c.get("profile.autofetch"))
        .is_some_and(|v| matches!(v.as_str(), "off" | "false" | "0"))
}

/// Zones a command is about to need file contents of: `zone checkout Z`, and the zone that owns each path named by
/// `add/rm/restore/mv/blame`. Pure, so it is unit tested: `owner` maps a repo-relative path to its zone.
fn zones_needed(
    cmd: &Cmd,
    rel: &dyn Fn(&str) -> Option<String>,
    owner: &dyn Fn(&str) -> String,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut path = |p: &str| {
        if let Some(r) = rel(p) {
            let z = owner(&r);
            if z != ROOT_ZONE && !out.contains(&z) {
                out.push(z);
            }
        }
    };
    match cmd {
        Cmd::Add { paths, .. } | Cmd::Rm { paths, .. } | Cmd::Restore { paths, .. } => paths.iter().for_each(|p| path(p)),
        Cmd::Mv { src, dst, .. } => {
            path(src);
            path(dst);
        }
        Cmd::Blame { path: p, .. } => path(p),
        Cmd::Zone(ZoneCmd::Checkout { zone, .. }) => {
            if let Some(z) = resolve(zone) {
                if z != ROOT_ZONE && !out.contains(&z) {
                    out.push(z);
                }
            }
        }
        _ => {}
    }
    out
}

/// Transparent fetch-on-access: before a command runs, a zone it names (or whose path it names) that `--auto` skipped is fetched
/// and subscribed, with a line on stderr saying so. Best effort and quiet when there is nothing to do: a failure prints a warning
/// and the command then runs (and fails) as it would have. Off with `VCS_AUTOFETCH=off` or `vcs config set profile.autofetch off`.
/// Returns the zone ids fetched. Contents of a skipped zone that a read command (`show`, `log -p`, `diff`) needs are fetched one
/// object at a time by the missing-object path in `run_lazy`, which does not subscribe the zone.
pub fn ensure_access(cmd: &Cmd) -> Vec<String> {
    if autofetch_off() {
        return vec![];
    }
    let probe = matches!(
        cmd,
        Cmd::Add { .. } | Cmd::Rm { .. } | Cmd::Restore { .. } | Cmd::Mv { .. } | Cmd::Blame { .. } | Cmd::Zone(ZoneCmd::Checkout { .. })
    );
    if !probe {
        return vec![];
    }
    let Ok((repo, cwd)) = open() else { return vec![] };
    if !repo.profile_state_dir().join(AUTO_MARKER).exists() {
        return vec![];
    }
    match ensure_access_inner(&repo, &cwd, cmd) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: could not fetch a skipped zone on access: {e}");
            vec![]
        }
    }
}

fn ensure_access_inner(repo: &Repo, cwd: &Path, cmd: &Cmd) -> Result<Vec<String>, Error> {
    let zones = repo.zones()?;
    let want = zones_needed(cmd, &|p| repo.rel_path(cwd, p).ok(), &|r| zones.owner(r).to_owned(), &|n| {
        zones.resolve(n)
    });
    let ws = repo.workspace()?;
    let rr = repo.remote_refs()?;
    let want: Vec<String> = want
        .into_iter()
        .filter(|z| !ws.subs.contains_key(z) && rr.head(z, vcs_repo::DEFAULT_BRANCH).is_some())
        .collect();
    if want.is_empty() {
        return Ok(vec![]);
    }
    let backend = remote::open_memo(&need_remote(repo)?, false)?;
    let mut done = Vec::new();
    for z in want {
        eprintln!(
            "zone {} was skipped by --auto; fetching it now (`vcs profile skipped` lists the others)",
            zones.label(&z)
        );
        repo.fetch(&backend, Some(std::slice::from_ref(&z)))?;
        repo.zone_add_fetching(&backend, &z, None)?;
        let _ = repo.profile_record(Kind::Read, now_ms(), &[(z.clone(), String::new())]);
        done.push(z);
    }
    Ok(done)
}

// ------------------------------------------------------------------ automatic recording

/// What to remember about a command before it runs (a commit changes branch heads).
pub fn before(cmd: &Cmd) -> Option<BTreeMap<String, vcs_objects::ObjectId>> {
    if !matches!(cmd, Cmd::Commit { .. }) {
        return None;
    }
    let (repo, _) = open().ok()?;
    let refs = repo.refs().ok()?;
    Some(refs.branches.iter().map(|((z, b), id)| (format!("{z}\u{0}{b}"), *id)).collect())
}

/// Record the command that just succeeded. Best effort: never fails the command, does nothing
/// when recording is off. `VCS_PROFILE=off` disables it for the process.
pub fn observe(cmd: &Cmd, before: Option<BTreeMap<String, vcs_objects::ObjectId>>) {
    let touches = matches!(
        cmd,
        Cmd::Add { .. }
            | Cmd::Rm { .. }
            | Cmd::Mv { .. }
            | Cmd::Restore { .. }
            | Cmd::Commit { .. }
            | Cmd::Status
            | Cmd::Diff { .. }
            | Cmd::Log { .. }
            | Cmd::Zone(ZoneCmd::Checkout { .. } | ZoneCmd::Add { .. })
    );
    if !touches || std::env::var("VCS_PROFILE").is_ok_and(|v| v == "off" || v == "0") {
        return;
    }
    let Ok((repo, cwd)) = open() else { return };
    if zp::is_disabled(&repo.profile_file()) {
        return;
    }
    let _ = observe_inner(&repo, &cwd, cmd, before);
}

fn observe_inner(repo: &Repo, cwd: &Path, cmd: &Cmd, before: Option<BTreeMap<String, vcs_objects::ObjectId>>) -> Result<(), Error> {
    let zones = repo.zones()?;
    let rel_hits = |paths: &[String]| -> Vec<(String, String)> {
        paths
            .iter()
            .filter_map(|p| repo.rel_path(cwd, p).ok().map(|rel| hit(&zones, &rel, cwd.join(p).is_dir())))
            .collect()
    };
    let (kind, hits): (Kind, Vec<(String, String)>) = match cmd {
        Cmd::Add { paths, .. } | Cmd::Rm { paths, .. } | Cmd::Restore { paths, .. } => (Kind::Edit, rel_hits(paths)),
        Cmd::Mv { src, dst, .. } => (Kind::Edit, rel_hits(&[src.clone(), dst.clone()])),
        Cmd::Commit { .. } => {
            let refs = repo.refs()?;
            let old = before.unwrap_or_default();
            let h = refs
                .branches
                .iter()
                .filter(|((z, b), id)| old.get(&format!("{z}\u{0}{b}")) != Some(*id))
                .map(|((z, _), _)| (z.clone(), String::new()))
                .collect();
            (Kind::Edit, h)
        }
        Cmd::Status | Cmd::Diff { .. } | Cmd::Log { .. } => {
            let here = repo.rel_path(cwd, ".")?;
            (Kind::Read, vec![hit(&zones, &here, true)])
        }
        Cmd::Zone(ZoneCmd::Checkout { zone, .. }) | Cmd::Zone(ZoneCmd::Add { name: zone, .. }) => (
            Kind::Read,
            zones.resolve(zone).map(|z| vec![(z, String::new())]).unwrap_or_default(),
        ),
        _ => return Ok(()),
    };
    repo.profile_record(kind, now_ms(), &hits)?;
    Ok(())
}
