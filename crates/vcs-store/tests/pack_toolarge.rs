//! Issue #574: lengths are 32-bit in the pack formats. An object that does not fit must fail the
//! write with an error, never wrap the length and write a corrupt pack. The buffers below are
//! zero-filled allocations the OS maps lazily, and the checks run before any byte is touched.

use std::borrow::Cow;
use vcs_objects::ObjectId;
use vcs_store::pack::{try_build_pack_fmt, write_pack_streaming, PackFormat};
use vcs_store::StoreError;

const HUGE: usize = (u32::MAX as usize) + 8;

fn id(n: u8) -> ObjectId {
    ObjectId::from_bytes([n; 32])
}

#[test]
fn build_rejects_an_object_over_4_gib_in_every_format() {
    for fmt in [PackFormat::V1, PackFormat::V2, PackFormat::V3, PackFormat::V4] {
        let e = try_build_pack_fmt(vec![(id(1), b"small".to_vec()), (id(2), vec![0u8; HUGE])], fmt)
            .err()
            .unwrap_or_else(|| panic!("{fmt:?} accepted a 4 GiB object"));
        assert!(matches!(e, StoreError::TooLarge { .. }), "{e}");
    }
}

#[test]
fn build_accepts_ordinary_objects() {
    let b = try_build_pack_fmt(vec![(id(1), b"small".to_vec())], PackFormat::V2).unwrap();
    assert!(b.name.starts_with("pack-"));
}

#[test]
fn streaming_writer_rejects_a_huge_entry_and_leaves_no_pack() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tmp")).unwrap();
    let objs = vec![Ok((id(1), Cow::Owned(vec![0u8; HUGE])))];
    let e = write_pack_streaming(dir.path(), 1, objs.into_iter()).err().expect("must fail");
    assert!(matches!(e, StoreError::TooLarge { .. }), "{e}");
    let left: Vec<_> = std::fs::read_dir(dir.path().join("pack"))
        .map(|d| d.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "a failed write must not publish a pack: {left:?}");
}
