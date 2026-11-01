//! Machine-readable `--json` output for the read commands. Schemas: docs/cli.md.
//!
//! Every document is one object on one line: `{"version":1,"command":"<name>",...}`.
//! Nothing but the document is written to stdout; errors keep going to stderr.
use crate::{open, zone_of_cwd, Cmd, ConflictCmd, OpCmd, ZoneCmd};
use std::fmt::Write as _;
use vcs_repo::repo::Change;
use vcs_repo::{ChangeKind, Error, Repo};

/// Bump when an existing field changes meaning or disappears (adding fields is not a bump).
pub const SCHEMA_VERSION: u32 = 1;

/// Minimal JSON value (the workspace has no serde; keeps the build offline).
pub enum J {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }
    fn opt(v: Option<String>) -> J {
        v.map_or(J::Null, J::Str)
    }
    pub fn render(&self) -> String {
        let mut o = String::new();
        self.write(&mut o);
        o
    }
    fn write(&self, o: &mut String) {
        match self {
            J::Null => o.push_str("null"),
            J::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => {
                let _ = write!(o, "{n}");
            }
            J::Str(s) => {
                o.push('"');
                for c in s.chars() {
                    match c {
                        '"' => o.push_str("\\\""),
                        '\\' => o.push_str("\\\\"),
                        '\n' => o.push_str("\\n"),
                        '\r' => o.push_str("\\r"),
                        '\t' => o.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(o, "\\u{:04x}", c as u32);
                        }
                        c => o.push(c),
                    }
                }
                o.push('"');
            }
            J::Arr(v) => {
                o.push('[');
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        o.push(',');
                    }
                    x.write(o);
                }
                o.push(']');
            }
            J::Obj(v) => {
                o.push('{');
                for (i, (k, x)) in v.iter().enumerate() {
                    if i > 0 {
                        o.push(',');
                    }
                    J::s(*k).write(o);
                    o.push(':');
                    x.write(o);
                }
                o.push('}');
            }
        }
    }
}

pub fn doc(command: &'static str, mut fields: Vec<(&'static str, J)>) -> J {
    let mut v = vec![("version", J::Num(SCHEMA_VERSION.into())), ("command", J::s(command))];
    v.append(&mut fields);
    J::Obj(v)
}

fn kind(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
    }
}

fn changes(v: &[Change]) -> J {
    J::Arr(
        v.iter()
            .map(|c| J::Obj(vec![("path", J::s(&c.path)), ("kind", J::s(kind(c.kind)))]))
            .collect(),
    )
}

fn conflict_list(v: &[vcs_repo::merge::ConflictRec]) -> J {
    J::Arr(
        v.iter()
            .map(|c| {
                J::Obj(vec![
                    ("path", J::s(&c.path)),
                    ("kind", J::s(vcs_repo::merge::kind_name(c.kind))),
                    ("resolved", J::Bool(c.resolved)),
                ])
            })
            .collect(),
    )
}

fn branches(repo: &Repo, zone: &str) -> Result<J, Error> {
    Ok(J::Arr(
        repo.branches(zone)?
            .into_iter()
            .map(|(b, id, cur)| {
                J::Obj(vec![
                    ("name", J::s(b)),
                    ("head", J::opt(id.map(|i| i.to_string()))),
                    ("current", J::Bool(cur)),
                ])
            })
            .collect(),
    ))
}

/// Per-file added/deleted line counts parsed from a unified diff produced by `Repo::diff`.
pub struct FileStat {
    pub path: String,
    pub added: u64,
    pub deleted: u64,
    pub binary: bool,
}

pub fn diff_stat(diff: &str) -> Vec<FileStat> {
    let mut out: Vec<FileStat> = Vec::new();
    let mut header = 0; // lines of ---/+++ still to skip for the current file
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("diff --vcs a/") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(a, _)| a);
            out.push(FileStat {
                path: path.into(),
                added: 0,
                deleted: 0,
                binary: false,
            });
            header = 0;
        } else if let Some(f) = out.last_mut() {
            if l.starts_with("Binary files ") && header == 0 {
                f.binary = true;
            } else if header == 0 && l.starts_with("--- ") {
                header = 1;
            } else if header == 1 && l.starts_with("+++ ") {
                header = 2;
            } else if header == 2 {
                if l.starts_with('+') {
                    f.added += 1;
                } else if l.starts_with('-') {
                    f.deleted += 1;
                }
            }
        }
    }
    out
}

