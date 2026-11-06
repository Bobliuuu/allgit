//! `rerere`: reuse recorded conflict resolutions (docs/rerere.md).
//!
//! A text conflict is identified by its hunks: each hunk is the pair (one side, other side), the
//! two sides are put in byte order so that "ours" and "theirs" may swap, the diff3 base section is
//! ignored, and the id is a hash over all hunks of the file. `rr-cache/<id>/preimage` holds the
//! whole conflicted file in that normal form (no labels), `rr-cache/<id>/postimage` the file as the
//! user resolved it. When the same conflict comes up again the resolution is replayed by a
//! three-way merge `merge3(preimage, conflicted file, postimage)`, so edits outside the conflict
//! carry over. `.vcs/rr-merge` maps the paths of the conflict in progress to their ids (git's
//! MERGE_RR). Everything is opt-in: `rerere.enabled`, or an existing `rr-cache` directory.

use crate::error::{io, msg, Error, Result};
use crate::index::Index;
use crate::merge::{has_conflict_markers, MergeState};
use crate::meta::write_atomic;
use crate::repo::Repo;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use vcs_merge::{merge3, Algorithm, TextMerge};
use vcs_store::ObjectStore;

/// Settings the CLI passes in from `rerere.enabled` / `rerere.autoUpdate`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RerereCfg {
    /// `None`: on exactly when `rr-cache` exists (git's rule).
    pub enabled: Option<bool>,
    /// Stage a file whose conflict was replayed (and mark it resolved).
    pub autoupdate: bool,
}

/// A conflicted file in rerere's normal form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized {
    pub id: String,
    pub image: Vec<u8>,
}

/// What `gc` removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RerereGc {
    pub resolved: usize,
    pub unresolved: usize,
}

fn marker(line: &[u8], m: &[u8; 7]) -> bool {
    line.starts_with(m) && matches!(line.get(7), None | Some(b' ' | b'\r' | b'\n'))
}

/// Normal form and id of a file with conflict markers; `None` when it has no complete, well-formed
/// conflict hunk (or the markers are nested or unterminated).
pub fn normalize(data: &[u8]) -> Option<Normalized> {
    #[derive(PartialEq)]
    enum St {
        Out,
        One,
        Base,
        Two,
    }
    let mut st = St::Out;
    let (mut one, mut two) = (Vec::new(), Vec::new());
    let mut image = Vec::with_capacity(data.len());
    let mut hasher = blake3::Hasher::new();
    let mut hunks = 0;
    for line in data.split_inclusive(|b| *b == b'\n') {
        if st != St::Out && marker(line, b"<<<<<<<") {
            return None; // a second opening marker inside a hunk
        }
        match st {
            St::Out if marker(line, b"<<<<<<<") => st = St::One,
            St::Out => image.extend_from_slice(line),
            St::One if marker(line, b"|||||||") => st = St::Base,
            St::One | St::Base if marker(line, b"=======") => st = St::Two,
            St::One => with_nl(&mut one, line),
            St::Base => {}
            St::Two if marker(line, b">>>>>>>") => {
                if one > two {
                    std::mem::swap(&mut one, &mut two);
                }
                hasher.update(&one).update(&[0]).update(&two).update(&[0]);
                image.extend_from_slice(b"<<<<<<<\n");
                image.extend_from_slice(&one);
                image.extend_from_slice(b"=======\n");
                image.extend_from_slice(&two);
                image.extend_from_slice(b">>>>>>>\n");
                one.clear();
                two.clear();
                hunks += 1;
                st = St::Out;
            }
            St::Two => with_nl(&mut two, line),
        }
    }
    if st != St::Out || hunks == 0 {
        return None;
    }
    let hex = hasher.finalize().to_hex();
    Some(Normalized {
        id: hex.as_str()[..40].to_owned(),
        image,
    })
}

fn with_nl(out: &mut Vec<u8>, line: &[u8]) {
    out.extend_from_slice(line);
    if !line.ends_with(b"\n") {
        out.push(b'\n');
    }
}

/// Replay `postimage` onto `current` (a file with markers) the way git does: merge the change
/// preimage -> postimage into the conflicted file. `None` when that is not clean.
fn replay(preimage: &[u8], current: &Normalized, postimage: &[u8]) -> Option<Vec<u8>> {
    match merge3(Algorithm::Myers, preimage, &current.image, postimage) {
        TextMerge::Clean(b) if !has_conflict_markers(&b) => Some(b),
        _ => None,
    }
}

