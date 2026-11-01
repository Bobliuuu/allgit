//! The everyday commands that live in their own modules (`bisect`, `grep`, ...). `main.rs` only
//! carries one flattened variant (`Cmd::More`) and one call to [`dispatch`]; new commands are
//! added here and in their module.

use clap::Subcommand;
use vcs_repo::Error;

#[derive(Subcommand, Clone)]
pub enum More {
    /// List, install or run client-side hooks (.vcs/hooks/, see docs/hooks.md)
    #[command(subcommand)]
    Hook(crate::hooks::HookCmd),
    /// Binary-search a zone's history for the commit that introduced a bug
    #[command(subcommand)]
    Bisect(crate::bisect::BisectCmd),
    /// Search tracked files (working tree, or a revision) for a regular expression
    Grep(crate::grep::GrepArgs),
    /// Name a commit after the nearest tag behind it
    Describe(crate::describe::DescribeArgs),
    /// Write the files of a revision as a .tar or .zip archive
    Archive(crate::archive::ArchiveArgs),
    /// Resolve revisions to commit ids
    RevParse(crate::plumb::RevParseArgs),
    /// List tracked files
    LsFiles(crate::plumb::LsFilesArgs),
    /// Show the type, size or content of an object
    CatFile(crate::plumb::CatFileArgs),
    /// Summarize history by author
    Shortlog(crate::plumb::ShortlogArgs),
    /// Best common ancestor of two revisions
    MergeBase(crate::histcmds::MergeBaseArgs),
    /// List the files that differ between two revisions (A/D/M and path)
    DiffTree(crate::histcmds::DiffTreeArgs),
    /// Move the checked-out branch of a zone to another commit (--soft, --mixed, --hard); acts on one zone
    Reset(crate::everyday::ResetArgs),
    /// Delete untracked files (-n lists, -f deletes)
    Clean(crate::everyday::CleanArgs),
    /// Run a git command line as its vcs equivalent, e.g. `vcs git log --oneline` (table: docs/git-to-vcs.md; `--map` lists it)
    #[command(name = "git")]
    Git(crate::gitmap::GitArgs),
}

pub fn dispatch(cmd: &More, json: bool) -> Result<(), Error> {
    match cmd {
        More::Hook(c) => crate::hooks::run(c),
        More::Bisect(c) => crate::bisect::run(c),
        More::Grep(a) => crate::grep::run(a),
        More::Describe(a) => crate::describe::run(a),
        More::Archive(a) => crate::archive::run(a),
        More::RevParse(a) => crate::plumb::rev_parse(a, json),
        More::LsFiles(a) => crate::plumb::ls_files(a, json),
        More::CatFile(a) => crate::plumb::cat_file(a, json),
        More::Shortlog(a) => crate::plumb::shortlog(a, json),
        More::MergeBase(a) => crate::histcmds::merge_base(a, json),
        More::DiffTree(a) => crate::histcmds::diff_tree(a, json),
        More::Reset(a) => crate::everyday::reset(a),
        More::Clean(a) => crate::everyday::clean(a),
        More::Git(a) => crate::gitmap::run(&a.args, json),
    }
}
