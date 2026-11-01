#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_repo::ignore::{IgnoreFile, Pattern};

// .vcsignore text; after a 0x1e byte, paths to match against (one per line).
fuzz_target!(|data: &[u8]| {
    let mut it = data.splitn(2, |b| *b == 0x1e);
    let text = String::from_utf8_lossy(it.next().unwrap_or(&[])).into_owned();
    let paths = String::from_utf8_lossy(it.next().unwrap_or(&[])).into_owned();
    let _ = IgnoreFile::parse("", &text);
    let _ = IgnoreFile::parse("a/b", &text);
    for l in text.lines().take(64) {
        if let Some(p) = Pattern::parse(l) {
            for path in paths.lines().take(16) {
                let _ = p.matches(path, false);
                let _ = p.matches(path, true);
            }
        }
    }
});
