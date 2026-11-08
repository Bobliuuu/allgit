//! Assisted zone suggestions: the protocol between `vcs zone suggest --assist <cmd>` and an external
//! assistant, and the strict validation of what comes back. Pure: no processes, no filesystem, no
//! network. The assistant only *proposes* path prefixes; nothing here (or anywhere in the assist
//! path) creates a zone. See docs/zone-assist.md.
//!
//! Request (one JSON object on the assistant's stdin; `kind` tells it apart from the merge-conflict
//! request of docs/merge.md, which has no `kind`):
//!
//! ```text
//! {"version":1,"kind":"zone-suggest","limits":{"min_files":3,"max_zones":20},
//!  "repo":{"total_files":N,"root_files":N},
//!  "tree":{"max_depth":4,"truncated":false,"dirs":[{"path":"crates/a","files":12}]},
//!  "manifests":["Cargo.toml","crates/a/Cargo.toml"],
//!  "codeowners":[{"pattern":"/web/","owners":["@alice"]}],
//!  "existing_zones":[{"name":"docs","path":"docs"}],
//!  "heuristic":[{"name":"a","path":"crates/a","files":12,"confidence_pct":85,
//!                "signals":[{"kind":"cargo-workspace","detail":"..."}]}]}
//! ```
//!
//! Response: `{"zones":[{"name":"web","path":"web","rationale":"...","confidence":0.7}]}`.
//! Every proposal is validated on its own; bad ones are reported and dropped, good ones kept.

use crate::zonedetect::SKIP_DIRS;
use std::collections::BTreeSet;
use vcs_objects::names::{paths_overlap, validate_path, validate_zone_name};

pub const VERSION: u32 = 1;
pub const KIND: &str = "zone-suggest";
/// Largest request vcs will send, in bytes. The assistant binaries refuse above the same size.
pub const MAX_REQUEST_BYTES: usize = 200 * 1024;
/// Largest response accepted, in bytes.
pub const MAX_RESPONSE_BYTES: usize = 1 << 20;
/// Directories listed in the tree (shallowest first); more are cut and `truncated` is set.
pub const MAX_TREE_DIRS: usize = 1500;
/// Deepest directory level listed.
pub const MAX_TREE_DEPTH: usize = 4;
pub const MAX_MANIFESTS: usize = 500;
pub const MAX_CODEOWNERS_RULES: usize = 200;
/// Proposals read from a response; later ones are ignored.
pub const MAX_PROPOSALS_READ: usize = 50;
pub const MAX_RATIONALE_BYTES: usize = 500;
const MAX_JSON_DEPTH: usize = 32;

const MANIFEST_NAMES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "BUILD",
    "BUILD.bazel",
];

/// A heuristic proposal as sent to the assistant.
pub struct HeuristicIn {
    pub name: String,
    pub path: String,
    pub files: u64,
    pub confidence_pct: u64,
    pub signals: Vec<(String, String)>,
}

/// Everything a request is built from.
pub struct ReqInput<'a> {
    pub total_files: u64,
    /// Paths of the committed files owned by the root zone.
    pub root_paths: Vec<&'a str>,
    pub codeowners: Vec<(String, Vec<String>)>,
    /// `(name, path)` of the zones that exist (root excluded).
    pub existing: Vec<(String, String)>,
    pub heuristic: Vec<HeuristicIn>,
    pub min_files: usize,
    pub max_zones: usize,
}

fn skip_segment(seg: &str) -> bool {
    seg.starts_with('.') || SKIP_DIRS.contains(&seg)
}

