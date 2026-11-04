//! Local usage profile and zone predictor (docs/zone-profile.md).
//!
//! The profile is a small text file (`.vcs/profile`) that never leaves the machine: it stores
//! only a time, a kind (read / edit / build / note), a zone id and a directory prefix of at most
//! [`MAX_DIR_DEPTH`] components. No file names, no command lines, no contents. The predictor is
//! pure: `(profile, hints, dependency graph, zone sizes, params) -> ranked zones -> plan`. Same
//! inputs always give the same output (ties break on zone name), so it can be tested exactly.

use crate::error::Result;
use crate::repo::Repo;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directories are recorded at most this deep (`a/b/c`); deeper components are dropped.
pub const MAX_DIR_DEPTH: usize = 3;
/// Events kept; older ones are dropped when the file grows past `COMPACT_BYTES`.
pub const KEEP_EVENTS: usize = 2000;
/// The file is rewritten to its newest `KEEP_EVENTS` events once it grows past this many bytes.
const COMPACT_BYTES: u64 = 128 * 1024;
const HEADER: &str = "vcs-profile 1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// checkout, status, diff, log, grep, file-access hint
    Read,
    /// add, rm, mv, restore, commit
    Edit,
    /// a command run through `vcs profile record -- <cmd>`
    Build,
}

