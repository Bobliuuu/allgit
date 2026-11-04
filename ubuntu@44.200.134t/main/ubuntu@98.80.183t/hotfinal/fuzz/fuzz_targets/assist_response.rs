#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_assist_claude::{api, judge, parse_request, Req};

// Fields separated by 0x1e: model text, ours, theirs, base, path. The model text (or API body) is
// untrusted: extract + validate must never panic.
fuzz_target!(|data: &[u8]| {
    let f: Vec<String> = data
        .split(|b| *b == 0x1e)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let get = |i: usize| f.get(i).cloned().unwrap_or_default();
    let path = get(4);
    let req = Req {
        language: vcs_merge::assist::language_of(&path).to_owned(),
        path,
        base: f.get(3).cloned(),
        ours: get(1),
        theirs: get(2),
    };
    let model = get(0);
    let _ = judge(&req, &model);
    let _ = api::parse_body(&model);
    let _ = parse_request(model.as_bytes());
    let _ = vcs_merge::assist::parse_response(model.as_bytes());
});