/// Encode a request. Only directory paths with file counts, manifest paths, CODEOWNERS rules, existing
/// zones and the heuristic proposals are included: no file contents and no other file names. The tree is
/// cut (deepest directories first) until the request fits [`MAX_REQUEST_BYTES`]; `Err` when even an empty
/// tree does not fit.
pub fn encode_request(inp: &ReqInput) -> Result<String, String> {
    use std::collections::BTreeMap;
    let mut dirs: BTreeMap<String, u64> = BTreeMap::new();
    let mut manifests: Vec<&str> = Vec::new();
    for p in &inp.root_paths {
        let segs: Vec<&str> = p.split('/').collect();
        let (file, dir_segs) = segs.split_last().ok_or("empty path")?;
        if dir_segs.iter().any(|s| skip_segment(s)) {
            continue;
        }
        if MANIFEST_NAMES.contains(file) {
            manifests.push(p);
        }
        for d in 1..=dir_segs.len().min(MAX_TREE_DEPTH) {
            *dirs.entry(dir_segs[..d].join("/")).or_default() += 1;
        }
    }
    manifests.sort_unstable();
    manifests.truncate(MAX_MANIFESTS);
    let mut dir_list: Vec<(String, u64)> = dirs.into_iter().collect();
    dir_list.sort_by(|a, b| a.0.matches('/').count().cmp(&b.0.matches('/').count()).then(a.0.cmp(&b.0)));
    let mut truncated = dir_list.len() > MAX_TREE_DIRS;
    dir_list.truncate(MAX_TREE_DIRS);
    let root_files = inp.root_paths.len() as u64;
    loop {
        let s = render(inp, &dir_list, &manifests, truncated, root_files);
        if s.len() <= MAX_REQUEST_BYTES {
            return Ok(s);
        }
        if dir_list.is_empty() {
            return Err(format!(
                "request is {} bytes even without a tree; limit is {MAX_REQUEST_BYTES}",
                s.len()
            ));
        }
        dir_list.truncate(dir_list.len() / 2);
        truncated = true;
    }
}

fn render(inp: &ReqInput, dirs: &[(String, u64)], manifests: &[&str], truncated: bool, root_files: u64) -> String {
    use vcs_merge::assist::write_str as w;
    let mut o = format!(
        "{{\"version\":{VERSION},\"kind\":\"{KIND}\",\"limits\":{{\"min_files\":{},\"max_zones\":{}}},\"repo\":{{\"total_files\":{},\"root_files\":{}}},\"tree\":{{\"max_depth\":{MAX_TREE_DEPTH},\"truncated\":{},\"dirs\":[",
        inp.min_files, inp.max_zones, inp.total_files, root_files, truncated
    );
    for (i, (p, n)) in dirs.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"path\":");
        w(&mut o, p);
        o.push_str(&format!(",\"files\":{n}}}"));
    }
    o.push_str("]},\"manifests\":[");
    for (i, m) in manifests.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        w(&mut o, m);
    }
    o.push_str("],\"codeowners\":[");
    for (i, (pat, owners)) in inp.codeowners.iter().take(MAX_CODEOWNERS_RULES).enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"pattern\":");
        w(&mut o, &pat.chars().take(200).collect::<String>());
        o.push_str(",\"owners\":[");
        for (j, ow) in owners.iter().take(10).enumerate() {
            if j > 0 {
                o.push(',');
            }
            w(&mut o, &ow.chars().take(100).collect::<String>());
        }
        o.push_str("]}");
    }
    o.push_str("],\"existing_zones\":[");
    for (i, (n, p)) in inp.existing.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"name\":");
        w(&mut o, n);
        o.push_str(",\"path\":");
        w(&mut o, p);
        o.push('}');
    }
    o.push_str("],\"heuristic\":[");
    for (i, h) in inp.heuristic.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        o.push_str("{\"name\":");
        w(&mut o, &h.name);
        o.push_str(",\"path\":");
        w(&mut o, &h.path);
        o.push_str(&format!(
            ",\"files\":{},\"confidence_pct\":{},\"signals\":[",
            h.files, h.confidence_pct
        ));
        for (j, (k, d)) in h.signals.iter().enumerate() {
            if j > 0 {
                o.push(',');
            }
            o.push_str("{\"kind\":");
            w(&mut o, k);
            o.push_str(",\"detail\":");
            w(&mut o, &d.chars().take(200).collect::<String>());
            o.push('}');
        }
        o.push_str("]}");
    }
    o.push_str("]}");
    o
}

