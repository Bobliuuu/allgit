//! Zone auto-detection: propose zone boundaries from signals, find the zone-to-zone dependency
//! graph, and compute the minimal zone set for a task. Pure analysis over a flat file list; it
//! never writes, never calls the network and never calls a model. See docs/zones-autodetect.md.
//!
//! Everything here is a *heuristic proposal*: manifests are read with small tolerant parsers
//! (not full TOML/YAML/Starlark), imports with line scanners. What is and is not detected is
//! listed in the document; unreadable content is reported, never guessed.

use crate::meta::{Zones, ROOT_ZONE};
use crate::treeops::Flat;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use vcs_objects::ObjectId;

/// Reads the content of a file by id; `None` when it is not available (not downloaded, too big...).
pub type Reader<'a> = &'a dyn Fn(&ObjectId) -> Option<Vec<u8>>;
/// Size in bytes of a file by id, if known.
pub type Sizer<'a> = &'a dyn Fn(&ObjectId) -> Option<u64>;

// ---------------------------------------------------------------- small helpers

fn base(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}
fn dir_of(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}
fn is_under(p: &str, prefix: &str) -> bool {
    prefix.is_empty() || p == prefix || (p.starts_with(prefix) && p.as_bytes().get(prefix.len()) == Some(&b'/'))
}
/// `dir/rel` with `.` and `..` resolved; `None` when it escapes the repository.
pub fn norm_join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() { vec![] } else { dir.split('/').collect() };
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}
fn utf8(b: Vec<u8>) -> Option<String> {
    String::from_utf8(b).ok()
}
fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| seg_match(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && seg_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && seg_match(rest, &s[1..]),
    }
}
fn glob(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| glob(rest, &path[i..])),
        Some((p, rest)) => !path.is_empty() && seg_match(p.as_bytes(), path[0].as_bytes()) && glob(rest, &path[1..]),
    }
}
fn glob_path(pat: &str, path: &str) -> bool {
    let pat = pat.trim_start_matches("./").trim_end_matches('/');
    let p: Vec<&str> = pat.split('/').collect();
    let s: Vec<&str> = if path.is_empty() { vec![] } else { path.split('/').collect() };
    glob(&p, &s)
}
fn quoted_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<(char, String)> = None;
    for c in s.chars() {
        match (&mut cur, c) {
            (None, '"') | (None, '\'') => cur = Some((c, String::new())),
            (Some((q, _)), c) if c == *q => {
                out.push(cur.take().unwrap().1);
            }
            (Some((_, b)), c) => b.push(c),
            _ => {}
        }
    }
    out
}
fn strip_toml_comment(l: &str) -> &str {
    let mut q: Option<char> = None;
    for (i, c) in l.char_indices() {
        match (q, c) {
            (None, '"') | (None, '\'') => q = Some(c),
            (Some(x), c) if c == x => q = None,
            (None, '#') => return &l[..i],
            _ => {}
        }
    }
    l
}
/// The quoted value of `key = "..."` found anywhere in `s` (inline tables included).
fn kv_str(s: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = s[from..].find(key) {
        let at = from + i;
        let before_ok =
            at == 0 || !s.as_bytes()[at - 1].is_ascii_alphanumeric() && s.as_bytes()[at - 1] != b'_' && s.as_bytes()[at - 1] != b'-';
        let rest = s[at + key.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            return quoted_strings(rest[1..].trim_start()).into_iter().next();
        }
        from = at + key.len();
    }
    None
}

// ---------------------------------------------------------------- tiny JSON reader (package.json)

#[derive(Debug, Clone)]
enum Jv {
    Null,
    Str(String),
    Arr(Vec<Jv>),
    Obj(Vec<(String, Jv)>),
    Other,
}
impl Jv {
    fn get(&self, k: &str) -> Option<&Jv> {
        match self {
            Jv::Obj(v) => v.iter().find(|(a, _)| a == k).map(|(_, b)| b),
            _ => None,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Jv::Str(s) => Some(s),
            _ => None,
        }
    }
}
struct JP<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}
impl JP<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn val(&mut self) -> Option<Jv> {
        self.ws();
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let r = match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    match *self.b.get(self.i)? {
                        b'}' => {
                            self.i += 1;
                            break;
                        }
                        b',' => self.i += 1,
                        _ => {
                            let Jv::Str(k) = self.val()? else { return None };
                            self.ws();
                            if *self.b.get(self.i)? != b':' {
                                return None;
                            }
                            self.i += 1;
                            v.push((k, self.val()?));
                        }
                    }
                }
                Jv::Obj(v)
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    match *self.b.get(self.i)? {
                        b']' => {
                            self.i += 1;
                            break;
                        }
                        b',' => self.i += 1,
                        _ => v.push(self.val()?),
                    }
                }
                Jv::Arr(v)
            }
            b'"' => {
                self.i += 1;
                let mut s = Vec::new();
                loop {
                    match *self.b.get(self.i)? {
                        b'"' => {
                            self.i += 1;
                            break;
                        }
                        b'\\' => {
                            self.i += 1;
                            let c = *self.b.get(self.i)?;
                            s.push(match c {
                                b'n' => b'\n',
                                b't' => b'\t',
                                b'u' => {
                                    self.i += 4;
                                    b'?'
                                }
                                c => c,
                            });
                            self.i += 1;
                        }
                        c => {
                            s.push(c);
                            self.i += 1;
                        }
                    }
                }
                Jv::Str(String::from_utf8_lossy(&s).into_owned())
            }
            b'n' => {
                self.i += 4;
                Jv::Null
            }
            _ => {
                while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b'}' | b']') && !self.b[self.i].is_ascii_whitespace() {
                    self.i += 1;
                }
                Jv::Other
            }
        };
        self.depth -= 1;
        Some(r)
    }
}
fn parse_json(s: &str) -> Option<Jv> {
    JP {
        b: s.as_bytes(),
        i: 0,
        depth: 0,
    }
    .val()
}

// ---------------------------------------------------------------- manifests

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Kind {
    Cargo,
    Npm,
    Go,
    Py,
}

#[derive(Debug, Clone)]
struct Manifest {
    dir: String,
    file: String,
    kind: Kind,
    name: Option<String>,
    /// Workspace member patterns (relative to `dir`).
    members: Vec<String>,
    dep_names: Vec<String>,
    /// Directories of local path dependencies (already resolved against `dir`).
    dep_dirs: Vec<(String, String)>,
    /// Cargo `workspace = true` dependency names.
    ws_uses: Vec<String>,
    /// Cargo `[workspace.dependencies]`: name -> directory.
    ws_dirs: BTreeMap<String, String>,
}
impl Manifest {
    fn new(dir: &str, file: &str, kind: Kind) -> Self {
        Manifest {
            dir: dir.into(),
            file: file.into(),
            kind,
            name: None,
            members: vec![],
            dep_names: vec![],
            dep_dirs: vec![],
            ws_uses: vec![],
            ws_dirs: BTreeMap::new(),
        }
    }
}

