#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_objects::Object;

fuzz_target!(|data: &[u8]| {
    if let Ok(o) = Object::decode(data) {
        // canonical form: one object, one encoding
        assert_eq!(o.encode(), data);
    }
});
