//! `vcs zone describe` and `vcs zone graph`: the zone ontology, a derived, read-only view of the
//! repository for tools and agents. Everything printed is computed from real repository state
//! (zone definitions, refs, workspace, commits, trees, operation log, held locks); nothing is
//! stored or invented. Schema and field provenance: docs/zone-ontology.md and
//! docs/zone-ontology.schema.json.

use crate::json::{doc, J};
use crate::{Cmd, ZoneCmd};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use vcs_objects::ObjectId;
use vcs_repo::meta::Zones;
use vcs_repo::treeops::{self, Flat};
use vcs_repo::zonedetect as zd;
use vcs_repo::{Error, Repo, ROOT_ZONE};

/// Schema identifier carried in every ontology document.
pub const SCHEMA: &str = "vcs.zone-ontology/1";

#[derive(Args, Clone)]
pub struct DescribeArgs {
    /// Describe only this zone (display name or id); default: every zone
    pub zone: Option<String>,
    /// Recent commits listed per zone (each with its changed files)
    #[arg(long, default_value_t = 5)]
    pub history: usize,
    /// Recent operations (local transaction log) listed
    #[arg(long, default_value_t = 10)]
    pub transactions: usize,
    /// Report which zone owns this repo-relative path (repeatable)
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<String>,
    /// Keep only these keys of each zone object (comma separated, e.g. `purpose,history`); `id` and `name`
    /// are always kept. Smaller output for agents; the schema's per-zone `required` list then does not apply.
    #[arg(long, value_delimiter = ',', value_name = "KEYS")]
    pub fields: Vec<String>,
}

#[derive(Args, Clone)]
pub struct GraphArgs {
    /// Also scan Go, Python and JS/TS imports (reads source files; slower)
    #[arg(long)]
    pub deep: bool,
}

pub fn intercept(cmd: &Cmd, json: bool) -> Option<Result<(), Error>> {
    match cmd {
        Cmd::Zone(ZoneCmd::Describe(a)) => Some(describe(a, json)),
        Cmd::Zone(ZoneCmd::Graph(a)) => Some(graph(a, json)),
        _ => None,
    }
}

fn s(v: impl Into<String>) -> J {
    J::Str(v.into())
}
fn opt(v: Option<String>) -> J {
    v.map_or(J::Null, J::Str)
}
fn arr_s<I: IntoIterator<Item = String>>(v: I) -> J {
    J::Arr(v.into_iter().map(J::Str).collect())
}
fn hex(id: &ObjectId) -> J {
    s(id.to_hex())
}

/// Branch whose head represents a zone: the checked-out one, else `main`.
struct Model {
    zones: Zones,
    flat: Flat,
    /// zone id -> (branch, head)
    heads: BTreeMap<String, (String, ObjectId)>,
    notes: Vec<String>,
}

fn load(repo: &Repo) -> Result<Model, Error> {
    let zones = repo.zones()?;
    let ws = repo.workspace()?;
    let refs = repo.refs()?;
    let rr = repo.remote_refs()?;
    let mut m = Model {
        zones: zones.clone(),
        flat: Flat::new(),
        heads: BTreeMap::new(),
        notes: vec![],
    };
    for z in zones.names() {
        let b = ws.subs.get(z).map_or("main", String::as_str);
        let Some(h) = refs.head(z, b).or_else(|| rr.head(z, b)) else {
            continue;
        };
        let Ok(c) = repo.commit_of(&h) else {
            m.notes
                .push(format!("zone {z}: head commit not available locally; contents not analysed"));
            continue;
        };
        let prefix = zones.path_of(z).unwrap_or("").to_owned();
        match treeops::flatten(repo.store(), Some(c.root), &prefix) {
            Ok(f) => {
                m.flat.extend(f);
                m.heads.insert(z.to_owned(), (b.to_owned(), h));
            }
            Err(_) => m
                .notes
                .push(format!("zone {z}: trees not available locally; contents not analysed")),
        }
    }
    Ok(m)
}

fn deps_of(repo: &Repo, m: &Model, deep: bool) -> zd::Deps {
    zd::dependencies(&m.flat, &m.zones, &|id| repo.content(id).ok(), deep)
}

// ------------------------------------------------------------------ purpose

