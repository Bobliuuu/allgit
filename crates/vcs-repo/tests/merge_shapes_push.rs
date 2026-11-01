//! `merge --no-ff|--squash|--ff-only|-m` and forced / lease / delete publishes.
use std::fs;
use vcs_objects::{Object, Signature};
use vcs_remote::{load_manifest, Backend, FsBackend, MemBackend};
use vcs_repo::{ForceSpec, MergeOpts, MergeOutcome, PublishOpts, Repo};

fn sig() -> Signature {
    Signature {
        name: "T".into(),
        email: "t@x".into(),
        timestamp: 1000,
    }
}
fn write(r: &Repo, p: &str, c: &str) {
    let abs = r.worktree().join(p);
    fs::create_dir_all(abs.parent().unwrap()).unwrap();
    fs::write(abs, c).unwrap();
}
fn read(r: &Repo, p: &str) -> String {
    fs::read_to_string(r.worktree().join(p)).unwrap()
}
fn commit_all(r: &Repo, m: &str) {
    r.add(&["".into()]).unwrap();
    r.commit(m, sig()).unwrap();
}
fn head(r: &Repo) -> vcs_objects::ObjectId {
    r.refs_snapshot_head("root", "main")
}
trait HeadOf {
    fn refs_snapshot_head(&self, zone: &str, branch: &str) -> vcs_objects::ObjectId;
}
impl HeadOf for Repo {
    fn refs_snapshot_head(&self, zone: &str, branch: &str) -> vcs_objects::ObjectId {
        self.branches(zone)
            .unwrap()
            .into_iter()
            .find(|(b, _, _)| b == branch)
            .unwrap()
            .1
            .unwrap()
    }
}
fn commit_parents(r: &Repo, id: &vcs_objects::ObjectId) -> usize {
    match r.store().get(id).unwrap() {
        Object::Commit(c) => c.parents.len(),
        o => panic!("{o:?}"),
    }
}

// ---------------------------------------------------------------- merge shapes

/// main has `base`; `feature` is one commit ahead (so a plain merge fast-forwards).
fn ahead() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "f", "1\n");
    commit_all(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "g", "new\n");
    commit_all(&r, "feature work");
    r.checkout("root", "main").unwrap();
    (d, r)
}

#[test]
fn no_ff_makes_a_merge_commit_where_a_fast_forward_was_possible() {
    let (_d, r) = ahead();
    let opts = MergeOpts {
        no_ff: true,
        message: Some("Merge feature (no-ff)".into()),
        ..Default::default()
    };
    let out = r.merge_with("root", "feature", false, &opts, sig()).unwrap();
    let MergeOutcome::Merged { commit } = out else { panic!("{out:?}") };
    assert_eq!(commit_parents(&r, &commit), 2);
    assert_eq!(read(&r, "g"), "new\n");
    let log = r.log("root", None, 1).unwrap();
    assert_eq!(log[0].1.message, "Merge feature (no-ff)");
    assert!(r.fsck().unwrap().problems.is_empty());
    // merging again is a no-op
    assert_eq!(
        r.merge_with("root", "feature", false, &opts, sig()).unwrap(),
        MergeOutcome::UpToDate
    );
}

#[test]
fn ff_only_fast_forwards_or_refuses_and_changes_nothing() {
    let (_d, r) = ahead();
    let opts = MergeOpts {
        ff_only: true,
        ..Default::default()
    };
    assert!(matches!(
        r.merge_with("root", "feature", false, &opts, sig()).unwrap(),
        MergeOutcome::FastForward { .. }
    ));
    // now diverge main
    r.create_branch("root", "other").unwrap();
    r.checkout("root", "other").unwrap();
    write(&r, "h", "o\n");
    commit_all(&r, "other");
    r.checkout("root", "main").unwrap();
    write(&r, "i", "m\n");
    commit_all(&r, "main");
    let before = head(&r);
    let e = r.merge_with("root", "other", false, &opts, sig()).unwrap_err().to_string();
    assert!(e.contains("cannot fast-forward"), "{e}");
    assert_eq!(head(&r), before);
    assert!(!r.worktree().join("h").exists());
}