impl Kind {
    fn code(self) -> char {
        match self {
            Kind::Read => 'r',
            Kind::Edit => 'e',
            Kind::Build => 'b',
        }
    }
    fn from_code(c: &str) -> Option<Kind> {
        match c {
            "r" => Some(Kind::Read),
            "e" => Some(Kind::Edit),
            "b" => Some(Kind::Build),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Read => "read",
            Kind::Edit => "edit",
            Kind::Build => "build",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub ts_ms: u64,
    pub kind: Kind,
    pub zone: String,
    /// Directory prefix inside the repository ("" = zone root), at most [`MAX_DIR_DEPTH`] deep.
    pub dir: String,
}

/// Keep the first [`MAX_DIR_DEPTH`] directory components of a repo-relative path's parent. A
/// path with no `/` has no directory. The final component (a file name) is never kept unless
/// `is_dir` says the whole path is a directory.
pub fn privacy_dir(path: &str, is_dir: bool) -> String {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty() && *c != "." && *c != "..").collect();
    let dirs = if is_dir || comps.is_empty() {
        &comps[..]
    } else {
        &comps[..comps.len() - 1]
    };
    dirs.iter().take(MAX_DIR_DEPTH).copied().collect::<Vec<_>>().join("/")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// `false` after `vcs profile off`: nothing is recorded and no event is used.
    pub disabled: bool,
    pub events: Vec<Event>,
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile> {
        let mut p = Profile::default();
        let mut lines = text.lines();
        match lines.next() {
            Some(h) if h.trim() == HEADER => {}
            _ => return crate::error::msg("not a vcs profile file (first line must be `vcs-profile 1`)"),
        }
        for l in lines {
            let l = l.trim_end();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if l == "disabled" {
                p.disabled = true;
                continue;
            }
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            if f.len() < 3 {
                continue; // tolerate damage: a torn last line must not lose the whole profile
            }
            let (Ok(ts), Some(kind)) = (f[0].parse::<u64>(), Kind::from_code(f[1])) else {
                continue;
            };
            p.events.push(Event {
                ts_ms: ts,
                kind,
                zone: f[2].to_owned(),
                dir: f.get(3).copied().unwrap_or("").to_owned(),
            });
        }
        Ok(p)
    }

    pub fn encode(&self) -> String {
        let mut s = format!("{HEADER}\n");
        if self.disabled {
            s.push_str("disabled\n");
        }
        for e in &self.events {
            s.push_str(&line(e));
        }
        s
    }

    /// Load `path`; a missing file is an empty, enabled profile.
    pub fn load(path: &Path) -> Result<Profile> {
        match std::fs::read_to_string(path) {
            Ok(t) => Profile::parse(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Profile::default()),
            Err(source) => Err(crate::error::Error::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::meta::write_atomic(path, self.encode().as_bytes())
    }

    /// Merge events of another profile (imported seed); duplicates are dropped.
    pub fn import(&mut self, other: &Profile) -> usize {
        let have: BTreeSet<(u64, Kind, String, String)> = self
            .events
            .iter()
            .map(|e| (e.ts_ms, e.kind, e.zone.clone(), e.dir.clone()))
            .collect();
        let mut n = 0;
        for e in &other.events {
            if !have.contains(&(e.ts_ms, e.kind, e.zone.clone(), e.dir.clone())) {
                self.events.push(e.clone());
                n += 1;
            }
        }
        self.events
            .sort_by(|a, b| (a.ts_ms, &a.zone, &a.dir).cmp(&(b.ts_ms, &b.zone, &b.dir)));
        n
    }
}

fn line(e: &Event) -> String {
    format!("{}\t{}\t{}\t{}\n", e.ts_ms, e.kind.code(), e.zone, e.dir)
}

/// True when the file says `disabled` (reads only its first bytes).
pub fn is_disabled(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut b = [0u8; 64];
    let n = f.read(&mut b).unwrap_or(0);
    let head = String::from_utf8_lossy(&b[..n]);
    head.lines().nth(1) == Some("disabled")
}

/// Append events. Honors `disabled`. Compacts when the file is large. Cheap: one open + one
/// write in the common case (no parse of the existing events).
pub fn append(path: &Path, events: &[Event]) -> Result<usize> {
    if events.is_empty() || is_disabled(path) {
        return Ok(0);
    }
    let io = |source| crate::error::Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path).map_err(io)?;
    let mut buf = String::new();
    if f.metadata().map_err(io)?.len() == 0 {
        buf.push_str(HEADER);
        buf.push('\n');
    }
    for e in events {
        buf.push_str(&line(e));
    }
    f.write_all(buf.as_bytes()).map_err(io)?;
    drop(f);
    if std::fs::metadata(path).map(|m| m.len() > COMPACT_BYTES).unwrap_or(false) {
        let mut cur = Profile::load(path)?;
        if cur.events.len() > KEEP_EVENTS {
            let drop = cur.events.len() - KEEP_EVENTS;
            cur.events.drain(..drop);
            cur.save(path)?;
        }
    }
    Ok(events.len())
}

impl Repo {
    /// `.vcs/profile` (shared by linked worktrees).
    pub fn profile_file(&self) -> PathBuf {
        self.common.join("profile")
    }
    pub fn profile_load(&self) -> Result<Profile> {
        Profile::load(&self.profile_file())
    }
    /// Record one event per `(zone, dir)`. Honors `vcs profile off` and `VCS_PROFILE=off`.
    pub fn profile_record(&self, kind: Kind, now_ms: u64, hits: &[(String, String)]) -> Result<usize> {
        if std::env::var("VCS_PROFILE").is_ok_and(|v| v == "off" || v == "0") {
            return Ok(0);
        }
        let mut seen = BTreeSet::new();
        let ev: Vec<Event> = hits
            .iter()
            .filter(|h| seen.insert((*h).clone()))
            .map(|(z, d)| Event {
                ts_ms: now_ms,
                kind,
                zone: z.clone(),
                dir: d.clone(),
            })
            .collect();
        append(&self.profile_file(), &ev)
    }
    /// Download the listed objects that are not stored yet, batched with ranged reads over the
    /// remote packs. Used for the few manifest files the predictor reads.
    pub fn profile_fetch_objects(&self, backend: &dyn vcs_remote::Backend, ids: &[vcs_objects::ObjectId]) -> Result<usize> {
        use vcs_store::ObjectStore;
        let mut want = BTreeSet::new();
        for id in ids {
            if !self.store.contains(id)? {
                want.insert(*id);
            }
        }
        let idx_cache = vcs_remote::IdxCacheBackend::new(backend, self.common.join("remote-idx"));
        self.prefetch_ids_with(&idx_cache, &self.counters.packs, want)
    }
    /// The newest `max_per_zone` commits of every zone's `main` (local branch, else the remote's) as co-change records. Needs the
    /// commit objects to be local (`--auto` fetches them for all zones before planning); a zone whose head is missing is skipped.
    pub fn cochange_records(&self, max_per_zone: usize) -> Result<Vec<ChangeRec>> {
        use vcs_store::ObjectStore;
        let refs = self.refs()?;
        let rr = self.remote_refs()?;
        let mut out = Vec::new();
        for z in self.zones()?.names() {
            let Some(head) = refs.head(z, crate::meta::DEFAULT_BRANCH).or(rr.head(z, crate::meta::DEFAULT_BRANCH)) else {
                continue;
            };
            let mut cur = Some(head);
            for _ in 0..max_per_zone {
                let Some(id) = cur else { break };
                if !self.store.contains(&id)? {
                    break;
                }
                let c = self.commit_obj(&id)?;
                out.push(ChangeRec {
                    zone: z.to_owned(),
                    author: c.author.email.clone(),
                    message: c.message.clone(),
                    ts: c.author.timestamp,
                });
                cur = c.parents.first().copied();
            }
        }
        Ok(out)
    }
    /// The `.vcs` directory (where the remote pack index cache lives).
    pub fn profile_state_dir(&self) -> PathBuf {
        self.common.clone()
    }
    pub fn profile_set_enabled(&self, on: bool) -> Result<()> {
        let mut p = self.profile_load()?;
        p.disabled = !on;
        if !on {
            p.events.clear(); // opting out also erases what was learned
        }
        p.save(&self.profile_file())
    }
    pub fn profile_clear(&self) -> Result<()> {
        let mut p = self.profile_load()?;
        p.events.clear();
        p.save(&self.profile_file())
    }
}

// ------------------------------------------------------------------ hints (checked-in)

/// Team hints from `.vcs-profile-hints.toml` (checked in) and `.vcs/profile-hints.toml` (local).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hints {
    pub role: Option<String>,
    /// role name -> zone names
    pub roles: BTreeMap<String, Vec<String>>,
    /// explicit zone dependency edges `from -> [to]` (added to the detected graph)
    pub deps: BTreeMap<String, Vec<String>>,
    pub pin: Vec<String>,
    pub exclude: Vec<String>,
    pub half_life_days: Option<f64>,
    pub max_zones: Option<usize>,
    pub max_bytes: Option<u64>,
}

enum Val {
    Str(String),
    Num(f64),
    List(Vec<String>),
}

fn parse_val(raw: &str) -> Option<Val> {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        let mut out = Vec::new();
        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            out.push(part.strip_prefix('"')?.strip_suffix('"')?.to_owned());
        }
        return Some(Val::List(out));
    }
    if let Some(s) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return Some(Val::Str(s.to_owned()));
    }
    raw.parse::<f64>().ok().map(Val::Num)
}