impl Repo {
    /// Configure rerere for this handle (the CLI reads `rerere.enabled` and `rerere.autoUpdate`).
    pub fn set_rerere(&mut self, cfg: RerereCfg) {
        self.rerere = cfg;
    }

    fn rr_dir(&self) -> PathBuf {
        self.common.join("rr-cache")
    }

    fn rr_file(&self, id: &str, name: &str) -> PathBuf {
        self.rr_dir().join(id).join(name)
    }

    fn rr_merge_path(&self) -> PathBuf {
        self.vcs.join("rr-merge")
    }

    /// Is rerere active for this handle?
    pub fn rerere_enabled(&self) -> bool {
        self.rerere.enabled.unwrap_or_else(|| self.rr_dir().is_dir())
    }

    /// (id, path) of the conflict in progress, in path order.
    fn rr_merge(&self) -> Vec<(String, String)> {
        let Ok(text) = fs::read_to_string(self.rr_merge_path()) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(id, p)| (id.to_owned(), p.to_owned()))
            .collect()
    }

    fn rr_merge_write(&self, entries: &[(String, String)]) -> Result<()> {
        let p = self.rr_merge_path();
        if entries.is_empty() {
            return match fs::remove_file(&p) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(Error::Io { path: p, source }),
            };
        }
        let mut s = String::new();
        for (id, path) in entries {
            s.push_str(&format!("{id}\t{path}\n"));
        }
        write_atomic(&p, s.as_bytes())
    }

    fn rr_put(&self, id: &str, name: &str, data: &[u8]) -> Result<()> {
        let dir = self.rr_dir().join(id);
        io(&dir, fs::create_dir_all(&dir))?;
        write_atomic(&dir.join(name), data)
    }

    fn rr_get(&self, id: &str, name: &str) -> Option<Vec<u8>> {
        fs::read(self.rr_file(id, name)).ok()
    }

    /// Remember that a recorded resolution was used (gc ages resolutions by their last use).
    fn rr_touch(&self, id: &str) {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(self.rr_file(id, "postimage")) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    /// Called when a merge, cherry-pick, revert or rebase stops on conflicts (state saved, files
    /// written, index not yet): remember each text conflict's preimage and replay a recorded
    /// resolution into the working file. With `rerere.autoUpdate` the file is also staged.
    pub(crate) fn rerere_on_conflicts(&self, st: &mut MergeState, ix: &mut Index) -> Result<()> {
        if !self.rerere_enabled() {
            return Ok(());
        }
        let mut entries = Vec::new();
        let mut replayed = Vec::new();
        for rec in st.conflicts.iter().filter(|c| c.has_markers && !c.resolved) {
            let Some(norm) = normalize(&self.store.get_content(&rec.placeholder)?) else {
                continue;
            };
            if self.rr_get(&norm.id, "preimage").is_none() {
                self.rr_put(&norm.id, "preimage", &norm.image)?;
            }
            if self.replay_into_worktree(&norm.id, &rec.path, &norm)? {
                eprintln!("Resolved '{}' using previous resolution.", rec.path);
                replayed.push(rec.path.clone()); // like git: a replayed path is no longer tracked
            } else {
                entries.push((norm.id.clone(), rec.path.clone()));
            }
        }
        self.rr_merge_write(&entries)?;
        if self.rerere.autoupdate && !replayed.is_empty() {
            self.stage_files(&replayed, ix)?;
            for c in st.conflicts.iter_mut().filter(|c| replayed.contains(&c.path)) {
                c.resolved = true;
            }
            self.save_merge_state(st)?;
        }
        Ok(())
    }

    /// Merge the recorded resolution `id` into the working file `path` (which holds `current`'s
    /// markers). True when the file was rewritten.
    fn replay_into_worktree(&self, id: &str, path: &str, current: &Normalized) -> Result<bool> {
        let (Some(pre), Some(post)) = (self.rr_get(id, "preimage"), self.rr_get(id, "postimage")) else {
            return Ok(false);
        };
        let Some(out) = replay(&pre, current, &post) else {
            return Ok(false);
        };
        let abs = self.worktree.join(path);
        io(&abs, fs::write(&abs, out))?;
        self.rr_touch(id);
        Ok(true)
    }

    /// Called when `path` is accepted as resolved with content `data` (lock held): store the
    /// postimage of a conflict rerere is tracking, unless one is already recorded.
    pub(crate) fn rerere_record(&self, path: &str, data: &[u8]) -> Result<()> {
        let Some((id, _)) = self.rr_merge().into_iter().find(|(_, p)| p == path) else {
            return Ok(());
        };
        if has_conflict_markers(data) || self.rr_file(&id, "postimage").exists() || self.rr_get(&id, "preimage").is_none() {
            return Ok(());
        }
        self.rr_put(&id, "postimage", data)?;
        eprintln!("Recorded resolution for '{path}'.");
        Ok(())
    }

    /// True when a rerere conflict is in progress (cheap check for hot paths).
    pub(crate) fn rerere_tracking(&self) -> bool {
        self.rr_merge_path().exists()
    }

    pub(crate) fn rerere_forget_state(&self) {
        let _ = fs::remove_file(self.rr_merge_path());
    }

    /// `vcs rerere`: record the resolution of every tracked file that no longer has markers, and
    /// replay recorded ones into files that still have them. Returns the lines to print.
    pub fn rerere_update(&self) -> Result<Vec<String>> {
        let _l = self.lock()?;
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for (id, path) in self.rr_merge() {
            let abs = self.worktree.join(&path);
            let Ok(data) = fs::read(&abs) else { continue };
            if has_conflict_markers(&data) {
                if let Some(norm) = normalize(&data) {
                    if norm.id == id && self.replay_into_worktree(&id, &path, &norm)? {
                        out.push(format!("Resolved '{path}' using previous resolution."));
                        continue;
                    }
                }
                keep.push((id, path));
                continue;
            }
            keep.push((id.clone(), path.clone()));
            if self.rr_file(&id, "postimage").exists() {
                // already recorded
            } else if self.rr_get(&id, "preimage").is_some() {
                self.rr_put(&id, "postimage", &data)?;
                out.push(format!("Recorded resolution for '{path}'."));
            }
        }
        self.rr_merge_write(&keep)?;
        Ok(out)
    }

    /// Paths of the conflict in progress that rerere is tracking.
    pub fn rerere_status(&self) -> Result<Vec<String>> {
        Ok(self.rr_merge().into_iter().map(|(_, p)| p).collect())
    }

    /// Conflicted paths of the merge in progress that still need a hand resolution: not staged yet
    /// and still holding markers (or not a text conflict rerere can handle).
    pub fn rerere_remaining(&self) -> Result<Vec<String>> {
        let Some(st) = self.merge_state()? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for c in st.conflicts.iter().filter(|c| !c.resolved) {
            let edited = c.has_markers && fs::read(self.worktree.join(&c.path)).is_ok_and(|d| !has_conflict_markers(&d));
            if !edited {
                out.push(c.path.clone());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Unified diff of each tracked file: its recorded preimage against the file now.
    pub fn rerere_diff(&self) -> Result<String> {
        let mut out = String::new();
        for (id, path) in self.rr_merge() {
            let Some(pre) = self.rr_get(&id, "preimage") else { continue };
            let Ok(cur) = fs::read(self.worktree.join(&path)) else { continue };
            let cur = if has_conflict_markers(&cur) {
                normalize(&cur).map_or(cur, |n| n.image)
            } else {
                cur
            };
            out.push_str(&crate::diff::unified(&path, Some(&pre), Some(&cur)));
        }
        Ok(out)
    }

    /// `vcs rerere forget <path>...`: drop the recorded resolution of the conflict in progress at
    /// each path and put the conflict's preimage back, so the next resolution is recorded afresh.
    pub fn rerere_forget(&self, paths: &[String]) -> Result<Vec<String>> {
        let _l = self.lock()?;
        let Some(st) = self.merge_state()? else {
            return msg("no remembered resolution: no merge in progress");
        };
        let mut out = Vec::new();
        for want in paths {
            let dir = format!("{}/", want.trim_end_matches('/'));
            let mut hit = false;
            for rec in st
                .conflicts
                .iter()
                .filter(|c| c.has_markers && (want.is_empty() || c.path == *want || c.path.starts_with(&dir)))
            {
                let Some(n) = normalize(&self.store.get_content(&rec.placeholder)?) else {
                    continue;
                };
                hit = true;
                if !self.rr_file(&n.id, "postimage").exists() {
                    return msg(format!("no remembered resolution for '{}'", rec.path));
                }
                let _ = fs::remove_file(self.rr_file(&n.id, "postimage"));
                self.rr_put(&n.id, "preimage", &n.image)?;
                out.push(format!("Updated preimage for '{}'", rec.path));
                out.push(format!("Forgot resolution for '{}'", rec.path));
            }
            if !hit {
                return msg(format!("no remembered resolution for '{want}'"));
            }
        }
        Ok(out)
    }

    /// `vcs rerere clear`: drop the unresolved preimages of the conflict in progress and stop
    /// tracking it. Recorded resolutions stay.
    pub fn rerere_clear(&self) -> Result<()> {
        let _l = self.lock()?;
        for (id, _) in self.rr_merge() {
            if !self.rr_file(&id, "postimage").exists() {
                let d = self.rr_dir().join(&id);
                let _ = fs::remove_dir_all(d);
            }
        }
        self.rr_merge_write(&[])
    }

    /// `vcs rerere gc`: delete resolutions not used for `resolved_days` and conflicts never
    /// resolved for `unresolved_days` (git's defaults: 60 and 15).
    pub fn rerere_gc(&self, resolved_days: u64, unresolved_days: u64) -> Result<RerereGc> {
        let _l = self.lock()?;
        let mut rep = RerereGc::default();
        let dir = self.rr_dir();
        let Ok(rd) = fs::read_dir(&dir) else { return Ok(rep) };
        let now = SystemTime::now();
        let age = |p: &PathBuf| -> Option<Duration> { fs::metadata(p).ok()?.modified().ok().and_then(|m| now.duration_since(m).ok()) };
        for e in rd.flatten() {
            let d = e.path();
            let (post, pre) = (d.join("postimage"), d.join("preimage"));
            let (limit, resolved) = if post.exists() {
                (resolved_days, true)
            } else {
                (unresolved_days, false)
            };
            let stamp = if resolved { &post } else { &pre };
            let old = age(stamp).is_none_or(|a| a >= Duration::from_secs(limit * 86_400));
            if old {
                io(&d, fs::remove_dir_all(&d))?;
                if resolved {
                    rep.resolved += 1;
                } else {
                    rep.unresolved += 1;
                }
            }
        }
        Ok(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &[u8] = b"top\n<<<<<<< ours (main)\nleft\n||||||| base\nold\n=======\nright\n>>>>>>> topic\nbottom\n";

    #[test]
    fn normal_form_drops_labels_and_base() {
        let n = normalize(A).unwrap();
        assert_eq!(n.image, b"top\n<<<<<<<\nleft\n=======\nright\n>>>>>>>\nbottom\n");
        assert_eq!(n.id.len(), 40);
    }

    #[test]
    fn swapped_sides_share_an_id_and_image() {
        let b = b"top\n<<<<<<< x\nright\n=======\nleft\n>>>>>>> y\nbottom\n";
        assert_eq!(normalize(A).unwrap(), normalize(b).unwrap());
    }

    #[test]
    fn different_content_different_id() {
        let c = b"top\n<<<<<<< x\nleft\n=======\nother\n>>>>>>> y\nbottom\n";
        assert_ne!(normalize(A).unwrap().id, normalize(c).unwrap().id);
    }

    #[test]
    fn malformed_or_marker_free_files_are_not_tracked() {
        assert!(normalize(b"plain\n").is_none());
        assert!(normalize(b"<<<<<<< a\nx\n=======\ny\n").is_none());
        assert!(normalize(b"<<<<<<< a\n<<<<<<< b\nx\n=======\ny\n>>>>>>> c\n").is_none());
    }

    #[test]
    fn replay_carries_the_resolution_across_changed_context() {
        let pre = normalize(A).unwrap();
        let post = b"top\nresolved\nbottom\n";
        let shifted = b"NEW\ntop\n<<<<<<< o\nleft\n=======\nright\n>>>>>>> t\nbottom\n";
        let cur = normalize(shifted).unwrap();
        assert_eq!(replay(&pre.image, &cur, post).unwrap(), b"NEW\ntop\nresolved\nbottom\n");
    }
}
