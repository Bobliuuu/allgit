#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_server::http::{pct_decode, pct_encode, read_request, Limits};

fuzz_target!(|data: &[u8]| {
    let mut r = std::io::BufReader::with_capacity(64, data);
    let limits = Limits {
        max_header_bytes: 1024,
        max_headers: 16,
    };
    let mut called = 0;
    if let Ok(req) = read_request(&mut r, &limits, &|_| 4096, &mut || called += 1) {
        assert!(req.body.len() <= 4096);
        let _ = req.query_param("a");
        let _ = req.header("content-length");
    }
    if let Ok(s) = std::str::from_utf8(data) {
        if let Some(d) = pct_decode(s) {
            let _ = pct_encode(&d, true);
        }
    }
});
