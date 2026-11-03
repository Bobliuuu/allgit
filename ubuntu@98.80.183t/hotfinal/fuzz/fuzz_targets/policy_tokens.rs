#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_server::{Policy, TokenTable};

// First byte selects tokens file (even) or policy file (odd); the rest is the file.
fuzz_target!(|data: &[u8]| {
    let Some((sel, rest)) = data.split_first() else { return };
    let text = String::from_utf8_lossy(rest);
    if sel % 2 == 0 {
        if let Ok(t) = TokenTable::parse(&text) {
            let _ = t.authenticate("tok");
            let _ = t.authenticate_at("tok", 1_000);
        }
    } else if let Ok(p) = Policy::parse(&text) {
        for who in ["alice", "bob", ""] {
            let _ = p.zone_access(who, "root");
            for path in ["src/main.rs", "a/b/c.txt", ".github/x", ""] {
                let _ = p.check_path(who, false, "root", path);
                let _ = p.check_path(who, true, "docs", path);
            }
        }
    }
});
