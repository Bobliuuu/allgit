//! Crash injection for the ref-update journal (docs/durability.md): kill after each step of a
//! journalled operation, simulate the loss of every unsynced rename (power cut), tear the journal
//! tail, fill the journal; always reopen and assert a consistent repository in which no operation
//! that completed (returned Ok) is lost.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use vcs_objects::{ObjectId, Signature};
use vcs_repo::{failpoint, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}

fn commit(r: &Repo, n: u32) -> ObjectId {
    fs::write(r.worktree().join("f"), n.to_string()).unwrap();
    r.add(&["".into()]).unwrap();
    r.commit(&format!("c{n}"), sig()).unwrap().commits[0].2
}

fn setup() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    commit(&r, 1);
    (d, r)
}

fn vcs(r: &Repo) -> PathBuf {
    r.worktree().join(".vcs")
}

fn descs(r: &Repo) -> Vec<String> {
    r.op_log(10_000).unwrap().into_iter().map(|(_, o)| o.description).collect()
}

fn tags(r: &Repo) -> Vec<String> {
    r.tag_list("root").unwrap().into_iter().map(|t| t.name).collect()
}

/// The repository is consistent: fsck clean and the newest operation's view equals the refs.
fn assert_consistent(r: &Repo) {
    let rep = r.fsck().unwrap();
    assert!(rep.problems.is_empty(), "fsck: {:?}", rep.problems);
    let head = r.op_head().unwrap().unwrap();
    let view = r.op_get(&head).unwrap().view;
    let refs = r.refs().unwrap();
    assert_eq!(view.branches, refs.branches, "op_head view disagrees with refs (branches)");
    assert_eq!(view.tags.unwrap_or_default(), refs.tags, "op_head view disagrees with refs (tags)");
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            fs::copy(e.path(), &t).unwrap();
        }
    }
}

/// Crash after every step of a tag create; the reopened repo is consistent and the tag exists
/// exactly when the journal commit point was reached.
#[test]
fn kill_after_each_step_of_tag_create_and_delete() {
    for delete in [false, true] {
        // how many injectable steps does one op have in this state?
        let steps = {
            let (_d, r) = setup();
            r.tag_create("root", "pre", None, None).unwrap();
            failpoint::reset();
            if delete {
                r.tag_delete("root", "pre").unwrap();
            } else {
                r.tag_create("root", "t", None, None).unwrap();
            }
            failpoint::writes()
        };
        assert!(steps >= 4, "expected the journal steps to be injectable, got {steps}");
        for n in 0..steps {
            let (d, r) = setup();
            r.tag_create("root", "pre", None, None).unwrap();
            failpoint::reset();
            failpoint::arm(Some(n));
            let res = if delete {
                r.tag_delete("root", "pre")
            } else {
                r.tag_create("root", "t", None, None)
            };
            failpoint::arm(None);
            assert!(res.is_err(), "step {n} should have crashed");
            drop(r);
            let r = Repo::discover(&d.path().join("r")).unwrap();
            // any writer finishes an interrupted record before it reads refs
            r.tag_create("root", "after", None, None).unwrap();
            assert_consistent(&r);
            let log = descs(&r);
            let name = if delete { "pre" } else { "t" };
            let key = format!("tag {} root/{name}", if delete { "delete" } else { "create" });
            let committed = log.iter().any(|l| l.starts_with(&key));
            // atomic: the ref and its operation record are both present or both absent
            let present = tags(&r).contains(&name.to_string());
            assert_eq!(
                present,
                committed != delete,
                "step {n} delete={delete}: tag vs op log disagree: {log:?}"
            );
            assert!(tags(&r).contains(&"after".to_string()));
            // the commit point is the journal fdatasync (after the first step)
            if n >= 1 {
                assert!(committed, "step {n} is after the commit point; the op must survive: {log:?}");
            } else {
                assert!(!committed, "step {n} is before the commit point; the op must not exist");
            }
        }
    }
}