// ---------------------------------------------------------------- response

/// Where a proposal in the merged output comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Only the assistant proposed this path.
    Assist,
    /// The assistant proposed the path of a heuristic proposal too.
    Both,
}
impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Assist => "assist",
            Source::Both => "both",
        }
    }
}

/// A validated proposal. Still only a proposal.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub name: String,
    pub path: String,
    pub files: usize,
    pub rationale: String,
    pub confidence_pct: u64,
    pub source: Source,
    /// Heuristic proposals whose path is nested in, or contains, this one (alternatives: pick one).
    pub overlaps_heuristic: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejected {
    pub name: String,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub accepted: Vec<Proposal>,
    pub rejected: Vec<Rejected>,
    pub notes: Vec<String>,
}

/// What a response is checked against.
pub struct Ctx<'a> {
    /// `(name, path)` of existing zones, root excluded.
    pub existing: &'a [(String, String)],
    /// Ids and display names already taken.
    pub taken_names: &'a BTreeSet<String>,
    /// `(name, path)` of the heuristic proposals.
    pub heuristic: &'a [(String, String)],
    pub min_files: usize,
    pub max_zones: usize,
    /// Committed root-zone files under this directory.
    pub count_under: &'a dyn Fn(&str) -> usize,
}

/// The path rules, without looking at the repository: relative, normalised, no `.`/`..`/empty or
/// dot-directory/vendor/build components, valid for a zone. A single trailing `/` is accepted.
pub fn check_path_syntax(raw: &str) -> Result<String, String> {
    let p = raw.strip_suffix('/').unwrap_or(raw);
    if p.is_empty() {
        return Err("empty path (the root zone cannot be proposed)".into());
    }
    if p.starts_with('/') || p.contains('\\') || p.chars().nth(1) == Some(':') {
        return Err("path must be relative to the repository root".into());
    }
    if p.chars().any(|c| c.is_control()) {
        return Err("path contains a control character".into());
    }
    for seg in p.split('/') {
        match seg {
            "" => return Err("path has an empty component".into()),
            "." | ".." => return Err("path has a `.` or `..` component".into()),
            s if skip_segment(s) => {
                return Err(format!("component {s:?} is a dot, vendor or build directory; never a zone"));
            }
            _ => {}
        }
    }
    validate_path(p).map_err(|e| e.to_string())?;
    Ok(p.to_owned())
}

