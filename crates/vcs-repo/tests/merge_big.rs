//! merge / rebase / cherry-pick / revert / stash on a wide tree (1500 files in 150 directories):
//! after every operation the working tree must equal an in-memory model of the expected content,
//! so a change dropped or invented by the subtree-skipping merge shows up as a diff, never as
//! silence. (The merge engine itself is compared with the flat engine in `src/treemerge.rs`.)

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vcs_objects::Signature;
use vcs_repo::{MergeOutcome, RebaseOutcome, Repo};

type Model = BTreeMap<String, String>;

fn sig(ts: u64) -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: ts,
    }
}

fn path_of(i: usize) -> String {
    format!("d{:02}/s{}/f{}.txt", i % 15, (i / 15) % 10, i)
}

fn body(i: usize) -> String {
    format!("L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nid {i}\n")
}

fn write(r: &Repo, m: &mut Model, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
    m.insert(p.to_owned(), c.to_owned());
}

fn remove(r: &Repo, m: &mut Model, p: &str) {
    fs::remove_file(r.worktree().join(p)).unwrap();
    m.remove(p);
}

fn append(r: &Repo, m: &mut Model, i: usize, tag: &str) {
    let p = path_of(i);
    let c = format!("{}{tag}\n", m[&p]);
    write(r, m, &p, &c);
}

fn commit(r: &Repo, msg: &str) {
    r.add(&["".to_string()]).unwrap();
    r.commit(msg, sig(1000)).unwrap();
}

fn disk(r: &Repo) -> Model {
    fn walk(root: &Path, dir: &Path, out: &mut Model) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".vcs") {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut m = Model::new();
    walk(r.worktree(), r.worktree(), &mut m);
    m
}

fn assert_disk(r: &Repo, want: &Model, what: &str) {
    let got = disk(r);
    let missing: Vec<_> = want.keys().filter(|k| !got.contains_key(*k)).take(5).collect();
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).take(5).collect();
    let differ: Vec<_> = want
        .iter()
        .filter(|(k, v)| got.get(*k).is_some_and(|g| g != *v))
        .map(|(k, _)| k)
        .take(5)
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty() && differ.is_empty(),
        "{what}: missing {missing:?}, unexpected {extra:?}, wrong content {differ:?}"
    );
}

fn clean(r: &Repo) -> bool {
    let s = r.status().unwrap();
    s.untracked.is_empty() && s.zones.iter().all(|a| a.staged.is_empty() && a.unstaged.is_empty()) && s.merging.is_none()
}

fn fsck(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}

const N: usize = 1500;
const SHARED: &str = "shared.txt";
const SHARED_BASE: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

/// Base commit on main with N files plus `shared.txt`; returns the repo and the model.
fn setup() -> (tempfile::TempDir, Repo, Model) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    let mut m = Model::new();
    for i in 0..N {
        write(&r, &mut m, &path_of(i), &body(i));
    }
    write(&r, &mut m, SHARED, SHARED_BASE);
    commit(&r, "base");
    (d, r, m)
}