/// Power cut after N completed operations: every unsynced rename (refs, op_head, op objects) is
/// lost, the boot id changed. The next open must restore them from the journal; no completed
/// operation may be missing.
#[test]
fn power_loss_after_completed_ops_loses_nothing() {
    let (d, r) = setup();
    let root = vcs(&r);
    // everything above is the durable baseline: copy it as the post-power-cut disk image
    let img = d.path().join("img");
    fs::create_dir_all(&img).unwrap();
    for f in ["refs", "op_head"] {
        fs::copy(root.join(f), img.join(f)).unwrap();
    }
    copy_dir(&root.join("objects"), &img.join("objects"));
    for i in 0..5 {
        r.tag_create("root", &format!("t{i}"), None, None).unwrap();
    }
    r.tag_delete("root", "t2").unwrap();
    let want_tags = tags(&r);
    let want_log = descs(&r);
    drop(r);
    // power cut: unsynced files revert to the baseline image; the journal (fdatasynced) survives
    for f in ["refs", "op_head"] {
        fs::copy(img.join(f), root.join(f)).unwrap();
    }
    fs::remove_dir_all(root.join("objects")).unwrap();
    copy_dir(&img.join("objects"), &root.join("objects"));
    // a different boot: scribble a foreign boot id into the journal header (with a valid checksum)
    let jp = root.join("oplog.journal");
    let mut j = fs::read(&jp).unwrap();
    j[12..28].copy_from_slice(&[0xAB; 16]);
    let c = blake3::hash(&j[..36]);
    j[36..44].copy_from_slice(&c.as_bytes()[..8]);
    fs::write(&jp, &j).unwrap();

    // a plain reader open repairs it, no writer needed
    let r = Repo::discover(&d.path().join("r")).unwrap();
    assert_eq!(tags(&r), want_tags);
    assert_eq!(descs(&r), want_log);
    assert_consistent(&r);
    // the repaired journal is a fresh generation: another reopen is a no-op
    let r2 = Repo::discover(&d.path().join("r")).unwrap();
    assert_eq!(descs(&r2), want_log);
}

/// Same-boot variant: only the renames of the LAST operation are missing (process killed after
/// the commit point). The next writer completes it.
#[test]
fn lost_renames_of_last_op_are_replayed_by_next_writer() {
    let (d, r) = setup();
    r.tag_create("root", "a", None, None).unwrap();
    let root = vcs(&r);
    let (refs_a, head_a) = (fs::read(root.join("refs")).unwrap(), fs::read(root.join("op_head")).unwrap());
    r.tag_create("root", "b", None, None).unwrap();
    let head_b = r.op_head().unwrap().unwrap();
    drop(r);
    fs::write(root.join("refs"), refs_a).unwrap();
    fs::write(root.join("op_head"), head_a).unwrap();
    let hex = head_b.to_hex();
    fs::remove_file(root.join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    let r = Repo::discover(&d.path().join("r")).unwrap();
    r.tag_create("root", "c", None, None).unwrap();
    assert_eq!(tags(&r), ["a", "b", "c"]);
    assert_consistent(&r);
}

/// A torn / garbage journal tail is ignored; earlier records stay valid.
#[test]
fn torn_journal_tail_is_ignored() {
    let (d, r) = setup();
    r.tag_create("root", "a", None, None).unwrap();
    r.tag_create("root", "b", None, None).unwrap();
    drop(r);
    let jp = d.path().join("r/.vcs/oplog.journal");
    let mut j = fs::read(&jp).unwrap();
    // find the end of valid data: first all-zero 64 byte run after the header
    let end = (64..j.len()).find(|&i| j[i..].iter().take(64).all(|b| *b == 0)).unwrap();
    // corrupt the last few bytes before the end (tears the newest record) and append junk
    for b in &mut j[end - 8..end] {
        *b ^= 0xFF;
    }
    j[end + 10..end + 40].copy_from_slice(&[0x5A; 30]);
    fs::write(&jp, &j).unwrap();
    let r = Repo::discover(&d.path().join("r")).unwrap();
    // the torn record "never happened" only if it was torn before it was acknowledged; here it
    // was acknowledged on disk already (refs/op_head applied), so the state stays consistent
    r.tag_create("root", "c", None, None).unwrap();
    assert!(tags(&r).contains(&"c".to_string()));
    assert_consistent(&r);
}

/// More operations than fit the journal: checkpoints happen, nothing is lost.
#[test]
fn journal_full_checkpoints_and_keeps_every_op() {
    let (_d, r) = setup();
    let n = 600;
    for i in 0..n {
        r.tag_create("root", &format!("tag{i:04}"), None, None).unwrap();
    }
    assert_eq!(tags(&r).len(), n);
    // init + commit + n tag creates
    assert_eq!(descs(&r).len(), 2 + n);
    assert_consistent(&r);
    let jlen = fs::metadata(vcs(&r).join("oplog.journal")).unwrap().len();
    assert_eq!(jlen, 64 * 1024, "journal is fixed size");
}

/// A writer outside the journal (an older vcs, a hand edit) must never be rolled back.
#[test]
fn foreign_refs_write_is_not_rolled_back() {
    let (_d, r) = setup();
    r.tag_create("root", "a", None, None).unwrap();
    let mut refs = r.refs().unwrap();
    let c = r.resolve_rev("root", "HEAD").unwrap();
    refs.tags.insert(("root".into(), "foreign".into()), c);
    fs::write(vcs(&r).join("refs"), refs.encode()).unwrap();
    r.tag_create("root", "b", None, None).unwrap();
    assert!(tags(&r).contains(&"foreign".to_string()));
    assert!(tags(&r).contains(&"b".to_string()));
}

/// Every non-journalled writer of `refs` must checkpoint first (`write_refs`); a raw
/// `write_atomic(...join("refs")...)` elsewhere would break the roll-forward invariant.
#[test]
fn no_raw_refs_writes_outside_the_journal_funnel() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut bad = Vec::new();
    for e in fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "rs") || p.file_name().unwrap() == "journal.rs" {
            continue;
        }
        let text = fs::read_to_string(&p).unwrap();
        for (i, l) in text.lines().enumerate() {
            let l = l.trim();
            // `Repo::init` writes the first, empty refs before any journal can exist
            if !l.starts_with("//") && l.contains("write_atomic(") && l.contains("join(\"refs\")") && !l.contains("r.vcs.join") {
                bad.push(format!("{}:{}", p.display(), i + 1));
            }
        }
    }
    assert!(bad.is_empty(), "use Repo::write_refs / commit_refs_op instead: {bad:?}");
}

