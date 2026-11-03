//! `vcs zone suggest|apply|subscribe` and `vcs clone --for`: zone auto-detection and
//! minimal-zone-set selection. Analysis lives in `vcs_repo::zonedetect`; this module gathers the
//! inputs (local refs, or remote metadata fetched into a throwaway store), renders text/JSON and
//! runs the explicit actions. Contract and limits: docs/zones-autodetect.md.
//!
//! Nothing here calls a model. `--task-assist <cmd>` runs a command the user names, on explicit
//! request, and treats its answer as a proposal that is validated against real zone names.

use crate::json::{doc, J};
use crate::{author, config, remote, Cmd, ZoneCmd};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::Path;
use vcs_objects::{Object, ObjectId};
use vcs_remote::Backend;
use vcs_repo::meta::Zones;
use vcs_repo::treeops::{self, Flat};
use vcs_repo::zonedetect as zd;
use vcs_repo::{Error, Repo, ROOT_ZONE};

#[derive(Args, Clone)]
pub struct SuggestArgs {
    /// Hide proposals below this confidence (0..1)
    #[arg(long, default_value_t = 0.4)]
    pub min_confidence: f64,
    /// Ignore candidates with fewer files than this
    #[arg(long, default_value_t = 3)]
    pub min_files: usize,
    /// Also analyse history: which proposed zones change in the same commits (reads commit trees)
    #[arg(long)]
    pub cochange: bool,
    /// How many commits of the root zone `--cochange` looks at
    #[arg(long, default_value_t = 500)]
    pub max_commits: usize,
}

#[derive(Args, Clone)]
pub struct ApplyArgs {
    /// Path or proposed name of a suggestion from `vcs zone suggest`; several make all the zones
    /// in one step (one status check, one new root commit, one write of the repository state)
    #[arg(required = true)]
    pub target: Vec<String>,
    /// Zone name to use instead of the proposed one (only with a single target)
    #[arg(long)]
    pub name: Option<String>,
    /// Actually do it. Without this only the plan is printed.
    #[arg(long)]
    pub yes: bool,
}

/// Selection options shared by `vcs clone` and `vcs zone subscribe`.
#[derive(Args, Clone, Default)]
pub struct ForArgs {
    /// Pull what is needed for this: a file, a directory, a zone name, or a symbol-ish name
    /// (`parse_config`, `net::Client`, `pkg.mod`). Repeatable. Plus the zones they depend on.
    #[arg(long = "for", value_name = "PATH|SYMBOL")]
    pub for_: Vec<String>,
    /// Describe a task in words; zones are chosen by offline keyword matching (a proposal)
    #[arg(long, value_name = "TEXT")]
    pub task: Option<String>,
    /// Pull the zones a person relates to (CODEOWNERS entries, and commit authorship)
    #[arg(long = "for-owner", value_name = "USER")]
    pub for_owner: Option<String>,
    /// Ask this command to propose zones for `--task` (JSON on stdin/stdout, see docs). Off unless given.
    #[arg(long, value_name = "CMD")]
    pub task_assist: Option<String>,
    /// Show why each zone is included
    #[arg(long)]
    pub explain: bool,
    /// Only print the plan with files and bytes; download no file content and change nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Do not add the zones the selection depends on
    #[arg(long)]
    pub no_deps: bool,
    /// Follow dependencies at most this many hops
    #[arg(long, value_name = "N")]
    pub max_depth: Option<u32>,
    /// Also scan source imports (Go, Python, JS/TS), downloading the source of selected zones to do so
    #[arg(long)]
    pub deep: bool,
    /// Search file contents for definitions of symbol-ish queries (remote: downloads source files)
    #[arg(long)]
    pub search_content: bool,
    /// Continue when some `--for` queries match nothing (default: refuse)
    #[arg(long)]
    pub partial: bool,
}

impl ForArgs {
    /// The queries a lean (partial) metadata fetch may rely on: only plain `--for` selections.
    fn queries_hint(&self) -> Option<&[String]> {
        (!self.for_.is_empty() && self.task.is_none() && self.for_owner.is_none() && !self.search_content && !self.deep)
            .then_some(self.for_.as_slice())
    }
    pub fn active(&self) -> bool {
        !self.for_.is_empty() || self.task.is_some() || self.for_owner.is_some()
    }
}

/// Hook called by `run` before the normal dispatch. `None`: not ours.
pub fn intercept(cmd: &Cmd, json: bool) -> Option<Result<(), Error>> {
    match cmd {
        Cmd::Zone(ZoneCmd::Suggest(a)) => Some(suggest(a, json)),
        Cmd::Zone(ZoneCmd::Apply(a)) => Some(apply(a, json)),
        Cmd::Zone(ZoneCmd::Subscribe(a)) => Some(subscribe(a, json)),
        Cmd::Clone {
            url,
            dir,
            zones,
            stats,
            sel,
        } if sel.active() => Some(clone_for(url, dir, zones, *stats, sel, json)),
        _ => None,
    }
}