fn strip_comment(l: &str) -> &str {
    let mut in_str = false;
    for (i, c) in l.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &l[..i],
            _ => {}
        }
    }
    l
}

impl Hints {
    /// Parse the small TOML subset documented in docs/zone-profile.md. Unknown keys are
    /// errors (a typo must not silently change what is pulled).
    pub fn parse(text: &str) -> Result<Hints> {
        let mut h = Hints::default();
        let mut section = String::new();
        for (n, raw) in text.lines().enumerate() {
            let l = strip_comment(raw).trim();
            if l.is_empty() {
                continue;
            }
            let bad = |why: &str| crate::error::msg::<Hints>(format!("profile hints line {}: {why}", n + 1));
            if let Some(s) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                section = s.trim().to_owned();
                continue;
            }
            let Some((k, v)) = l.split_once('=') else {
                return bad("expected `key = value`");
            };
            let (k, k_unq) = (k.trim(), k.trim().trim_matches('"'));
            let Some(val) = parse_val(v) else {
                return bad("value must be a string, a number or a list of strings");
            };
            match (section.as_str(), k, val) {
                ("profile", "role", Val::Str(s)) => h.role = Some(s),
                ("profile", "half_life_days", Val::Num(x)) if x > 0.0 => h.half_life_days = Some(x),
                ("profile", "max_zones", Val::Num(x)) if x >= 1.0 => h.max_zones = Some(x as usize),
                ("profile", "max_bytes", Val::Num(x)) if x >= 1.0 => h.max_bytes = Some(x as u64),
                ("pin", "zones", Val::List(v)) => h.pin = v,
                ("exclude", "zones", Val::List(v)) => h.exclude = v,
                ("deps", _, Val::List(v)) => {
                    h.deps.entry(k_unq.to_owned()).or_default().extend(v);
                }
                (s, "zones", Val::List(v)) if s.starts_with("role.") => {
                    h.roles
                        .entry(s["role.".len()..].trim().trim_matches('"').to_owned())
                        .or_default()
                        .extend(v);
                }
                (s, k, _) => return bad(&format!("unknown key `{k}` in section [{s}]")),
            }
        }
        Ok(h)
    }

    /// Union: lists are concatenated, scalars from `other` win when set.
    pub fn merge(&mut self, other: Hints) {
        if other.role.is_some() {
            self.role = other.role;
        }
        for (k, v) in other.roles {
            self.roles.entry(k).or_default().extend(v);
        }
        for (k, v) in other.deps {
            self.deps.entry(k).or_default().extend(v);
        }
        self.pin.extend(other.pin);
        self.exclude.extend(other.exclude);
        self.half_life_days = other.half_life_days.or(self.half_life_days);
        self.max_zones = other.max_zones.or(self.max_zones);
        self.max_bytes = other.max_bytes.or(self.max_bytes);
    }

    pub fn role_zones(&self) -> &[String] {
        self.role.as_ref().and_then(|r| self.roles.get(r)).map(Vec::as_slice).unwrap_or(&[])
    }
}

// ------------------------------------------------------------------ predictor

#[derive(Clone, Debug)]
pub struct Params {
    pub now_ms: u64,
    pub half_life_days: f64,
    /// Events closer together than this belong to one work session (for co-access).
    pub session_gap_ms: u64,
    pub w_read: f64,
    pub w_build: f64,
    pub w_edit: f64,
    /// Score passed to a dependency per hop, relative to the depending zone's score.
    pub dep_factor: f64,
    pub max_depth: u32,
    /// Weight of co-access (zones used in the same sessions).
    pub co_factor: f64,
    /// Weight of co-change mined from history (zones committed together).
    pub cochange_factor: f64,
    /// Fewer shared groups than this are a coincidence, not evidence.
    pub cochange_min_support: u32,
    /// Prior score of a zone named by the user's role hint.
    pub role_prior: f64,
    /// Zones scoring below this have no real evidence.
    pub min_score: f64,
}

impl Default for Params {
    fn default() -> Params {
        Params {
            now_ms: 0,
            half_life_days: 14.0,
            session_gap_ms: 30 * 60 * 1000,
            w_read: 1.0,
            w_build: 1.5,
            w_edit: 3.0,
            dep_factor: 0.3,
            max_depth: 2,
            co_factor: 0.4,
            cochange_factor: 0.35,
            cochange_min_support: 2,
            role_prior: 0.5,
            min_score: 0.05,
        }
    }
}

/// Zone dependency edges, `from -> [to]` (from needs to).
#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub deps: BTreeMap<String, BTreeSet<String>>,
    /// Zones that were committed together in the repository's history (see [`mine_cochange`]).
    pub cochange: Cochange,
}

/// One commit as the co-change miner sees it: which zone, who, what message, when (seconds).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeRec {
    pub zone: String,
    pub author: String,
    pub message: String,
    pub ts: u64,
}

/// Co-change counts: `changes[z]` = groups of commits that touched `z`; `pairs[(a, b)]` = groups that touched both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cochange {
    pub changes: BTreeMap<String, u32>,
    pub pairs: BTreeMap<(String, String), u32>,
    pub groups: u32,
}

impl Cochange {
    /// P(b changes | a changed), smoothed by one pseudo-group, and the counts behind it.
    pub fn cond(&self, a: &str, b: &str) -> Option<(f64, u32, u32)> {
        let n = *self.pairs.get(&(a.to_owned(), b.to_owned()))?;
        let t = self.changes.get(a).copied().unwrap_or(0);
        Some((f64::from(n) / (f64::from(t) + 1.0), n, t))
    }
}

