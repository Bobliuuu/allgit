//! Idempotency under a shrunken applied-txid window (`Publisher::with_applied_window`, #252).
//!
//! The manifest remembers the newest N txids so that a publisher whose CAS response was lost
//! recognises its own transaction on the next Load ("applied but ack lost"). A smaller N makes
//! the manifest smaller (32 bytes per entry) but weakens that recognition once N other
//! publishes have landed in between. These tests state exactly what is and is not lost:
//!  * within the window: the retry is always `AlreadyApplied` (exactly-once);
//!  * past the window, disjoint branches: the retry is rejected `Diverged`, the transaction's
//!    effect is still present exactly once (a spurious error, never a double apply);
//!  * past the window with the branch back at its old value (ABA): the retry applies AGAIN.
//!    This is the failure the window bound exists to prevent, kept as a negative-result test
//!    so the requirement `window > publishes during a publisher's retry horizon` stays visible.
//!
//! The default window (4096 = `MAX_APPLIED`) is exercised by `model.rs`'s `duplicate_submission_applies_once`.

use vcs_objects::*;
use vcs_remote::manifest::MAX_APPLIED;
use vcs_remote::*;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, msg: &str) -> ObjectId {
    let t = s.put(&Object::Tree(Tree::new(vec![]).unwrap())).unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root: t,
        change_id: ChangeId::from_bytes([msg.len() as u8; 16]),
        author: sig(),
        message: msg.into(),
    }))
    .unwrap()
}

fn tx(zone: &str, old: Option<ObjectId>, new: ObjectId, msg: &str) -> Transaction {
    Transaction {
        updates: vec![RefUpdate {
            zone: zone.into(),
            branch: "main".into(),
            old,
            new,
        }],
        author: sig(),
        message: msg.into(),
        lock_ops: vec![],
    }
}

fn run(b: &MemBackend, s: &MemoryStore, t: Transaction, window: usize) -> Result<Outcome, PublishError> {
    Publisher::new(b, s, t).with_loose_objects().with_applied_window(window).run()
}

/// Publisher A applies `ta` but its CAS response is lost; it stops in `Load` (retry pending).
fn lost_ack<'a>(b: &'a MemBackend, s: &'a MemoryStore, ta: Transaction, window: usize) -> Publisher<'a> {
    let mut p = Publisher::new(b, s, ta).with_loose_objects().with_applied_window(window);
    while !p.next_is_cas() {
        assert_eq!(p.step().unwrap(), Step::Continue);
    }
    b.inject_cas_fault(CasFault::LostAck);
    assert_eq!(p.step().unwrap(), Step::Continue, "lost ack is retried, not fatal");
    p
}

#[test]
fn within_the_window_a_lost_ack_retry_is_already_applied() {
    for others in 0..3usize {
        let (b, s) = (MemBackend::new(), MemoryStore::new());
        let a1 = commit(&s, "a", None, "a1");
        let p = lost_ack(&b, &s, tx("a", None, a1, "A"), others + 1);
        let mut head = None;
        for i in 0..others {
            let c = commit(&s, "b", head, &format!("b{i}"));
            run(&b, &s, tx("b", head, c, &format!("B{i}")), others + 1).unwrap();
            head = Some(c);
        }
        assert_eq!(
            p.run().unwrap(),
            Outcome::AlreadyApplied,
            "{others} publishes in between, window {}",
            others + 1
        );
        let (m, _) = load_manifest(&b).unwrap();
        assert_eq!(m.head("a", "main"), Some(a1));
        assert_eq!(m.generation as usize, others + 1, "A applied exactly once");
    }
}

#[test]
fn past_the_window_disjoint_retry_is_diverged_never_doubled() {
    let (b, s) = (MemBackend::new(), MemoryStore::new());
    let a1 = commit(&s, "a", None, "a1");
    let p = lost_ack(&b, &s, tx("a", None, a1, "A"), 1);
    let b1 = commit(&s, "b", None, "b1");
    run(&b, &s, tx("b", None, b1, "B"), 1).unwrap(); // pushes A's txid out of the window of 1
    match p.run() {
        Err(PublishError::Diverged { zone, .. }) => assert_eq!(zone, "a"),
        other => panic!("expected Diverged, got {other:?}"),
    }
    let (m, _) = load_manifest(&b).unwrap();
    assert_eq!((m.head("a", "main"), m.generation, m.applied.len()), (Some(a1), 2, 1));
}

/// NEGATIVE RESULT: ABA past the window applies the same transaction twice.
#[test]
fn past_the_window_aba_retry_applies_twice() {
    let (b, s) = (MemBackend::new(), MemoryStore::new());
    let a0 = commit(&s, "a", None, "a0");
    run(&b, &s, tx("a", None, a0, "init"), 1).unwrap();
    let a1 = commit(&s, "a", Some(a0), "a1");
    let p = lost_ack(&b, &s, tx("a", Some(a0), a1, "A"), 1);
    run(&b, &s, tx("a", Some(a1), a0, "revert"), 1).unwrap(); // a1 -> a0, window 1 forgets A
    assert_eq!(
        p.run().unwrap(),
        Outcome::Applied { generation: 4 },
        "A re-applied: the window was too small"
    );
    // the same schedule with the default window is exactly-once
    let (b, s) = (MemBackend::new(), MemoryStore::new());
    let a0 = commit(&s, "a", None, "a0");
    run(&b, &s, tx("a", None, a0, "init"), MAX_APPLIED).unwrap();
    let a1 = commit(&s, "a", Some(a0), "a1");
    let p = lost_ack(&b, &s, tx("a", Some(a0), a1, "A"), MAX_APPLIED);
    run(&b, &s, tx("a", Some(a1), a0, "revert"), MAX_APPLIED).unwrap();
    assert_eq!(p.run().unwrap(), Outcome::AlreadyApplied);
    assert_eq!(load_manifest(&b).unwrap().0.generation, 3);
}

#[test]
fn window_is_clamped_and_prunes_oldest_first() {
    let (b, s) = (MemBackend::new(), MemoryStore::new());
    let mut head = None;
    let mut txids = vec![];
    for i in 0..5 {
        let c = commit(&s, "a", head, &format!("c{i}"));
        let t = tx("a", head, c, &format!("t{i}"));
        let p = Publisher::new(&b, &s, t).with_loose_objects().with_applied_window(3);
        txids.push(p.txid());
        p.run().unwrap();
        head = Some(c);
    }
    let (m, _) = load_manifest(&b).unwrap();
    assert_eq!(m.applied, txids[2..].to_vec(), "newest three, oldest dropped first");
    let c = commit(&s, "a", head, "z");
    let p = Publisher::new(&b, &s, tx("a", head, c, "z")).with_applied_window(0);
    p.run().unwrap();
    assert_eq!(load_manifest(&b).unwrap().0.applied.len(), 1, "window 0 is clamped to 1");
}
