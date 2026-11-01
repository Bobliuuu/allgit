//! Security review (#282): a zstd frame header that claims far more output than any encoder can
//! produce from its input is refused before the output buffer is allocated; legitimate extreme
//! ratios (all zeros) still decode.

use vcs_store::pack::{decode_block, decode_payload};

/// `[magic][FHD: 8-byte content size, single segment][size][one RLE block of 128 KiB][1 byte]`: a
/// well-formed frame header whose body cannot possibly produce `claimed` bytes.
fn lying_frame(claimed: u64) -> Vec<u8> {
    let mut f = vec![0x28, 0xB5, 0x2F, 0xFD, 0xE0];
    f.extend_from_slice(&claimed.to_le_bytes());
    let block: u32 = 1 | (1 << 1) | ((128 * 1024) << 3); // last block, RLE, 128 KiB
    f.extend_from_slice(&block.to_le_bytes()[..3]);
    f.push(0);
    f
}

#[test]
fn lying_content_size_is_refused_before_allocation() {
    for claimed in [200u64 << 20, 1 << 30, 4_000_000_000] {
        let mut payload = vec![vcs_store::pack::CODEC_ZSTD];
        payload.extend(lying_frame(claimed));
        let e = decode_payload(2, &payload).unwrap_err();
        assert!(e.contains("no encoder produces that"), "{claimed}: {e}");
    }
    // a lying block header (v3 data block)
    let mut blk = vec![vcs_store::pack::CODEC_BLOCK_ZSTD_DATA];
    blk.extend(lying_frame(200 << 20));
    let e = decode_block(&blk).map(|_| ()).unwrap_err();
    assert!(e.contains("no encoder produces that"), "{e}");
}

#[test]
fn small_claims_and_honest_extreme_ratios_still_decode() {
    // below the floor any ratio is fine (and fails later as a length mismatch, not as a ratio error)
    let mut payload = vec![vcs_store::pack::CODEC_ZSTD];
    payload.extend(lying_frame(512 * 1024));
    let e = decode_payload(2, &payload).unwrap_err();
    assert!(!e.contains("no encoder produces that"), "{e}");
    // the most compressible real input: 96 MiB of zeros
    let raw = vec![0u8; 96 << 20];
    let z = zstd::bulk::compress(&raw, 3).unwrap();
    let ratio = raw.len() / z.len();
    let mut p = vec![vcs_store::pack::CODEC_ZSTD];
    p.extend(&z);
    let out = decode_payload(2, &p).unwrap();
    assert_eq!(out.len(), raw.len());
    assert!(ratio < 65_536, "real zstd ratio {ratio} must stay under the cap");
    eprintln!("96 MiB of zeros: {} compressed bytes, ratio {ratio}:1", z.len());
}