/// Parse a response and validate every proposal. `Err` only when the response as a whole is unusable.
pub fn parse_and_validate(bytes: &[u8], ctx: &Ctx) -> Result<Outcome, String> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!("response too large ({} bytes)", bytes.len()));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "response is not UTF-8".to_owned())?;
    let v = parse_json(text).map_err(|e| format!("response is not valid JSON ({e})"))?;
    let Jv::Obj(_) = v else {
        return Err("response is not a JSON object".into());
    };
    if let Some(ver) = v.get("version") {
        if !matches!(ver, Jv::Num(n) if *n == f64::from(VERSION)) {
            return Err("unsupported response version (expected 1)".into());
        }
    }
    if let Some(k) = v.get("kind") {
        if k.as_str() != Some(KIND) {
            return Err(format!("response kind is not {KIND:?}"));
        }
    }
    let Some(Jv::Arr(items)) = v.get("zones") else {
        return Err("response has no \"zones\" array".into());
    };
    let mut out = Outcome::default();
    if items.len() > MAX_PROPOSALS_READ {
        out.notes.push(format!(
            "assistant sent {} proposals; only the first {MAX_PROPOSALS_READ} were read",
            items.len()
        ));
    }
    struct Cand {
        name: String,
        path: String,
        rationale: String,
        conf: f64,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for it in items.iter().take(MAX_PROPOSALS_READ) {
        let name = it.get("name").and_then(Jv::as_str);
        let path = it.get("path").and_then(Jv::as_str);
        let shown_name = clip(name.unwrap_or(""), 80);
        let shown_path = clip(path.unwrap_or(""), 200);
        let mut rej = |reason: String| {
            out.rejected.push(Rejected {
                name: shown_name.clone(),
                path: shown_path.clone(),
                reason,
            })
        };
        let (Some(name), Some(path)) = (name, path) else {
            rej("proposal needs string \"name\" and \"path\"".into());
            continue;
        };
        let conf = match it.get("confidence") {
            Some(Jv::Num(c)) if c.is_finite() && (0.0..=1.0).contains(c) => *c,
            _ => {
                rej("\"confidence\" must be a number from 0 to 1".into());
                continue;
            }
        };
        let Some(rationale) = it.get("rationale").and_then(Jv::as_str) else {
            rej("proposal needs a string \"rationale\"".into());
            continue;
        };
        cands.push(Cand {
            name: name.to_owned(),
            path: path.to_owned(),
            rationale: clip(rationale, MAX_RATIONALE_BYTES),
            conf,
        });
    }
    // Best first; ties: shorter path, then the order sent (the sort is stable).
    cands.sort_by(|a, b| b.conf.total_cmp(&a.conf).then(a.path.len().cmp(&b.path.len())));
    for c in cands {
        let mut rej = |reason: String| {
            out.rejected.push(Rejected {
                name: clip(&c.name, 80),
                path: clip(&c.path, 200),
                reason,
            })
        };
        if let Err(e) = validate_zone_name(&c.name) {
            rej(format!("bad zone name: {e}"));
            continue;
        }
        let path = match check_path_syntax(&c.path) {
            Ok(p) => p,
            Err(e) => {
                rej(e);
                continue;
            }
        };
        if let Some((n, _)) = ctx.existing.iter().find(|(_, p)| !p.is_empty() && paths_overlap(p, &path)) {
            rej(format!("overlaps existing zone {n:?}"));
            continue;
        }
        let n = (ctx.count_under)(&path);
        if n == 0 {
            rej("no committed files under that path in the root zone (does not exist, or is already in a zone)".into());
            continue;
        }
        if n < ctx.min_files {
            rej(format!("only {n} file(s), fewer than min_files {}", ctx.min_files));
            continue;
        }
        if let Some(p) = out.accepted.iter().find(|p| paths_overlap(&p.path, &path)) {
            rej(format!(
                "overlaps proposal {:?} (path {:?}); proposals may not overlap",
                p.name, p.path
            ));
            continue;
        }
        let same_h = ctx.heuristic.iter().find(|(_, p)| *p == path);
        if ctx.taken_names.contains(&c.name) {
            rej("zone name already in use".into());
            continue;
        }
        if out.accepted.iter().any(|p| p.name == c.name) {
            rej("name used by an earlier proposal".into());
            continue;
        }
        if same_h.is_none() && ctx.heuristic.iter().any(|(n, _)| *n == c.name) {
            rej("name is used by a heuristic proposal for another path".into());
            continue;
        }
        if out.accepted.len() >= ctx.max_zones {
            rej(format!("over the limit of {} proposals", ctx.max_zones));
            continue;
        }
        let overlaps = ctx
            .heuristic
            .iter()
            .filter(|(_, p)| *p != path && paths_overlap(p, &path))
            .map(|(n, _)| n.clone())
            .collect();
        out.accepted.push(Proposal {
            name: c.name,
            path,
            files: n,
            rationale: c.rationale,
            confidence_pct: (c.conf * 100.0).round() as u64,
            source: if same_h.is_some() { Source::Both } else { Source::Assist },
            overlaps_heuristic: overlaps,
        });
    }
    Ok(out)
}

/// Control characters become spaces; cut at `max` bytes on a character boundary.
fn clip(s: &str, max: usize) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if s.len() <= max {
        return s;
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    format!("{}...", &s[..n])
}

// ---------------------------------------------------------------- minimal JSON