/// Mine zones that change together. A zone has its own commit chain, so there is no single "commit touched files of these
/// zones" record; the signal is **one author, one message, within `window_s` seconds**: a `vcs commit` that moves several zones,
/// and an import of one git commit that fans out to several zones, both look like that. Commits are grouped by
/// (author, message) and split where the gap to the previous one exceeds `window_s`; every group counts once per zone.
/// Groups of one zone carry no pair but count toward `changes` (so P(b|a) is a share of all of a's changes). A group of more than
/// `max_group` zones is a bulk change (the first import, a repository-wide rename): it says nothing about which zones belong
/// together, so it is dropped entirely.
pub fn mine_cochange(recs: &[ChangeRec], window_s: u64, max_group: usize) -> Cochange {
    let mut order: Vec<&ChangeRec> = recs.iter().collect();
    order.sort_by(|a, b| (&a.author, &a.message, a.ts, &a.zone).cmp(&(&b.author, &b.message, b.ts, &b.zone)));
    let mut out = Cochange::default();
    let mut i = 0;
    while i < order.len() {
        let mut j = i + 1;
        while j < order.len()
            && order[j].author == order[i].author
            && order[j].message == order[i].message
            && order[j].ts.saturating_sub(order[j - 1].ts) <= window_s
        {
            j += 1;
        }
        let zs: BTreeSet<&str> = order[i..j].iter().map(|r| r.zone.as_str()).collect();
        i = j;
        if zs.len() > max_group {
            continue;
        }
        out.groups += 1;
        for a in &zs {
            *out.changes.entry((*a).to_owned()).or_default() += 1;
            for b in &zs {
                if a != b {
                    *out.pairs.entry(((*a).to_owned(), (*b).to_owned())).or_default() += 1;
                }
            }
        }
    }
    out
}

