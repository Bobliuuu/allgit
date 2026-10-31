//! Import recorded Perforce output into the root zone, one commit per submitted changelist.
//!
//! FIXTURE-TESTED ONLY. No Perforce server or `p4` binary was available when this was written;
//! the importer has never been run against real `p4` output. The formats below are implemented
//! from Perforce's published documentation and may differ from what a real server emits:
//!
//! * `p4 changes -l` text: "Change N on YYYY/MM/DD by user@client [*pending*]" followed by a
//!   tab-indented description (P4 Command Reference, `p4 changes`).
//! * `p4 describe -s N` text: "Change N by user@client on YYYY/MM/DD HH:MM:SS", the
//!   tab-indented description, "Affected files ..." and "... //depot/path#rev action" lines
//!   (P4 Command Reference, `p4 describe`). The text form carries no file type; files are
//!   then treated as plain `text`. Times are server-local; they are read as UTC.
//! * `p4 -G describe -s N`: marshalled dictionary with `change`, `user`, `client`, `time` (epoch
//!   seconds), `desc`, `status` and indexed `depotFileN`/`revN`/`actionN`/`typeN` (see
//!   [`crate::p4_marshal`]). Preferred because it carries the file type.
//! * `p4 print -q //depot/path#rev`: raw file content, no header.
//! * Actions (P4 "Command Reference: Actions"): add, edit, branch, integrate, import, move/add
//!   write content; delete, move/delete, purge, archive remove the file. integrate/branch
//!   are plain content changes (no merge parentage is created). A move is delete + add.
//! * File types (P4 "File types"): `+x` (or legacy `xtext`, `xbinary`...) is Executable;
//!   `symlink` is a Symlink whose content is the target. Types whose bytes we cannot
//!   faithfully reproduce (`apple`, `resource`, `utf16`, `+k` keyword expansion) are imported
//!   as plain bytes and listed in `type_warnings`.
//!
//! Fixture directory layout (all files produced by redirecting the p4 commands):
//!
//! ```text
//! <dir>/changes.txt                 p4 changes -l -s submitted //depot/...   (optional; else describe/ is scanned)
//! <dir>/describe/<N>.marshal        p4 -G describe -s N     (or)
//! <dir>/describe/<N>.txt            p4 describe -s N
//! <dir>/print/<N>/<depot path>      p4 print -q //depot/path#rev, path without leading "//"
//! ```
//!
//! Mapping: the view file maps depot paths to paths in the root zone ([`P4View`]). The change
//! number is kept as a `Perforce-Change: N` trailer in the commit message and in the table
//! `.vcs/p4-import` (`<commit id> p4:<N>:root`), so re-import skips known changelists.

use crate::error::{msg, Error, Result};
use crate::import::is_ancestor;
use crate::meta::{write_atomic, ROOT_ZONE};
use crate::p4_marshal;
use crate::repo::Repo;
use crate::treeops::{self, Flat};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::names::{validate_branch_name, validate_path};
use vcs_objects::{ChangeId, Commit, EntryMode, Object, ObjectId, RefUpdate, Signature, Transaction};
use vcs_store::ObjectStore;

const MAP_FILE: &str = "p4-import";

/// Depot-to-zone path mapping, in the spirit of a client view. One rule per line:
/// `//depot/proj/... app/...` maps a subtree; `-//depot/proj/secret/...` excludes one. A line
/// may map a single file (`//depot/a.txt docs/a.txt`). Later rules win. A right-hand side of
/// the form `//client/app/...` has its `//client/` stripped. Blank lines and `#` comments are
/// ignored. Only a trailing `...` wildcard is supported (no `*`, `%%1`, or quoted paths).
#[derive(Debug, Clone)]
pub struct P4View {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    exclude: bool,
    depot: String,
    local: String,
    tree: bool,
}

