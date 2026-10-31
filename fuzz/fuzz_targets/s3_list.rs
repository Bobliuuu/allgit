#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = vcs_store_s3::parse_list_page(s, "");
        let _ = vcs_store_s3::parse_list_page(s, "repo/");
    }
});