// ------------------------------------------------------------------ view of a repository

struct View<'a> {
    zones: Zones,
    files: Flat,
    /// Branch head commit used per zone.
    heads: BTreeMap<String, ObjectId>,
    owned: Option<Repo>,
    borrowed: Option<&'a Repo>,
    _tmp: Option<tempfile::TempDir>,
    backend: Option<&'a dyn Backend>,
    sizes: HashMap<ObjectId, u64>,
    remote: bool,
    manifest: Option<vcs_remote::Manifest>,
    /// Zones whose head tree is in `files`.
    loaded: BTreeSet<String>,
    notes: Vec<String>,
}

/// Zones to load first for `queries`, or `None` when some query needs the whole tree to resolve.
fn lean_zones(zones: &Zones, queries: &[String]) -> Option<Vec<String>> {
    let mut out = vec![ROOT_ZONE.to_owned()];
    for q in queries {
        let q = q.trim().trim_start_matches("./").trim_end_matches('/');
        let z = match zones.resolve(q) {
            Some(z) => z,
            None => {
                let o = zones.owner(q).to_owned();
                if o == ROOT_ZONE && !q.contains('/') {
                    return None;
                }
                o
            }
        };
        if !out.contains(&z) {
            out.push(z);
        }
    }
    Some(out)
}

fn ev<E: std::fmt::Display>(e: E) -> Error {
    Error::Msg(e.to_string())
}

impl<'a> View<'a> {
    fn repo(&self) -> &Repo {
        self.owned.as_ref().or(self.borrowed).expect("a view has a repository")
    }

