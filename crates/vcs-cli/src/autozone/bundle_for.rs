//! `vcs clone <bundle-file> --for PATH` (also `--task`, `--for-owner`): the bundle is unpacked into a
//! throwaway planning repository, the same zone selection and dependency closure as for a remote
//! runs over it, and the destination then gets only the selected zones (refs and checkout).
//! The other zones' objects and refs are kept, so `vcs zone add` can add them later.

use super::{names_of, report, select, ForArgs, View};
use crate::author;
use std::path::Path;
use vcs_repo::{bundle, Error, Repo};

/// `Ok(false)`: `src` is not a bundle file (the caller treats it as a remote URL).
pub(super) fn try_clone_for(src: &str, dir: &Path, a: &ForArgs, json: bool) -> Result<bool, Error> {
    let p = Path::new(src);
    if !p.is_file() || !bundle::is_bundle_file(p) {
        return Ok(false);
    }
    if !a.dry_run && dir.exists() && std::fs::read_dir(dir).map(|mut x| x.next().is_some()).unwrap_or(true) {
        return Err(Error::Msg(format!("destination {} exists and is not empty", dir.display())));
    }
    let tmp = tempfile::tempdir().map_err(|source| Error::Io {
        path: "tempdir".into(),
        source,
    })?;
    let plan = tmp.path().join("plan");
    let repo = Repo::bundle_plan_repo(p, &plan, author()?)?;
    let sel = {
        let mut view = View::local(&repo)?;
        let sel = select(&mut view, a)?;
        report(&view, &sel, a, json, "clone", None, a.dry_run);
        sel
    };
    drop(repo);
    if a.dry_run {
        return Ok(true);
    }
    let want = names_of(&sel);
    Repo::clone_from_bundle_zones(p, dir, author()?, &want)?;
    if !json {
        println!(
            "Cloned bundle {src} into {} with {} of the bundle's zones; the rest can be added later with `vcs zone add`.",
            dir.display(),
            want.len()
        );
    }
    Ok(true)
}