impl Graph {
    pub fn add(&mut self, from: &str, to: &str) {
        if from != to {
            self.deps.entry(from.to_owned()).or_default().insert(to.to_owned());
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranked {
    pub zone: String,
    pub score: f64,
    /// Decayed frequency of direct use, relative to the most used zone (0..1).
    pub direct: f64,
    pub dep: f64,
    pub co: f64,
    /// Seeded by zones that were committed together with a used zone (from history, not the profile).
    pub cochange: f64,
    pub hint: f64,
    pub pinned: bool,
    pub excluded: bool,
    pub reasons: Vec<String>,
}

/// Rank every zone in `zones`. Deterministic: order is score desc, then name.
pub fn rank(profile: &Profile, hints: &Hints, graph: &Graph, zones: &[String], p: &Params) -> Vec<Ranked> {
    let known: BTreeSet<&str> = zones.iter().map(String::as_str).collect();
    let hl_ms = p.half_life_days * 86_400_000.0;
    let mut direct: BTreeMap<&str, f64> = BTreeMap::new();
    let mut count: BTreeMap<&str, (u32, u32, u32)> = BTreeMap::new();
    let mut events: Vec<&Event> = Vec::new();
    if !profile.disabled {
        for e in &profile.events {
            if !known.contains(e.zone.as_str()) {
                continue;
            }
            events.push(e);
            let age = p.now_ms.saturating_sub(e.ts_ms) as f64;
            let w = match e.kind {
                Kind::Read => p.w_read,
                Kind::Build => p.w_build,
                Kind::Edit => p.w_edit,
            };
            *direct.entry(e.zone.as_str()).or_default() += w * (-(age / hl_ms) * std::f64::consts::LN_2).exp();
            let c = count.entry(e.zone.as_str()).or_default();
            match e.kind {
                Kind::Read => c.0 += 1,
                Kind::Edit => c.1 += 1,
                Kind::Build => c.2 += 1,
            }
        }
    }
    let max = direct.values().cloned().fold(0.0f64, f64::max);
    let dn: BTreeMap<&str, f64> = direct.iter().map(|(z, v)| (*z, if max > 0.0 { v / max } else { 0.0 })).collect();

    // sessions: events sorted by time, split where the gap exceeds session_gap_ms
    events.sort_by_key(|e| e.ts_ms);
    let mut sessions: Vec<BTreeSet<&str>> = Vec::new();
    let mut last = None;
    for e in &events {
        if last.is_none_or(|l: u64| e.ts_ms.saturating_sub(l) > p.session_gap_ms) {
            sessions.push(BTreeSet::new());
        }
        sessions.last_mut().unwrap().insert(e.zone.as_str());
        last = Some(e.ts_ms);
    }
    let mut sess_of: BTreeMap<&str, f64> = BTreeMap::new();
    let mut both: BTreeMap<(&str, &str), f64> = BTreeMap::new();
    for s in &sessions {
        for a in s {
            *sess_of.entry(a).or_default() += 1.0;
            for b in s {
                if a != b {
                    *both.entry((a, b)).or_default() += 1.0;
                }
            }
        }
    }

    let mut out: BTreeMap<String, Ranked> = zones
        .iter()
        .map(|z| {
            (
                z.clone(),
                Ranked {
                    zone: z.clone(),
                    direct: dn.get(z.as_str()).copied().unwrap_or(0.0),
                    ..Default::default()
                },
            )
        })
        .collect();
    for (z, c) in &count {
        out.get_mut(*z)
            .unwrap()
            .reasons
            .push(format!("used directly: {} read, {} edit, {} build event(s)", c.0, c.1, c.2));
    }

    // dependency expansion: BFS from every used zone over the (profile-independent) graph
    let mut all = graph.clone();
    for (from, tos) in &hints.deps {
        for t in tos {
            all.add(from, t);
        }
    }
    // zones that seed the expansion: used zones, role-hinted zones and pinned zones
    let mut seeds: BTreeMap<&str, f64> = dn.clone();
    for n in hints.role_zones() {
        if let Some(z) = zones.iter().find(|z| *z == n) {
            *seeds.entry(z.as_str()).or_default() += p.role_prior;
        }
    }
    for n in &hints.pin {
        if let Some(z) = zones.iter().find(|z| *z == n) {
            *seeds.entry(z.as_str()).or_default() += 1.0;
        }
    }
    for (src, w) in &seeds {
        if *w <= 0.0 {
            continue;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::from([*src]);
        let mut frontier = vec![*src];
        for depth in 1..=p.max_depth {
            let mut next = Vec::new();
            for f in &frontier {
                if let Some(tos) = all.deps.get(*f) {
                    for t in tos {
                        if seen.insert(t.as_str()) {
                            next.push(t.as_str());
                            if let Some(r) = out.get_mut(t.as_str()) {
                                let add = w * p.dep_factor.powi(depth as i32);
                                r.dep += add;
                                r.reasons.push(format!("dependency of {src} (hop {depth}, +{add:.3})"));
                            }
                        }
                    }
                }
            }
            frontier = next;
        }
    }
    // co-access: P(b used | a used) over sessions, smoothed
    for ((a, b), n) in &both {
        let w = dn.get(a).copied().unwrap_or(0.0);
        if w <= 0.0 {
            continue;
        }
        let cond = n / (sess_of[a] + 1.0);
        let add = w * p.co_factor * cond;
        if let Some(r) = out.get_mut(*b) {
            r.co += add;
            r.reasons.push(format!(
                "used in the same session as {a} ({} of {} sessions, +{add:.3})",
                n, sess_of[a]
            ));
        }
    }
    // co-change from history: P(b changed | a changed) in the repository's own commits, from every seed
    for (a, w) in &seeds {
        if *w <= 0.0 {
            continue;
        }
        for b in zones {
            if b == a {
                continue;
            }
            if let Some((cond, n, t)) = graph.cochange.cond(a, b).filter(|(_, n, _)| *n >= p.cochange_min_support) {
                let add = w * p.cochange_factor * cond;
                if let Some(r) = out.get_mut(b.as_str()) {
                    r.cochange += add;
                    r.reasons.push(format!("committed together with {a} ({n} of {t} changes, +{add:.3})"));
                }
            }
        }
    }
    // hints
    let resolve = |n: &str| -> Option<String> { known.contains(n).then(|| n.to_owned()) };
    for n in hints.role_zones() {
        if let Some(z) = resolve(n) {
            let r = out.get_mut(&z).unwrap();
            r.hint += p.role_prior;
            r.reasons.push(format!(
                "role `{}` hint (+{:.3})",
                hints.role.as_deref().unwrap_or(""),
                p.role_prior
            ));
        }
    }
    for n in &hints.pin {
        if let Some(z) = resolve(n) {
            let r = out.get_mut(&z).unwrap();
            r.pinned = true;
            r.reasons.push("pinned by hints".into());
        }
    }
    for n in &hints.exclude {
        if let Some(z) = resolve(n) {
            let r = out.get_mut(&z).unwrap();
            r.excluded = true;
            r.reasons.push("excluded by hints".into());
        }
    }
    let mut v: Vec<Ranked> = out.into_values().collect();
    for r in &mut v {
        r.score = r.direct + r.dep + r.co + r.cochange + r.hint;
    }
    v.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.score.total_cmp(&a.score)).then(a.zone.cmp(&b.zone)));
    v
}

#[derive(Clone, Debug, Default)]
pub struct Budget {
    pub max_zones: Option<usize>,
    pub max_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub zone: String,
    pub score: f64,
    pub bytes: u64,
    /// Why it is hot, or why it was left out of the hot set.
    pub why: String,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Subscribed with file contents, in the order to fetch.
    pub hot: Vec<Choice>,
    /// Everything else: commits and trees only, contents on demand. Never dropped silently.
    pub skipped: Vec<Choice>,
}

/// Choose the hot set. `sizes` maps zone -> stored bytes of its head (unknown = 0).
/// `always` zones (the root zone, zones already subscribed) are hot, cost nothing against the
/// budgets (which count only newly added zones and bytes) and are never removed. Pinned zones come next, then ranked zones with `score >= min_score`.
pub fn plan(ranked: &[Ranked], sizes: &BTreeMap<String, u64>, always: &BTreeSet<String>, budget: &Budget, min_score: f64) -> Plan {
    let mut plan = Plan::default();
    let mut bytes = 0u64;
    let size = |z: &str| sizes.get(z).copied().unwrap_or(0);
    for r in ranked.iter().filter(|r| always.contains(&r.zone)) {
        plan.hot.push(Choice {
            zone: r.zone.clone(),
            score: r.score,
            bytes: size(&r.zone),
            why: "always kept (root or already subscribed)".into(),
        });
    }
    let mut chosen = 0usize; // budgets count zones added by this plan, not the ones kept
    for r in ranked.iter().filter(|r| !always.contains(&r.zone)) {
        let b = size(&r.zone);
        let skip = |why: String| Choice {
            zone: r.zone.clone(),
            score: r.score,
            bytes: b,
            why,
        };
        if r.excluded {
            plan.skipped.push(skip("excluded by hints".into()));
        } else if !r.pinned && r.score < min_score {
            plan.skipped.push(skip("no evidence in the profile".into()));
        } else if !r.pinned && budget.max_zones.is_some_and(|m| chosen >= m) {
            plan.skipped
                .push(skip(format!("over the zone budget ({})", budget.max_zones.unwrap())));
        } else if !r.pinned && budget.max_bytes.is_some_and(|m| bytes + b > m) {
            plan.skipped.push(skip(format!(
                "over the byte budget (needs {b} B, {} B left)",
                budget.max_bytes.unwrap().saturating_sub(bytes)
            )));
        } else {
            bytes += b;
            chosen += 1;
            let why = if r.pinned {
                "pinned by hints".to_owned()
            } else {
                format!("score {:.3}", r.score)
            };
            plan.hot.push(Choice {
                zone: r.zone.clone(),
                score: r.score,
                bytes: b,
                why,
            });
        }
    }
    plan
}

/// Parse `64M`, `2G`, `500K`, `1234`.
pub fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1u64 << 10),
        'm' | 'M' => (&s[..s.len() - 1], 1 << 20),
        'g' | 'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok()?.checked_mul(mul)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;
    fn ev(day: u64, kind: Kind, zone: &str) -> Event {
        Event {
            ts_ms: day * DAY,
            kind,
            zone: zone.into(),
            dir: String::new(),
        }
    }
    fn zs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn params(now_day: u64) -> Params {
        Params {
            now_ms: now_day * DAY,
            ..Params::default()
        }
    }