fn parse_cargo(dir: &str, file: &str, text: &str) -> Manifest {
    let mut m = Manifest::new(dir, file, Kind::Cargo);
    let mut table = String::new();
    let mut sub: Option<String> = None;
    let mut collecting: Option<String> = None; // multi-line array buffer for workspace members
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if let Some(buf) = &mut collecting {
            buf.push_str(line);
            if line.contains(']') {
                m.members.extend(quoted_strings(buf));
                collecting = None;
            }
            continue;
        }
        if line.starts_with('[') {
            table = line.trim_matches(|c| c == '[' || c == ']').trim().to_owned();
            sub = None;
            if let Some((head, last)) = table.rsplit_once('.') {
                if head.ends_with("dependencies") {
                    sub = Some(last.trim_matches(|c| c == '"' || c == '\'').to_owned());
                    m.dep_names.push(sub.clone().unwrap());
                }
            }
            continue;
        }
        let is_deps = table.ends_with("dependencies");
        if table == "package" {
            if let Some(n) = kv_str(line, "name") {
                m.name = Some(n);
            }
        } else if table == "workspace" && line.starts_with("members") {
            let rest = line.split_once('=').map_or("", |x| x.1);
            if rest.contains(']') {
                m.members.extend(quoted_strings(rest));
            } else {
                collecting = Some(rest.to_owned());
            }
        } else if is_deps {
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim().trim_matches('"').to_owned();
                if k.is_empty() {
                    continue;
                }
                if table == "workspace.dependencies" {
                    if let Some(p) = kv_str(v, "path").and_then(|p| norm_join(dir, &p)) {
                        m.ws_dirs.insert(k.clone(), p);
                    }
                    continue;
                }
                m.dep_names.push(k.clone());
                if let Some(p) = kv_str(v, "path").and_then(|p| norm_join(dir, &p)) {
                    m.dep_dirs.push((k.clone(), p));
                }
                if v.replace(' ', "").contains("workspace=true") {
                    m.ws_uses.push(k);
                }
            }
        } else if let Some(s) = &sub {
            if let Some(p) = kv_str(line, "path").and_then(|p| norm_join(dir, &p)) {
                if table.starts_with("workspace.") {
                    m.ws_dirs.insert(s.clone(), p);
                } else {
                    m.dep_dirs.push((s.clone(), p));
                }
            }
            if line.replace(' ', "").starts_with("workspace=true") {
                m.ws_uses.push(s.clone());
            }
        }
    }
    m
}

