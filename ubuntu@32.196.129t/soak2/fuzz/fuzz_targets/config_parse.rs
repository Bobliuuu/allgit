#![no_main]
use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "../../crates/vcs-cli/src/config.rs"]
mod config;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let _ = config::parse(s, std::path::Path::new("fuzz"));
    if let Some((k, v)) = s.split_once('\n') {
        let _ = config::validate_key(k);
        let _ = config::validate_value(k, v);
    }
});
