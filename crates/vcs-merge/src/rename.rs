//! Rename/move detection and rename-aware three-way tree merge.
//!
//! A rename is detected between the base and one side as a path that disappeared paired with a
//! path that appeared: first by identical blob id (and mode), then by content similarity over
//! cheap fingerprints (sorted hashes of the trimmed non-blank lines; similarity is the Dice
//! coefficient of the two line multisets). Pairing is one-to-one and refuses ties.
//!
//! [`merge_flat_renames`] then rewrites the inputs so that an ordinary [`merge_flat_with`] sees the
//! renamed file at its new path in all three versions; an edit on the other side is merged into the
//! renamed file by the usual content merge (text or structural). Cases it will not guess at stay
//! as structured conflicts: both sides rename the same file to different paths
//! ([`ConflictKind::RenameRename`]), and a rename whose target path also exists on the other side
//! is not applied (the old add/add or delete/modify behaviour is kept).

use crate::error::MergeError;
use crate::tree::{merge_flat_with, BlobSource, Conflict, ConflictKind, Flat, TreeMerge};
use crate::Algorithm;
use std::collections::{BTreeMap, BTreeSet};
use vcs_objects::{EntryMode, ObjectId};

/// Default similarity threshold in per mille (500 = 50 % of lines shared).
pub const DEFAULT_THRESHOLD: u32 = 500;
/// Blobs larger than this are only matched exactly.
const MAX_FP_BYTES: usize = 1 << 20;
/// Inexact matching is skipped when (deleted x added) exceeds this many pairs.
const MAX_PAIRS: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
    /// Similarity in per mille; 1000 for an identical blob.
    pub score: u32,
}

fn base_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn fingerprint(data: &[u8]) -> Option<Vec<u64>> {
    if data.len() > MAX_FP_BYTES || data[..data.len().min(8000)].contains(&0) {
        return None;
    }
    let mut v: Vec<u64> = data
        .split(|&b| b == b'\n')
        .map(|l| l.trim_ascii())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let h = blake3::hash(l);
            u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap())
        })
        .collect();
    v.sort_unstable();
    Some(v)
}

/// Dice coefficient in per mille of two sorted multisets.
fn similarity(a: &[u64], b: &[u64]) -> u32 {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (mut i, mut j, mut common) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    (2000 * common / (a.len() + b.len())) as u32
}

