//! The everyday commands that live in their own modules (`bisect`, `grep`, ...). `main.rs` only
//! carries one flattened variant (`Cmd::More`) and one call to [`dispatch`]; new commands are
//! added here and in their module.

use clap::Subcommand;
use vcs_repo::Error;

#[derive(Subcommand, Clone)]
pub enum More {
    /// Binary-search a zone's history for the commit that introduced a bug
    #[command(subcommand)]
    Bisect(crate::bisect::BisectCmd),
}

pub fn dispatch(cmd: &More) -> Result<(), Error> {
    match cmd {
        More::Bisect(c) => crate::bisect::run(c),
    }
}