#[test]
fn wide_merge_keeps_both_sides_and_conflict_abort_restores_everything() {
    let (_d, r, base) = setup();
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    let mut feat = base.clone();
    for i in (0..N).step_by(62) {
        append(&r, &mut feat, i, "feature");
    }
    for i in [1usize, 2, 3, 4, 5] {
        remove(&r, &mut feat, &path_of(i * 10 + 400));
    }
    write(&r, &mut feat, "newdir/deep/er/a.txt", "feature new\n");
    write(&r, &mut feat, SHARED, "1\n2\n3\n4\n5\n6\n7\n8\n9\nFEATURE\n");
    commit(&r, "feature work");
    r.checkout("root", "main").unwrap();
    assert_disk(&r, &base, "back on main");
    let mut main = base.clone();
    for i in (15..N).step_by(74) {
        append(&r, &mut main, i, "main");
    }
    write(&r, &mut main, "newdir/other/b.txt", "main new\n");
    write(&r, &mut main, SHARED, "MAIN\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    commit(&r, "main work");

    // expected result of the merge: union of both sides' changes (the edited sets are disjoint
    // except `shared.txt`, whose edits are on different lines)
    let mut want = main.clone();
    for (p, c) in &feat {
        if base.get(p) != Some(c) && p != SHARED {
            want.insert(p.clone(), c.clone());
        }
    }
    for p in base.keys() {
        if !feat.contains_key(p) {
            want.remove(p);
        }
    }
    want.insert(SHARED.into(), "MAIN\n2\n3\n4\n5\n6\n7\n8\n9\nFEATURE\n".into());
    let main_head = r.refs().unwrap().head("root", "main").unwrap();
    let out = r.merge("root", "feature", sig(2000)).unwrap();
    let MergeOutcome::Merged { commit: merged } = out else {
        panic!("{out:?}")
    };
    assert_disk(&r, &want, "after clean merge");
    assert!(clean(&r));
    let c = r.log("root", None, 1).unwrap().remove(0);
    assert_eq!(c.0, merged);
    assert_eq!(c.1.parents.len(), 2);
    assert_eq!(c.1.parents[0], main_head);
    fsck(&r);

    // a conflicting merge on top: the same line of shared.txt and some other file edited differently
    r.create_branch("root", "clash").unwrap();
    r.checkout("root", "clash").unwrap();
    let mut clash = want.clone();
    write(&r, &mut clash, SHARED, "MAIN\n2\n3\n4\n5\n6\n7\n8\n9\nCLASH\n");
    append(&r, &mut clash, 100, "clash");
    commit(&r, "clash");
    r.checkout("root", "main").unwrap();
    let mut main2 = want.clone();
    write(&r, &mut main2, SHARED, "MAIN\n2\n3\n4\n5\n6\n7\n8\n9\nMAIN2\n");
    append(&r, &mut main2, 200, "main2");
    commit(&r, "main2");
    let out = r.merge("root", "clash", sig(2001)).unwrap();
    let MergeOutcome::Conflicted { paths } = out else {
        panic!("{out:?}")
    };
    assert_eq!(paths, vec![SHARED.to_owned()]);
    // every non-conflicting change from both sides is already in the working tree
    let g = disk(&r);
    assert_eq!(g[&path_of(100)], clash[&path_of(100)]);
    assert_eq!(g[&path_of(200)], main2[&path_of(200)]);
    assert!(g[SHARED].contains("<<<<<<<"));
    r.merge_abort().unwrap();
    assert_disk(&r, &main2, "after merge --abort");
    assert!(clean(&r));
    fsck(&r);
}

#[test]
fn long_rebase_keeps_every_commit_and_survives_a_conflict_in_the_middle() {
    let (_d, r, base) = setup();
    r.create_branch("root", "long").unwrap();
    r.checkout("root", "long").unwrap();
    let mut long = base.clone();
    let mut tips = Vec::new();
    for c in 0..40usize {
        for k in 0..3usize {
            append(&r, &mut long, 800 + (c * 7 + k * 13) % 600, &format!("long {c} {k}"));
        }
        if c == 20 {
            write(&r, &mut long, SHARED, "1\n2\n3\n4\n5\n6\n7\n8\n9\nLONG\n");
        }
        commit(&r, &format!("long{c}"));
        tips.push(r.log("root", None, 1).unwrap().remove(0).1.change_id);
    }
    r.checkout("root", "main").unwrap();
    let mut main = base.clone();
    for i in (0..700).step_by(23) {
        append(&r, &mut main, i, "main");
    }
    write(&r, &mut main, SHARED, "1\n2\n3\n4\n5\n6\n7\n8\n9\nMAIN\n");
    commit(&r, "main work");
    r.checkout("root", "long").unwrap();

    // expected: main's changes + all long edits; shared.txt conflicts (resolved by hand below)
    let mut want = main.clone();
    for (p, c) in &long {
        if base.get(p) != Some(c) && p != SHARED {
            want.insert(p.clone(), c.clone());
        }
    }
    let out = r.rebase("root", "main", sig(3000)).unwrap();
    let RebaseOutcome::Conflicted { paths, .. } = out else {
        panic!("{out:?}")
    };
    assert_eq!(paths, vec![SHARED.to_owned()]);
    // while stopped the commits replayed so far are in the working tree
    let g = disk(&r);
    assert!(
        g[&path_of(835)].contains("long 5 0"),
        "commits before the conflict are materialized"
    );
    fs::write(r.worktree().join(SHARED), "1\n2\n3\n4\n5\n6\n7\n8\n9\nRESOLVED\n").unwrap();
    r.add(&[SHARED.to_owned()]).unwrap();
    let out = r.rebase_continue(sig(3001)).unwrap();
    // `replayed` counts the picks since the last stop: the resolved one plus the 19 after it
    assert!(matches!(out, RebaseOutcome::Done { replayed: 20, .. }), "{out:?}");
    want.insert(SHARED.into(), "1\n2\n3\n4\n5\n6\n7\n8\n9\nRESOLVED\n".into());
    assert_disk(&r, &want, "after rebase");
    assert!(clean(&r));
    let log = r.log("root", Some("long"), 100).unwrap();
    assert_eq!(log.len(), 1 + 1 + 40, "base + main work + 40 replayed");
    let ids: Vec<_> = log.iter().take(40).rev().map(|(_, c)| c.change_id).collect();
    assert_eq!(ids, tips, "change ids are kept");
    fsck(&r);
    // undo restores the original branch exactly
    r.op_undo().unwrap();
    assert_disk(&r, &long, "after op undo");
    fsck(&r);
}

#[test]
fn pick_revert_and_stash_on_a_wide_tree() {
    let (_d, r, base) = setup();
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    let mut feat = base.clone();
    for i in (0..N).step_by(101) {
        append(&r, &mut feat, i, "feature");
    }
    commit(&r, "feature edit");
    r.checkout("root", "main").unwrap();
    let mut main = base.clone();
    for i in (50..N).step_by(97) {
        append(&r, &mut main, i, "main");
    }
    commit(&r, "main edit");

    let mut want = main.clone();
    for (p, c) in &feat {
        if base.get(p) != Some(c) {
            want.insert(p.clone(), c.clone());
        }
    }
    let MergeOutcome::Merged { .. } = r.cherry_pick("root", "feature", sig(4000)).unwrap() else {
        panic!()
    };
    assert_disk(&r, &want, "after cherry-pick");
    assert!(clean(&r));
    let MergeOutcome::Merged { .. } = r.revert("root", "HEAD", sig(4001)).unwrap() else {
        panic!()
    };
    assert_disk(&r, &main, "after reverting the pick");
    assert!(clean(&r));
    assert!(r.cherry_pick("root", "feature", sig(4002)).is_ok());
    assert!(
        r.revert("root", "feature", sig(4003)).is_ok(),
        "reverting the original commit undoes it again"
    );
    fsck(&r);

    // stash: edits in several directories, one staged, one deleted, then pop restores them all
    let mut dirty = main.clone();
    append(&r, &mut dirty, 3, "wip1");
    append(&r, &mut dirty, 777, "wip2");
    remove(&r, &mut dirty, &path_of(1000));
    write(&r, &mut dirty, "newfile.txt", "staged\n");
    r.add(&["newfile.txt".to_owned()]).unwrap();
    r.stash_push(Some("wip"), sig(5000)).unwrap();
    let mut tracked_only = main.clone();
    tracked_only.remove("newfile.txt");
    assert_disk(&r, &tracked_only, "after stash push");
    assert!(clean(&r));
    let out = r.stash_pop(0).unwrap();
    assert_eq!(out, vcs_repo::StashPopOutcome::Applied);
    assert_disk(&r, &dirty, "after stash pop");
    fsck(&r);
}
