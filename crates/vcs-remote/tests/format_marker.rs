//! Minimum-reader format marker (issue #567): new writers raise it before writing anything an older
//! binary cannot read, and a binary below it refuses with a clear message instead of a decode error,
//! without having changed anything.

use vcs_objects::*;
use vcs_remote::compact::{compact, CompactOptions};
use vcs_remote::format::{self, Format, FormatError};
use vcs_remote::gc::{remote_gc, RemoteGcOptions};
use vcs_remote::sharded::migrate_to_sharded;
use vcs_remote::*;
use vcs_store::pack::PackFormat;
use vcs_store::{MemoryStore, ObjectStore};

fn sig() -> Signature {
    Signature {
        name: "t".into(),
        email: "t@x".into(),
        timestamp: 1,
    }
}

fn commit(s: &MemoryStore, zone: &str, parent: Option<ObjectId>, n: u32) -> ObjectId {
    let blob = s.put_blob(&n.to_le_bytes()).unwrap();
    let root = s
        .put(&Object::Tree(
            Tree::new(vec![TreeEntry {
                name: "f".into(),
                mode: EntryMode::File,
                id: blob,
            }])
            .unwrap(),
        ))
        .unwrap();
    s.put(&Object::Commit(Commit {
        zone: zone.into(),
        parents: parent.into_iter().collect(),
        root,
        change_id: ChangeId::from_bytes([n as u8; 16]),
        author: sig(),
        message: format!("{zone}{n}"),
    }))
    .unwrap()
}

fn publish(b: &dyn Backend, s: &MemoryStore, zone: &str, old: Option<ObjectId>, n: u32) -> Result<ObjectId, PublishError> {
    let new = commit(s, zone, old, n);
    Publisher::new(
        b,
        s,
        Transaction {
            updates: vec![RefUpdate {
                zone: zone.into(),
                branch: "main".into(),
                old,
                new,
            }],
            author: sig(),
            message: "m".into(),
            lock_ops: vec![],
        },
    )
    .run()?;
    Ok(new)
}

/// A manifest large enough that `encode` picks the compact v5 layout.
fn seed_large_manifest(b: &MemBackend) {
    let mut m = Manifest::default();
    for i in 0..3000u32 {
        let mut id = [0u8; 32];
        id[..4].copy_from_slice(&i.to_le_bytes());
        m.branches.insert(("big".into(), format!("branch-number-{i:05}")), ObjectId::from_bytes(id));
    }
    assert!(m.encode_legacy().len() > manifest::COMPACT_THRESHOLD);
    assert!(b.put_if_absent(manifest::KEY, &m.encode()).unwrap());
}

fn put_marker(b: &MemBackend, level: u32, writer: &str) {
    let f = Format {
        min_reader: level,
        writer: writer.into(),
    };
    assert!(b.put_if_absent(format::KEY, &f.encode()).unwrap());
}

#[test]
fn small_repositories_never_get_a_marker() {
    let b = MemBackend::new();
    let s = MemoryStore::new();
    let c = publish(&b, &s, "z", None, 1).unwrap();
    publish(&b, &s, "z", Some(c), 2).unwrap();
    assert!(b.get(format::KEY).unwrap().is_none(), "baseline repositories stay byte-compatible with unmarked binaries");
    assert_eq!(format::level(&b).unwrap(), format::LEVEL_BASELINE);
    format::check_as(&b, 1).unwrap();
}

#[test]
fn writer_raises_the_marker_before_storing_a_v5_manifest() {
    let b = MemBackend::new();
    seed_large_manifest(&b);
    assert!(b.get(format::KEY).unwrap().is_none());
    let s = MemoryStore::new();
    publish(&b, &s, "z", None, 1).unwrap();
    assert_eq!(b.get(manifest::KEY).unwrap().unwrap().data[4], 5, "the manifest is now the compact layout");
    assert_eq!(format::level(&b).unwrap(), format::LEVEL_COMPACT);
    // old reader / new writer: a level-1 binary refuses with an actionable message
    let e = format::check_as(&b, 1).unwrap_err();
    assert!(matches!(e, FormatError::TooNew { needed: 2, supported: 1, .. }), "{e:?}");
    let msg = e.to_string();
    assert!(msg.contains("requires vcs format level 2") && msg.contains("docs/upgrade-rollback.md"), "{msg}");
}

#[test]
fn binary_below_the_marker_refuses_and_changes_nothing() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(10_000_000);
    let s = MemoryStore::new();
    let c = publish(&b, &s, "z", None, 1).unwrap();
    let before = b.get(manifest::KEY).unwrap().unwrap();
    // a repository from the future: this binary supports LEVEL, the marker asks for more
    put_marker(&b, format::LEVEL + 1, "vcs 99.0.0");
    let keys_before = b.list("").unwrap();

    let msg = |e: &dyn std::fmt::Display| e.to_string();
    let e = load_manifest(&b).unwrap_err();
    assert!(matches!(e, PublishError::Format(FormatError::TooNew { .. })), "{e:?}");
    assert!(msg(&e).contains("vcs 99.0.0"), "names the writer: {e}");
    // publish, fetch, gc and compaction all refuse
    let e = publish(&b, &s, "z", Some(c), 2).unwrap_err();
    assert!(matches!(e, PublishError::Format(_)), "{e:?}");
    assert!(fetch_heads(&b, &MemoryStore::new(), None).is_err());
    let g = RemoteGcOptions {
        dry_run: false,
        ..Default::default()
    };
    assert!(matches!(remote_gc(&b, &g), Err(vcs_remote::gc::GcError::Format(_))));
    let co = CompactOptions {
        dry_run: false,
        all: true,
        ..Default::default()
    };
    assert!(compact(&b, &co).is_err());
    // and none of them wrote anything
    assert_eq!(b.get(manifest::KEY).unwrap().unwrap(), before);
    assert_eq!(b.list("").unwrap(), keys_before);
}