#[test]
fn squash_stages_the_result_without_moving_the_branch() {
    let (_d, r) = ahead();
    let before = head(&r);
    let opts = MergeOpts {
        squash: true,
        ..Default::default()
    };
    let out = r.merge_with("root", "feature", false, &opts, sig()).unwrap();
    assert_eq!(out, MergeOutcome::Squashed { changed: 1 });
    assert_eq!(head(&r), before, "branch must not move");
    assert_eq!(read(&r, "g"), "new\n");
    let st = r.status().unwrap();
    assert!(st.merging.is_none(), "squash leaves no merge state");
    assert!(st.zones.iter().any(|z| !z.staged.is_empty()), "result is staged");
    r.commit("squashed", sig()).unwrap();
    assert_eq!(commit_parents(&r, &head(&r)), 1, "squash commit has a single parent");
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn squash_of_a_diverged_branch_merges_content_and_conflicts_change_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    write(&r, "f", "a\nb\nc\nd\ne\nf\ng\nh\n");
    commit_all(&r, "base");
    r.create_branch("root", "feature").unwrap();
    r.checkout("root", "feature").unwrap();
    write(&r, "f", "a\nFEAT\nc\nd\ne\nf\ng\nh\n");
    write(&r, "new", "n\n");
    commit_all(&r, "feat");
    r.checkout("root", "main").unwrap();
    write(&r, "f", "a\nMAIN\nc\nd\ne\nf\ng\nh\n");
    commit_all(&r, "main");
    let before = head(&r);
    let opts = MergeOpts {
        squash: true,
        ..Default::default()
    };
    let e = r.merge_with("root", "feature", false, &opts, sig()).unwrap_err().to_string();
    assert!(e.contains("would conflict"), "{e}");
    assert_eq!(head(&r), before);
    assert_eq!(read(&r, "f"), "a\nMAIN\nc\nd\ne\nf\ng\nh\n");
    assert!(r.status().unwrap().merging.is_none());
    // a disjoint squash works
    r.checkout("root", "feature").unwrap();
    write(&r, "f", "a\nb\nc\nd\ne\nf\ng\nh\n");
    commit_all(&r, "feat drop f edit");
    r.checkout("root", "main").unwrap();
    let out = r.merge_with("root", "feature", false, &opts, sig()).unwrap();
    assert!(matches!(out, MergeOutcome::Squashed { .. }), "{out:?}");
    assert_eq!(read(&r, "new"), "n\n");
    assert_eq!(head(&r), before);
}

#[test]
fn merge_shapes_are_exclusive() {
    let (_d, r) = ahead();
    let opts = MergeOpts {
        no_ff: true,
        squash: true,
        ..Default::default()
    };
    assert!(r.merge_with("root", "feature", false, &opts, sig()).is_err());
}

// ---------------------------------------------------------------- forced publish

/// A and B cloned at X (remote main = X).
fn two_users(remote: &dyn Backend, dir: &std::path::Path) -> (Repo, Repo) {
    let a = Repo::init(&dir.join("a")).unwrap();
    write(&a, "f", "x\n");
    commit_all(&a, "X");
    a.publish(remote, None, sig()).unwrap();
    let b = Repo::clone_from(remote, "mem", &dir.join("b"), None).unwrap();
    (a, b)
}
fn remote_head(remote: &dyn Backend) -> vcs_objects::ObjectId {
    load_manifest(remote).unwrap().0.head("root", "main").unwrap()
}
fn force(branch: &str, spec: ForceSpec) -> PublishOpts {
    PublishOpts {
        branches: vec![branch.into()],
        force: Some(spec),
        delete: vec![],
    }
}

#[test]
fn plain_publish_still_refuses_non_fast_forward_and_points_at_force() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    b.publish(&remote, None, sig()).unwrap();
    write(&a, "f", "a\n");
    commit_all(&a, "A");
    a.fetch(&remote, None).unwrap();
    let e = a.publish(&remote, None, sig()).unwrap_err().to_string();
    assert!(e.contains("vcs publish --force"), "{e}");
}

