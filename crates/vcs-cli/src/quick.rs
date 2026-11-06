//! Argument fast path for the cheapest everyday commands.
//!
//! `Cli::parse_from` builds clap's whole command tree (every subcommand and flag, about 1 MB of
//! heap and 300 page faults) before it looks at argv, which costs more than git's entire run for
//! `branch`, `tag`, `stash list` and the like. For a handful of exact argv shapes this module
//! builds the same `Cli` value by hand. Anything it does not recognise (a flag it does not know,
//! a leading global option, non-UTF-8, an argument starting with `-`) returns `None` and the
//! normal clap path runs, so error messages and `--help` are unchanged. The tests assert that
//! every shape yields exactly the value clap yields.

use crate::cmds::More;
use crate::plumb::{CatFileArgs, LsFilesArgs};
use crate::{Cli, Cmd, OpCmd, StashCmd};
use std::ffi::OsString;
use std::path::PathBuf;

fn cli(cmd: Cmd) -> Cli {
    Cli {
        chdir: None,
        json: false,
        ca_pem: None,
        no_hooks: false,
        cmd,
    }
}

/// A positional argument: anything that clap would take as a value, here only strings that do not
/// start with `-` (so no option, no `--`, no bare `-`) and are not empty.
fn pos(s: &str) -> Option<String> {
    (!s.is_empty() && !s.starts_with('-')).then(|| s.to_owned())
}

fn branch(name: Option<String>, start: Option<String>, delete: bool, rename: bool, force_move: bool) -> Cmd {
    Cmd::Branch {
        name,
        start,
        zone: None,
        delete,
        force: false,
        at: None,
        rename,
        force_move,
        remotes: false,
    }
}

fn tag(name: Option<String>, rev: Option<String>, message: Option<String>, delete: bool) -> Cmd {
    Cmd::Tag {
        name,
        rev,
        zone: None,
        message,
        signature: None,
        sign: false,
        delete,
        force: false,
    }
}

fn show(rev: Option<String>) -> Cmd {
    Cmd::Show {
        rev,
        zone: None,
        opts: crate::difftool::DiffArgs {
            rename_threshold: 50, // clap's default_value_t
            ..Default::default()
        },
    }
}

fn cat_file(flag: &str, object: &str) -> Option<Cmd> {
    Some(Cmd::More(More::CatFile(CatFileArgs {
        object: pos(object)?,
        batch: false,
        batch_check: false,
        type_: flag == "-t",
        size: flag == "-s",
        print: flag == "-p",
        zone: None,
    })))
}

fn ls_files(stage: bool, rev: Option<String>) -> Cmd {
    Cmd::More(More::LsFiles(LsFilesArgs {
        stage,
        rev,
        depth: None,
        others: false,
        paths: Vec::new(),
    }))
}