/// Renames from `base` to `side`. Only paths absent on one side and present on the other take part;
/// symlinks are never similarity-matched. Deterministic.
pub fn detect_renames(blobs: &dyn BlobSource, base: &Flat, side: &Flat, threshold: u32) -> Result<Vec<Rename>, MergeError> {
    let gone: Vec<&String> = base.keys().filter(|p| !side.contains_key(*p)).collect();
    let added: Vec<&String> = side.keys().filter(|p| !base.contains_key(*p)).collect();
    if gone.is_empty() || added.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut used_from: BTreeSet<&String> = BTreeSet::new();
    let mut used_to: BTreeSet<&String> = BTreeSet::new();

    // exact: same blob id and mode
    type Group<'a> = (Vec<&'a String>, Vec<&'a String>);
    let mut by_id: BTreeMap<(EntryMode, ObjectId), Group> = BTreeMap::new();
    for p in &gone {
        by_id.entry(base[*p]).or_default().0.push(*p);
    }
    for p in &added {
        if let Some(e) = by_id.get_mut(&side[*p]) {
            e.1.push(*p);
        }
    }
    for (ds, as_) in by_id.values() {
        if ds.is_empty() || as_.is_empty() {
            continue;
        }
        let mut ds_left: Vec<&String> = ds.clone();
        let mut as_left: Vec<&String> = Vec::new();
        for a in as_ {
            if let Some(i) = ds_left.iter().position(|d| base_name(d) == base_name(a)) {
                let d = ds_left.remove(i);
                out.push(Rename {
                    from: d.clone(),
                    to: (*a).clone(),
                    score: 1000,
                });
                used_from.insert(d);
                used_to.insert(a);
            } else {
                as_left.push(*a);
            }
        }
        if ds_left.len() == 1 && as_left.len() == 1 {
            out.push(Rename {
                from: ds_left[0].clone(),
                to: as_left[0].clone(),
                score: 1000,
            });
            used_from.insert(ds_left[0]);
            used_to.insert(as_left[0]);
        }
        // otherwise ambiguous (copies): leave unpaired
    }

    // similarity
    let ds: Vec<&String> = gone
        .iter()
        .copied()
        .filter(|p| !used_from.contains(*p) && base[*p].0 != EntryMode::Symlink)
        .collect();
    let as_: Vec<&String> = added
        .iter()
        .copied()
        .filter(|p| !used_to.contains(*p) && side[*p].0 != EntryMode::Symlink)
        .collect();
    if !ds.is_empty() && !as_.is_empty() && ds.len().saturating_mul(as_.len()) <= MAX_PAIRS {
        let fp = |flat: &Flat, p: &String| -> Result<Option<Vec<u64>>, MergeError> { Ok(fingerprint(&blobs.blob(&flat[p].1)?)) };
        let dfp: Vec<Option<Vec<u64>>> = ds.iter().map(|p| fp(base, p)).collect::<Result<_, _>>()?;
        let afp: Vec<Option<Vec<u64>>> = as_.iter().map(|p| fp(side, p)).collect::<Result<_, _>>()?;
        // (score, same basename, from idx, to idx)
        let mut cands: Vec<(u32, bool, usize, usize)> = Vec::new();
        for (i, df) in dfp.iter().enumerate() {
            let Some(df) = df else { continue };
            for (j, af) in afp.iter().enumerate() {
                let Some(af) = af else { continue };
                let (lo, hi) = (df.len().min(af.len()), df.len().max(af.len()));
                if lo == 0 || 2000 * lo / (lo + hi) < threshold as usize {
                    continue;
                }
                let s = similarity(df, af);
                if s >= threshold {
                    cands.push((s, base_name(ds[i]) == base_name(as_[j]), i, j));
                }
            }
        }
        cands.sort_by(|x, y| (y.0, y.1).cmp(&(x.0, x.1)).then(ds[x.2].cmp(ds[y.2])).then(as_[x.3].cmp(as_[y.3])));
        let (mut ud, mut ua) = (vec![false; ds.len()], vec![false; as_.len()]);
        for (k, &(s, same, i, j)) in cands.iter().enumerate() {
            if ud[i] || ua[j] {
                continue;
            }
            // a tie with another still-free candidate for the same source or target is ambiguous
            let tie = cands[k + 1..]
                .iter()
                .take_while(|c| (c.0, c.1) == (s, same))
                .any(|c| (c.2 == i && !ua[c.3]) || (c.3 == j && !ud[c.2]));
            ud[i] = true;
            ua[j] = true;
            if !tie {
                out.push(Rename {
                    from: ds[i].clone(),
                    to: as_[j].clone(),
                    score: s,
                });
            }
        }
    }
    out.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(out)
}

/// Result of a rename-aware merge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenameMerge {
    pub merge: TreeMerge,
    /// Renames that were applied (the file was merged at its new path), from the left side.
    pub left_renames: Vec<Rename>,
    /// Same, from the right side.
    pub right_renames: Vec<Rename>,
}

fn mv(flat: &mut Flat, from: &str, to: &str) {
    if let Some(e) = flat.remove(from) {
        flat.insert(to.to_owned(), e);
    }
}