#[test]
fn force_with_default_lease_replaces_the_head_and_records_it_everywhere() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    b.publish(&remote, None, sig()).unwrap();
    let y = remote_head(&remote);
    write(&a, "f", "a\n");
    commit_all(&a, "A");
    let z = head(&a);
    a.fetch(&remote, None).unwrap(); // lease: "I have seen Y"
    let rep = a
        .publish_with(&remote, None, sig(), "t@x", &force("main", ForceSpec::default()))
        .unwrap();
    assert_eq!(rep.forced, vec![("root".to_string(), "main".to_string(), y, z)]);
    assert_eq!(remote_head(&remote), z);
    // the replaced head is recorded: remote tx (as `old`), local op log (full id)
    let (m, _) = load_manifest(&remote).unwrap();
    let last = m.applied.last().unwrap();
    let tx = remote.get(&format!("tx/{}", last.to_hex())).unwrap().expect("tx stored");
    let Object::Transaction(t) = Object::decode(&tx.data).unwrap() else {
        panic!()
    };
    assert_eq!(t.updates[0].old, Some(y));
    assert_eq!(t.message, "publish --force-with-lease");
    let ops = a.op_log(5).unwrap();
    let desc = &ops[0].1.description;
    assert!(desc.contains("publish --force") && desc.contains(&y.to_hex()), "{desc}");
    // and the old head is recoverable as a branch
    a.create_branch_at("root", "main-before-force", &y.to_hex()).unwrap();
    assert_eq!(
        a.branches("root")
            .unwrap()
            .iter()
            .find(|(n, ..)| n == "main-before-force")
            .unwrap()
            .1,
        Some(y)
    );
    // the replaced head stays in the local store (the stored transaction is a GC root)
    assert!(a.store().contains(&y).unwrap());
}

#[test]
fn stale_lease_is_refused_and_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    b.publish(&remote, None, sig()).unwrap();
    let y = remote_head(&remote);
    write(&a, "f", "a\n");
    commit_all(&a, "A"); // A has NOT fetched Y
    let e = a
        .publish_with(&remote, None, sig(), "t@x", &force("main", ForceSpec::default()))
        .unwrap_err()
        .to_string();
    assert!(e.contains("stale info"), "{e}");
    assert_eq!(remote_head(&remote), y, "remote untouched");
}

#[test]
fn never_fetched_branch_has_nothing_to_lease_against() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let a = Repo::init(&d.path().join("a")).unwrap();
    write(&a, "f", "1\n");
    commit_all(&a, "1");
    a.publish(&remote, None, sig()).unwrap();
    // second repo with unrelated history, never fetched
    let c = Repo::init(&d.path().join("c")).unwrap();
    write(&c, "f", "other\n");
    commit_all(&c, "other");
    let e = c
        .publish_with(&remote, None, sig(), "t@x", &force("main", ForceSpec::default()))
        .unwrap_err()
        .to_string();
    assert!(e.contains("never fetched"), "{e}");
}

#[test]
fn explicit_lease_value_and_without_lease() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    b.publish(&remote, None, sig()).unwrap();
    let y = remote_head(&remote);
    write(&a, "f", "a\n");
    commit_all(&a, "A");
    let z = head(&a);
    let spec = |id: &str| ForceSpec {
        without_lease: false,
        expect: [("main".to_string(), id.to_string())].into(),
    };
    // wrong expectation (the head A last published) -> refused
    let wrong = &y.to_hex()[..8].replace(|c: char| c != 'z', "0"); // 8 zeros-ish: not y
    let e = a
        .publish_with(&remote, None, sig(), "t@x", &force("main", spec(wrong)))
        .unwrap_err()
        .to_string();
    assert!(e.contains("stale lease"), "{e}");
    assert_eq!(remote_head(&remote), y);
    // right expectation by prefix, even though A never fetched
    a.publish_with(&remote, None, sig(), "t@x", &force("main", spec(&y.to_hex()[..10])))
        .unwrap();
    assert_eq!(remote_head(&remote), z);
    // without-lease: replaces whatever is there, recorded as such
    write(&b, "f", "b2\n");
    commit_all(&b, "B2");
    let z2 = head(&b);
    let rep = b
        .publish_with(
            &remote,
            None,
            sig(),
            "t@x",
            &force(
                "main",
                ForceSpec {
                    without_lease: true,
                    ..Default::default()
                },
            ),
        )
        .unwrap();
    assert_eq!(rep.forced.len(), 1);
    assert_eq!(remote_head(&remote), z2);
    let (m, _) = load_manifest(&remote).unwrap();
    let tx = remote.get(&format!("tx/{}", m.applied.last().unwrap().to_hex())).unwrap().unwrap();
    let Object::Transaction(t) = Object::decode(&tx.data).unwrap() else {
        panic!()
    };
    assert_eq!(t.message, "publish --force-without-lease");
}