/// The `Cli` clap would build for `argv[1..]`, for the shapes listed in the tests; else `None`.
pub fn parse(argv: &[OsString]) -> Option<Cli> {
    let strs: Vec<&str> = argv.iter().skip(1).map(|a| a.to_str()).collect::<Option<_>>()?;
    let cmd = match strs.as_slice() {
        ["init"] => Cmd::Init { path: None },
        ["init", p] => Cmd::Init {
            path: Some(PathBuf::from(pos(p)?)),
        },
        ["branch"] => branch(None, None, false, false, false),
        ["branch", "-d", n] => branch(Some(pos(n)?), None, true, false, false),
        ["branch", "-m", n] => branch(Some(pos(n)?), None, false, true, false),
        ["branch", "-m", a, b] => branch(Some(pos(a)?), Some(pos(b)?), false, true, false),
        ["branch", "-M", n] => branch(Some(pos(n)?), None, false, false, true),
        ["branch", "-M", a, b] => branch(Some(pos(a)?), Some(pos(b)?), false, false, true),
        ["branch", n] => branch(Some(pos(n)?), None, false, false, false),
        ["branch", n, s] => branch(Some(pos(n)?), Some(pos(s)?), false, false, false),
        ["tag"] => tag(None, None, None, false),
        ["tag", "-d", n] => tag(Some(pos(n)?), None, None, true),
        ["tag", n] => tag(Some(pos(n)?), None, None, false),
        ["tag", n, "-m", m] => tag(Some(pos(n)?), None, Some(pos(m)?), false),
        ["show"] => show(None),
        ["show", r] => show(Some(pos(r)?)),
        ["cat-file", f @ ("-p" | "-t" | "-s"), o] => cat_file(f, o)?,
        ["ls-files"] => ls_files(false, None),
        ["ls-files", "-s"] => ls_files(true, None),
        ["ls-files", "--rev", r] => ls_files(false, Some(pos(r)?)),
        ["ls-files", "-s", "--rev", r] => ls_files(true, Some(pos(r)?)),
        ["stash", "list"] => Cmd::Stash(StashCmd::List),
        ["worktree", "list"] => Cmd::Worktree(crate::worktree_cmd::WorktreeCmd::List),
        ["op", "log"] => Cmd::Op(OpCmd::Log { limit: 20 }),
        _ => return None,
    };
    Some(cli(cmd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Every field of the variants the fast path can produce (anything else: "other").
    fn sig(c: &Cli) -> String {
        let head = format!("{:?}{}{:?}{}", c.chdir, c.json, c.ca_pem, c.no_hooks);
        let body = match &c.cmd {
            Cmd::Init { path } => format!("init {path:?}"),
            Cmd::Branch {
                name,
                start,
                zone,
                delete,
                force,
                at,
                rename,
                force_move,
                remotes,
            } => format!("branch {name:?} {start:?} {zone:?} {delete} {force} {at:?} {rename} {force_move} {remotes}"),
            Cmd::Tag {
                name,
                rev,
                zone,
                message,
                signature,
                sign,
                delete,
                force,
            } => format!("tag {name:?} {rev:?} {zone:?} {message:?} {signature:?} {sign} {delete} {force}"),
            Cmd::Show { rev, zone, opts } => format!("show {rev:?} {zone:?} {opts:?}"),
            Cmd::More(More::CatFile(a)) => format!("cat-file {a:?}"),
            Cmd::More(More::LsFiles(a)) => format!("ls-files {a:?}"),
            Cmd::Stash(StashCmd::List) => "stash list".into(),
            Cmd::Worktree(crate::worktree_cmd::WorktreeCmd::List) => "worktree list".into(),
            Cmd::Op(OpCmd::Log { limit }) => format!("op log {limit}"),
            _ => "other".into(),
        };
        format!("{head} {body}")
    }

    fn argv(a: &[&str]) -> Vec<OsString> {
        std::iter::once("vcs").chain(a.iter().copied()).map(OsString::from).collect()
    }

    #[test]
    fn fast_path_equals_clap() {
        let shapes: &[&[&str]] = &[
            &["init"],
            &["init", "proj"],
            &["branch"],
            &["branch", "topic"],
            &["branch", "topic", "main"],
            &["branch", "-d", "topic"],
            &["branch", "-m", "new"],
            &["branch", "-m", "old", "new"],
            &["branch", "-M", "new"],
            &["branch", "-M", "old", "new"],
            &["tag"],
            &["tag", "v1"],
            &["tag", "-d", "v1"],
            &["tag", "v1", "-m", "rel"],
            &["stash", "list"],
            &["worktree", "list"],
            &["op", "log"],
            &["show"],
            &["show", "HEAD~1"],
            &["cat-file", "-p", "HEAD:f.txt"],
            &["cat-file", "-t", "HEAD"],
            &["cat-file", "-s", "HEAD:a/b"],
            &["ls-files"],
            &["ls-files", "-s"],
            &["ls-files", "--rev", "HEAD"],
            &["ls-files", "-s", "--rev", "HEAD"],
        ];
        for s in shapes {
            let fast = parse(&argv(s)).unwrap_or_else(|| panic!("no fast path for {s:?}"));
            let slow = Cli::try_parse_from(argv(s)).unwrap_or_else(|e| panic!("clap rejects {s:?}: {e}"));
            assert_eq!(sig(&fast), sig(&slow), "{s:?}");
            assert_ne!(sig(&slow).split(' ').next_back(), Some("other"), "{s:?} is not a covered variant");
        }
    }

    #[test]
    fn unknown_shapes_fall_back_to_clap() {
        for s in [
            &["branch", "-x"][..],
            &["branch", "--zone", "root"],
            &["branch", "-d"],
            &["tag", "-d", "a", "b"],
            &["tag", "v1", "-m"],
            &["init", "-h"],
            &["init", "--", "-p"],
            &["branch", "-"],
            &["branch", "--json"],
            &["stash", "list", "x"],
            &["op", "log", "-n", "5"],
            &["-C", "d", "branch"],
            &["--json", "branch"],
            &["status"],
            &["show", "--stat"],
            &["show", "a", "b"],
            &["cat-file", "-p"],
            &["cat-file", "-x", "a"],
            &["cat-file", "-p", "-t"],
            &["ls-files", "a.txt"],
            &["ls-files", "--rev"],
        ] {
            assert!(parse(&argv(s)).is_none(), "{s:?} must use clap");
        }
        assert!(parse(&[OsString::from("vcs")]).is_none());
    }
}