/// Like [`merge_flat_with`], but a file renamed (or moved) on one side is merged with the other
/// side's edits to it. See the module docs for what stays a conflict.
#[allow(clippy::too_many_arguments)]
pub fn merge_flat_renames(
    blobs: &dyn BlobSource,
    alg: Algorithm,
    structural: bool,
    threshold: u32,
    base: &Flat,
    left: &Flat,
    right: &Flat,
) -> Result<RenameMerge, MergeError> {
    let rl = detect_renames(blobs, base, left, threshold)?;
    let rr = detect_renames(blobs, base, right, threshold)?;
    let (mut b2, mut l2, mut r2) = (base.clone(), left.clone(), right.clone());
    let lmap: BTreeMap<&str, &Rename> = rl.iter().map(|r| (r.from.as_str(), r)).collect();
    let rmap: BTreeMap<&str, &Rename> = rr.iter().map(|r| (r.from.as_str(), r)).collect();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let (mut al, mut ar) = (Vec::new(), Vec::new());
    let mut rr_conflicts: Vec<Conflict> = Vec::new();

    let mut froms: BTreeSet<&str> = lmap.keys().copied().collect();
    froms.extend(rmap.keys().copied());
    for f in froms {
        match (lmap.get(f), rmap.get(f)) {
            (Some(a), Some(b)) if a.to != b.to => {
                let mut c = Conflict {
                    path: f.to_owned(),
                    kind: ConflictKind::RenameRename,
                    base: base.get(f).map(|e| e.1),
                    left: left.get(&a.to).map(|e| e.1),
                    right: right.get(&b.to).map(|e| e.1),
                    base_mode: base.get(f).map(|e| e.0),
                    left_mode: left.get(&a.to).map(|e| e.0),
                    right_mode: right.get(&b.to).map(|e| e.0),
                    hunks: None,
                };
                c.path = f.to_owned();
                rr_conflicts.push(c);
                for p in [f, a.to.as_str(), b.to.as_str()] {
                    b2.remove(p);
                    l2.remove(p);
                    r2.remove(p);
                }
            }
            (Some(a), Some(_)) => {
                // both renamed to the same path: line the base up with it
                if claimed.insert(a.to.clone()) {
                    mv(&mut b2, f, &a.to);
                    al.push((*a).clone());
                    ar.push((*a).clone());
                }
            }
            (Some(a), None) => {
                if right.contains_key(&a.to) || !claimed.insert(a.to.clone()) {
                    continue;
                }
                mv(&mut b2, f, &a.to);
                mv(&mut r2, f, &a.to);
                al.push((*a).clone());
            }
            (None, Some(b)) => {
                if left.contains_key(&b.to) || !claimed.insert(b.to.clone()) {
                    continue;
                }
                mv(&mut b2, f, &b.to);
                mv(&mut l2, f, &b.to);
                ar.push((*b).clone());
            }
            (None, None) => {}
        }
    }
    let mut merge = merge_flat_with(blobs, alg, structural, &b2, &l2, &r2)?;
    if !rr_conflicts.is_empty() {
        merge.conflicts.extend(rr_conflicts);
        merge.conflicts.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(RenameMerge {
        merge,
        left_renames: al,
        right_renames: ar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::MapBlobs;

    const F: EntryMode = EntryMode::File;
    fn lines(n: usize, tag: &str) -> String {
        (0..n).map(|i| format!("line {tag} {i}\n")).collect()
    }
    struct Fx(MapBlobs);
    impl Fx {
        fn e(&mut self, s: &str) -> (EntryMode, ObjectId) {
            (F, self.0.add(s.as_bytes()))
        }
    }
    fn flat(v: Vec<(&str, (EntryMode, ObjectId))>) -> Flat {
        v.into_iter().map(|(k, e)| (k.to_owned(), e)).collect()
    }
    fn run(fx: &Fx, b: &Flat, l: &Flat, r: &Flat) -> RenameMerge {
        merge_flat_renames(&fx.0, Algorithm::Myers, false, DEFAULT_THRESHOLD, b, l, r).unwrap()
    }

    #[test]
    fn exact_and_similar() {
        let mut fx = Fx(MapBlobs::default());
        let a = lines(20, "a");
        let a2 = a.replace("line a 3\n", "changed\n");
        let b = flat(vec![("x/a.txt", fx.e(&a)), ("keep", fx.e("k\n"))]);
        let exact = flat(vec![("y/a.txt", fx.e(&a)), ("keep", fx.e("k\n"))]);
        let sim = flat(vec![("y/b.txt", fx.e(&a2)), ("keep", fx.e("k\n"))]);
        let unrelated = flat(vec![("y/b.txt", fx.e(&lines(20, "z"))), ("keep", fx.e("k\n"))]);
        let r = detect_renames(&fx.0, &b, &exact, 500).unwrap();
        assert_eq!(
            r,
            vec![Rename {
                from: "x/a.txt".into(),
                to: "y/a.txt".into(),
                score: 1000
            }]
        );
        let r = detect_renames(&fx.0, &b, &sim, 500).unwrap();
        assert_eq!(r.len(), 1);
        assert!(r[0].score >= 900 && r[0].score < 1000);
        assert!(detect_renames(&fx.0, &b, &unrelated, 500).unwrap().is_empty());
    }

    #[test]
    fn rename_left_edit_right() {
        let mut fx = Fx(MapBlobs::default());
        let a = lines(20, "a");
        let edited = a.replace("line a 15\n", "right edit\n");
        let b = flat(vec![("old.txt", fx.e(&a))]);
        let l = flat(vec![("new/dir.txt", fx.e(&a))]);
        let r = flat(vec![("old.txt", fx.e(&edited))]);
        let m = run(&fx, &b, &l, &r);
        assert!(m.merge.conflicts.is_empty());
        assert_eq!(m.merge.entries.len(), 1);
        assert_eq!(m.merge.entries["new/dir.txt"], fx.e(&edited));
        // plain merge conflicts (delete/modify) on the same input
        let plain = crate::merge_flat(&fx.0, Algorithm::Myers, &b, &l, &r).unwrap();
        assert_eq!(plain.conflicts.len(), 1);
        // symmetric
        let m2 = run(&fx, &b, &r, &l);
        assert_eq!(m2.merge.entries, m.merge.entries);
    }

    #[test]
    fn both_edit_and_rename_merge_content() {
        let mut fx = Fx(MapBlobs::default());
        let a = lines(30, "a");
        let le = a.replace("line a 2\n", "left\n");
        let re = a.replace("line a 25\n", "right\n");
        let b = flat(vec![("old", fx.e(&a))]);
        let l = flat(vec![("moved", fx.e(&le))]);
        let r = flat(vec![("old", fx.e(&re))]);
        let m = run(&fx, &b, &l, &r);
        assert!(m.merge.conflicts.is_empty(), "{:?}", m.merge.conflicts);
        let both = a.replace("line a 2\n", "left\n").replace("line a 25\n", "right\n");
        assert_eq!(m.merge.entries["moved"], fx.e(&both));
    }

    #[test]
    fn rename_rename_is_a_conflict() {
        let mut fx = Fx(MapBlobs::default());
        let a = lines(20, "a");
        let b = flat(vec![("old", fx.e(&a))]);
        let l = flat(vec![("one", fx.e(&a))]);
        let r = flat(vec![("two", fx.e(&a))]);
        let m = run(&fx, &b, &l, &r);
        assert!(m.merge.entries.is_empty());
        assert_eq!(m.merge.conflicts.len(), 1);
        assert_eq!(m.merge.conflicts[0].kind, ConflictKind::RenameRename);
        assert_eq!(m.merge.conflicts[0].path, "old");
        // same rename on both sides is clean
        let r2 = flat(vec![("one", fx.e(&a))]);
        let m = run(&fx, &b, &l, &r2);
        assert!(m.merge.conflicts.is_empty());
        assert_eq!(m.merge.entries.len(), 1);
    }

    #[test]
    fn rename_vs_delete_conflicts_and_ambiguity_not_guessed() {
        let mut fx = Fx(MapBlobs::default());
        let a = lines(20, "a");
        let b = flat(vec![("old", fx.e(&a))]);
        let l = flat(vec![("new", fx.e(&a.replace("line a 4\n", "edit\n")))]);
        let r = Flat::new();
        let m = run(&fx, &b, &l, &r);
        assert_eq!(m.merge.conflicts.len(), 1);
        // rename without edits against a delete: the delete wins cleanly
        let l0 = flat(vec![("new", fx.e(&a))]);
        assert!(run(&fx, &b, &l0, &r).merge.entries.is_empty());
        assert_eq!(m.merge.conflicts[0].kind, ConflictKind::DeleteModify);
        // two identical new files from one old: a copy, not a rename
        let l = flat(vec![("c1", fx.e(&a)), ("c2", fx.e(&a))]);
        assert!(detect_renames(&fx.0, &b, &l, 500).unwrap().is_empty());
    }
}