#[derive(Debug, Clone, PartialEq)]
enum Jv {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Jv>),
    Obj(Vec<(String, Jv)>),
}
impl Jv {
    fn get(&self, k: &str) -> Option<&Jv> {
        match self {
            Jv::Obj(v) => v.iter().find(|(a, _)| a == k).map(|(_, b)| b),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Jv::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn parse_json(s: &str) -> Result<Jv, String> {
    let mut p = P { b: s.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}
impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", c as char, self.i))
        }
    }
    fn lit(&mut self, w: &str, v: Jv) -> Result<Jv, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.i))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Jv, String> {
        if depth > MAX_JSON_DEPTH {
            return Err("nesting too deep".into());
        }
        match self.b.get(self.i) {
            None => Err("unexpected end".into()),
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Jv::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Jv::Obj(m));
                        }
                        _ => return Err(format!("expected , or }} at byte {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Jv::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Jv::Arr(a));
                        }
                        _ => return Err(format!("expected , or ] at byte {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Jv::Str(self.string()?)),
            Some(b't') => self.lit("true", Jv::Bool(true)),
            Some(b'f') => self.lit("false", Jv::Bool(false)),
            Some(b'n') => self.lit("null", Jv::Null),
            Some(b'-' | b'0'..=b'9') => {
                let st = self.i;
                while matches!(self.b.get(self.i), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[st..self.i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(Jv::Num)
                    .ok_or_else(|| format!("bad number at byte {st}"))
            }
            Some(c) => Err(format!("unexpected {:?} at byte {}", *c as char, self.i)),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("short \\u escape")?;
        let v = u32::from_str_radix(std::str::from_utf8(h).map_err(|_| "bad \\u")?, 16).map_err(|_| "bad \\u escape")?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let st = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[st..self.i]).map_err(|_| "bad UTF-8")?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.b.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err("lone surrogate".into());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("lone surrogate".into());
                                }
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            out.push(char::from_u32(cp).ok_or("bad code point")?);
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => return Err("unterminated or control character in string".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(spec: &[(&str, usize)]) -> Vec<String> {
        spec.iter()
            .flat_map(|(d, n)| (0..*n).map(move |i| format!("{d}/f{i}.txt")))
            .collect()
    }

    fn input(paths: &[String]) -> ReqInput<'_> {
        ReqInput {
            total_files: paths.len() as u64,
            root_paths: paths.iter().map(String::as_str).collect(),
            codeowners: vec![("/web/".into(), vec!["@alice".into()])],
            existing: vec![("docs".into(), "docs".into())],
            heuristic: vec![HeuristicIn {
                name: "a".into(),
                path: "crates/a".into(),
                files: 3,
                confidence_pct: 85,
                signals: vec![("cargo-workspace".into(), "member".into())],
            }],
            min_files: 3,
            max_zones: 20,
        }
    }

    #[test]
    fn request_has_dirs_and_no_file_names() {
        let mut paths = files(&[("crates/a", 3), ("web/ui", 4), ("node_modules/x", 5), (".hid/den", 2)]);
        paths.push("crates/a/Cargo.toml".into());
        paths.push("README.md".into());
        let s = encode_request(&input(&paths)).unwrap();
        assert!(s.len() <= MAX_REQUEST_BYTES);
        assert!(s.contains("\"kind\":\"zone-suggest\"") && s.contains("\"version\":1"));
        assert!(s.contains("{\"path\":\"crates/a\",\"files\":4}"), "{s}");
        assert!(s.contains("\"manifests\":[\"crates/a/Cargo.toml\"]"));
        assert!(s.contains("\"owners\":[\"@alice\"]"));
        // no individual file names, no vendor or dot directories
        assert!(!s.contains("f0.txt") && !s.contains("README") && !s.contains("node_modules") && !s.contains(".hid"));
        let v = parse_json(&s).unwrap();
        assert!(matches!(v.get("tree"), Some(Jv::Obj(_))));
    }

    #[test]
    fn big_tree_is_cut_to_the_cap() {
        let paths: Vec<String> = (0..4000).map(|i| format!("d{i}/sub{i}/file.txt")).collect();
        let s = encode_request(&input(&paths)).unwrap();
        assert!(s.len() <= MAX_REQUEST_BYTES);
        assert!(s.contains("\"truncated\":true"));
        // shallow directories survive the cut
        assert!(s.contains("\"path\":\"d0\""));
    }

    #[test]
    fn oversize_without_tree_is_an_error() {
        let paths = vec!["a/b.txt".to_owned()];
        let mut i = input(&paths);
        i.codeowners = (0..200)
            .map(|k| (format!("/{}/", "x".repeat(190) + &k.to_string()), vec!["@o".repeat(30)]))
            .collect();
        i.heuristic = (0..300)
            .map(|k| HeuristicIn {
                name: format!("n{k}"),
                path: "p".repeat(200),
                files: 1,
                confidence_pct: 1,
                signals: vec![("k".into(), "d".repeat(200)); 5],
            })
            .collect();
        assert!(encode_request(&i).is_err());
    }

    fn check(resp: &str, count: &dyn Fn(&str) -> usize) -> Outcome {
        let existing = vec![("docs".to_owned(), "docs".to_owned())];
        let taken: BTreeSet<String> = ["docs".to_owned(), "root".to_owned()].into();
        let heur = vec![("a".to_owned(), "crates/a".to_owned())];
        let ctx = Ctx {
            existing: &existing,
            taken_names: &taken,
            heuristic: &heur,
            min_files: 3,
            max_zones: 3,
            count_under: count,
        };
        parse_and_validate(resp.as_bytes(), &ctx).unwrap()
    }
    fn five(p: &str) -> usize {
        if p.starts_with("docs") || p.starts_with("ghost") {
            0
        } else if p == "tiny" {
            1
        } else {
            5
        }
    }
    fn one(path: &str, name: &str) -> String {
        format!(r#"{{"zones":[{{"name":"{name}","path":"{path}","rationale":"r","confidence":0.6}}]}}"#)
    }

    #[test]
    fn accepts_a_good_proposal_and_labels_it() {
        let o = check(&one("web/", "web"), &five);
        assert_eq!(o.accepted.len(), 1, "{:?}", o.rejected);
        let p = &o.accepted[0];
        assert_eq!(
            (p.path.as_str(), p.files, p.confidence_pct, p.source),
            ("web", 5, 60, Source::Assist)
        );
        let o = check(&one("crates/a", "alpha"), &five);
        assert_eq!(o.accepted[0].source, Source::Both);
        let o = check(&one("crates", "all"), &five);
        assert_eq!(o.accepted[0].overlaps_heuristic, ["a"]);
    }

    #[test]
    fn rejects_bad_paths() {
        for (path, why) in [
            ("/etc", "relative"),
            ("../x", ".."),
            ("a/../b", ".."),
            ("a//b", "empty component"),
            ("./a", "`.`"),
            ("", "empty"),
            ("a\\\\b", "relative"),
            ("C:/x", "relative"),
            (".hidden/x", "dot"),
            ("a/node_modules", "vendor"),
            ("a/target", "vendor"),
            ("vendor/x", "vendor"),
            ("a/.vcs", "dot"),
            ("ghost", "no committed files"),
            ("tiny", "min_files"),
            ("docs/sub", "overlaps existing zone"),
            ("docs", "overlaps existing zone"),
        ] {
            let o = check(&one(path, "z"), &five);
            assert!(o.accepted.is_empty(), "{path} accepted");
            assert!(o.rejected[0].reason.contains(why), "{path}: {}", o.rejected[0].reason);
        }
    }

    #[test]
    fn rejects_bad_names_overlaps_and_oversize() {
        for name in ["", "-x", ".x", "a b", "a/b", "docs", "root", "a", &"n".repeat(65)] {
            let o = check(&one("web", name), &five);
            assert!(o.accepted.is_empty(), "name {name:?} accepted");
        }
        // two overlapping proposals: the more confident one wins
        let r = r#"{"zones":[
            {"name":"inner","path":"web/ui","rationale":"r","confidence":0.5},
            {"name":"outer","path":"web","rationale":"r","confidence":0.9}]}"#;
        let o = check(r, &five);
        assert_eq!(o.accepted.len(), 1);
        assert_eq!(o.accepted[0].name, "outer");
        assert!(o.rejected[0].reason.contains("overlaps proposal"));
        // limit (3) and duplicate names
        let r = r#"{"zones":[
            {"name":"a1","path":"p1","rationale":"r","confidence":0.5},
            {"name":"a2","path":"p2","rationale":"r","confidence":0.5},
            {"name":"a3","path":"p3","rationale":"r","confidence":0.5},
            {"name":"a4","path":"p4","rationale":"r","confidence":0.5},
            {"name":"a1","path":"p5","rationale":"r","confidence":0.4}]}"#;
        let o = check(r, &five);
        assert_eq!(o.accepted.len(), 3);
        assert_eq!(o.rejected.len(), 2);
        // too many proposals read
        let many: Vec<String> = (0..60)
            .map(|i| format!(r#"{{"name":"n{i}","path":"p{i}","rationale":"r","confidence":0.5}}"#))
            .collect();
        let o = check(&format!(r#"{{"zones":[{}]}}"#, many.join(",")), &five);
        assert!(o.notes.iter().any(|n| n.contains("first 50")));
    }

    #[test]
    fn rejects_bad_fields_and_whole_responses() {
        for bad in [
            r#"{"zones":[{"name":"w","path":"web","rationale":"r"}]}"#,
            r#"{"zones":[{"name":"w","path":"web","rationale":"r","confidence":1.5}]}"#,
            r#"{"zones":[{"name":"w","path":"web","rationale":"r","confidence":"high"}]}"#,
            r#"{"zones":[{"name":"w","path":"web","confidence":0.5}]}"#,
            r#"{"zones":[{"name":"w","path":5,"rationale":"r","confidence":0.5}]}"#,
            r#"{"zones":[7]}"#,
        ] {
            let o = check(bad, &five);
            assert!(o.accepted.is_empty() && o.rejected.len() == 1, "{bad}");
        }
        let existing = vec![];
        let taken = BTreeSet::new();
        let ctx = Ctx {
            existing: &existing,
            taken_names: &taken,
            heuristic: &[],
            min_files: 1,
            max_zones: 5,
            count_under: &five,
        };
        for bad in [
            &b"not json"[..],
            b"[]",
            b"{}",
            b"{\"zones\":{}}",
            b"{\"version\":2,\"zones\":[]}",
            b"{\"kind\":\"merge\",\"zones\":[]}",
            b"{\"zones\":[]} trailing",
            b"\xff\xfe",
        ] {
            assert!(parse_and_validate(bad, &ctx).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
        let deep = format!("{}{}", "[".repeat(40), "]".repeat(40));
        assert!(parse_and_validate(deep.as_bytes(), &ctx).is_err());
        let huge = vec![b' '; MAX_RESPONSE_BYTES + 1];
        assert!(parse_and_validate(&huge, &ctx).unwrap_err().contains("too large"));
        // unknown fields and escapes are fine
        let ok = parse_and_validate(
            br#"{"version":1,"kind":"zone-suggest","extra":null,"zones":[{"name":"w","path":"w\u0065b","rationale":"a\nb \ud83d\ude00","confidence":0.55e0,"x":1}]}"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(ok.accepted[0].path, "web");
        assert_eq!(ok.accepted[0].rationale, "a b \u{1f600}");
    }

    #[test]
    fn rationale_is_clipped() {
        let long = "x".repeat(2000);
        let r = format!(r#"{{"zones":[{{"name":"w","path":"web","rationale":"{long}","confidence":0.5}}]}}"#);
        let o = check(&r, &five);
        assert!(o.accepted[0].rationale.len() <= MAX_RATIONALE_BYTES + 3);
    }
}