impl P4View {
    pub fn parse(text: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (exclude, line) = match line.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, line.strip_prefix('+').unwrap_or(line)),
            };
            let mut it = line.split_whitespace();
            let (Some(l), r) = (it.next(), it.next()) else {
                return msg(format!("view line {}: expected `<depot> <path>`", n + 1));
            };
            if it.next().is_some() || (r.is_none() && !exclude) {
                return msg(format!("view line {}: expected `<depot> <path>`", n + 1));
            }
            if !l.starts_with("//") {
                return msg(format!("view line {}: depot path must start with //", n + 1));
            }
            let tree = l.ends_with("...");
            let depot = if tree { l.trim_end_matches("...") } else { l };
            if depot.contains("...") || depot.contains('*') || depot.contains("%%") {
                return msg(format!("view line {}: only a trailing ... wildcard is supported", n + 1));
            }
            let local = match r {
                None => String::new(),
                Some(r) => {
                    let mut r = r;
                    if r.starts_with("//") {
                        r = r[2..].split_once('/').map_or("", |x| x.1);
                    }
                    if r.contains('*') || r.contains("%%") {
                        return msg(format!("view line {}: unsupported wildcard", n + 1));
                    }
                    if r.ends_with("...") != tree && !exclude {
                        return msg(format!("view line {}: both sides must end in ... or neither", n + 1));
                    }
                    r.trim_end_matches("...").trim_matches('/').to_owned()
                }
            };
            rules.push(Rule {
                exclude,
                depot: depot.to_owned(),
                local,
                tree,
            });
        }
        if rules.iter().all(|r| r.exclude) {
            return msg("view has no mapping rules");
        }
        Ok(P4View { rules })
    }

    /// Map a depot path; `None` if unmapped or excluded.
    pub fn map(&self, depot_path: &str) -> Option<String> {
        let mut out = None;
        for r in &self.rules {
            let hit = if r.tree {
                depot_path.starts_with(&r.depot)
            } else {
                depot_path == r.depot
            };
            if !hit {
                continue;
            }
            out = if r.exclude {
                None
            } else {
                let rest = if r.tree { &depot_path[r.depot.len()..] } else { "" };
                Some(match (r.local.is_empty(), r.tree) {
                    (true, _) => rest.to_owned(),
                    (false, true) => format!("{}/{rest}", r.local),
                    (false, false) => r.local.clone(),
                })
            };
        }
        out
    }
}

#[derive(Debug, Default)]
pub struct P4Report {
    pub changes_imported: usize,
    /// Already in `.vcs/p4-import`.
    pub changes_skipped: usize,
    /// Pending/shelved changelists, or ones with no file inside the view.
    pub changes_not_imported: Vec<u64>,
    pub objects_written: usize,
    pub head: Option<ObjectId>,
    pub paths_skipped: Vec<String>,
    pub type_warnings: Vec<String>,
}

struct FileRec {
    depot: String,
    action: String,
    ftype: String,
}

struct Change {
    n: u64,
    user: String,
    time: u64,
    desc: String,
    files: Vec<FileRec>,
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error::Io { path: path.into(), source }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// "2024/01/02" or "2024/01/02 12:34:56" as UTC epoch seconds.
fn parse_date(date: &str, time: Option<&str>) -> Result<i64> {
    let bad = || Error::Msg(format!("bad p4 date {date:?}"));
    let mut it = date.split('/').map(|x| x.parse::<i64>());
    let (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) = (it.next(), it.next(), it.next()) else {
        return Err(bad());
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1970..=9999).contains(&y) {
        return Err(bad());
    }
    let mut secs = 0;
    if let Some(t) = time {
        let p: Vec<i64> = t.split(':').filter_map(|x| x.parse().ok()).collect();
        if p.len() != 3 {
            return Err(bad());
        }
        secs = p[0] * 3600 + p[1] * 60 + p[2];
    }
    Ok(days_from_civil(y, m, d) * 86400 + secs)
}

/// Description lines: strip one leading tab; drop surrounding blank lines.
fn clean_desc(lines: &[&str]) -> String {
    let v: Vec<&str> = lines.iter().map(|l| l.strip_prefix('\t').unwrap_or(l)).collect();
    v.join("\n").trim().to_owned()
}

/// `p4 changes -l`: (number, user, time, desc) for submitted changes; pending ones are returned
/// with `pending = true`.
fn parse_changes_text(text: &str) -> Result<Vec<(u64, bool)>> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Change ") {
            let n = rest
                .split_whitespace()
                .next()
                .and_then(|x| x.parse().ok())
                .ok_or_else(|| Error::Msg(format!("bad changes line {line:?}")))?;
            out.push((n, rest.contains("*pending*")));
        }
    }
    Ok(out)
}