    #[test]
    fn privacy_dir_drops_file_names_and_depth() {
        assert_eq!(privacy_dir("a/b/c/d/e.rs", false), "a/b/c");
        assert_eq!(privacy_dir("README.md", false), "");
        assert_eq!(privacy_dir("a/b", true), "a/b");
        assert_eq!(privacy_dir("a/../b/x.rs", false), "a/b");
    }

    #[test]
    fn profile_round_trip_and_torn_line() {
        let p = Profile {
            disabled: false,
            events: vec![ev(1, Kind::Edit, "web"), ev(2, Kind::Read, "api")],
        };
        let t = p.encode();
        assert_eq!(Profile::parse(&t).unwrap(), p);
        let torn = format!("{t}12\tr");
        assert_eq!(Profile::parse(&torn).unwrap(), p);
        assert!(Profile::parse("nonsense").is_err());
    }

    #[test]
    fn recency_and_kind_decide_direct_rank() {
        let prof = Profile {
            disabled: false,
            events: vec![ev(1, Kind::Read, "old"), ev(99, Kind::Read, "new"), ev(98, Kind::Edit, "edited")],
        };
        let r = rank(
            &prof,
            &Hints::default(),
            &Graph::default(),
            &zs(&["old", "new", "edited", "none"]),
            &params(100),
        );
        let order: Vec<&str> = r.iter().map(|x| x.zone.as_str()).collect();
        assert_eq!(order, ["edited", "new", "old", "none"]);
        assert_eq!(r[0].direct, 1.0);
        assert!(r[3].score == 0.0);
    }

    #[test]
    fn frequency_beats_single_recent_event() {
        let mut ev_ = vec![ev(99, Kind::Read, "once")];
        for d in 90..99 {
            ev_.push(ev(d, Kind::Read, "often"));
        }
        let prof = Profile {
            disabled: false,
            events: ev_,
        };
        let r = rank(&prof, &Hints::default(), &Graph::default(), &zs(&["once", "often"]), &params(100));
        assert_eq!(r[0].zone, "often");
    }

    #[test]
    fn dependencies_expand_with_decay() {
        let prof = Profile {
            disabled: false,
            events: vec![ev(100, Kind::Edit, "app")],
        };
        let mut g = Graph::default();
        g.add("app", "core");
        g.add("core", "util");
        g.add("util", "deep");
        let r = rank(
            &prof,
            &Hints::default(),
            &g,
            &zs(&["app", "core", "util", "deep", "other"]),
            &params(100),
        );
        let by = |n: &str| r.iter().find(|x| x.zone == n).unwrap().clone();
        assert!((by("core").dep - 0.3).abs() < 1e-9);
        assert!((by("util").dep - 0.09).abs() < 1e-9);
        assert_eq!(by("deep").score, 0.0, "max_depth is 2");
        assert_eq!(by("other").score, 0.0);
        let order: Vec<&str> = r.iter().map(|x| x.zone.as_str()).collect();
        assert_eq!(&order[..3], ["app", "core", "util"]);
    }

    #[test]
    fn hinted_and_pinned_zones_seed_dependencies_too() {
        let hints = Hints::parse("[profile]\nrole = \"r\"\n[role.r]\nzones = [\"api\"]\n[pin]\nzones = [\"tools\"]\n").unwrap();
        let mut g = Graph::default();
        g.add("api", "core");
        g.add("tools", "shared");
        let r = rank(
            &Profile::default(),
            &hints,
            &g,
            &zs(&["api", "core", "tools", "shared", "x"]),
            &params(1),
        );
        let by = |n: &str| r.iter().find(|x| x.zone == n).unwrap().clone();
        assert!((by("core").dep - 0.5 * 0.3).abs() < 1e-9);
        assert!((by("shared").dep - 0.3).abs() < 1e-9);
        assert_eq!(by("x").score, 0.0);
    }