#[test]
fn raise_is_monotone_and_idempotent() {
    let b = MemBackend::new();
    assert!(!format::raise(&b, format::LEVEL_BASELINE).unwrap(), "level 1 needs no marker");
    assert!(format::raise(&b, 2).unwrap());
    assert!(!format::raise(&b, 2).unwrap());
    assert!(format::raise(&b, 3).unwrap());
    assert!(!format::raise(&b, 2).unwrap(), "never lowers");
    assert_eq!(format::level(&b).unwrap(), 3);
    // a writer that only supports level 2 will not touch a level-3 repository
    assert!(matches!(format::raise_as(&b, 2, 2), Err(FormatError::TooNew { .. })));
}

#[test]
fn malformed_and_future_marker_versions_are_refused() {
    let b = MemBackend::new();
    assert!(b.put_if_absent(format::KEY, b"garbage").unwrap());
    assert!(matches!(format::check(&b), Err(FormatError::Malformed(_))));
    let b = MemBackend::new();
    let mut bytes = Format {
        min_reader: 2,
        writer: "x".into(),
    }
    .encode();
    bytes[4] = 2; // a marker layout version this binary does not know
    assert!(b.put_if_absent(format::KEY, &bytes).unwrap());
    assert!(matches!(format::check(&b), Err(FormatError::TooNew { .. })));
    // round trip
    let f = Format {
        min_reader: 3,
        writer: "vcs 1.2.3".into(),
    };
    assert_eq!(Format::decode(&f.encode()).unwrap(), f);
}

#[test]
fn compaction_to_v3_raises_and_v2_does_not() {
    for (fmt, want) in [(PackFormat::V2, 1), (PackFormat::V3, 2)] {
        let b = MemBackend::new().with_gc();
        b.set_now_ms(1_000_000);
        let s = MemoryStore::new();
        let mut head = None;
        for n in 1..=3 {
            head = Some(publish(&b, &s, "z", head, n).unwrap());
        }
        let o = CompactOptions {
            dry_run: false,
            all: true,
            format: fmt,
            ..Default::default()
        };
        compact(&b, &o).unwrap();
        assert_eq!(format::level(&b).unwrap(), want, "{fmt:?}");
    }
}

#[test]
fn migrate_sharded_raises_to_level_3() {
    let b = MemBackend::new();
    let s = MemoryStore::new();
    publish(&b, &s, "z", None, 1).unwrap();
    migrate_to_sharded(&b).unwrap();
    assert_eq!(format::level(&b).unwrap(), format::LEVEL_SHARDED);
    assert!(format::check_as(&b, 2).is_err(), "a level-2 binary no longer opens it");
}

#[test]
fn lower_is_refused_while_data_needs_the_level_and_works_after_rollback() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    seed_large_manifest(&b);
    let s = MemoryStore::new();
    publish(&b, &s, "z", None, 1).unwrap();
    assert_eq!(format::level(&b).unwrap(), 2);
    // v5 manifest still in place: lowering would turn a clean refusal back into a decode error
    let e = format::lower(&b, 1).unwrap_err();
    assert!(matches!(e, FormatError::InUse { .. }), "{e:?}");
    assert_eq!(format::level(&b).unwrap(), 2);
    // restore a pre-upgrade (v1-v4) manifest, as the runbook says
    let small = Manifest::default().encode();
    let cur = b.get(manifest::KEY).unwrap().unwrap();
    b.put_if_match(manifest::KEY, &small, &cur.tag).unwrap().unwrap();
    assert!(format::lower(&b, 1).unwrap());
    assert!(b.get(format::KEY).unwrap().is_none());
    format::check_as(&b, 1).unwrap();
}

#[test]
fn lower_is_refused_while_v3_packs_exist() {
    let b = MemBackend::new().with_gc();
    b.set_now_ms(1_000_000);
    let s = MemoryStore::new();
    let mut head = None;
    for n in 1..=3 {
        head = Some(publish(&b, &s, "z", head, n).unwrap());
    }
    let o = CompactOptions {
        dry_run: false,
        all: true,
        format: PackFormat::V3,
        ..Default::default()
    };
    compact(&b, &o).unwrap();
    assert!(matches!(format::lower(&b, 1), Err(FormatError::InUse { .. })));
    let o2 = CompactOptions {
        format: PackFormat::V2,
        ..o
    };
    // re-compacting to v2 needs >= 2 packs; add one more
    publish(&b, &s, "z", head, 9).unwrap();
    compact(&b, &o2).unwrap();
    assert!(format::lower(&b, 1).unwrap());
}