fn parse_describe_text(n: u64, text: &str) -> Result<Change> {
    let lines: Vec<&str> = text.lines().collect();
    let hdr = lines.first().copied().unwrap_or("");
    let w: Vec<&str> = hdr.split_whitespace().collect();
    // Change N by user@client on DATE TIME [*pending*]
    if w.len() < 6 || w[0] != "Change" || w[2] != "by" || w[4] != "on" || w[1].parse::<u64>().ok() != Some(n) {
        return msg(format!("describe {n}: unexpected header {hdr:?}"));
    }
    if hdr.contains("*pending*") {
        return msg(format!("describe {n}: pending changelist"));
    }
    let user = w[3].split('@').next().unwrap_or(w[3]).to_owned();
    let time = parse_date(w[5], w.get(6).copied().filter(|t| t.contains(':')))? as u64;
    let mut desc_lines = Vec::new();
    let mut files = Vec::new();
    let mut in_files = false;
    for l in &lines[1..] {
        if l.starts_with("Affected files ...") {
            in_files = true;
        } else if l.starts_with("Jobs fixed ...") || l.starts_with("Differences ...") || l.starts_with("Moved files ...") {
            in_files = false;
        } else if in_files {
            if let Some(r) = l.strip_prefix("... ") {
                let (path_rev, action) = r
                    .rsplit_once(' ')
                    .ok_or_else(|| Error::Msg(format!("describe {n}: bad file line {l:?}")))?;
                let depot = path_rev.rsplit_once('#').map_or(path_rev, |x| x.0);
                files.push(FileRec {
                    depot: depot.to_owned(),
                    action: action.to_owned(),
                    ftype: "text".into(),
                });
            }
        } else if l.starts_with('\t') || (l.is_empty() && !desc_lines.is_empty()) {
            desc_lines.push(*l);
        }
    }
    Ok(Change {
        n,
        user,
        time,
        desc: clean_desc(&desc_lines),
        files,
    })
}

fn parse_describe_marshal(n: u64, bytes: &[u8]) -> Result<Change> {
    let vals = p4_marshal::parse_all(bytes)?;
    let d = vals
        .iter()
        .find(|v| v.get_text("change").is_some())
        .ok_or_else(|| Error::Msg(format!("describe {n}: no change record")))?;
    if d.get_text("change").and_then(|c| c.parse::<u64>().ok()) != Some(n) {
        return msg(format!("describe {n}: record is for another change"));
    }
    if d.get_text("status").as_deref().is_some_and(|s| s != "submitted") {
        return msg(format!("describe {n}: not submitted"));
    }
    let time = d
        .get_text("time")
        .and_then(|t| t.parse().ok())
        .ok_or_else(|| Error::Msg(format!("describe {n}: missing time")))?;
    let mut files = Vec::new();
    for i in 0.. {
        let Some(depot) = d.get_text(&format!("depotFile{i}")) else {
            break;
        };
        files.push(FileRec {
            depot,
            action: d.get_text(&format!("action{i}")).unwrap_or_default(),
            ftype: d.get_text(&format!("type{i}")).unwrap_or_else(|| "text".into()),
        });
    }
    Ok(Change {
        n,
        user: d.get_text("user").unwrap_or_else(|| "unknown".into()),
        time,
        desc: d.get_text("desc").unwrap_or_default().trim().to_owned(),
        files,
    })
}

/// (mode, warning) for a Perforce file type string such as `text+x`, `xtext`, `symlink`.
fn file_type(t: &str) -> (EntryMode, Option<String>) {
    let (base, mods) = t.split_once('+').unwrap_or((t, ""));
    let legacy_x = matches!(
        base,
        "xtext" | "xbinary" | "xtempobj" | "xunicode" | "xutf16" | "xutf8" | "cxtext" | "kxtext"
    );
    let legacy_k = matches!(base, "ktext" | "kxtext");
    let mode = if base == "symlink" {
        EntryMode::Symlink
    } else if legacy_x || mods.contains('x') {
        EntryMode::Executable
    } else {
        EntryMode::File
    };
    let warn = if base.contains("apple") || base.contains("resource") {
        Some("resource fork / apple file is stored as the bytes p4 print returned")
    } else if base.contains("utf16") {
        Some("utf16 content is stored as the bytes p4 print returned")
    } else if legacy_k || mods.contains('k') {
        Some("keyword expansion (+k) is not undone; expanded text is stored")
    } else {
        None
    };
    (mode, warn.map(str::to_owned))
}