    #[test]
    fn co_access_pulls_in_companions() {
        // web and ui are used in the same three sessions; docs is used alone
        let mut e = vec![];
        for d in [10, 20, 30] {
            e.push(Event {
                ts_ms: d * DAY,
                kind: Kind::Edit,
                zone: "web".into(),
                dir: String::new(),
            });
            e.push(Event {
                ts_ms: d * DAY + 60_000,
                kind: Kind::Read,
                zone: "ui".into(),
                dir: String::new(),
            });
        }
        e.push(ev(5, Kind::Read, "docs"));
        let prof = Profile {
            disabled: false,
            events: e,
        };
        let r = rank(
            &prof,
            &Hints::default(),
            &Graph::default(),
            &zs(&["web", "ui", "docs", "x"]),
            &params(31),
        );
        let ui = r.iter().find(|x| x.zone == "ui").unwrap();
        assert!(ui.co > 0.0);
        assert!(ui.co < 0.4, "smoothed, never above co_factor");
        assert_eq!(r.iter().find(|x| x.zone == "x").unwrap().score, 0.0);
    }

    #[test]
    fn disabled_profile_is_ignored_but_hints_work() {
        let prof = Profile {
            disabled: true,
            events: vec![ev(100, Kind::Edit, "web")],
        };
        let hints = Hints::parse("[profile]\nrole = \"backend\"\n[role.backend]\nzones = [\"api\"]\n").unwrap();
        let r = rank(&prof, &hints, &Graph::default(), &zs(&["web", "api"]), &params(100));
        assert_eq!(r[0].zone, "api");
        assert_eq!(r[1].score, 0.0);
    }

    #[test]
    fn hints_parse_and_reject_typos() {
        let h = Hints::parse(
            "# team\n[profile]\nrole = \"frontend\"\nhalf_life_days = 7\nmax_zones = 5 # cap\n[role.frontend]\nzones = [\"web\", \"ui\"]\n[deps]\nweb = [\"ui\"]\n[pin]\nzones = [\"tools\"]\n[exclude]\nzones = [\"legacy\"]\n",
        )
        .unwrap();
        assert_eq!(h.role.as_deref(), Some("frontend"));
        assert_eq!(h.role_zones(), ["web", "ui"]);
        assert_eq!(h.deps["web"], ["ui"]);
        assert_eq!((h.half_life_days, h.max_zones), (Some(7.0), Some(5)));
        assert!(Hints::parse("[profile]\nrolle = \"x\"\n").is_err());
        assert!(Hints::parse("[profile]\nrole = nope\n").is_err());
    }

    #[test]
    fn plan_respects_budgets_pins_excludes_and_lists_every_skip() {
        let prof = Profile {
            disabled: false,
            events: vec![
                ev(100, Kind::Edit, "a"),
                ev(99, Kind::Edit, "b"),
                ev(98, Kind::Edit, "c"),
                ev(97, Kind::Edit, "d"),
            ],
        };
        let hints = Hints::parse("[pin]\nzones = [\"p\"]\n[exclude]\nzones = [\"d\"]\n").unwrap();
        let names = zs(&["root", "a", "b", "c", "d", "p", "z"]);
        let r = rank(&prof, &hints, &Graph::default(), &names, &params(100));
        let sizes: BTreeMap<String, u64> = [("root", 10), ("a", 100), ("b", 5000), ("c", 100), ("d", 1), ("p", 7), ("z", 1)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let always = BTreeSet::from(["root".to_owned()]);
        let pl = plan(
            &r,
            &sizes,
            &always,
            &Budget {
                max_zones: Some(4),
                max_bytes: Some(1000),
            },
            0.05,
        );
        let hot: Vec<&str> = pl.hot.iter().map(|c| c.zone.as_str()).collect();
        assert_eq!(
            hot,
            ["root", "p", "a", "c"],
            "b is over the byte budget, c still fits; d is excluded"
        );
        let mut all: Vec<&str> = hot
            .iter()
            .chain(pl.skipped.iter().map(|c| c.zone.as_str()).collect::<Vec<_>>().iter())
            .copied()
            .collect();
        all.sort();
        assert_eq!(
            all,
            ["a", "b", "c", "d", "p", "root", "z"],
            "every zone is hot or listed as skipped"
        );
        assert!(pl.skipped.iter().find(|c| c.zone == "b").unwrap().why.contains("byte budget"));
        assert!(pl.skipped.iter().find(|c| c.zone == "z").unwrap().why.contains("no evidence"));
        let pl2 = plan(
            &r,
            &sizes,
            &always,
            &Budget {
                max_zones: Some(2),
                max_bytes: None,
            },
            0.05,
        );
        assert_eq!(pl2.hot.len(), 3, "root + p + a: the budget counts added zones");
    }

    #[test]
    fn ranking_is_deterministic() {
        let prof = Profile {
            disabled: false,
            events: vec![ev(5, Kind::Read, "b"), ev(5, Kind::Read, "a")],
        };
        let n = zs(&["a", "b", "c"]);
        let x = rank(&prof, &Hints::default(), &Graph::default(), &n, &params(6));
        let y = rank(&prof, &Hints::default(), &Graph::default(), &n, &params(6));
        assert_eq!(x, y);
        assert_eq!(x[0].zone, "a", "equal scores break on name");
    }

    #[test]
    fn bytes_parse() {
        assert_eq!(parse_bytes("64M"), Some(64 << 20));
        assert_eq!(parse_bytes("2g"), Some(2 << 30));
        assert_eq!(parse_bytes("12"), Some(12));
        assert_eq!(parse_bytes("x"), None);
    }

    #[test]
    fn append_honors_off_and_compacts() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("profile");
        assert_eq!(append(&f, &[ev(1, Kind::Read, "a")]).unwrap(), 1);
        assert_eq!(Profile::load(&f).unwrap().events.len(), 1);
        let mut p = Profile::load(&f).unwrap();
        p.disabled = true;
        p.events.clear();
        p.save(&f).unwrap();
        assert_eq!(append(&f, &[ev(2, Kind::Read, "a")]).unwrap(), 0);
        assert!(Profile::load(&f).unwrap().events.is_empty());
        p.disabled = false;
        p.save(&f).unwrap();
        let many: Vec<Event> = (0..6005u64)
            .map(|i| Event {
                ts_ms: i,
                kind: Kind::Read,
                zone: "a".into(),
                dir: format!("x/{}", "y".repeat(30)),
            })
            .collect();
        append(&f, &many).unwrap();
        let after = Profile::load(&f).unwrap();
        assert_eq!(after.events.len(), KEEP_EVENTS);
        assert_eq!(after.events.last().unwrap().ts_ms, 6004);
    }