fn parse_package_json(dir: &str, file: &str, text: &str) -> Option<Manifest> {
    let j = parse_json(text)?;
    let mut m = Manifest::new(dir, file, Kind::Npm);
    m.name = j.get("name").and_then(Jv::str).map(str::to_owned);
    for k in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
        if let Some(Jv::Obj(v)) = j.get(k) {
            for (n, val) in v {
                m.dep_names.push(n.clone());
                if let Some(p) = val.str().and_then(|s| s.strip_prefix("file:")).and_then(|p| norm_join(dir, p)) {
                    m.dep_dirs.push((n.clone(), p));
                }
            }
        }
    }
    let ws = match j.get("workspaces") {
        Some(Jv::Arr(a)) => Some(a.clone()),
        Some(o @ Jv::Obj(_)) => match o.get("packages") {
            Some(Jv::Arr(a)) => Some(a.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(a) = ws {
        m.members = a.iter().filter_map(|x| x.str().map(str::to_owned)).collect();
    }
    Some(m)
}

fn parse_go_mod(dir: &str, file: &str, text: &str) -> Manifest {
    let mut m = Manifest::new(dir, file, Kind::Go);
    let mut block: Option<&str> = None;
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(b) = block {
            if line == ")" {
                block = None;
                continue;
            }
            go_directive(&mut m, b, line, dir);
            continue;
        }
        let (kw, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        if rest == "(" {
            block = Some(match kw {
                "require" => "require",
                "replace" => "replace",
                _ => "other",
            });
        } else {
            go_directive(&mut m, kw, rest, dir);
        }
    }
    m
}
fn go_directive(m: &mut Manifest, kw: &str, rest: &str, dir: &str) {
    match kw {
        "module" => m.name = Some(rest.trim_matches('"').to_owned()),
        "require" => {
            if let Some(n) = rest.split_whitespace().next() {
                m.dep_names.push(n.to_owned());
            }
        }
        "replace" => {
            if let Some((l, r)) = rest.split_once("=>") {
                let target = r.split_whitespace().next().unwrap_or("");
                if target.starts_with('.') {
                    if let (Some(p), Some(n)) = (norm_join(dir, target), l.split_whitespace().next()) {
                        m.dep_dirs.push((n.to_owned(), p));
                    }
                }
            }
        }
        _ => {}
    }
}

fn py_norm(n: &str) -> String {
    n.to_ascii_lowercase().replace(['_', '.'], "-")
}
fn parse_pyproject(dir: &str, file: &str, text: &str) -> Manifest {
    let mut m = Manifest::new(dir, file, Kind::Py);
    let mut table = String::new();
    let mut in_deps_array = false;
    for raw in text.lines() {
        let line = strip_toml_comment(raw).trim();
        if line.starts_with('[') && !in_deps_array {
            table = line.trim_matches(|c| c == '[' || c == ']').trim().to_owned();
            continue;
        }
        let project_like = table == "project" || table == "tool.poetry";
        if project_like && m.name.is_none() {
            if let Some(n) = kv_str(line, "name") {
                m.name = Some(py_norm(&n));
            }
        }
        let poetry_deps = table.starts_with("tool.poetry") && table.ends_with("dependencies");
        if in_deps_array || (table == "project" && line.starts_with("dependencies")) {
            for s in quoted_strings(line) {
                let n: String = s
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                    .collect();
                if !n.is_empty() {
                    m.dep_names.push(py_norm(&n));
                }
            }
            in_deps_array = !line.contains(']');
        } else if poetry_deps || table.starts_with("tool.uv.sources") {
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim().trim_matches('"');
                if !k.is_empty() && k != "python" {
                    m.dep_names.push(py_norm(k));
                }
                if let Some(p) = kv_str(v, "path").and_then(|p| norm_join(dir, &p)) {
                    m.dep_dirs.push((py_norm(k), p));
                }
            }
        }
    }
    m
}

fn kind_of(file: &str) -> Option<Kind> {
    match base(file) {
        "Cargo.toml" => Some(Kind::Cargo),
        "package.json" => Some(Kind::Npm),
        "go.mod" => Some(Kind::Go),
        "pyproject.toml" => Some(Kind::Py),
        _ => None,
    }
}

const MAX_MANIFEST: usize = 1 << 20;

fn load_manifests(files: &Flat, read: Reader) -> (Vec<Manifest>, usize) {
    let mut out = Vec::new();
    let mut unreadable = 0;
    for (p, (_, id)) in files {
        let Some(kind) = kind_of(p) else { continue };
        if p.contains("/node_modules/") || p.starts_with("node_modules/") {
            continue;
        }
        let Some(text) = read(id).filter(|b| b.len() <= MAX_MANIFEST).and_then(utf8) else {
            unreadable += 1;
            continue;
        };
        let dir = dir_of(p);
        let m = match kind {
            Kind::Cargo => Some(parse_cargo(dir, p, &text)),
            Kind::Npm => parse_package_json(dir, p, &text),
            Kind::Go => Some(parse_go_mod(dir, p, &text)),
            Kind::Py => Some(parse_pyproject(dir, p, &text)),
        };
        match m {
            Some(m) => out.push(m),
            None => unreadable += 1,
        }
    }
    (out, unreadable)
}

// ---------------------------------------------------------------- suggest

#[derive(Clone, Debug)]
pub struct Signal {
    pub kind: String,
    pub detail: String,
    pub weight: f64,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub name: String,
    pub path: String,
    pub files: u64,
    pub bytes: Option<u64>,
    pub confidence: f64,
    pub signals: Vec<Signal>,
    /// Names of other suggested zones this one depends on (manifests, and imports when readable).
    pub depends_on: Vec<String>,
    /// Zones that change in the same commits (only with history analysis).
    pub coupled_with: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Coupling {
    pub a: String,
    pub b: String,
    pub both: u32,
    pub either: u32,
    pub jaccard: f64,
}

#[derive(Default, Debug)]
pub struct SuggestReport {
    pub zones: Vec<Suggestion>,
    /// Candidates below `min_confidence` that were not listed.
    pub hidden_low_confidence: usize,
    pub total_files: u64,
    pub root_files_remaining: u64,
    /// Groups of suggested zones that depend on each other (a cycle): consider keeping them together.
    pub cycles: Vec<Vec<String>>,
    pub coupling: Vec<Coupling>,
    /// Connected groups of zones with strong co-change (Jaccard >= 0.5).
    pub clusters: Vec<Vec<String>>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SuggestOpts {
    pub min_files: usize,
    pub min_confidence: f64,
    pub max_zones: usize,
}
impl Default for SuggestOpts {
    fn default() -> Self {
        SuggestOpts {
            min_files: 3,
            min_confidence: 0.4,
            max_zones: 100,
        }
    }
}

const CONTAINERS: &[&str] = &[
    "crates",
    "packages",
    "apps",
    "services",
    "libs",
    "modules",
    "projects",
    "components",
    "plugins",
    "cmd",
    "pkgs",
    "tools",
];
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "out",
    "__pycache__",
    "third_party",
];

#[derive(Default)]
struct Cand {
    sigs: Vec<Signal>,
    name: Option<String>,
}
fn sig(kind: &str, detail: impl Into<String>, weight: f64) -> Signal {
    Signal {
        kind: kind.into(),
        detail: detail.into(),
        weight,
    }
}

/// Parse CODEOWNERS text into `(pattern, owners)` rules in file order.
pub fn parse_codeowners(text: &str) -> Vec<(String, Vec<String>)> {
    text.lines()
        .filter_map(|l| {
            let l = l.split('#').next().unwrap_or("").trim();
            let mut it = l.split_whitespace();
            let pat = it.next()?;
            let owners: Vec<String> = it.map(str::to_owned).collect();
            Some((pat.to_owned(), owners))
        })
        .collect()
}
/// Does a CODEOWNERS (gitignore-like) pattern match this file path?
pub fn codeowners_match(pat: &str, path: &str) -> bool {
    let anchored = pat.starts_with('/') || pat.trim_end_matches('/').contains('/');
    let dir_only = pat.ends_with('/');
    let core = pat.trim_start_matches('/').trim_end_matches('/');
    if core.is_empty() {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    let pat_segs: Vec<&str> = core.split('/').collect();
    let n = pat_segs.len();
    let starts: Vec<usize> = if anchored { vec![0] } else { (0..segs.len()).collect() };
    for s in starts {
        // pattern equals segs[s..s+n] (a directory prefix, or the file itself when !dir_only)
        if s + n <= segs.len() && glob(&pat_segs, &segs[s..s + n]) && (s + n < segs.len() || !dir_only) {
            return true;
        }
    }
    false
}
/// Where a CODEOWNERS file may live.
pub const CODEOWNERS_PATHS: &[&str] = &["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"];

fn sanitize_name(s: &str) -> String {
    let s: String = s
        .trim_start_matches('@')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_owned();
    let s: String = s.chars().take(60).collect();
    if s.is_empty() {
        "zone".into()
    } else {
        s
    }
}

/// Propose zones for the root-owned part of `files`. `commits` (optional) is, per commit, the
/// repo-relative paths it changed; used only to annotate coupling between the proposed zones.
pub fn suggest(
    files: &Flat,
    zones: &Zones,
    read: Reader,
    size: Sizer,
    commits: Option<&[Vec<String>]>,
    opts: &SuggestOpts,
) -> SuggestReport {
    let mut rep = SuggestReport::default();
    let root_files: Vec<(&String, &ObjectId)> = files
        .iter()
        .filter(|(p, _)| zones.owner(p) == ROOT_ZONE)
        .map(|(p, (_, id))| (p, id))
        .collect();
    rep.total_files = files.len() as u64;
    rep.root_files_remaining = root_files.len() as u64;
    let (manifests, unreadable) = load_manifests(files, read);
    if unreadable > 0 {
        rep.notes.push(format!(
            "{unreadable} manifest file(s) could not be read or parsed and were ignored"
        ));
    }
    let mut cands: BTreeMap<String, Cand> = BTreeMap::new();
    let mut add = |path: &str, s: Signal, name: Option<String>| {
        if path.is_empty() || zones.owner(&format!("{path}/x")) != ROOT_ZONE {
            return;
        }
        let c = cands.entry(path.to_owned()).or_default();
        if !c.sigs.iter().any(|x| x.kind == s.kind) {
            c.sigs.push(s);
        }
        if c.name.is_none() {
            c.name = name;
        }
    };
    // Manifest dirs by kind, for workspace member expansion.
    let mut by_kind: HashMap<Kind, BTreeSet<String>> = HashMap::new();
    for m in &manifests {
        by_kind.entry(m.kind).or_default().insert(m.dir.clone());
    }
    let mut members_of_workspace: BTreeSet<(Kind, String)> = BTreeSet::new();
    for m in &manifests {
        if m.members.is_empty() {
            continue;
        }
        let (kname, lbl) = match m.kind {
            Kind::Cargo => ("cargo-workspace", "Cargo workspace member"),
            Kind::Npm => ("npm-workspaces", "package.json workspaces entry"),
            _ => continue,
        };
        for pat in &m.members {
            if pat.starts_with('!') {
                continue;
            }
            let Some(full) = norm_join(&m.dir, pat.trim_end_matches('/')) else {
                continue;
            };
            for d in &by_kind[&m.kind] {
                if !d.is_empty() && glob_path(&full, d) {
                    members_of_workspace.insert((m.kind, d.clone()));
                    let nm = manifests
                        .iter()
                        .find(|x| x.kind == m.kind && &x.dir == d)
                        .and_then(|x| x.name.clone());
                    add(d, sig(kname, format!("{lbl} ({}: \"{pat}\")", m.file), 0.85), nm);
                }
            }
        }
    }
    for m in &manifests {
        if m.dir.is_empty() || members_of_workspace.contains(&(m.kind, m.dir.clone())) {
            continue;
        }
        let (k, d, w) = match m.kind {
            Kind::Cargo if !m.members.is_empty() && m.name.is_none() => continue, // a pure workspace root
            Kind::Cargo => ("cargo-package", "standalone Cargo package", 0.6),
            Kind::Npm if !m.members.is_empty() => continue,
            Kind::Npm => ("npm-package", "standalone package.json", 0.6),
            Kind::Go => ("go-module", "go.mod (Go module)", 0.8),
            Kind::Py => ("python-project", "pyproject.toml (Python project)", 0.7),
        };
        add(&m.dir, sig(k, format!("{d}: {}", m.file), w), m.name.clone());
    }
    for (p, _) in &root_files {
        let b = base(p);
        let d = dir_of(p);
        let (k, w, what) = match b {
            "BUILD" | "BUILD.bazel" => ("bazel-package", 0.5, "Bazel BUILD file"),
            "pom.xml" => ("maven-module", 0.6, "Maven module"),
            "build.gradle" | "build.gradle.kts" => ("gradle-project", 0.6, "Gradle project"),
            _ => continue,
        };
        add(d, sig(k, format!("{what}: {p}"), w), None);
    }
    // CODEOWNERS: a rule naming a plain directory is a statement that the directory has owners.
    for co in CODEOWNERS_PATHS {
        let Some((_, id)) = files.get(*co) else { continue };
        let Some(text) = read(id).and_then(utf8) else { continue };
        for (pat, owners) in parse_codeowners(&text) {
            let core = pat.trim_start_matches('/').trim_end_matches('/');
            if core.is_empty() || core.contains(['*', '?', '[']) || owners.is_empty() {
                continue;
            }
            let prefix = format!("{core}/");
            if root_files.iter().any(|(p, _)| p.starts_with(&prefix)) {
                add(core, sig("codeowners", format!("{co}: {pat} -> {}", owners.join(" ")), 0.5), None);
            }
        }
    }
    // Directory structure.
    let mut tops: BTreeSet<String> = BTreeSet::new();
    for (p, _) in &root_files {
        let mut it = p.split('/');
        let (Some(a), Some(b)) = (it.next(), it.next()) else { continue };
        if a.starts_with('.') || SKIP_DIRS.contains(&a) {
            continue;
        }
        if CONTAINERS.contains(&a) {
            if it.next().is_some() || p.matches('/').count() >= 2 {
                tops.insert(format!("{a}/{b}"));
            }
        } else {
            tops.insert(a.to_owned());
        }
    }
    for t in &tops {
        let detail = match t.split_once('/') {
            Some((c, _)) => format!("child of container directory {c}/"),
            None => "top-level directory".to_owned(),
        };
        add(t, sig("directory", detail, 0.3), None);
    }
    // Score, measure, resolve overlaps.
    struct Scored {
        path: String,
        name: Option<String>,
        sigs: Vec<Signal>,
        conf: f64,
        files: u64,
        bytes: Option<u64>,
    }
    let mut scored: Vec<Scored> = Vec::new();
    let mut too_small = 0usize;
    for (path, c) in cands {
        let mut nfiles = 0u64;
        let mut bytes = Some(0u64);
        for (p, id) in &root_files {
            if is_under(p, &path) {
                nfiles += 1;
                bytes = match (bytes, size(id)) {
                    (Some(b), Some(s)) => Some(b + s),
                    _ => None,
                };
            }
        }
        if (nfiles as usize) < opts.min_files {
            too_small += 1;
            continue;
        }
        let conf = 1.0 - c.sigs.iter().fold(1.0, |a, s| a * (1.0 - s.weight));
        scored.push(Scored {
            path,
            name: c.name,
            sigs: c.sigs,
            conf: conf.min(0.99),
            files: nfiles,
            bytes,
        });
    }
    if too_small > 0 {
        rep.notes.push(format!(
            "{too_small} candidate(s) with fewer than {} files were dropped",
            opts.min_files
        ));
    }
    scored.sort_by(|a, b| {
        b.conf
            .partial_cmp(&a.conf)
            .unwrap()
            .then(a.path.len().cmp(&b.path.len()))
            .then(a.path.cmp(&b.path))
    });
    let mut accepted: Vec<Scored> = Vec::new();
    for s in scored {
        if let Some(o) = accepted.iter().find(|a| is_under(&a.path, &s.path) || is_under(&s.path, &a.path)) {
            rep.notes.push(format!(
                "dropped candidate {}/ ({:.2}): overlaps {}/ ({:.2}); zones may not overlap",
                s.path, s.conf, o.path, o.conf
            ));
            continue;
        }
        accepted.push(s);
    }
    let mut hidden = 0;
    accepted.retain(|s| {
        let keep = s.conf >= opts.min_confidence;
        if !keep {
            hidden += 1;
        }
        keep
    });
    rep.hidden_low_confidence = hidden;
    if accepted.len() > opts.max_zones {
        rep.notes.push(format!(
            "{} more candidate(s) beyond --max-zones {} were cut",
            accepted.len() - opts.max_zones,
            opts.max_zones
        ));
        accepted.truncate(opts.max_zones);
    }
    accepted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut used: BTreeSet<String> = zones.names().map(str::to_owned).collect();
    for s in &accepted {
        let mut name = sanitize_name(
            s.name
                .as_deref()
                .map_or_else(|| base(&s.path), |n| n.rsplit('/').next().unwrap_or(n)),
        );
        if used.contains(&name) {
            let alt = sanitize_name(&s.path.replace('/', "-"));
            name = if used.contains(&alt) { format!("{alt}-2") } else { alt };
        }
        used.insert(name.clone());
        rep.zones.push(Suggestion {
            name,
            path: s.path.clone(),
            files: s.files,
            bytes: s.bytes,
            confidence: s.conf,
            signals: s.sigs.clone(),
            depends_on: vec![],
            coupled_with: vec![],
        });
    }
    rep.root_files_remaining = root_files
        .iter()
        .filter(|(p, _)| !rep.zones.iter().any(|z| is_under(p, &z.path)))
        .count() as u64;
    // Dependencies between the proposed zones, computed against the proposed layout.
    if !rep.zones.is_empty() {
        let mut hypo = zones.clone();
        let mut ok = true;
        for z in &rep.zones {
            if hypo.create(&z.name, &z.path).is_err() {
                ok = false;
            }
        }
        if ok {
            let deps = dependencies(files, &hypo, read, false);
            let names: Vec<String> = rep.zones.iter().map(|z| z.name.clone()).collect();
            for z in &mut rep.zones {
                for (a, b) in deps.edges.keys() {
                    if *a == z.name && names.contains(b) {
                        z.depends_on.push(b.clone());
                    }
                }
            }
            rep.cycles = deps.cycles_among(&names);
        }
        if let Some(commits) = commits {
            annotate_coupling(&mut rep, commits);
        }
    }
    rep
}

fn annotate_coupling(rep: &mut SuggestReport, commits: &[Vec<String>]) {
    let zs: Vec<(String, String)> = rep.zones.iter().map(|z| (z.name.clone(), z.path.clone())).collect();
    let mut single: BTreeMap<usize, u32> = BTreeMap::new();
    let mut pair: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    for c in commits {
        let mut hit: BTreeSet<usize> = BTreeSet::new();
        for p in c {
            if let Some(i) = zs.iter().position(|(_, zp)| is_under(p, zp)) {
                hit.insert(i);
            }
        }
        for &i in &hit {
            *single.entry(i).or_default() += 1;
        }
        let h: Vec<usize> = hit.into_iter().collect();
        for (x, &i) in h.iter().enumerate() {
            for &j in &h[x + 1..] {
                *pair.entry((i, j)).or_default() += 1;
            }
        }
    }
    let mut parent: Vec<usize> = (0..zs.len()).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        if p[x] != x {
            let r = find(p, p[x]);
            p[x] = r;
        }
        p[x]
    }
    for (&(i, j), &both) in &pair {
        let either = single[&i] + single[&j] - both;
        let jac = both as f64 / either as f64;
        if both >= 3 && jac >= 0.3 {
            rep.coupling.push(Coupling {
                a: zs[i].0.clone(),
                b: zs[j].0.clone(),
                both,
                either,
                jaccard: jac,
            });
            rep.zones[i].coupled_with.push(zs[j].0.clone());
            rep.zones[j].coupled_with.push(zs[i].0.clone());
            if jac >= 0.5 {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    rep.coupling.sort_by(|a, b| b.jaccard.partial_cmp(&a.jaccard).unwrap());
    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (i, z) in zs.iter().enumerate() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(z.0.clone());
    }
    rep.clusters = groups.into_values().filter(|g| g.len() > 1).collect();
    rep.notes
        .push(format!("co-change analysed over {} commit(s) of the root zone", commits.len()));
}

// ---------------------------------------------------------------- dependencies between zones

/// Zone-to-zone dependency edges (`from` needs `to`) with human-readable reasons.
#[derive(Default, Debug, Clone)]
pub struct Deps {
    pub edges: BTreeMap<(String, String), Vec<String>>,
    pub notes: Vec<String>,
}
impl Deps {
    pub fn out_of(&self, z: &str) -> Vec<(&str, &[String])> {
        self.edges
            .iter()
            .filter(|((a, _), _)| a == z)
            .map(|((_, b), r)| (b.as_str(), r.as_slice()))
            .collect()
    }
    fn add(&mut self, from: &str, to: &str, reason: String) {
        if from == to {
            return;
        }
        let r = self.edges.entry((from.to_owned(), to.to_owned())).or_default();
        if r.len() < 4 && !r.contains(&reason) {
            r.push(reason);
        }
    }
    /// Strongly connected groups (size > 1) among `names`.
    pub fn cycles_among(&self, names: &[String]) -> Vec<Vec<String>> {
        // Reachability is enough for the small graphs here: a and b are in a cycle iff each reaches the other.
        let reach = |s: &str| -> BTreeSet<String> {
            let mut seen = BTreeSet::new();
            let mut q = VecDeque::from([s.to_owned()]);
            while let Some(x) = q.pop_front() {
                for (y, _) in self.out_of(&x) {
                    if seen.insert(y.to_owned()) {
                        q.push_back(y.to_owned());
                    }
                }
            }
            seen
        };
        let rs: BTreeMap<&String, BTreeSet<String>> = names.iter().map(|n| (n, reach(n))).collect();
        let mut done: BTreeSet<String> = BTreeSet::new();
        let mut out = Vec::new();
        for n in names {
            if done.contains(n.as_str()) || !rs[n].contains(n) {
                continue;
            }
            let g: Vec<String> = names.iter().filter(|m| rs[n].contains(*m) && rs[*m].contains(n)).cloned().collect();
            for m in &g {
                done.insert(m.clone());
            }
            if g.len() > 1 {
                out.push(g);
            }
        }
        out
    }
}

fn zone_at(zones: &Zones, dir: &str) -> String {
    zones
        .owner(&if dir.is_empty() { "x".to_owned() } else { format!("{dir}/x") })
        .to_owned()
}

/// Compute the dependency edges between the zones of `zones` over `files`. Manifests are always
/// read (Cargo path deps and `workspace = true`, package.json deps and `file:` deps, go.mod
/// require/replace, pyproject deps). With `imports`, source files that are readable are scanned
/// for Go, Python and JS/TS imports (this reads many files; unreadable ones are counted, not guessed).
pub fn dependencies(files: &Flat, zones: &Zones, read: Reader, imports: bool) -> Deps {
    let mut d = Deps::default();
    let (manifests, unreadable) = load_manifests(files, read);
    if unreadable > 0 {
        d.notes.push(format!(
            "{unreadable} manifest(s) unreadable (not downloaded or unparsable): their dependencies are unknown"
        ));
    }
    // name -> dir per ecosystem
    let mut names: HashMap<(Kind, String), String> = HashMap::new();
    for m in &manifests {
        if let Some(n) = &m.name {
            names.entry((m.kind, n.clone())).or_insert_with(|| m.dir.clone());
        }
    }
    let ws_roots: Vec<&Manifest> = manifests
        .iter()
        .filter(|m| m.kind == Kind::Cargo && !m.ws_dirs.is_empty())
        .collect();
    for m in &manifests {
        let from = zone_at(zones, &m.dir);
        for (n, dir) in &m.dep_dirs {
            d.add(&from, &zone_at(zones, dir), format!("{}: path dependency {n} -> {dir}/", m.file));
        }
        if m.kind == Kind::Cargo {
            for n in &m.ws_uses {
                let hit = ws_roots
                    .iter()
                    .filter(|w| is_under(&m.dir, &w.dir))
                    .filter_map(|w| w.ws_dirs.get(n))
                    .next();
                if let Some(dir) = hit {
                    d.add(
                        &from,
                        &zone_at(zones, dir),
                        format!("{}: workspace dependency {n} -> {dir}/", m.file),
                    );
                }
            }
        }
        for n in &m.dep_names {
            if let Some(dir) = names.get(&(m.kind, n.clone())) {
                if *dir != m.dir {
                    let what = match m.kind {
                        Kind::Go => "requires module",
                        Kind::Npm => "depends on workspace package",
                        Kind::Py => "depends on project",
                        Kind::Cargo => "depends on crate",
                    };
                    d.add(&from, &zone_at(zones, dir), format!("{}: {what} {n} ({dir}/)", m.file));
                }
            }
        }
    }
    if imports {
        scan_imports(files, zones, read, &manifests, &mut d);
    }
    d
}

fn scan_imports(files: &Flat, zones: &Zones, read: Reader, manifests: &[Manifest], d: &mut Deps) {
    // Python top-level names -> zones.
    let mut py_tops: HashMap<String, BTreeSet<String>> = HashMap::new();
    for p in files.keys() {
        if let Some(parent) = p.strip_suffix("/__init__.py") {
            py_tops
                .entry(base(parent).to_owned())
                .or_default()
                .insert(zone_at(zones, dir_of(p)));
        } else if let Some(stem) = p.strip_suffix(".py") {
            py_tops.entry(base(stem).to_owned()).or_default().insert(zone_at(zones, dir_of(p)));
        }
    }
    let gomods: Vec<(&str, &str)> = manifests
        .iter()
        .filter(|m| m.kind == Kind::Go)
        .filter_map(|m| m.name.as_deref().map(|n| (n, m.dir.as_str())))
        .collect();
    let npm: HashMap<&str, &str> = manifests
        .iter()
        .filter(|m| m.kind == Kind::Npm)
        .filter_map(|m| m.name.as_deref().map(|n| (n, m.dir.as_str())))
        .collect();
    let (mut scanned, mut unread) = (0usize, 0usize);
    for (p, (_, id)) in files {
        let ext = p.rsplit('.').next().unwrap_or("");
        if !matches!(ext, "go" | "py" | "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs") || p.contains("/node_modules/") {
            continue;
        }
        let Some(text) = read(id).filter(|b| b.len() <= (2 << 20)).and_then(utf8) else {
            unread += 1;
            continue;
        };
        scanned += 1;
        let from = zone_at(zones, dir_of(p));
        match ext {
            "go" => {
                let mut in_block = false;
                for l in text.lines() {
                    let l = l.trim();
                    let spec = if in_block {
                        if l == ")" {
                            in_block = false;
                            continue;
                        }
                        quoted_strings(l).into_iter().next()
                    } else if l.starts_with("import (") {
                        in_block = true;
                        continue;
                    } else if let Some(r) = l.strip_prefix("import ") {
                        quoted_strings(r).into_iter().next()
                    } else {
                        None
                    };
                    let Some(spec) = spec else { continue };
                    // longest matching module path
                    if let Some((m, dir)) = gomods
                        .iter()
                        .filter(|(m, _)| spec == *m || spec.starts_with(&format!("{m}/")))
                        .max_by_key(|(m, _)| m.len())
                    {
                        let rest = spec[m.len()..].trim_start_matches('/');
                        let target = norm_join(dir, rest).unwrap_or_default();
                        d.add(&from, &zone_at(zones, &target), format!("{p}: imports {spec}"));
                    }
                }
            }
            "py" => {
                for l in text.lines() {
                    let l = l.trim_start();
                    let m = l.strip_prefix("from ").or_else(|| l.strip_prefix("import "));
                    let Some(m) = m else { continue };
                    if m.starts_with('.') {
                        continue;
                    }
                    let top: String = m.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if let Some(zs) = py_tops.get(&top) {
                        if zs.len() == 1 {
                            d.add(&from, zs.iter().next().unwrap(), format!("{p}: imports {top}"));
                        }
                    }
                }
            }
            _ => {
                for l in text.lines() {
                    for q in quoted_strings(l) {
                        if !(l.contains("import") || l.contains("require")) || l.trim_start().starts_with("//") {
                            continue;
                        }
                        if q.starts_with('.') {
                            if let Some(t) = norm_join(dir_of(p), &q) {
                                if files
                                    .keys()
                                    .any(|f| f == &t || f.starts_with(&format!("{t}/")) || f.starts_with(&format!("{t}.")))
                                {
                                    d.add(&from, &zone_at(zones, dir_of(&t)), format!("{p}: imports {q}"));
                                }
                            }
                        } else {
                            let pkg: String = if q.starts_with('@') {
                                q.splitn(3, '/').take(2).collect::<Vec<_>>().join("/")
                            } else {
                                q.split('/').next().unwrap_or("").to_owned()
                            };
                            if let Some(dir) = npm.get(pkg.as_str()) {
                                d.add(&from, &zone_at(zones, dir), format!("{p}: imports {q}"));
                            }
                        }
                    }
                }
            }
        }
    }
    if unread > 0 {
        d.notes.push(format!(
            "import scan: {scanned} source file(s) read, {unread} not readable (not downloaded?); their imports are unknown"
        ));
    } else {
        d.notes
            .push(format!("import scan: {scanned} source file(s) read (Go, Python, JS/TS only)"));
    }
}

// ---------------------------------------------------------------- closure

#[derive(Clone, Debug)]
pub struct Seed {
    pub zone: String,
    pub why: String,
}
#[derive(Clone, Debug)]
pub struct Included {
    pub zone: String,
    pub reasons: Vec<String>,
    /// True when requested directly, false when pulled in as a dependency.
    pub direct: bool,
    pub depth: u32,
}

/// Seed zones plus everything they depend on (BFS; cycles terminate; `max_depth` limits hops).
pub fn closure(seeds: &[Seed], deps: &Deps, follow: bool, max_depth: Option<u32>) -> Vec<Included> {
    let mut out: Vec<Included> = Vec::new();
    let mut q: VecDeque<usize> = VecDeque::new();
    for s in seeds {
        match out.iter_mut().find(|i| i.zone == s.zone) {
            Some(i) => i.reasons.push(s.why.clone()),
            None => {
                out.push(Included {
                    zone: s.zone.clone(),
                    reasons: vec![s.why.clone()],
                    direct: true,
                    depth: 0,
                });
                q.push_back(out.len() - 1);
            }
        }
    }
    if !follow {
        q.clear();
    }
    while let Some(i) = q.pop_front() {
        let (z, depth) = (out[i].zone.clone(), out[i].depth);
        if max_depth.is_some_and(|m| depth >= m) {
            continue;
        }
        for (to, why) in deps.out_of(&z) {
            let reason = format!("dependency of {z}: {}", why.first().map_or("", String::as_str));
            match out.iter_mut().find(|x| x.zone == to) {
                Some(x) => {
                    if !x.reasons.contains(&reason) && x.reasons.len() < 6 {
                        x.reasons.push(reason);
                    }
                }
                None => {
                    out.push(Included {
                        zone: to.to_owned(),
                        reasons: vec![reason],
                        direct: false,
                        depth: depth + 1,
                    });
                    q.push_back(out.len() - 1);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- query resolution

#[derive(Clone, Debug)]
pub struct Match {
    pub zone: String,
    pub query: String,
    pub how: String,
}

#[derive(Default, Debug)]
pub struct Resolved {
    pub matches: Vec<Match>,
    /// `(query, why nothing matched)`; never silently widened to "everything".
    pub unresolved: Vec<(String, String)>,
    pub notes: Vec<String>,
}

const DEF_KEYWORDS: &[&str] = &[
    "fn",
    "def",
    "class",
    "func",
    "function",
    "struct",
    "enum",
    "trait",
    "type",
    "interface",
    "const",
    "impl",
    "mod",
    "module",
    "object",
];
const CODE_EXT: &[&str] = &[
    "rs", "go", "py", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "swift", "scala", "php",
];

fn defines(text: &str, sym: &str) -> bool {
    for l in text.lines() {
        let l = l.trim_start();
        let mut toks = l.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|t| !t.is_empty());
        let mut prev = "";
        for t in toks.by_ref().take(8) {
            if t == sym && DEF_KEYWORDS.contains(&prev) {
                return true;
            }
            prev = t;
        }
    }
    false
}

/// Map one query to zones: an exact file or directory, a zone name, a file-name/module match,
/// and (with `content`) a definition (`fn x`, `def x`, `class x`...) in a readable file.
pub fn resolve_queries(queries: &[String], files: &Flat, zones: &Zones, read: Reader, content: bool) -> Resolved {
    let mut r = Resolved::default();
    let mut unreadable = 0usize;
    for q0 in queries {
        let q = q0.trim().trim_start_matches("./").trim_end_matches('/');
        let mut found: Vec<Match> = Vec::new();
        let push = |found: &mut Vec<Match>, z: &str, how: String| {
            if !found.iter().any(|m| m.zone == z) {
                found.push(Match {
                    zone: z.to_owned(),
                    query: q0.clone(),
                    how,
                });
            }
        };
        if q.is_empty() {
            r.unresolved.push((q0.clone(), "empty query".into()));
            continue;
        }
        if files.contains_key(q) {
            push(&mut found, zones.owner(q), format!("file {q}"));
        } else {
            let pre = format!("{q}/");
            let mut n = 0;
            for p in files.keys().filter(|p| p.starts_with(&pre)) {
                n += 1;
                push(&mut found, zones.owner(p), format!("directory {q}/ ({n}+ file(s))"));
            }
        }
        if found.is_empty() {
            if let Some(z) = zones.resolve(q) {
                push(&mut found, &z, format!("zone name {q}"));
            }
        }
        if found.is_empty() {
            // A zone path that holds no files at the head still names the zone.
            let zp = zones
                .names()
                .find(|n| zones.path_of(n) == Some(q) && !q.is_empty())
                .map(str::to_owned);
            if let Some(z) = zp {
                push(&mut found, &z, format!("zone path {q}"));
            }
        }
        if found.is_empty() {
            // symbol-ish: a::b::c, a.b.c, pkg/mod; match on file stems and path components
            let parts: Vec<&str> = q.split([':', '.', '/']).filter(|s| !s.is_empty()).collect();
            let last = parts.last().copied().unwrap_or(q);
            let as_path = parts.join("/");
            for p in files.keys() {
                let stem = base(p).rsplit_once('.').map_or(base(p), |x| x.0);
                if stem.eq_ignore_ascii_case(last)
                    || p.contains(&format!("/{as_path}."))
                    || p.starts_with(&format!("{as_path}."))
                    || p.contains(&format!("/{as_path}/"))
                    || p.starts_with(&format!("{as_path}/"))
                {
                    push(&mut found, zones.owner(p), format!("file/module name match: {p}"));
                }
            }
        }
        if found.is_empty() && content {
            let sym = q.rsplit([':', '.']).next().unwrap_or(q);
            for (p, (_, id)) in files {
                let ext = p.rsplit('.').next().unwrap_or("");
                if !CODE_EXT.contains(&ext) {
                    continue;
                }
                match read(id).filter(|b| b.len() <= (2 << 20)).and_then(utf8) {
                    Some(t) => {
                        if defines(&t, sym) {
                            push(&mut found, zones.owner(p), format!("definition of `{sym}` in {p}"));
                        }
                    }
                    None => unreadable += 1,
                }
            }
        }
        if found.is_empty() {
            let why = if content {
                "no file, directory, zone, file name or readable definition matched".to_owned()
            } else {
                "no file, directory, zone or file name matched (definitions are only searched in files that are available locally)"
                    .to_owned()
            };
            r.unresolved.push((q0.clone(), why));
        }
        r.matches.extend(found);
    }
    if unreadable > 0 {
        r.notes.push(format!(
            "symbol search skipped {unreadable} file(s) whose content is not available locally"
        ));
    }
    r
}

const STOP: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "into",
    "when",
    "make",
    "add",
    "fix",
    "update",
    "change",
    "should",
    "need",
    "needs",
    "please",
    "code",
    "file",
    "files",
    "use",
    "using",
    "not",
    "are",
    "can",
    "all",
    "new",
    "bug",
    "issue",
    "feature",
    "implement",
    "support",
    "work",
    "working",
    "test",
    "tests",
    "was",
    "has",
    "have",
    "does",
    "doesn",
    "about",
    "than",
    "then",
    "our",
    "its",
    "get",
    "set",
];

/// Offline keyword mapping of a task description to zones. Deterministic, no model: tokens
/// (>= 3 chars, minus stop words) are matched against zone names, zone paths and file stems.
pub fn map_task(text: &str, files: &Flat, zones: &Zones) -> Resolved {
    let mut r = Resolved::default();
    let mut terms: Vec<String> = Vec::new();
    for t in text.split(|c: char| !c.is_alphanumeric()) {
        // split camelCase too
        let mut cur = String::new();
        let mut parts = vec![];
        for c in t.chars() {
            if c.is_uppercase() && !cur.is_empty() && cur.chars().last().is_some_and(|x| x.is_lowercase()) {
                parts.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        }
        parts.push(cur);
        for p in parts {
            let p = p.to_lowercase();
            if p.len() >= 3 && !STOP.contains(&p.as_str()) && !terms.contains(&p) {
                terms.push(p);
            }
        }
    }
    if terms.is_empty() {
        r.unresolved.push((text.to_owned(), "no usable keywords in the task text".into()));
        return r;
    }
    let mut score: BTreeMap<String, (u32, Vec<String>)> = BTreeMap::new();
    for z in zones.names() {
        let zp = zones.path_of(z).unwrap_or("").to_lowercase();
        let label = zones.label(z).to_lowercase();
        let comps: Vec<&str> = zp.split(['/', '-', '_', '.']).collect();
        for t in &terms {
            let mut s = 0;
            if *t == label || label.split(['-', '_', '.']).any(|x| x == t) {
                s += 3;
            }
            if comps.iter().any(|c| c == t) {
                s += 3;
            }
            if s > 0 {
                let e = score.entry(z.to_owned()).or_default();
                e.0 += s;
                e.1.push(format!("`{t}` names the zone"));
            }
        }
    }
    for p in files.keys() {
        let low = p.to_lowercase();
        let stem = base(&low).rsplit_once('.').map_or(base(&low), |x| x.0).to_owned();
        let parts: Vec<&str> = stem.split(['-', '_', '.']).collect();
        for t in &terms {
            if stem == *t || parts.contains(&t.as_str()) {
                let e = score.entry(zones.owner(p).to_owned()).or_default();
                if e.1.len() < 12 {
                    e.0 += 2;
                    e.1.push(format!("`{t}` matches file {p}"));
                }
            }
        }
    }
    let top = score.values().map(|v| v.0).max().unwrap_or(0);
    if top < 2 {
        r.unresolved.push((
            text.to_owned(),
            format!("none of the keywords {terms:?} matched a zone name, zone path or file name"),
        ));
        return r;
    }
    let mut ranked: Vec<_> = score
        .into_iter()
        .filter(|(_, v)| v.0 as f64 >= 0.4 * top as f64 && v.0 >= 2)
        .collect();
    ranked.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    ranked.truncate(5);
    for (z, (s, why)) in ranked {
        let mut w = why;
        w.truncate(3);
        r.matches.push(Match {
            zone: z,
            query: text.to_owned(),
            how: format!("task keywords (score {s}): {}", w.join("; ")),
        });
    }
    r.notes
        .push("task text mapped by keyword matching only; a proposal, not an understanding of the task".into());
    r
}

// ---------------------------------------------------------------- owners

fn owner_is(token: &str, user: &str) -> bool {
    let t = token.trim_start_matches('@').to_lowercase();
    let u = user.trim_start_matches('@').to_lowercase();
    t == u
}

/// Zones holding files whose CODEOWNERS rule (last matching rule wins) lists `user`.
pub fn zones_for_owner(user: &str, files: &Flat, zones: &Zones, read: Reader) -> Vec<Match> {
    let mut rules = Vec::new();
    let mut src = "";
    for co in CODEOWNERS_PATHS {
        if let Some((_, id)) = files.get(*co) {
            if let Some(t) = read(id).and_then(utf8) {
                rules = parse_codeowners(&t);
                src = co;
                break;
            }
        }
    }
    let mut per: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut total: BTreeMap<String, u64> = BTreeMap::new();
    for p in files.keys() {
        let z = zones.owner(p).to_owned();
        *total.entry(z.clone()).or_default() += 1;
        if let Some((pat, owners)) = rules.iter().rev().find(|(pat, _)| codeowners_match(pat, p)) {
            if owners.iter().any(|o| owner_is(o, user)) {
                let e = per.entry(z).or_insert((0, pat.clone()));
                e.0 += 1;
            }
        }
    }
    per.into_iter()
        .map(|(z, (n, pat))| Match {
            how: format!("{src}: `{pat}` lists {user} for {n} of {} file(s)", total[&z]),
            zone: z,
            query: format!("owner:{user}"),
        })
        .collect()
}

/// Does a commit author (name/email) denote `user`?
pub fn author_is(name: &str, email: &str, user: &str) -> bool {
    let u = user.trim_start_matches('@').to_lowercase();
    name.to_lowercase() == u || email.to_lowercase() == u || email.split('@').next().map(str::to_lowercase).as_deref() == Some(u.as_str())
}

/// Zone-level change statistics -> zones where `user` authored enough of the sampled commits.
pub fn zones_by_authorship(user: &str, stats: &BTreeMap<String, (u32, u32)>, min_commits: u32, min_share: f64) -> Vec<Match> {
    stats
        .iter()
        .filter(|(_, (mine, total))| *mine >= min_commits && *total > 0 && *mine as f64 / *total as f64 >= min_share)
        .map(|(z, (mine, total))| Match {
            zone: z.clone(),
            query: format!("owner:{user}"),
            how: format!(
                "authored {mine} of the last {total} commit(s) of the zone ({:.0}%)",
                100.0 * *mine as f64 / *total as f64
            ),
        })
        .collect()
}

/// Files and (known) bytes per zone over `files`.
pub fn zone_stats(files: &Flat, zones: &Zones, size: Sizer) -> BTreeMap<String, (u64, Option<u64>)> {
    let mut m: BTreeMap<String, (u64, Option<u64>)> = zones.names().map(|n| (n.to_owned(), (0, Some(0)))).collect();
    for (p, (_, id)) in files {
        let e = m.entry(zones.owner(p).to_owned()).or_insert((0, Some(0)));
        e.0 += 1;
        e.1 = match (e.1, size(id)) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob_path("crates/*", "crates/a"));
        assert!(!glob_path("crates/*", "crates/a/b"));
        assert!(glob_path("packages/**", "packages/a/b"));
        assert!(glob_path("a/b", "a/b"));
        assert!(codeowners_match("/docs/", "docs/x/y.md"));
        assert!(codeowners_match("*.rs", "a/b/c.rs"));
        assert!(!codeowners_match("/a/", "b/a/x"));
        assert!(codeowners_match("a/", "b/a/x"));
    }
    #[test]
    fn cargo_parse() {
        let m = parse_cargo(
            "crates/a",
            "crates/a/Cargo.toml",
            "[package]\nname = \"a\"\n[dependencies]\nb = { path = \"../b\", version = \"1\" }\nserde = \"1\"\nw.workspace = true\n[dependencies.c]\npath = \"../c\"\n",
        );
        assert_eq!(m.name.as_deref(), Some("a"));
        let dirs: Vec<_> = m.dep_dirs.iter().map(|x| x.1.as_str()).collect();
        assert_eq!(dirs, ["crates/b", "crates/c"]);
        let w = parse_cargo(
            "",
            "Cargo.toml",
            "[workspace]\nmembers = [\n  \"crates/*\", # x\n  \"tools/t\",\n]\n[workspace.dependencies]\nz = { path = \"crates/z\" }\n",
        );
        assert_eq!(w.members, ["crates/*", "tools/t"]);
        assert_eq!(w.ws_dirs["z"], "crates/z");
    }
    #[test]
    fn go_and_json() {
        let g = parse_go_mod(
            "svc/a",
            "svc/a/go.mod",
            "module ex.com/a\nrequire (\n  ex.com/b v1.0.0\n  github.com/x/y v1\n)\nreplace ex.com/b => ../b\n",
        );
        assert_eq!(g.name.as_deref(), Some("ex.com/a"));
        assert!(g.dep_names.contains(&"ex.com/b".to_owned()));
        assert_eq!(g.dep_dirs[0].1, "svc/b");
        let p = parse_package_json(
            "",
            "package.json",
            r#"{"name":"r","workspaces":{"packages":["pkgs/*"]},"dependencies":{"a":"1"}}"#,
        )
        .unwrap();
        assert_eq!(p.members, ["pkgs/*"]);
        assert_eq!(p.dep_names, ["a"]);
        assert!(parse_package_json("", "package.json", "{not json").is_none());
    }
    #[test]
    fn defs() {
        assert!(defines("pub async fn parse_config(x: u8) {}", "parse_config"));
        assert!(defines("class Foo(Base):", "Foo"));
        assert!(!defines("let x = parse_config();", "parse_config"));
    }
}