#[test]
fn force_without_names_forces_each_checked_out_non_fast_forward_branch_under_its_lease() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    b.publish(&remote, None, sig()).unwrap();
    write(&a, "f", "a\n");
    commit_all(&a, "A");
    let o = PublishOpts {
        force: Some(ForceSpec::default()),
        ..Default::default()
    };
    // not fetched: the lease protects B's work even without a branch name
    assert!(a
        .publish_with(&remote, None, sig(), "t@x", &o)
        .unwrap_err()
        .to_string()
        .contains("stale info"));
    a.fetch(&remote, None).unwrap();
    assert_eq!(a.publish_with(&remote, None, sig(), "t@x", &o).unwrap().forced.len(), 1);
}

/// Two users on a directory remote both hold lease X and force at the same moment: exactly one
/// wins, the loser fails stale and nothing is lost.
#[test]
fn two_user_race_on_a_directory_remote_one_lease_wins() {
    let d = tempfile::tempdir().unwrap();
    let rdir = d.path().join("remote");
    let remote = FsBackend::open(&rdir).unwrap();
    let (a, b) = two_users(&remote, d.path());
    let x = remote_head(&remote);
    write(&a, "f", "a\n");
    commit_all(&a, "A");
    write(&b, "f", "b\n");
    commit_all(&b, "B");
    let (za, zb) = (head(&a), head(&b));
    // both are diverged from X? No: X is an ancestor of both, so force only matters once one lands.
    // Make both truly non-fast-forward by letting a third commit land first.
    let c = Repo::clone_from(&remote, "fs", &d.path().join("c"), None).unwrap();
    write(&c, "f", "c\n");
    commit_all(&c, "C");
    c.publish(&remote, None, sig()).unwrap();
    let y = remote_head(&remote);
    assert_ne!(x, y);
    a.fetch(&remote, None).unwrap();
    b.fetch(&remote, None).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let run = |r: Repo| {
        let (rd, bar) = (rdir.clone(), barrier.clone());
        std::thread::spawn(move || {
            let be = FsBackend::open(&rd).unwrap();
            bar.wait();
            r.publish_with(&be, None, sig(), "t@x", &force("main", ForceSpec::default()))
                .map(|_| ())
        })
    };
    let (ha, hb) = (run(a), run(b));
    let (ra, rb) = (ha.join().unwrap(), hb.join().unwrap());
    assert!(ra.is_ok() != rb.is_ok(), "exactly one force wins: {ra:?} / {rb:?}");
    let winner = remote_head(&remote);
    assert!(winner == za || winner == zb);
    let loser = if ra.is_ok() { rb } else { ra };
    let msg = loser.unwrap_err().to_string();
    assert!(msg.contains("moved") || msg.contains("stale"), "{msg}");
}

#[test]
fn delete_remote_branch_is_a_lease_and_restorable() {
    let d = tempfile::tempdir().unwrap();
    let remote = MemBackend::new();
    let (a, b) = two_users(&remote, d.path());
    a.create_branch("root", "topic").unwrap();
    a.checkout("root", "topic").unwrap();
    write(&a, "t", "t\n");
    commit_all(&a, "topic work");
    let topic = a
        .branches("root")
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == "topic")
        .unwrap()
        .1
        .unwrap();
    a.checkout("root", "main").unwrap();
    // publish the topic branch: check it out, publish, switch back
    a.checkout("root", "topic").unwrap();
    a.publish(&remote, None, sig()).unwrap();
    a.checkout("root", "main").unwrap();
    assert!(load_manifest(&remote).unwrap().0.head("root", "topic").is_some());
    // B never fetched the topic branch -> delete refused
    let del = PublishOpts {
        delete: vec!["topic".into()],
        ..Default::default()
    };
    assert!(b.publish_with(&remote, None, sig(), "t@x", &del).is_err());
    assert!(load_manifest(&remote).unwrap().0.head("root", "topic").is_some());
    // A last pushed it, so A's view matches: delete works
    let rep = a.publish_with(&remote, None, sig(), "t@x", &del).unwrap();
    assert_eq!(rep.deleted, vec![("root".to_string(), "topic".to_string(), topic)]);
    assert!(load_manifest(&remote).unwrap().0.head("root", "topic").is_none());
    assert!(load_manifest(&remote).unwrap().0.head("root", "main").is_some(), "main untouched");
    // deleting something that is not there
    assert!(a
        .publish_with(&remote, None, sig(), "t@x", &del)
        .unwrap_err()
        .to_string()
        .contains("no branch"));
    // the deleted head is in the op log and recoverable
    assert!(a.op_log(3).unwrap()[0].1.description.contains(&topic.to_hex()));
    a.create_branch_at("root", "topic-restored", &topic.to_hex()).unwrap();
}
