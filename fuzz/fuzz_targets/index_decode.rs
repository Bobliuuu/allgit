#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_repo::index::Index;

fuzz_target!(|data: &[u8]| {
    if let Ok(i) = Index::decode(data) {
        let again = Index::decode(&i.encode()).expect("re-encoded index must decode");
        assert_eq!(again.encode(), i.encode());
    }
});