fn toml_description(text: &str) -> Option<String> {
    for l in text.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("description") {
            let rest = rest.trim_start();
            if let Some(v) = rest.strip_prefix('=') {
                let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if !v.is_empty() {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

fn json_description(text: &str) -> Option<String> {
    let i = text.find("\"description\"")?;
    let rest = text[i + 13..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut esc = false;
    for c in rest.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return (!out.is_empty()).then_some(out);
        } else {
            out.push(c);
        }
    }
    None
}

fn readme_summary(text: &str) -> Option<String> {
    let mut heading = None;
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() || t.starts_with("[!") || t.starts_with('<') || t.starts_with("![") || t.starts_with("```") {
            continue;
        }
        if let Some(h) = t.strip_prefix('#') {
            if heading.is_none() {
                heading = Some(h.trim_start_matches('#').trim().to_owned());
            }
            continue;
        }
        return Some(t.chars().take(200).collect());
    }
    heading.filter(|h| !h.is_empty())
}

/// Purpose text and where it came from, read from the zone's head tree (manifest description,
/// then README). `None` when the zone says nothing about itself.
fn purpose(repo: &Repo, flat: &Flat, zones: &Zones, zone: &str) -> Option<(String, String)> {
    let base = zones.path_of(zone).unwrap_or("");
    let at = |name: &str| {
        if base.is_empty() {
            name.to_owned()
        } else {
            format!("{base}/{name}")
        }
    };
    let read = |name: &str| -> Option<String> {
        let p = at(name);
        let (_, id) = flat.get(&p)?;
        String::from_utf8(repo.content(id).ok()?).ok()
    };
    for (file, f) in [
        ("Cargo.toml", toml_description as fn(&str) -> Option<String>),
        ("pyproject.toml", toml_description),
        ("package.json", json_description),
    ] {
        if let Some(d) = read(file).and_then(|t| f(&t)) {
            return Some((d, at(file)));
        }
    }
    for file in ["README.md", "README", "readme.md", "README.txt"] {
        if let Some(d) = read(file).and_then(|t| readme_summary(&t)) {
            return Some((d, at(file)));
        }
    }
    None
}

fn manifest_kind(flat: &Flat, zones: &Zones, zone: &str) -> Option<&'static str> {
    let base = zones.path_of(zone).unwrap_or("");
    let at = |name: &str| {
        if base.is_empty() {
            name.to_owned()
        } else {
            format!("{base}/{name}")
        }
    };
    [
        ("Cargo.toml", "cargo"),
        ("package.json", "npm"),
        ("go.mod", "go"),
        ("pyproject.toml", "python"),
        ("pom.xml", "maven"),
    ]
    .into_iter()
    .find(|(f, _)| flat.contains_key(&at(f)))
    .map(|(_, k)| k)
}

// ------------------------------------------------------------------ owners

/// CODEOWNERS rules (file order) from the first CODEOWNERS file found in any zone's head tree.
fn codeowners(repo: &Repo, flat: &Flat) -> Option<(String, Vec<(String, Vec<String>)>)> {
    for p in zd::CODEOWNERS_PATHS {
        if let Some((_, id)) = flat.get(*p) {
            if let Ok(t) = repo.content(id) {
                return Some(((*p).to_owned(), zd::parse_codeowners(&String::from_utf8_lossy(&t))));
            }
        }
    }
    None
}

fn owners_for(rules: &[(String, Vec<String>)], path: &str) -> Vec<String> {
    let probe = if path.is_empty() { "x".to_owned() } else { format!("{path}/x") };
    rules
        .iter()
        .rev()
        .find(|(pat, _)| zd::codeowners_match(pat, &probe))
        .map(|(_, o)| o.clone())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ per-zone documents

fn changed_files(repo: &Repo, c: &vcs_objects::Commit) -> Vec<(String, &'static str)> {
    let parent_root = c.parents.first().and_then(|p| repo.commit_of(p).ok()).map(|p| p.root);
    let Ok(d) = treeops::diff_trees_files(repo.store(), parent_root, Some(c.root), "") else {
        return vec![];
    };
    let mut out: BTreeMap<String, &'static str> = BTreeMap::new();
    for p in d.old.keys() {
        out.insert(p.clone(), if d.new.contains_key(p) { "modified" } else { "deleted" });
    }
    for p in d.new.keys() {
        out.entry(p.clone()).or_insert("added");
    }
    out.into_iter().collect()
}

const FILE_CAP: usize = 100;
const COUNT_CAP: usize = 1000;
const AUTHOR_WINDOW: usize = 200;

fn history(repo: &Repo, zone: &str, branch: &str, n: usize) -> Result<(J, J, J), Error> {
    let log = repo.log(zone, Some(branch), COUNT_CAP.max(n))?;
    let recent: Vec<J> = log
        .iter()
        .take(n)
        .map(|(id, c)| {
            let files = changed_files(repo, c);
            let truncated = files.len() > FILE_CAP;
            J::Obj(vec![
                ("id", hex(id)),
                ("change_id", s(c.change_id.to_hex())),
                ("author", s(&c.author.name)),
                ("email", s(&c.author.email)),
                ("timestamp", J::Num(c.author.timestamp)),
                ("subject", s(c.message.lines().next().unwrap_or(""))),
                ("parents", J::Arr(c.parents.iter().map(hex).collect())),
                ("files_changed", J::Num(files.len() as u64)),
                (
                    "files",
                    J::Arr(
                        files
                            .iter()
                            .take(FILE_CAP)
                            .map(|(p, k)| J::Obj(vec![("path", s(p)), ("kind", s(*k))]))
                            .collect(),
                    ),
                ),
                ("files_truncated", J::Bool(truncated)),
            ])
        })
        .collect();
    let mut authors: BTreeMap<(String, String), u64> = BTreeMap::new();
    for (_, c) in log.iter().take(AUTHOR_WINDOW) {
        *authors.entry((c.author.name.clone(), c.author.email.clone())).or_default() += 1;
    }
    let mut a: Vec<_> = authors.into_iter().collect();
    a.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
    let authors = J::Arr(
        a.into_iter()
            .take(5)
            .map(|((n, e), k)| J::Obj(vec![("name", s(n)), ("email", s(e)), ("commits", J::Num(k))]))
            .collect(),
    );
    let count = J::Obj(vec![
        ("commits", J::Num(log.len() as u64)),
        ("capped", J::Bool(log.len() >= COUNT_CAP)),
    ]);
    Ok((J::Arr(recent), authors, count))
}

fn deps_json(deps: &zd::Deps, zones: &Zones, zone: &str) -> (J, J) {
    let out: Vec<J> = deps
        .out_of(zone)
        .into_iter()
        .map(|(to, r)| {
            J::Obj(vec![
                ("zone", s(zones.label(to))),
                ("id", s(to)),
                ("reasons", arr_s(r.iter().cloned())),
            ])
        })
        .collect();
    let inn: Vec<J> = deps
        .edges
        .iter()
        .filter(|((_, b), _)| b == zone)
        .map(|((a, _), r)| {
            J::Obj(vec![
                ("zone", s(zones.label(a))),
                ("id", s(a)),
                ("reasons", arr_s(r.iter().cloned())),
            ])
        })
        .collect();
    (J::Arr(out), J::Arr(inn))
}

fn describe(a: &DescribeArgs, json: bool) -> Result<(), Error> {
    let (repo, _) = crate::open()?;
    let m = load(&repo)?;
    let deps = deps_of(&repo, &m, false);
    let ws = repo.workspace()?;
    let refs = repo.refs()?;
    let sparse = repo.sparse_list()?;
    let held = repo.lockable_held()?;
    let owners = codeowners(&repo, &m.flat);
    let only = match &a.zone {
        Some(n) => Some(repo.resolve_zone(n)?),
        None => None,
    };
    if let Some(z) = &only {
        if !m.zones.exists(z) {
            return Err(Error::Msg(format!("no such zone {z:?}")));
        }
    }
    let mut zj = Vec::new();
    for z in m.zones.names() {
        if only.as_deref().is_some_and(|o| o != z) {
            continue;
        }
        let path = m.zones.path_of(z).unwrap_or("").to_owned();
        let purpose = purpose(&repo, &m.flat, &m.zones, z).map_or(J::Null, |(t, src)| J::Obj(vec![("text", s(t)), ("source", s(src))]));
        let (recent, authors, count) = match m.heads.get(z) {
            Some((b, _)) => history(&repo, z, b, a.history)?,
            None => (J::Arr(vec![]), J::Arr(vec![]), J::Null),
        };
        let co_rules = owners.as_ref().map(|(_, r)| owners_for(r, &path)).unwrap_or_default();
        let nested: Vec<J> = m
            .zones
            .names()
            .filter(|o| *o != z && z == ROOT_ZONE)
            .map(|o| s(m.zones.path_of(o).unwrap_or("")))
            .collect();
        let (dep_on, dep_by) = deps_json(&deps, &m.zones, z);
        let files = m.flat.keys().filter(|p| m.zones.owner(p) == z).count() as u64;
        let locks: Vec<J> = held
            .iter()
            .filter(|(p, _)| m.zones.owner(p) == z)
            .map(|(p, h)| J::Obj(vec![("path", s(m.zones.relative(z, p))), ("expires_ms", J::Num(h.expires_ms))]))
            .collect();
        let branches: Vec<J> = refs
            .branches_of(z)
            .into_iter()
            .map(|(b, h)| {
                let cur = ws.subs.get(z) == Some(&b);
                J::Obj(vec![("name", s(&b)), ("head", hex(&h)), ("checked_out", J::Bool(cur))])
            })
            .collect();
        let head = m.heads.get(z);
        zj.push(J::Obj(vec![
            ("id", s(z)),
            ("name", s(m.zones.label(z))),
            ("path", s(&path)),
            ("is_root", J::Bool(z == ROOT_ZONE)),
            ("kind", opt(manifest_kind(&m.flat, &m.zones, z).map(str::to_owned))),
            ("purpose", purpose),
            (
                "owners",
                J::Obj(vec![
                    ("codeowners", arr_s(co_rules)),
                    ("codeowners_file", opt(owners.as_ref().map(|(p, _)| p.clone()))),
                    ("top_authors", authors),
                    ("author_window_commits", J::Num(AUTHOR_WINDOW as u64)),
                ]),
            ),
            (
                "path_ownership",
                J::Obj(vec![
                    ("prefix", s(&path)),
                    ("owns_rest_of_repo", J::Bool(z == ROOT_ZONE)),
                    ("excluded_prefixes", J::Arr(nested)),
                    ("tracked_files", J::Num(files)),
                ]),
            ),
            ("dependencies", J::Obj(vec![("depends_on", dep_on), ("depended_on_by", dep_by)])),
            (
                "subscription",
                J::Obj(vec![
                    ("subscribed", J::Bool(ws.subs.contains_key(z))),
                    ("branch", opt(ws.subs.get(z).cloned())),
                    ("sparse_paths", sparse.get(z).map_or(J::Null, |v| arr_s(v.iter().cloned()))),
                ]),
            ),
            ("branches", J::Arr(branches)),
            (
                "history",
                J::Obj(vec![
                    ("branch", opt(head.map(|h| h.0.clone()))),
                    ("head", opt(head.map(|h| h.1.to_hex()))),
                    ("count", count),
                    ("recent", recent),
                ]),
            ),
            ("locks", J::Obj(vec![("held_locally", J::Arr(locks))])),
        ]));
    }
    let mut owner_of = Vec::new();
    for p in &a.paths {
        let p = p.trim_matches('/');
        let z = m.zones.owner(p);
        owner_of.push(J::Obj(vec![
            ("path", s(p)),
            ("zone", s(m.zones.label(z))),
            ("id", s(z)),
            ("zone_path", s(m.zones.path_of(z).unwrap_or(""))),
            ("relative", s(m.zones.relative(z, p))),
        ]));
    }
    if !a.fields.is_empty() {
        let known: Vec<&str> = vec![
            "path",
            "is_root",
            "kind",
            "purpose",
            "owners",
            "path_ownership",
            "dependencies",
            "subscription",
            "branches",
            "history",
            "locks",
        ];
        if let Some(bad) = a.fields.iter().find(|f| !known.contains(&f.as_str())) {
            return Err(Error::Msg(format!("unknown field {bad:?}; known: {}", known.join(", "))));
        }
        for z in zj.iter_mut() {
            if let J::Obj(v) = z {
                v.retain(|(k, _)| matches!(*k, "id" | "name") || a.fields.iter().any(|f| f == k));
            }
        }
    }
    let tx = transactions(&repo, &m.zones, a.transactions)?;
    let mut notes = m.notes.clone();
    notes.extend(deps.notes.iter().cloned());
    notes.push("locks: only locks this checkout holds; remote lock state needs `vcs locks`".into());
    if !json {
        return print_text(&zj, &tx);
    }
    let d = doc(
        "zone-describe",
        vec![
            ("schema", s(SCHEMA)),
            ("zones", J::Arr(zj)),
            ("owner_of", J::Arr(owner_of)),
            ("transactions", tx),
            ("notes", arr_s(notes)),
        ],
    );
    println!("{}", d.render());
    Ok(())
}

/// The operation log as transactions: which (zone, branch) heads each operation moved.
fn transactions(repo: &Repo, zones: &Zones, limit: usize) -> Result<J, Error> {
    let log = repo.op_log(limit.saturating_add(1))?;
    let mut out = Vec::new();
    for (id, op) in log.iter().take(limit) {
        let parent_view = match op.parents.first() {
            Some(p) => Some(repo.op_get(p)?.view),
            None => None,
        };
        let empty = BTreeMap::new();
        let old = parent_view.as_ref().map_or(&empty, |v| &v.branches);
        let mut moves = Vec::new();
        let keys: BTreeSet<_> = old.keys().chain(op.view.branches.keys()).collect();
        for k in keys {
            let (o, n) = (old.get(k), op.view.branches.get(k));
            if o != n {
                moves.push(J::Obj(vec![
                    ("zone", s(zones.label(&k.0))),
                    ("id", s(&k.0)),
                    ("branch", s(&k.1)),
                    ("from", opt(o.map(ObjectId::to_hex))),
                    ("to", opt(n.map(ObjectId::to_hex))),
                ]));
            }
        }
        let old_ws = parent_view.as_ref().map(|v| &v.workspace);
        let subs: BTreeSet<String> = op
            .view
            .workspace
            .iter()
            .filter(|(z, b)| old_ws.and_then(|w| w.get(*z)) != Some(*b))
            .map(|(z, _)| z.clone())
            .chain(
                old_ws
                    .into_iter()
                    .flat_map(|w| w.keys())
                    .filter(|z| !op.view.workspace.contains_key(*z))
                    .cloned(),
            )
            .collect();
        let touched: BTreeSet<String> = moves
            .iter()
            .filter_map(|j| match j {
                J::Obj(v) => v.iter().find(|(k, _)| *k == "zone").and_then(|(_, x)| match x {
                    J::Str(z) => Some(z.clone()),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        out.push(J::Obj(vec![
            ("id", hex(id)),
            ("description", s(&op.description)),
            ("author", s(&op.author.name)),
            ("timestamp", J::Num(op.author.timestamp)),
            ("current", J::Bool(out.is_empty())),
            ("zones_touched", arr_s(touched)),
            ("branch_moves", J::Arr(moves)),
            ("subscription_changes", arr_s(subs.into_iter().map(|z| zones.label(&z).to_owned()))),
        ]));
    }
    Ok(J::Arr(out))
}

fn get<'a>(j: &'a J, k: &str) -> Option<&'a J> {
    match j {
        J::Obj(v) => v.iter().find(|(n, _)| *n == k).map(|(_, x)| x),
        _ => None,
    }
}
fn str_of(j: Option<&J>) -> String {
    match j {
        Some(J::Str(x)) => x.clone(),
        _ => String::new(),
    }
}

fn print_text(zones: &[J], tx: &J) -> Result<(), Error> {
    for z in zones {
        let name = str_of(get(z, "name"));
        let path = str_of(get(z, "path"));
        println!("zone {name}  path {}", if path.is_empty() { "(rest of repo)" } else { &path });
        if let Some(p) = get(z, "purpose") {
            if !matches!(p, J::Null) {
                println!("  purpose: {} [{}]", str_of(get(p, "text")), str_of(get(p, "source")));
            }
        }
        if let Some(J::Arr(d)) = get(z, "dependencies").and_then(|d| get(d, "depends_on")) {
            let n: Vec<String> = d.iter().map(|x| str_of(get(x, "zone"))).collect();
            if !n.is_empty() {
                println!("  depends on: {}", n.join(", "));
            }
        }
        if let Some(J::Arr(d)) = get(z, "dependencies").and_then(|d| get(d, "depended_on_by")) {
            let n: Vec<String> = d.iter().map(|x| str_of(get(x, "zone"))).collect();
            if !n.is_empty() {
                println!("  depended on by: {}", n.join(", "));
            }
        }
        if let Some(J::Arr(c)) = get(z, "owners").and_then(|o| get(o, "codeowners")) {
            let n: Vec<String> = c.iter().map(|x| str_of(Some(x))).collect();
            if !n.is_empty() {
                println!("  owners: {}", n.join(" "));
            }
        }
    }
    if let J::Arr(t) = tx {
        for o in t {
            println!("op {}  {}", &str_of(get(o, "id"))[..12], str_of(get(o, "description")));
        }
    }
    println!("(use --json for the full machine-readable document)");
    Ok(())
}

// ------------------------------------------------------------------ graph

fn reach(adj: &BTreeMap<String, BTreeSet<String>>, from: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut q = VecDeque::from([from.to_owned()]);
    while let Some(x) = q.pop_front() {
        for y in adj.get(&x).into_iter().flatten() {
            if y != from && seen.insert(y.clone()) {
                q.push_back(y.clone());
            }
        }
    }
    seen.into_iter().collect()
}

fn graph(a: &GraphArgs, json: bool) -> Result<(), Error> {
    let (repo, _) = crate::open()?;
    let m = load(&repo)?;
    let deps = deps_of(&repo, &m, a.deep);
    let ws = repo.workspace()?;
    let names: Vec<String> = m.zones.names().map(str::to_owned).collect();
    let mut fwd: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rev: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ((x, y), _) in &deps.edges {
        fwd.entry(x.clone()).or_default().insert(y.clone());
        rev.entry(y.clone()).or_default().insert(x.clone());
    }
    let label = |z: &str| m.zones.label(z).to_owned();
    let list = |v: Vec<String>| arr_s(v.into_iter().map(|z| label(&z)));
    let nodes: Vec<J> = names
        .iter()
        .map(|z| {
            J::Obj(vec![
                ("id", s(z)),
                ("name", s(label(z))),
                ("path", s(m.zones.path_of(z).unwrap_or(""))),
                ("subscribed", J::Bool(ws.subs.contains_key(z))),
                ("depends_on", list(fwd.get(z).into_iter().flatten().cloned().collect())),
                ("depends_on_transitive", list(reach(&fwd, z))),
                ("dependents", list(rev.get(z).into_iter().flatten().cloned().collect())),
                ("dependents_transitive", list(reach(&rev, z))),
            ])
        })
        .collect();
    let edges: Vec<J> = deps
        .edges
        .iter()
        .map(|((x, y), r)| {
            J::Obj(vec![
                ("from", s(label(x))),
                ("to", s(label(y))),
                ("kind", s("depends_on")),
                ("reasons", arr_s(r.iter().cloned())),
            ])
        })
        .collect();
    let cycles: Vec<J> = deps.cycles_among(&names).into_iter().map(list).collect();
    let mut path_index: Vec<(String, String)> = names
        .iter()
        .map(|z| (m.zones.path_of(z).unwrap_or("").to_owned(), label(z)))
        .collect();
    path_index.sort();
    let mut notes = m.notes.clone();
    notes.extend(deps.notes.iter().cloned());
    if !json {
        for e in &edges {
            println!("{} -> {}  ({})", str_of(get(e, "from")), str_of(get(e, "to")), "depends_on");
        }
        if edges.is_empty() {
            println!("no dependencies between zones found");
        }
        println!("(use --json for the full machine-readable document)");
        return Ok(());
    }
    let d = doc(
        "zone-graph",
        vec![
            ("schema", s(SCHEMA)),
            ("nodes", J::Arr(nodes)),
            ("edges", J::Arr(edges)),
            ("cycles", J::Arr(cycles)),
            (
                "path_index",
                J::Arr(
                    path_index
                        .into_iter()
                        .map(|(p, z)| J::Obj(vec![("prefix", s(p)), ("zone", s(z))]))
                        .collect(),
                ),
            ),
            ("imports_scanned", J::Bool(a.deep)),
            ("notes", arr_s(notes)),
        ],
    );
    println!("{}", d.render());
    Ok(())
}
