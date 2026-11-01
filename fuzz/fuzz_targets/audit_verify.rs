#![no_main]
use libfuzzer_sys::fuzz_target;

// First byte: 0 = no key, else a fixed key; the rest is the log file.
fuzz_target!(|data: &[u8]| {
    let Some((k, log)) = data.split_first() else { return };
    let key = (*k != 0).then(|| [*k; 32]);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("audit.log");
    std::fs::write(&p, log).unwrap();
    let _ = vcs_server::audit::verify_file(&p, key.as_ref());
    let _ = vcs_server::AuditLog::open(&p, key);
    if let Ok(s) = std::str::from_utf8(log) {
        let _ = vcs_server::audit::parse_key(s);
    }
});
