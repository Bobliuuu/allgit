//! Real-history merge replay (issue #108).
//!
//! `cargo run -p vcs-repo --release --features structural,structural-python,structural-typescript,structural-go \
//!    --example replay -- <repo-dir> <branch> [--max N] [--tsv]`
//!
//! For every merge commit (two parents, exactly one merge base) of `<branch>` in the root zone of an
//! imported repository (`vcs import-git`), re-run the merge of its two parents with four engines and
//! compare with the tree the real merge commit has:
//!   text = line diff3; struct = structural drivers; text+ren / struct+ren = the same with rename detection.
//! Merges are sampled evenly (newest first) down to `--max`. Pure library calls; nothing is written.
//!
//! "content-merged" = a file the engine merged cleanly into a new blob (both sides changed it, or a
//! renamed file with edits on the other side). "exact" = that blob is byte-identical to the file in the
//! real merge commit. "parse ok" = the merged blob parses with the language's tree-sitter grammar
//! (only for languages whose structural feature is compiled in).

use std::collections::BTreeMap;
use std::time::Instant;
use vcs_merge::{merge_flat_renames, merge_flat_with, Algorithm, StoreBlobs, TreeMerge};
use vcs_repo::treeops::{flatten, Flat};
use vcs_repo::{Repo, ROOT_ZONE};

const VARIANTS: [&str; 4] = ["text", "struct", "text+ren", "struct+ren"];

#[derive(Default, Clone)]
struct LangStat {
    conflicts: u64,
    merged: u64,
    exact: u64,
    parse_total: u64,
    parse_ok: u64,
    /// the real merged file for the same paths parses (baseline for parse_ok)
    hist_parse_ok: u64,
}

#[derive(Default, Clone)]
struct Stat {
    merges: u64,
    clean_merges: u64,
    exact_merges: u64,
    renames_applied: u64,
    micros: u128,
    langs: BTreeMap<&'static str, LangStat>,
}

fn lang_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "py" => "python",
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "md" | "rst" | "txt" => "docs",
        _ => "other",
    }
}

#[allow(unused_variables)]
fn parses(path: &str, data: &[u8]) -> Option<bool> {
    #[cfg(any(feature = "structural", feature = "structural-python", feature = "structural-typescript", feature = "structural-go"))]
    {
        let l = vcs_merge::structural::lang_for_path(path)?;
        return Some(vcs_merge::structural::parses_as(l, data));
    }
    #[allow(unreachable_code)]
    None
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 2 {
        eprintln!("usage: replay <repo-dir> <branch> [--max N] [--tsv]");
        std::process::exit(2);
    }
    let mut max = 300usize;
    let tsv = a.iter().any(|x| x == "--tsv");
    if let Some(i) = a.iter().position(|x| x == "--max") {
        max = a[i + 1].parse().expect("--max N");
    }
    let repo = Repo::discover(std::path::Path::new(&a[0])).expect("open repo");
    let log = repo.log(ROOT_ZONE, Some(&a[1]), usize::MAX).expect("log");
    let merges: Vec<_> = log.iter().filter(|(_, c)| c.parents.len() == 2).collect();
    let total_merges = merges.len();
    let stride = (merges.len() / max.max(1)).max(1);
    let sample: Vec<_> = merges.iter().step_by(stride).take(max).collect();
    let store = repo.store();
    let blobs = StoreBlobs(store);
    let mut stats: Vec<Stat> = vec![Stat::default(); 4];
    let (mut used, mut skipped_bases) = (0u64, 0u64);
    for (_, c) in sample {
        let bases = repo.merge_bases(&c.parents[0], &c.parents[1]).expect("bases");
        let [base] = bases.as_slice() else {
            skipped_bases += 1;
            continue;
        };
        let root = |id| -> Flat { flatten(store, Some(repo_commit_root(&repo, id)), "").expect("flatten") };
        let (fb, f1, f2) = (root(base), root(&c.parents[0]), root(&c.parents[1]));
        let actual = flatten(store, Some(c.root), "").expect("flatten merged");
        used += 1;
        for vi in 0..VARIANTS.len() {
            let structural = vi % 2 == 1;
            let ren = vi >= 2;
            let t = Instant::now();
            let (tm, nren): (TreeMerge, u64) = if ren {
                let r = merge_flat_renames(&blobs, Algorithm::Myers, structural, vcs_merge::rename::DEFAULT_THRESHOLD, &fb, &f1, &f2).expect("merge");
                let n = (r.left_renames.len() + r.right_renames.len()) as u64;
                (r.merge, n)
            } else {
                (merge_flat_with(&blobs, Algorithm::Myers, structural, &fb, &f1, &f2).expect("merge"), 0)
            };
            let s = &mut stats[vi];
            s.micros += t.elapsed().as_micros();
            s.merges += 1;
            s.renames_applied += nren;
            let clean = tm.conflicts.is_empty();
            s.clean_merges += clean as u64;
            s.exact_merges += (clean && tm.entries == actual) as u64;
            for cf in &tm.conflicts {
                s.langs.entry(lang_of(&cf.path)).or_default().conflicts += 1;
            }
            for (p, e) in &tm.entries {
                let Some(data) = tm.new_blobs.get(&e.1) else { continue };
                let ls = s.langs.entry(lang_of(p)).or_default();
                ls.merged += 1;
                ls.exact += (actual.get(p) == Some(e)) as u64;
                if let Some(ok) = parses(p, data) {
                    ls.parse_total += 1;
                    ls.parse_ok += ok as u64;
                    if let Some(h) = actual.get(p).and_then(|h| vcs_store_get(store, &h.1)) {
                        ls.hist_parse_ok += parses(p, &h).unwrap_or(false) as u64;
                    }
                }
            }
        }
    }
    println!("repo={} branch={} total_merge_commits={} sampled={} replayed={} skipped_not_single_base={}", a[0], a[1], total_merges, merges.iter().step_by(stride).take(max).count(), used, skipped_bases);
    for (vi, name) in VARIANTS.iter().enumerate() {
        let s = &stats[vi];
        println!(
            "variant={} merges={} clean={} exact_tree={} renames_applied={} total_ms={}",
            name,
            s.merges,
            s.clean_merges,
            s.exact_merges,
            s.renames_applied,
            s.micros / 1000
        );
        for (l, ls) in &s.langs {
            if tsv {
                println!("LANG\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", name, l, ls.conflicts, ls.merged, ls.exact, ls.parse_total, ls.parse_ok, ls.hist_parse_ok);
            } else {
                println!(
                    "  {:<11} conflicts={:<5} content-merged={:<5} exact={:<5} parse_ok={}/{} (history file parses: {})",
                    l, ls.conflicts, ls.merged, ls.exact, ls.parse_ok, ls.parse_total, ls.hist_parse_ok
                );
            }
        }
    }
}

fn repo_commit_root(repo: &Repo, id: &vcs_objects::ObjectId) -> vcs_objects::ObjectId {
    match repo.store().get(id).expect("commit") {
        vcs_objects::Object::Commit(c) => c.root,
        _ => panic!("not a commit"),
    }
}

fn vcs_store_get(store: &dyn vcs_store::ObjectStore, id: &vcs_objects::ObjectId) -> Option<Vec<u8>> {
    store.get_content(id).ok()
}
