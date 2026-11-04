//! Refs as a base plus an append-only delta log (feature `refs-delta`): equivalence with a model,
//! torn tails, a crash at every write, compaction, and operation-log tag deltas.
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use vcs_repo::{failpoint, Repo};

fn init_env() {
    // every test in this file wants delta mode from 8 tags on
    std::env::set_var("VCS_REFS_DELTA_MIN", "8");
    std::env::set_var("VCS_REFS_LOG_FLOOR", "6000");
}

fn sig() -> vcs_objects::Signature {
    vcs_objects::Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn setup(tags: usize) -> (tempfile::TempDir, Repo) {
    init_env();
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    fs::write(r.worktree().join("a"), "1").unwrap();
    r.add(&["".into()]).unwrap();
    r.commit("m", sig()).unwrap();
    for i in 0..tags {
        r.tag_create("root", &format!("t{i:04}"), None, None).unwrap();
    }
    (d, r)
}

fn vcs(r: &Repo) -> PathBuf {
    r.worktree().join(".vcs")
}

fn tags(r: &Repo) -> BTreeSet<String> {
    r.tag_list("root").unwrap().into_iter().map(|t| t.name).collect()
}

fn reopen(d: &tempfile::TempDir) -> Repo {
    Repo::discover(&d.path().join("r")).unwrap()
}

#[test]
fn switches_to_delta_mode_and_appends_instead_of_rewriting() {
    let (d, r) = setup(12);
    let (on, _) = r.refs_delta_info();
    assert!(on, "12 tags >= 8: delta mode");
    let feats = fs::read_to_string(vcs(&r).join("features")).unwrap();
    assert!(feats.lines().any(|l| l == "refs-delta"), "{feats}");
    let refs_before = fs::read(vcs(&r).join("refs")).unwrap();
    r.tag_create("root", "late", None, None).unwrap();
    r.tag_delete("root", "t0003").unwrap();
    assert_eq!(
        fs::read(vcs(&r).join("refs")).unwrap(),
        refs_before,
        "refs is untouched by a tag op"
    );
    let (_, log) = r.refs_delta_info();
    assert!(log > 20, "log has records: {log}");
    let r2 = reopen(&d);
    let t = tags(&r2);
    assert!(t.contains("late") && !t.contains("t0003") && t.len() == 12);
    // a tag op never parses the table: create reads the base only for the duplicate check
    let refs = r2.refs().unwrap();
    assert!(refs.tags.is_unparsed());
}

#[test]
fn random_ops_match_a_model_across_reopens() {
    let (d, mut r) = setup(10);
    let mut model = tags(&r);
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for step in 0..120 {
        match next() % 5 {
            0 | 1 => {
                let n = format!("n{step}");
                r.tag_create("root", &n, None, None).unwrap();
                model.insert(n);
            }
            2 => {
                if let Some(n) = model.iter().nth((next() % model.len() as u64) as usize).cloned() {
                    r.tag_delete("root", &n).unwrap();
                    model.remove(&n);
                }
            }
            3 => {
                r.create_branch("root", &format!("b{step}")).unwrap();
            }
            _ => {
                fs::write(r.worktree().join("a"), format!("{step}")).unwrap();
                r.add(&["".into()]).unwrap();
                r.commit("m", sig()).unwrap();
            }
        }
        if step % 7 == 0 {
            r = reopen(&d);
        }
        assert_eq!(tags(&r), model, "step {step}");
    }
    assert!(r.refs_delta_info().0);
    // undo walks the operation log through tag deltas
    r.op_undo().unwrap();
    assert!(tags(&r).len() <= model.len() + 1);
}

#[test]
fn torn_or_corrupt_log_tail_is_dropped_never_misread() {
    let (d, r) = setup(8);
    let mut states = vec![tags(&r)];
    for i in 0..6 {
        r.tag_create("root", &format!("x{i}"), None, None).unwrap();
        states.push(tags(&r));
    }
    let log_p = vcs(&r).join("refs.log");
    let full = fs::read(&log_p).unwrap();
    let mut last = 0usize;
    for len in 0..=full.len() {
        fs::write(&log_p, &full[..len]).unwrap();
        let got = tags(&reopen(&d));
        let k = states
            .iter()
            .position(|s| *s == got)
            .unwrap_or_else(|| panic!("len {len}: state is none of the committed ones"));
        assert!(k >= last, "state went backwards at len {len}");
        last = k;
    }
    assert_eq!(last, states.len() - 1);
    // a flipped byte anywhere in the records ends the replay there
    for pos in (20..full.len()).step_by(3) {
        let mut b = full.clone();
        b[pos] ^= 0x55;
        fs::write(&log_p, &b).unwrap();
        let got = tags(&reopen(&d));
        assert!(states.contains(&got), "byte {pos}");
    }
    // a log of another base is ignored
    let mut b = full.clone();
    b[5] ^= 1;
    fs::write(&log_p, &b).unwrap();
    assert_eq!(tags(&reopen(&d)), states[0]);
    // writing after a torn tail truncates it
    fs::write(&log_p, &full[..full.len() - 3]).unwrap();
    let r = reopen(&d);
    r.tag_create("root", "after-tear", None, None).unwrap();
    assert!(tags(&reopen(&d)).contains("after-tear"));
}

#[test]
fn crash_at_every_write_leaves_the_old_or_the_new_state() {
    // 7 tags: the victim converts the repository (new base, then one rename); 10: it appends
    for (start, n) in (0..24).flat_map(|n| [(7, n), (10, n)]) {
        let (d, r) = setup(start);
        let before = tags(&r);
        failpoint::reset();
        failpoint::arm(Some(n));
        let res = r.tag_create("root", "victim", None, None);
        let writes = failpoint::writes();
        failpoint::arm(None);
        drop(r);
        let r = reopen(&d);
        let got = tags(&r);
        let mut after = before.clone();
        after.insert("victim".into());
        assert!(got == before || got == after, "start={start} n={n}: {got:?}");
        if res.is_ok() {
            assert_eq!(got, after);
        }
        // the repository keeps working, and the next op does not lose the survivor
        r.tag_create("root", "next", None, None).unwrap();
        let t = tags(&reopen(&d));
        assert!(t.contains("next") && t.len() > before.len());
        let _ = writes;
    }
}

#[test]
fn compaction_swaps_the_base_in_one_rename_and_stays_correct() {
    let (d, r) = setup(10);
    let base0 = fs::read(vcs(&r).join("refs")).unwrap();
    // enough branch ops to push the log past its bound
    let mut compacted = false;
    for i in 0..400 {
        r.create_branch("root", &format!("a-very-long-branch-name-to-fill-the-log-{i:05}"))
            .unwrap();
        if fs::read(vcs(&r).join("refs")).unwrap() != base0 {
            compacted = true;
            break;
        }
    }
    assert!(compacted, "log never compacted");
    let (_, log) = r.refs_delta_info();
    assert!(log < 6000, "log restarted after compaction: {log}");
    assert_eq!(tags(&reopen(&d)).len(), 10);
}

#[test]
fn operations_store_tag_deltas_and_undo_restores_tags() {
    let (_d, r) = setup(12);
    r.tag_create("root", "one", None, None).unwrap();
    r.tag_create("root", "two", None, None).unwrap();
    let log = r.op_log(3).unwrap();
    assert!(
        log[0].1.view.tags.is_none() && log[0].1.view.tags_delta.is_some(),
        "newest op is a delta"
    );
    assert_eq!(log[0].1.view.tags_delta.as_ref().unwrap().len(), 1);
    let two = log[0].0;
    r.tag_delete("root", "one").unwrap();
    assert!(!tags(&r).contains("one"));
    r.op_restore(&two).unwrap();
    let t = tags(&r);
    assert!(t.contains("one") && t.contains("two") && t.len() == 14);
    r.op_undo().unwrap();
    r.op_undo().unwrap();
    assert!(tags(&r).len() >= 12);
}
