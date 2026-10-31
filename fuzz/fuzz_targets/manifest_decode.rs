#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_remote::Manifest;

fuzz_target!(|data: &[u8]| {
    if let Ok(m) = Manifest::decode(data) {
        assert_eq!(m.encode(), data);
    }
});
