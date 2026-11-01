#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_merge::CommitGraphFile;

fuzz_target!(|data: &[u8]| {
    // the file is protected by a trailing checksum, so also feed a resealed copy
    let mut sealed = data.to_vec();
    let sum = *blake3::hash(&sealed).as_bytes();
    sealed.extend_from_slice(&sum);
    for b in [data, &sealed[..]] {
        if let Ok(g) = CommitGraphFile::decode(b) {
            let again = CommitGraphFile::decode(&g.encode()).expect("re-encoded graph must decode");
            assert_eq!(again.encode(), g.encode());
        }
    }
});