    fn rec(zone: &str, who: &str, msg: &str, ts: u64) -> ChangeRec {
        ChangeRec {
            zone: zone.into(),
            author: who.into(),
            message: msg.into(),
            ts,
        }
    }

    #[test]
    fn cochange_groups_one_author_one_message_within_the_window() {
        let recs = vec![
            rec("api", "a@x", "rename field", 1000),
            rec("web", "a@x", "rename field", 1001),
            rec("api", "a@x", "tune cache", 5000),
            rec("api", "b@x", "rename field", 1002), // another author: not the same change
            rec("web", "a@x", "rename field", 9000), // same message, far apart: another group
        ];
        let c = mine_cochange(&recs, 120, 8);
        assert_eq!(c.groups, 4);
        assert_eq!(c.changes["api"], 3);
        assert_eq!(c.changes["web"], 2);
        assert_eq!(c.pairs[&("api".into(), "web".into())], 1);
        assert_eq!(c.pairs[&("web".into(), "api".into())], 1);
        assert!(!c.pairs.contains_key(&("api".into(), "api".into())));
        // P(web | api) = 1 pair of 3 api changes, smoothed by one pseudo-group
        let (p, n, t) = c.cond("api", "web").unwrap();
        assert_eq!((n, t), (1, 3));
        assert!((p - 0.25).abs() < 1e-9);
        assert!(c.cond("api", "none").is_none());
    }

    #[test]
    fn cochange_ignores_bulk_commits_and_single_coincidences() {
        let mut recs = Vec::new();
        // an import that touched 10 zones at once, and one pair that met once
        for z in 0..10 {
            recs.push(rec(&format!("z{z}"), "a@x", "import", 10));
        }
        recs.push(rec("api", "a@x", "once", 500));
        recs.push(rec("web", "a@x", "once", 501));
        let c = mine_cochange(&recs, 120, 8);
        assert_eq!(c.groups, 1, "the import is dropped");
        assert!(!c.changes.contains_key("z0"));
        let prof = Profile { disabled: false, events: vec![ev(99, Kind::Edit, "api")] };
        let g = Graph { cochange: c, ..Graph::default() };
        let r = rank(&prof, &Hints::default(), &g, &zs(&["api", "web"]), &params(100));
        assert_eq!(r.iter().find(|x| x.zone == "web").unwrap().cochange, 0.0, "one shared group is below the support threshold");
    }

    #[test]
    fn cochange_lifts_a_zone_that_is_committed_with_a_used_zone_and_is_deterministic() {
        let prof = Profile {
            disabled: false,
            events: vec![ev(99, Kind::Edit, "api")],
        };
        let mut recs = Vec::new();
        for i in 0..10 {
            recs.push(rec("api", "a@x", &format!("m{i}"), 100 * i));
            recs.push(rec("schema", "a@x", &format!("m{i}"), 100 * i + 1));
        }
        recs.push(rec("other", "a@x", "lone", 5)); // never with api
        let g = Graph {
            cochange: mine_cochange(&recs, 120, 8),
            ..Graph::default()
        };
        let zones = zs(&["api", "schema", "other", "idle"]);
        let r = rank(&prof, &Hints::default(), &g, &zones, &params(100));
        let by = |z: &str| r.iter().find(|x| x.zone == z).unwrap();
        assert!(by("schema").cochange > 0.2, "{:?}", by("schema"));
        assert!(by("schema").score >= Params::default().min_score);
        assert!(by("schema").reasons.iter().any(|x| x.contains("committed together with api (10 of 10")));
        assert_eq!(by("other").cochange, 0.0);
        assert_eq!(by("idle").score, 0.0);
        assert_eq!(r, rank(&prof, &Hints::default(), &g, &zones, &params(100)));
        // without history the same profile gives schema nothing
        let none = rank(&prof, &Hints::default(), &Graph::default(), &zones, &params(100));
        assert_eq!(none.iter().find(|x| x.zone == "schema").unwrap().score, 0.0);
    }
}
