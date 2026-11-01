//! `vcs bundle create|verify` and the bundle paths of `vcs clone` / `vcs fetch`.
//! The format and its checks live in `vcs_repo::bundle` (docs/bundle.md).

use clap::Subcommand;
use std::path::{Path, PathBuf};
use vcs_objects::Signature;
use vcs_repo::bundle::{self, BundleInfo};
use vcs_repo::{Error, Repo};

#[derive(Subcommand, Clone)]
pub enum BundleCmd {
    /// Write a bundle of branches or zones (default: every branch) to a file
    Create {
        file: PathBuf,
        /// `zone` (all its branches), `zone/branch`, or a root-zone branch name
        selectors: Vec<String>,
        /// Leave out everything reachable from this revision; it becomes a prerequisite
        #[arg(long)]
        since: Option<String>,
    },
    /// Check a bundle's hashes, structure and completeness (needs no repository)
    Verify { file: PathBuf },
}

fn describe(info: &BundleInfo) {
    println!(
        "bundle v{}: {} object(s), {} byte(s), pack format {}",
        info.version, info.objects, info.bytes, info.pack_format
    );
    for h in &info.heads {
        println!("  {}/{} @ {}", h.zone, h.branch, h.id.short());
    }
    for p in &info.prerequisites {
        println!("  requires commit {}", p.short());
    }
}

pub fn run(cmd: BundleCmd) -> Result<(), Error> {
    match cmd {
        BundleCmd::Create { file, selectors, since } => {
            let cwd = std::env::current_dir().map_err(|source| Error::Io { path: ".".into(), source })?;
            let repo = Repo::discover(&cwd)?;
            let info = repo.bundle_create(&file, &selectors, since.as_deref())?;
            println!("wrote {}", file.display());
            describe(&info);
        }
        BundleCmd::Verify { file } => {
            let info = bundle::verify_file(&file)?;
            describe(&info);
            if let Some(repo) = std::env::current_dir().ok().and_then(|c| Repo::discover(&c).ok()) {
                let missing = repo.bundle_missing_prerequisites(&file, &info)?;
                if missing.is_empty() {
                    println!("prerequisites: all present in this repository");
                } else {
                    for m in missing {
                        println!("prerequisite missing here: {}", m.short());
                    }
                }
            }
            println!("ok");
        }
    }
    Ok(())
}

/// `vcs clone <bundle> <dir>`; returns false when `src` is not a bundle file.
pub fn try_clone(src: &str, dir: &Path, zones: &[String], author: Signature) -> Result<bool, Error> {
    let p = Path::new(src);
    if !p.is_file() || !bundle::is_bundle_file(p) {
        return Ok(false);
    }
    if !zones.is_empty() {
        return Err(Error::Msg(
            "--zone is not supported when cloning a bundle (a bundle applies all of its zones)".into(),
        ));
    }
    Repo::clone_from_bundle(p, dir, author)?;
    let info = bundle::verify_file(p)?;
    println!("Cloned bundle {src} into {}", dir.display());
    describe(&info);
    Ok(true)
}

/// `vcs fetch <bundle>`.
pub fn fetch(repo: &Repo, file: &Path, author: Signature) -> Result<(), Error> {
    let rep = repo.bundle_fetch(file, author)?;
    if rep.updated.is_empty() {
        println!("nothing new in the bundle");
    }
    for (z, b, _, new) in rep.updated {
        println!("fetched {z}/{b} @ {}", new.short());
    }
    Ok(())
}