/// The first journalled operation marks the repository with the `ref-journal` feature, so a
/// binary that does not know the journal refuses to open it instead of ignoring the journal.
#[test]
fn journal_marks_repo_with_ref_journal_feature() {
    let (d, r) = setup();
    r.tag_create("root", "a", None, None).unwrap();
    let feats = fs::read_to_string(vcs(&r).join("features")).unwrap();
    assert!(feats.lines().any(|l| l == "ref-journal"), "features: {feats:?}");
    drop(r);
    // the current binary still opens it
    Repo::discover(&d.path().join("r")).unwrap();
    // a future/unknown feature is refused (the same gate an older binary applies to `ref-journal`)
    fs::write(d.path().join("r/.vcs/features"), format!("{feats}some-future-feature\n")).unwrap();
    let e = Repo::discover(&d.path().join("r")).err().expect("must refuse").to_string();
    assert!(e.contains("some-future-feature"), "{e}");
}

/// Crash after the journal record is durable but before any rename (refs, op_head stay old): the
/// next writer rolls the record forward. If the record is torn instead, the op never happened.
#[test]
fn journal_written_but_refs_not_applied_then_torn_record() {
    for tear in [false, true] {
        let (d, r) = setup();
        r.tag_create("root", "base", None, None).unwrap();
        let before_refs = fs::read(vcs(&r).join("refs")).unwrap();
        let before_head = fs::read(vcs(&r).join("op_head")).unwrap();
        r.tag_create("root", "victim", None, None).unwrap();
        drop(r);
        // simulate: journal fsynced, renames never happened
        fs::write(d.path().join("r/.vcs/refs"), &before_refs).unwrap();
        fs::write(d.path().join("r/.vcs/op_head"), &before_head).unwrap();
        if tear {
            let jp = d.path().join("r/.vcs/oplog.journal");
            let mut j = fs::read(&jp).unwrap();
            let end = (64..j.len()).find(|&i| j[i..].iter().take(64).all(|b| *b == 0)).unwrap();
            for b in &mut j[end - 8..end] {
                *b ^= 0xFF; // tear the newest record's payload tail
            }
            fs::write(&jp, &j).unwrap();
        }
        let r = Repo::discover(&d.path().join("r")).unwrap();
        r.tag_create("root", "after", None, None).unwrap(); // takes the lock => recovery
        let t = tags(&r);
        assert_eq!(t.contains(&"victim".to_string()), !tear, "tear={tear}: {t:?}");
        assert!(t.contains(&"base".to_string()) && t.contains(&"after".to_string()));
        assert_consistent(&r);
    }
}

/// Deferred ref durability: a power cut that loses the whole unsynced tail (journal record and
/// renames alike) leaves the previous consistent state; only the most recent ref updates vanish.
#[test]
fn power_cut_losing_deferred_ref_updates_leaves_previous_state() {
    let (d, r) = setup();
    r.tag_create("root", "a", None, None).unwrap();
    r.flush_refs().unwrap();
    let root = vcs(&r);
    let img = d.path().join("img");
    copy_dir(&root, &img);
    r.tag_create("root", "b", None, None).unwrap();
    r.tag_delete("root", "a").unwrap();
    assert!(root.join("oplog.dirty").exists());
    drop(r);
    for f in ["refs", "op_head", "oplog.journal"] {
        fs::copy(img.join(f), root.join(f)).unwrap();
    }
    let _ = fs::remove_file(root.join("oplog.dirty"));
    let r = Repo::discover(&d.path().join("r")).unwrap();
    assert_eq!(tags(&r), ["a"]);
    assert_consistent(&r);
    r.tag_create("root", "c", None, None).unwrap();
    assert_eq!(tags(&r), ["a", "c"]);
    assert_consistent(&r);
}
