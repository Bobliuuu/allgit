#![no_main]
use libfuzzer_sys::fuzz_target;
use std::path::Path;
use vcs_store::pack::PackIndex;

fuzz_target!(|data: &[u8]| {
    // The trailing checksum would reject nearly every input before the parser; reseal so the
    // structural checks are what the fuzzer explores.
    let _ = PackIndex::parse(data, Path::new("fuzz.idx"));
    if data.len() > 32 {
        let mut b = data[..data.len() - 32].to_vec();
        let sum = *blake3::hash(&b).as_bytes();
        b.extend_from_slice(&sum);
        let _ = PackIndex::parse(&b, Path::new("fuzz.idx"));
    }
});
