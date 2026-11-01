#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_objects::ObjectId;
use vcs_remote::{Backend, MemBackend};

// A remote pack read with ranged GETs: covers the pack v1 index parser, the trailer check and the
// length arithmetic of fetch_from_pack. Input: 32-byte wanted id, u16 LE index length, index, pack.
fuzz_target!(|data: &[u8]| {
    if data.len() < 34 {
        return;
    }
    let id: [u8; 32] = data[..32].try_into().unwrap();
    let n = u16::from_le_bytes([data[32], data[33]]) as usize;
    let rest = &data[34..];
    let (idx, pack) = rest.split_at(n.min(rest.len()));
    let b = MemBackend::new();
    b.put_if_absent("packs/x.idx", idx).unwrap();
    b.put_if_absent("packs/x.pack", pack).unwrap();
    let _ = vcs_remote::packfetch::fetch_from_pack(&b, "packs/x", &ObjectId::from_bytes(id));
});