pub fn stat_json(stats: &[FileStat], staged: bool) -> J {
    doc(
        "diff",
        vec![
            ("staged", J::Bool(staged)),
            (
                "files",
                J::Arr(
                    stats
                        .iter()
                        .map(|s| {
                            J::Obj(vec![
                                ("path", J::s(&s.path)),
                                ("added", J::Num(s.added)),
                                ("deleted", J::Num(s.deleted)),
                                ("binary", J::Bool(s.binary)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("added", J::Num(stats.iter().map(|s| s.added).sum())),
            ("deleted", J::Num(stats.iter().map(|s| s.deleted).sum())),
        ],
    )
}

/// Build the JSON document for `cmd`, or `None` when the command has no JSON form.
pub fn build(cmd: &Cmd) -> Option<Result<J, Error>> {
    match cmd {
        Cmd::Status | Cmd::Log { .. } | Cmd::Branch { .. } | Cmd::Diff { .. } | Cmd::Zone(ZoneCmd::List) => {}
        Cmd::Conflict(ConflictCmd::List) | Cmd::Op(OpCmd::Log { .. }) => {}
        _ => return None,
    }
    Some(build_inner(cmd))
}

fn build_inner(cmd: &Cmd) -> Result<J, Error> {
    let (repo, cwd) = open()?;
    Ok(match cmd {
        Cmd::Status => {
            let st = repo.status()?;
            let defs = repo.zones()?;
            let zones = st
                .zones
                .iter()
                .map(|a| {
                    J::Obj(vec![
                        ("zone", J::s(defs.label(&a.zone))),
                        ("zone_id", J::s(&a.zone)),
                        ("path", J::s(&a.path)),
                        ("branch", J::s(&a.branch)),
                        ("head", J::opt(a.head.map(|h| h.to_string()))),
                        ("staged", changes(&a.staged)),
                        ("unstaged", changes(&a.unstaged)),
                    ])
                })
                .collect();
            let strs = |v: &[String]| J::Arr(v.iter().map(J::s).collect());
            let merging = match &st.merging {
                Some(m) => J::Obj(vec![
                    ("zone", J::s(&m.zone)),
                    ("branch", J::s(&m.branch)),
                    ("theirs", J::s(&m.theirs_label)),
                    ("conflicts", conflict_list(&m.conflicts)),
                ]),
                None => J::Null,
            };
            let clean = st.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && st.untracked.is_empty();
            doc(
                "status",
                vec![
                    ("zones", J::Arr(zones)),
                    ("untracked", strs(&st.untracked)),
                    ("unsubscribed", strs(&st.unsubscribed)),
                    ("unsupported", strs(&st.unsupported)),
                    ("merging", merging),
                    ("rebasing", J::Bool(repo.rebase_state()?.is_some())),
                    ("clean", J::Bool(clean)),
                ],
            )
        }
        Cmd::Log {
            zone,
            limit,
            branch,
            oneline,
            graph,
            author,
            paths,
            x,
            ..
        } => {
            let zone = zone_of_cwd(&repo, &cwd, zone.clone())?;
            let defs = repo.zones()?;
            let o = crate::logfmt::Opts {
                oneline: *oneline,
                graph: *graph,
                author: author.clone(),
                color: false,
                paths: paths.clone(),
                x: x.clone(),
            };
            let commits = crate::logfmt::select(&repo, &cwd, &zone, branch.as_deref(), *limit, &o)?
                .0
                .into_iter()
                .map(|(id, c)| {
                    J::Obj(vec![
                        ("id", J::s(id.to_string())),
                        ("change_id", J::s(c.change_id.to_string())),
                        ("zone", J::s(defs.label(&c.zone))),
                        (
                            "author",
                            J::Obj(vec![
                                ("name", J::s(&c.author.name)),
                                ("email", J::s(&c.author.email)),
                                ("timestamp", J::Num(c.author.timestamp)),
                            ]),
                        ),
                        ("message", J::s(&c.message)),
                    ])
                })
                .collect();
            doc("log", vec![("zone", J::s(defs.label(&zone))), ("commits", J::Arr(commits))])
        }
        Cmd::Diff { staged, .. } => stat_json(&diff_stat(&repo.diff(*staged)?), *staged),
        Cmd::Branch { name, zone, .. } => {
            if name.is_some() {
                return Err(Error::Msg("--json only applies to listing branches".into()));
            }
            let zone = zone_of_cwd(&repo, &cwd, zone.clone())?;
            doc("branch", vec![("zone", J::s(&zone)), ("branches", branches(&repo, &zone)?)])
        }
        Cmd::Zone(_) => {
            let zones = repo.zones()?;
            let ws = repo.workspace()?;
            let refs = repo.refs()?;
            let list = zones
                .names()
                .map(|n| {
                    J::Obj(vec![
                        ("name", J::s(zones.label(n))),
                        ("id", J::s(n)),
                        ("path", J::s(zones.path_of(n).unwrap_or(""))),
                        ("subscribed", J::Bool(ws.subs.contains_key(n))),
                        ("branch", J::opt(ws.subs.get(n).cloned())),
                        ("branch_count", J::Num(refs.branches_of(n).len() as u64)),
                    ])
                })
                .collect();
            doc("zone-list", vec![("zones", J::Arr(list))])
        }
        Cmd::Conflict(_) => doc("conflict-list", vec![("conflicts", conflict_list(&repo.conflicts()?))]),
        Cmd::Op(OpCmd::Log { limit }) => {
            let head = repo.op_head()?;
            let ops = repo
                .op_log(*limit)?
                .into_iter()
                .map(|(id, op)| {
                    J::Obj(vec![
                        ("id", J::s(id.to_string())),
                        ("current", J::Bool(Some(id) == head)),
                        ("description", J::s(&op.description)),
                        (
                            "author",
                            J::Obj(vec![
                                ("name", J::s(&op.author.name)),
                                ("email", J::s(&op.author.email)),
                                ("timestamp", J::Num(op.author.timestamp)),
                            ]),
                        ),
                    ])
                })
                .collect();
            doc("op-log", vec![("operations", J::Arr(ops))])
        }
        _ => unreachable!("filtered by build()"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_renders() {
        let j = J::Obj(vec![
            ("a", J::s("q\"\\\n\u{1}é")),
            ("b", J::Arr(vec![J::Null, J::Bool(true), J::Num(3)])),
        ]);
        assert_eq!(j.render(), r#"{"a":"q\"\\\n\u0001é","b":[null,true,3]}"#);
    }

    #[test]
    fn stat_counts_lines() {
        let d =
            "diff --vcs a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n-old\n+new\n+more\ndiff --vcs a/b b/b\nBinary files a/b and b/b differ\n";
        let s = diff_stat(d);
        assert_eq!((s[0].path.as_str(), s[0].added, s[0].deleted), ("x", 2, 1));
        assert!(s[1].binary);
    }
}