fn p4_change_id(n: u64) -> ChangeId {
    let mut h = blake3::Hasher::new_derive_key("all-git 2026 p4 import change id v1");
    h.update(format!("p4:{n}:{ROOT_ZONE}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h.finalize().as_bytes()[..16]);
    ChangeId::from_bytes(b)
}

fn load_changes(dir: &Path) -> Result<Vec<Change>> {
    let mut numbers: Vec<u64> = Vec::new();
    let cp = dir.join("changes.txt");
    match fs::read_to_string(&cp) {
        Ok(t) => numbers.extend(parse_changes_text(&t)?.into_iter().filter(|c| !c.1).map(|c| c.0)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let dp = dir.join("describe");
            for e in fs::read_dir(&dp).map_err(|e| io(&dp, e))? {
                let name = e.map_err(|e| io(&dp, e))?.file_name().to_string_lossy().into_owned();
                if let Some(n) = name.split('.').next().and_then(|x| x.parse().ok()) {
                    numbers.push(n);
                }
            }
        }
        Err(e) => return Err(io(&cp, e)),
    }
    numbers.sort_unstable();
    numbers.dedup();
    let mut out = Vec::new();
    for n in numbers {
        let m = dir.join("describe").join(format!("{n}.marshal"));
        let t = dir.join("describe").join(format!("{n}.txt"));
        let c = if m.exists() {
            parse_describe_marshal(n, &fs::read(&m).map_err(|e| io(&m, e))?)?
        } else if t.exists() {
            parse_describe_text(n, &fs::read_to_string(&t).map_err(|e| io(&t, e))?)?
        } else {
            return msg(format!("no describe output for change {n} in {}", dir.display()));
        };
        out.push(c);
    }
    Ok(out)
}

impl Repo {
    /// Import recorded Perforce output from `dir` onto branch `branch` of the root zone.
    /// See the module docs for the layout; fixture-tested only.
    pub fn import_p4(&self, dir: &Path, view: &P4View, branch: &str) -> Result<P4Report> {
        validate_branch_name(branch)?;
        let changes = load_changes(dir)?;
        let _l = self.lock()?;
        let mp = self.vcs.join(MAP_FILE);
        let mut map: BTreeMap<String, ObjectId> = BTreeMap::new();
        match fs::read_to_string(&mp) {
            Ok(t) => {
                for l in t.lines() {
                    let (hex, key) = l.split_once(' ').ok_or_else(|| Error::Corrupt("p4-import: bad line".into()))?;
                    let id = ObjectId::from_hex(hex).map_err(|_| Error::Corrupt("p4-import: bad id".into()))?;
                    map.insert(key.to_owned(), id);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io(&mp, e)),
        }
        let mut refs = self.refs()?;
        let old_head = refs.head(ROOT_ZONE, branch);
        let mut tip = old_head;
        let mut flat: Option<Flat> = None;
        let mut rep = P4Report::default();
        let mut new_keys = false;

        for ch in &changes {
            let key = format!("p4:{}:{ROOT_ZONE}", ch.n);
            if let Some(id) = map.get(&key) {
                if self.store.contains(id)? {
                    rep.changes_skipped += 1;
                    continue;
                }
            }
            // collect operations inside the view first; a change with none produces no commit
            struct Op {
                path: String,
                body: Option<(EntryMode, Vec<u8>)>,
            }
            let mut ops = Vec::new();
            for f in &ch.files {
                let Some(path) = view.map(&f.depot) else { continue };
                if validate_path(&path).is_err() {
                    rep.paths_skipped.push(format!("{} (change {})", f.depot, ch.n));
                    continue;
                }
                match f.action.as_str() {
                    "delete" | "move/delete" | "purge" | "archive" => ops.push(Op { path, body: None }),
                    "add" | "edit" | "branch" | "integrate" | "import" | "move/add" => {
                        let (mode, warn) = file_type(&f.ftype);
                        if let Some(w) = warn {
                            rep.type_warnings.push(format!("{}: {} ({w})", f.depot, f.ftype));
                        }
                        let rel = f.depot.trim_start_matches('/');
                        if rel.split('/').any(|c| c.is_empty() || c == "..") {
                            return msg(format!("change {}: unsafe depot path {:?}", ch.n, f.depot));
                        }
                        let pp = dir.join("print").join(ch.n.to_string()).join(rel);
                        let mut data = fs::read(&pp).map_err(|e| io(&pp, e))?;
                        if mode == EntryMode::Symlink && data.last() == Some(&b'\n') {
                            data.pop(); // fixture files end in a newline; a link target never does
                        }
                        ops.push(Op {
                            path,
                            body: Some((mode, data)),
                        });
                    }
                    other => return msg(format!("change {}: unsupported action {other:?} on {}", ch.n, f.depot)),
                }
            }
            if ops.is_empty() {
                rep.changes_not_imported.push(ch.n);
                continue;
            }
            let mut fl = match flat.take() {
                Some(f) => f,
                None => match tip {
                    Some(t) => treeops::flatten(&self.store, Some(self.commit_obj(&t)?.root), "")?,
                    None => Flat::new(),
                },
            };
            for op in ops {
                let dir_prefix = format!("{}/", op.path);
                fl.retain(|k, _| !k.starts_with(&dir_prefix)); // a file replaces a directory
                match op.body {
                    None => {
                        fl.remove(&op.path);
                    }
                    Some((mode, data)) => {
                        let id = vcs_objects::content_id(&data);
                        if !self.store.contains(&id)? {
                            rep.objects_written += 1;
                        }
                        self.store.put_content(&data)?;
                        // a file may not sit below an existing file
                        let mut anc = op.path.as_str();
                        while let Some((p, _)) = anc.rsplit_once('/') {
                            fl.remove(p);
                            anc = p;
                        }
                        fl.insert(op.path, (mode, id));
                    }
                }
            }
            let root = treeops::build(&self.store, fl.iter().map(|(p, (m, id))| (p.as_str(), *m, *id)))?;
            let message = format!("{}\n\nPerforce-Change: {}\n", ch.desc, ch.n);
            let commit = Object::Commit(Commit {
                zone: ROOT_ZONE.into(),
                parents: tip.into_iter().collect(),
                root,
                change_id: p4_change_id(ch.n),
                author: Signature {
                    name: ch.user.clone(),
                    email: format!("{}@perforce", ch.user),
                    timestamp: ch.time,
                },
                message,
            });
            if Object::decode(&commit.encode()).is_err() {
                return msg(format!("change {}: commit exceeds object limits", ch.n));
            }
            let id = commit.id();
            if !self.store.contains(&id)? {
                rep.objects_written += 1;
            }
            self.store.put(&commit)?;
            map.insert(key, id);
            new_keys = true;
            tip = Some(id);
            flat = Some(fl);
            rep.changes_imported += 1;
        }

        rep.head = tip;
        if tip != old_head {
            let new = tip.expect("a commit was made");
            let ws = self.workspace()?;
            let zones = self.zones()?;
            let checked_out = ws.subs.get(ROOT_ZONE).map(String::as_str) == Some(branch);
            if let Some(o) = old_head {
                if checked_out {
                    return msg(format!(
                        "branch {branch:?} is checked out and already has history; import onto another --branch"
                    ));
                }
                if !is_ancestor(self, &o, &new)? {
                    return msg(format!("branch {branch:?} has local history that is not part of the import"));
                }
            }
            let update = RefUpdate {
                zone: ROOT_ZONE.into(),
                branch: branch.to_owned(),
                old: old_head,
                new,
            };
            let mut ix = None;
            if old_head.is_none() && checked_out {
                let st = self.status()?;
                self.require_clean(ROOT_ZONE, &st)?;
                let newf = treeops::flatten(&self.store, Some(self.commit_obj(&new)?.root), "")?;
                let mut i = self.index()?;
                let oldf = self.head_flat(ROOT_ZONE, &zones, &ws, &refs)?;
                self.transition(ROOT_ZONE, &zones, &oldf, &newf, &mut i)?;
                ix = Some(i);
            }
            self.store.sync()?;
            if new_keys {
                let mut t = String::new();
                for (k, id) in &map {
                    t.push_str(&format!("{} {k}\n", id.to_hex()));
                }
                write_atomic(&mp, t.as_bytes())?;
            }
            let author = Signature {
                name: "vcs import-p4".into(),
                email: "import@localhost".into(),
                timestamp: self.commit_obj(&new)?.author.timestamp,
            };
            let message = format!("import perforce history ({} change(s))", rep.changes_imported);
            self.store.put(&Object::Transaction(Transaction {
                updates: vec![update.clone()],
                author: author.clone(),
                message: message.clone(),
                lock_ops: vec![],
            }))?;
            refs.apply(&[update])?;
            self.store.sync()?;
            if let Some(mut i) = ix {
                self.save_index(&mut i)?;
            }
            write_atomic(&self.vcs.join("refs"), &refs.encode())?;
            self.record_op(&message, Some(author))?;
        }
        Ok(rep)
    }
}