    /// The local repository: every zone whose head commit and trees are present.
    fn local(repo: &'a Repo) -> Result<View<'a>, Error> {
        let zones = repo.zones()?;
        let ws = repo.workspace()?;
        let refs = repo.refs()?;
        let rr = repo.remote_refs()?;
        let mut v = View {
            zones: zones.clone(),
            files: Flat::new(),
            heads: BTreeMap::new(),
            owned: None,
            borrowed: Some(repo),
            _tmp: None,
            backend: None,
            sizes: HashMap::new(),
            remote: false,
            manifest: None,
            loaded: BTreeSet::new(),
            notes: vec![],
        };
        for z in zones.names() {
            let b = ws.subs.get(z).map_or("main", String::as_str);
            let Some(h) = refs.head(z, b).or_else(|| rr.head(z, b)) else {
                continue;
            };
            v.add_zone(z, h)?;
        }
        Ok(v)
    }

    /// The remote's zones and trees, fetched (commits and trees only, no file content) into a
    /// throwaway store that is deleted when the view is dropped.
    ///
    /// With `queries`, and when every query can be mapped to a zone from the manifest alone (a zone
    /// name, a path inside a zone, a path with a `/`), only the root zone and those zones are
    /// loaded; zones pulled in by dependencies are loaded as they are found. Otherwise all zones
    /// are loaded (symbol, task and owner queries need every tree).
    fn remote(backend: &'a dyn Backend, dest: Option<&Path>, queries: Option<&[String]>) -> Result<View<'a>, Error> {
        let (m, _) = vcs_remote::load_manifest(backend).map_err(ev)?;
        if m.branches.is_empty() {
            return Err(Error::Msg("the remote has no published branches".into()));
        }
        let (tmp, repo) = match dest {
            Some(d) => {
                if d.exists() && std::fs::read_dir(d).map(|mut x| x.next().is_some()).unwrap_or(true) {
                    return Err(Error::Msg(format!("destination {} exists and is not empty", d.display())));
                }
                (None, Repo::init(d)?)
            }
            None => {
                let tmp = tempfile::tempdir().map_err(|source| Error::Io {
                    path: "tempdir".into(),
                    source,
                })?;
                let repo = Repo::init(tmp.path())?;
                (Some(tmp), repo)
            }
        };
        let mut zones = Zones::new();
        for (n, p) in &m.zones {
            zones.create(n, p)?;
        }
        for (id, l) in &m.labels {
            if zones.exists(id) {
                let _ = zones.set_label(id, l);
            }
        }
        let initial = queries.and_then(|qs| lean_zones(&zones, qs));
        let got = vcs_remote::fetch_heads_lazy_with(backend, repo.store(), initial.as_deref(), &m).map_err(ev)?;
        let mut v = View {
            zones: zones.clone(),
            files: Flat::new(),
            heads: BTreeMap::new(),
            owned: Some(repo),
            borrowed: None,
            _tmp: tmp,
            backend: Some(backend),
            sizes: HashMap::new(),
            remote: true,
            manifest: Some(m),
            loaded: BTreeSet::new(),
            notes: vec![],
        };
        let mut per: BTreeMap<String, Vec<(String, ObjectId)>> = BTreeMap::new();
        for (z, b, id) in got {
            per.entry(z).or_default().push((b, id));
        }
        for z in zones.names().map(str::to_owned).collect::<Vec<_>>() {
            let Some(list) = per.get(&z) else { continue };
            let pick = list.iter().find(|(b, _)| b == "main").or(list.first()).unwrap();
            if pick.0 != "main" {
                v.notes
                    .push(format!("zone {z} has no `main` branch on the remote; using {}", pick.0));
            }
            v.add_zone(&z, pick.1)?;
        }
        Ok(v)
    }

    /// Fetch commits and trees of more zones of a remote view.
    fn load_zones(&mut self, zs: &[String]) -> Result<(), Error> {
        let (Some(b), Some(m)) = (self.backend, self.manifest.clone()) else {
            return Ok(());
        };
        let got = vcs_remote::fetch_heads_lazy_with(b, self.repo().store(), Some(zs), &m).map_err(ev)?;
        let mut per: BTreeMap<String, Vec<(String, ObjectId)>> = BTreeMap::new();
        for (z, br, id) in got {
            per.entry(z).or_default().push((br, id));
        }
        for z in zs {
            let Some(list) = per.get(z) else { continue };
            let pick = list.iter().find(|(b, _)| b == "main").or(list.first()).unwrap();
            self.add_zone(z, pick.1)?;
        }
        Ok(())
    }

    fn add_zone(&mut self, zone: &str, head: ObjectId) -> Result<(), Error> {
        self.loaded.insert(zone.to_owned());
        let repo = self.repo();
        let c = match repo.commit_of(&head) {
            Ok(c) => c,
            Err(_) => {
                self.notes
                    .push(format!("zone {zone}: head commit is not available locally; zone not analysed"));
                return Ok(());
            }
        };
        let prefix = self.zones.path_of(zone).unwrap_or("").to_owned();
        match treeops::flatten(repo.store(), Some(c.root), &prefix) {
            Ok(f) => {
                self.files.extend(f);
                self.heads.insert(zone.to_owned(), head);
            }
            Err(_) => self
                .notes
                .push(format!("zone {zone}: its trees are not available locally; zone not analysed")),
        }
        Ok(())
    }

    /// Content if it is already in the local store. Never touches the network.
    fn read(&self, id: &ObjectId) -> Option<Vec<u8>> {
        self.repo().content(id).ok()
    }

    /// Fetch the listed objects in batches (remote views only). Already-stored ids are skipped.
    fn prefetch(&self, ids: &[ObjectId]) -> Result<usize, Error> {
        let Some(b) = self.backend else { return Ok(0) };
        let store = self.repo().store();
        let mut want = Vec::new();
        for id in ids {
            if !store.contains(id)? {
                want.push(*id);
            }
        }
        want.sort();
        want.dedup();
        for chunk in want.chunks(4000) {
            for (_, obj) in vcs_remote::fetch_objects(b, chunk).map_err(ev)? {
                store.put(&obj)?;
            }
        }
        store.sync()?;
        Ok(want.len())
    }

    /// Stored sizes of every file from the remote's pack indexes (one request per pack).
    fn load_remote_sizes(&mut self) -> Result<(), Error> {
        let Some(b) = self.backend else { return Ok(()) };
        let ids: Vec<ObjectId> = self.files.values().map(|(_, id)| *id).collect();
        let (m, unknown) = vcs_remote::packfetch::stored_sizes(b, &ids).map_err(ev)?;
        self.sizes = m;
        if unknown > 0 {
            self.notes.push(format!(
                "{unknown} file(s) are stored loose on the remote (not in a pack): their size is unknown and counted as 0 bytes"
            ));
        }
        Ok(())
    }

    fn size(&self, id: &ObjectId) -> Option<u64> {
        if let Some(s) = self.sizes.get(id) {
            return Some(*s);
        }
        if self.remote {
            return Some(0);
        }
        match self.repo().store().get(id).ok()? {
            Object::Blob(b) => Some(b.len() as u64),
            Object::Chunked(m) => Some(m.total_len),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ small output helpers

fn s(x: impl Into<String>) -> J {
    J::Str(x.into())
}
fn n(x: u64) -> J {
    J::Num(x)
}
fn strs(v: &[String]) -> J {
    J::Arr(v.iter().map(|x| s(x.clone())).collect())
}
fn pct(f: f64) -> u64 {
    (f * 100.0).round().clamp(0.0, 100.0) as u64
}
fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
fn opt_n(x: Option<u64>) -> J {
    x.map_or(J::Null, J::Num)
}
fn emit(j: J) {
    println!("{}", j.render());
}

// ------------------------------------------------------------------ zone suggest / apply

fn commit_paths(repo: &Repo, max: usize) -> Result<Vec<Vec<String>>, Error> {
    let refs = repo.refs()?;
    let ws = repo.workspace()?;
    let b = ws.subs.get(ROOT_ZONE).map_or("main", String::as_str);
    let mut cur = refs.head(ROOT_ZONE, b);
    let mut out = Vec::new();
    while let (Some(id), true) = (cur, out.len() < max) {
        let c = repo.commit_of(&id)?;
        let parent = c.parents.first().copied();
        let proot = match parent {
            Some(p) => Some(repo.commit_of(&p)?.root),
            None => None,
        };
        let d = treeops::diff_trees(repo.store(), proot, Some(c.root), "")?;
        let set: BTreeSet<String> = d.old.keys().chain(d.new.keys()).cloned().collect();
        out.push(set.into_iter().collect());
        cur = parent;
    }
    Ok(out)
}

fn compute_suggestions<'a>(repo: &'a Repo, a: &SuggestArgs, min_conf: f64) -> Result<(View<'a>, zd::SuggestReport), Error> {
    let mut v = View::local(repo)?;
    let commits = if a.cochange {
        Some(commit_paths(repo, a.max_commits)?)
    } else {
        None
    };
    let opts = zd::SuggestOpts {
        min_files: a.min_files,
        min_confidence: min_conf,
        ..Default::default()
    };
    let read = |id: &ObjectId| v.read(id);
    let size = |id: &ObjectId| v.size(id);
    let mut rep = zd::suggest(&v.files, &v.zones, &read, &size, commits.as_deref(), &opts);
    rep.notes.append(&mut v.notes);
    Ok((v, rep))
}

fn suggestion_json(z: &zd::Suggestion) -> J {
    J::Obj(vec![
        ("name", s(&z.name)),
        ("path", s(&z.path)),
        ("files", n(z.files)),
        ("bytes", opt_n(z.bytes)),
        ("confidence_pct", n(pct(z.confidence))),
        (
            "signals",
            J::Arr(
                z.signals
                    .iter()
                    .map(|g| {
                        J::Obj(vec![
                            ("kind", s(&g.kind)),
                            ("detail", s(&g.detail)),
                            ("weight_pct", n(pct(g.weight))),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("depends_on", strs(&z.depends_on)),
        ("coupled_with", strs(&z.coupled_with)),
    ])
}

fn suggest(a: &SuggestArgs, json: bool) -> Result<(), Error> {
    let (repo, _) = crate::open()?;
    let (v, rep) = compute_suggestions(&repo, a, a.min_confidence)?;
    if json {
        emit(doc(
            "zone suggest",
            vec![
                ("zones", J::Arr(rep.zones.iter().map(suggestion_json).collect())),
                ("hidden_low_confidence", n(rep.hidden_low_confidence as u64)),
                ("total_files", n(rep.total_files)),
                ("root_files_remaining", n(rep.root_files_remaining)),
                ("cycles", J::Arr(rep.cycles.iter().map(|c| strs(c)).collect())),
                (
                    "coupling",
                    J::Arr(
                        rep.coupling
                            .iter()
                            .map(|c| {
                                J::Obj(vec![
                                    ("a", s(&c.a)),
                                    ("b", s(&c.b)),
                                    ("commits_both", n(c.both.into())),
                                    ("commits_either", n(c.either.into())),
                                    ("jaccard_pct", n(pct(c.jaccard))),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("clusters", J::Arr(rep.clusters.iter().map(|c| strs(c)).collect())),
                ("notes", strs(&rep.notes)),
                ("history_rewritten", J::Bool(false)),
            ],
        ));
        return Ok(());
    }
    let _ = v;
    println!(
        "{} tracked file(s); {} proposal(s); proposals are read-only, nothing was changed.",
        rep.total_files,
        rep.zones.len()
    );
    for z in &rep.zones {
        println!(
            "\n{}  path {}/  {} file(s){}  confidence {:.2}",
            z.name,
            z.path,
            z.files,
            z.bytes.map_or(String::new(), |b| format!(", {}", human(b))),
            z.confidence
        );
        for g in &z.signals {
            println!("    - {}: {}", g.kind, g.detail);
        }
        if !z.depends_on.is_empty() {
            println!("    depends on: {}", z.depends_on.join(", "));
        }
        if !z.coupled_with.is_empty() {
            println!("    changes together with: {}", z.coupled_with.join(", "));
        }
    }
    if rep.hidden_low_confidence > 0 {
        println!(
            "\n{} lower-confidence candidate(s) hidden (see --min-confidence).",
            rep.hidden_low_confidence
        );
    }
    for c in &rep.cycles {
        println!("note: {} depend on each other; consider keeping them in one zone", c.join(", "));
    }
    for c in &rep.clusters {
        println!(
            "note: {} change in the same commits (jaccard >= 0.5); consider keeping them together",
            c.join(", ")
        );
    }
    for x in &rep.notes {
        println!("note: {x}");
    }
    if !rep.zones.is_empty() {
        println!("\napply one with `vcs zone apply <path>` (plan only), then add --yes. Existing history is not rewritten; see docs/zones-autodetect.md.");
    }
    Ok(())
}

fn apply(a: &ApplyArgs, json: bool) -> Result<(), Error> {
    let (repo, _) = crate::open()?;
    let sa = SuggestArgs {
        min_confidence: 0.0,
        min_files: 1,
        cochange: false,
        max_commits: 0,
    };
    let (_v, rep) = compute_suggestions(&repo, &sa, 0.0)?;
    if a.name.is_some() && a.target.len() > 1 {
        return Err(Error::Msg("--name needs exactly one target".into()));
    }
    let mut splits = Vec::new();
    for target in &a.target {
        let t = target.trim_matches('/');
        let Some(z) = rep.zones.iter().find(|z| z.path == t || z.name == t) else {
            return Err(Error::Msg(format!(
                "{:?} is not a current suggestion of `vcs zone suggest` (paths: {}). `apply` only adopts suggestions.",
                target,
                rep.zones.iter().map(|z| z.path.as_str()).collect::<Vec<_>>().join(", ")
            )));
        };
        splits.push((a.name.clone().unwrap_or_else(|| z.name.clone()), z.path.clone()));
    }
    let reps = repo.zone_split_many(&splits, author()?, !a.yes)?;
    if json {
        for rep2 in &reps {
            emit(doc(
                "zone apply",
                vec![
                    ("applied", J::Bool(a.yes)),
                    ("zone", s(&rep2.zone)),
                    ("path", s(&rep2.path)),
                    ("files", n(rep2.files as u64)),
                    ("root_commit", rep2.root_commit.map_or(J::Null, |c| s(c.to_string()))),
                    ("zone_commit", rep2.zone_commit.map_or(J::Null, |c| s(c.to_string()))),
                    ("history_rewritten", J::Bool(false)),
                ],
            ));
        }
        return Ok(());
    }
    for rep2 in &reps {
        let name = &rep2.zone;
        if !a.yes {
            println!(
                "plan (nothing changed): split {}/ ({} file(s)) into new zone {name}",
                rep2.path, rep2.files
            );
            println!(
                "  - root zone gets one new commit that removes {}/ from its tree; its old commits are untouched",
                rep2.path
            );
            println!("  - zone {name} gets one root commit with that directory; no file data is copied or rewritten");
            println!("  - the working tree is not touched; {name}'s own history starts at the split");
            println!(
                "  - collaborators: after you publish, a plain `vcs sync` removes {}/ for them until they run `vcs zone add {name}`",
                rep2.path
            );
        } else {
            println!(
                "created zone {name} owning {}/ ({} file(s)); root commit {}, zone commit {}",
                rep2.path,
                rep2.files,
                rep2.root_commit.unwrap().short(),
                rep2.zone_commit.unwrap().short()
            );
        }
    }
    if !a.yes {
        println!("run again with --yes to do it");
    } else {
        println!("history was not rewritten: commits before the split stay in the root zone");
    }
    Ok(())
}

// ------------------------------------------------------------------ selection

struct Selection {
    included: Vec<zd::Included>,
    unresolved: Vec<(String, String)>,
    notes: Vec<String>,
}

const MANIFESTS: &[&str] = &["Cargo.toml", "package.json", "go.mod", "pyproject.toml"];
const SOURCE_EXT: &[&str] = &["go", "py", "js", "jsx", "ts", "tsx", "mjs", "cjs"];
const CODE_EXT: &[&str] = &[
    "rs", "go", "py", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "swift", "scala", "php",
];

fn ids_where(v: &View, f: impl Fn(&str) -> bool) -> Vec<ObjectId> {
    v.files.iter().filter(|(p, _)| f(p)).map(|(_, (_, id))| *id).collect()
}
fn ext(p: &str) -> &str {
    p.rsplit('.').next().unwrap_or("")
}

fn run_assist(cmd: &str, task: &str, zones: &Zones) -> Result<Vec<String>, Error> {
    let mut parts = cmd.split_whitespace();
    let prog = parts.next().ok_or_else(|| Error::Msg("--task-assist needs a command".into()))?;
    let mut req = String::from("{\"task\":");
    req.push_str(&J::Str(task.to_owned()).render());
    req.push_str(",\"zones\":[");
    let mut first = true;
    for z in zones.names() {
        if !first {
            req.push(',');
        }
        first = false;
        req.push_str(&J::Obj(vec![("name", s(z)), ("path", s(zones.path_of(z).unwrap_or("")))]).render());
    }
    req.push_str("]}");
    let mut child = std::process::Command::new(prog)
        .args(parts)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| Error::Msg(format!("cannot start --task-assist command {prog:?}: {e}")))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = stdin.write_all(req.as_bytes());
        drop(stdin);
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Ok(o)) => return Err(Error::Msg(format!("--task-assist command exited with {}", o.status))),
        Ok(Err(e)) => return Err(Error::Msg(format!("--task-assist command failed: {e}"))),
        Err(_) => {
            // SAFETY: plain kill(2) on the child we spawned; a stale pid at worst fails with ESRCH.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            return Err(Error::Msg("--task-assist command timed out after 30 s".into()));
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let after = text
        .split("\"zones\"")
        .nth(1)
        .ok_or_else(|| Error::Msg("--task-assist answer has no \"zones\" array".into()))?;
    let arr = after.split(']').next().unwrap_or("");
    let mut names = Vec::new();
    for q in arr.split('"').skip(1).step_by(2) {
        match zones.resolve(q) {
            Some(id) => names.push(id),
            None => return Err(Error::Msg(format!("--task-assist proposed unknown zone {q:?}; refusing to guess"))),
        }
    }
    Ok(names)
}

fn select(v: &mut View, a: &ForArgs) -> Result<Selection, Error> {
    if !a.active() {
        return Err(Error::Msg("nothing to select: give --for, --task or --for-owner".into()));
    }
    // One batch for every manifest and CODEOWNERS file, so this costs requests per pack, not per file.
    let co_ids = ids_where(v, |p| zd::CODEOWNERS_PATHS.contains(&p));
    let mut ids = ids_where(v, |p| MANIFESTS.contains(&p.rsplit('/').next().unwrap_or("")));
    ids.extend(co_ids);
    v.prefetch(&ids)?;
    if a.search_content && v.remote {
        let code = ids_where(v, |p| CODE_EXT.contains(&ext(p)));
        v.prefetch(&code)?;
    }
    let mut matches: Vec<zd::Match> = Vec::new();
    let mut unresolved = Vec::new();
    let mut notes = Vec::new();
    {
        let read = |id: &ObjectId| v.read(id);
        if !a.for_.is_empty() {
            let r = zd::resolve_queries(&a.for_, &v.files, &v.zones, &read, a.search_content || !v.remote);
            matches.extend(r.matches);
            unresolved.extend(r.unresolved);
            notes.extend(r.notes);
        }
        if let Some(t) = &a.task {
            let r = zd::map_task(t, &v.files, &v.zones);
            matches.extend(r.matches);
            unresolved.extend(r.unresolved);
            notes.extend(r.notes);
            if let Some(cmd) = &a.task_assist {
                for z in run_assist(cmd, t, &v.zones)? {
                    matches.push(zd::Match {
                        zone: z,
                        query: t.clone(),
                        how: format!("proposed by --task-assist `{cmd}` (validated against real zone names)"),
                    });
                }
                // an assistant answer rescues an otherwise unresolved task
                if matches.iter().any(|m| m.query == *t) {
                    unresolved.retain(|(q, _)| q != t);
                }
            }
        } else if a.task_assist.is_some() {
            return Err(Error::Msg("--task-assist needs --task".into()));
        }
        if let Some(u) = &a.for_owner {
            let mut m = zd::zones_for_owner(u, &v.files, &v.zones, &read);
            let mut stats: BTreeMap<String, (u32, u32)> = BTreeMap::new();
            for (z, h) in &v.heads {
                let mut cur = Some(*h);
                let (mut mine, mut total) = (0u32, 0u32);
                while let (Some(id), true) = (cur, total < 300) {
                    let Ok(c) = v.repo().commit_of(&id) else { break };
                    total += 1;
                    if zd::author_is(&c.author.name, &c.author.email, u) {
                        mine += 1;
                    }
                    cur = c.parents.first().copied();
                }
                stats.insert(z.clone(), (mine, total));
            }
            m.extend(zd::zones_by_authorship(u, &stats, 3, 0.2));
            if m.is_empty() {
                unresolved.push((
                    format!("owner:{u}"),
                    "no CODEOWNERS entry lists this user and no zone has enough commits by them (>= 3 commits and >= 20% of the last 300)"
                        .into(),
                ));
            }
            matches.extend(m);
        }
    }
    if !unresolved.is_empty() && !a.partial {
        let list: Vec<String> = unresolved.iter().map(|(q, w)| format!("  {q:?}: {w}")).collect();
        return Err(Error::Msg(format!(
            "could not resolve:\n{}\n(refusing to guess; --partial continues with what did resolve)",
            list.join("\n")
        )));
    }
    if matches.is_empty() {
        return Err(Error::Msg("nothing resolved to a zone".into()));
    }
    let seeds: Vec<zd::Seed> = matches
        .iter()
        .map(|m| zd::Seed {
            zone: m.zone.clone(),
            why: format!("{}: {}", m.query, m.how),
        })
        .collect();
    let follow = !a.no_deps;
    let scan = a.deep && follow;
    let mut included: Vec<zd::Included> = Vec::new();
    let mut deps = zd::Deps::default();
    for _ in 0..12 {
        let man = ids_where(v, |p| MANIFESTS.contains(&p.rsplit('/').next().unwrap_or("")));
        v.prefetch(&man)?;
        if scan {
            let zs: BTreeSet<&str> = included.iter().map(|i| i.zone.as_str()).collect();
            let src = ids_where(v, |p| SOURCE_EXT.contains(&ext(p)) && zs.contains(v.zones.owner(p)));
            v.prefetch(&src)?;
        }
        deps = zd::dependencies(&v.files, &v.zones, &|id| v.read(id), scan);
        let next = zd::closure(&seeds, &deps, follow, a.max_depth);
        let missing: Vec<String> = next
            .iter()
            .filter(|i| v.remote && !v.loaded.contains(&i.zone))
            .map(|i| i.zone.clone())
            .collect();
        let same = next.len() == included.len() && next.iter().zip(&included).all(|(x, y)| x.zone == y.zone);
        included = next;
        if !missing.is_empty() {
            v.load_zones(&missing)?;
            continue;
        }
        if same {
            break;
        }
    }
    notes.extend(deps.notes.iter().cloned());
    if follow && !a.deep {
        notes.push("dependencies come from manifests only; pass --deep to also scan source imports".into());
    }
    if !included.iter().any(|i| i.zone == ROOT_ZONE) {
        notes.push("the root zone (top-level files, shared tooling) is not included".into());
    }
    notes.extend(v.notes.iter().cloned());
    Ok(Selection {
        included,
        unresolved,
        notes,
    })
}

/// Render a selection. Returns nothing; `planning` is `(requests, bytes)` spent so far, if measured.
fn report(v: &View, sel: &Selection, a: &ForArgs, json: bool, command: &'static str, planning: Option<(u64, u64)>, dry: bool) {
    let stats = zd::zone_stats(&v.files, &v.zones, &|id| v.size(id));
    // A remote view loads only the zones it needs; the others have unknown size, not zero.
    let known = |z: &str| !v.remote || v.loaded.contains(z);
    let (mut sf, mut sb, mut bytes_known) = (0u64, 0u64, true);
    let (mut af, mut ab) = (0u64, 0u64);
    let all_known = stats.keys().all(|z| known(z));
    for (z, (f, b)) in &stats {
        af += f;
        ab += b.unwrap_or(0);
        if sel.included.iter().any(|i| &i.zone == z) {
            sf += f;
            sb += b.unwrap_or(0);
            bytes_known &= b.is_some();
        }
    }
    let kind = if v.remote { "stored_estimate" } else { "content" };
    let fcount = |z: &str, f: u64| if known(z) { n(f) } else { J::Null };
    let bcount = |z: &str, b: Option<u64>| if known(z) { opt_n(b) } else { J::Null };
    if json {
        let zones: Vec<J> = sel
            .included
            .iter()
            .map(|i| {
                let (f, b) = stats.get(&i.zone).copied().unwrap_or((0, Some(0)));
                J::Obj(vec![
                    ("name", s(v.zones.label(&i.zone))),
                    ("id", s(&i.zone)),
                    ("path", s(v.zones.path_of(&i.zone).unwrap_or(""))),
                    ("direct", J::Bool(i.direct)),
                    ("depth", n(i.depth.into())),
                    ("reasons", strs(&i.reasons)),
                    ("files", fcount(&i.zone, f)),
                    ("bytes", bcount(&i.zone, b)),
                ])
            })
            .collect();
        let skipped: Vec<J> = stats
            .iter()
            .filter(|(z, _)| !sel.included.iter().any(|i| &i.zone == *z))
            .map(|(z, (f, b))| {
                J::Obj(vec![
                    ("name", s(v.zones.label(z))),
                    ("id", s(z)),
                    ("path", s(v.zones.path_of(z).unwrap_or(""))),
                    ("files", fcount(z, *f)),
                    ("bytes", bcount(z, *b)),
                ])
            })
            .collect();
        let mut f = vec![
            ("dry_run", J::Bool(dry)),
            ("source", s(if v.remote { "remote" } else { "local" })),
            ("bytes_kind", s(kind)),
            ("zones", J::Arr(zones)),
            ("not_selected", J::Arr(skipped)),
            (
                "totals",
                J::Obj(vec![
                    ("files_selected", n(sf)),
                    ("bytes_selected", n(sb)),
                    ("bytes_complete", J::Bool(bytes_known)),
                    ("files_all", if all_known { n(af) } else { J::Null }),
                    ("bytes_all", if all_known { n(ab) } else { J::Null }),
                ]),
            ),
            (
                "unresolved",
                J::Arr(
                    sel.unresolved
                        .iter()
                        .map(|(q, w)| J::Obj(vec![("query", s(q)), ("why", s(w))]))
                        .collect(),
                ),
            ),
            ("notes", strs(&sel.notes)),
        ];
        if let Some((r, b)) = planning {
            f.push(("planning_cost", J::Obj(vec![("requests", n(r)), ("bytes", n(b))])));
        }
        emit(doc(command, f));
        return;
    }
    println!(
        "selected {} of {} zone(s): {} file(s), {} ({}){}",
        sel.included.len(),
        stats.len(),
        sf,
        human(sb),
        if v.remote {
            "stored size from pack indexes, an estimate"
        } else {
            "content size"
        },
        if dry { "  [dry run]" } else { "" }
    );
    if all_known {
        println!("whole repository: {af} file(s), {}", human(ab));
    } else {
        println!("whole repository: not measured (only the zones needed were looked at)");
    }
    for i in &sel.included {
        let (f, b) = stats.get(&i.zone).copied().unwrap_or((0, Some(0)));
        println!(
            "  {:<16} {:<24} {:>8} file(s) {:>10}  {}",
            v.zones.label(&i.zone),
            match v.zones.path_of(&i.zone) {
                Some("") | None => "(rest of repo)".to_owned(),
                Some(p) => format!("{p}/"),
            },
            f,
            b.map_or("?".into(), human),
            if i.direct { "requested" } else { "dependency" }
        );
        if a.explain {
            for r in &i.reasons {
                println!("      because {r}");
            }
        }
    }
    for (q, w) in &sel.unresolved {
        println!("unresolved: {q:?}: {w}");
    }
    for x in &sel.notes {
        println!("note: {x}");
    }
    if let Some((r, b)) = planning {
        println!(
            "planning cost so far: {r} request(s), {} downloaded (commits, trees and manifests; no other file content)",
            human(b)
        );
    }
}

fn names_of(sel: &Selection) -> Vec<String> {
    sel.included.iter().map(|i| i.zone.clone()).collect()
}

// ------------------------------------------------------------------ subscribe / clone

fn subscribe(a: &ForArgs, json: bool) -> Result<(), Error> {
    let (repo, _) = crate::open()?;
    let url = repo
        .remote_url()?
        .or_else(|| config::Config::load().ok().and_then(|c| c.get("remote.default")));
    let backend = match &url {
        Some(u) => Some(remote::open_backend(u)?),
        None => None,
    };
    let counting = backend.as_deref().map(vcs_remote::CountingBackend::new);
    let mut view = match &counting {
        Some(c) => View::remote(c, None, a.queries_hint())?,
        None => View::local(&repo)?,
    };
    let sel = select(&mut view, a)?;
    view.load_remote_sizes()?;
    let planning = counting.as_ref().map(|c| {
        let st = c.stats();
        (st.requests, st.bytes_down)
    });
    report(&view, &sel, a, json, "zone subscribe", planning, a.dry_run);
    if a.dry_run {
        return Ok(());
    }
    let ws = repo.workspace()?;
    let mut added = Vec::new();
    for z in names_of(&sel) {
        if ws.subs.contains_key(&z) {
            continue;
        }
        match (&counting, &url) {
            (Some(c), _) => {
                repo.fetch(c, Some(std::slice::from_ref(&z)))?;
                repo.zone_add_fetching(c, &z, None)?;
            }
            _ => repo.zone_add(&z, None)?,
        }
        added.push(z);
    }
    if !json {
        println!(
            "subscribed to {} zone(s): {} ({} already subscribed); nothing was unsubscribed",
            added.len(),
            if added.is_empty() { "(none)".into() } else { added.join(", ") },
            sel.included.len() - added.len()
        );
    }
    Ok(())
}

fn clone_for(url: &str, dir: &Path, zones: &[String], stats: bool, a: &ForArgs, json: bool) -> Result<(), Error> {
    if !zones.is_empty() {
        return Err(Error::Msg("--zone and --for/--task/--for-owner cannot be combined; use one".into()));
    }
    let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
    let url = remote::normalize_url(url, &cwd);
    if !a.dry_run && dir.exists() && std::fs::read_dir(dir).map(|mut x| x.next().is_some()).unwrap_or(true) {
        return Err(Error::Msg(format!("destination {} exists and is not empty", dir.display())));
    }
    let backend = remote::open_memo(&url, true)?;
    // Planning reads commits and trees (and the few manifest files) into a throwaway store, so a
    // plan that is refused or only a dry run leaves nothing behind and fsck of the clone stays clean.
    let plan_ct = vcs_remote::CountingBackend::new(&backend);
    let (sel, planning);
    {
        let mut view = View::remote(&plan_ct, None, a.queries_hint())?;
        sel = select(&mut view, a)?;
        view.load_remote_sizes()?;
        let st = plan_ct.stats();
        planning = (st.requests, st.bytes_down);
        report(&view, &sel, a, json, "clone", Some(planning), a.dry_run);
    }
    if a.dry_run {
        return Ok(());
    }
    let ct = vcs_remote::CountingBackend::new(&backend);
    let want = names_of(&sel);
    Repo::clone_from(&ct, &url, dir, Some(&want))?;
    if stats {
        let d = ct.stats();
        eprintln!(
            "clone total transfer (planning included): requests: {}, downloaded: {} bytes (planning {} + clone {})",
            d.requests + planning.0,
            d.bytes_down + planning.1,
            planning.1,
            d.bytes_down
        );
    }
    if !json {
        println!(
            "Cloned {url} into {} with {} of the repository's zones; the rest can be added later with `vcs zone add`.",
            dir.display(),
            want.len()
        );
    }
    Ok(())
}
