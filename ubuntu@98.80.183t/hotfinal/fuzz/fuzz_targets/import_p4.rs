#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_repo::{P4View, Repo};

// Fields separated by 0x1e: flags byte, view text, changes.txt, describe/1.marshal, describe/1.txt,
// print/1/depot/a.txt.
fuzz_target!(|data: &[u8]| {
    let f: Vec<&[u8]> = data.split(|b| *b == 0x1e).collect();
    let get = |i: usize| f.get(i).copied().unwrap_or(&[]);
    let which = get(0).first().copied().unwrap_or(0);
    let Ok(view) = P4View::parse(&String::from_utf8_lossy(get(1))) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("p4");
    std::fs::create_dir_all(d.join("describe")).unwrap();
    std::fs::create_dir_all(d.join("print/1/depot")).unwrap();
    if which & 1 != 0 {
        std::fs::write(d.join("changes.txt"), get(2)).unwrap();
    }
    if which & 2 != 0 {
        std::fs::write(d.join("describe/1.marshal"), get(3)).unwrap();
    } else {
        std::fs::write(d.join("describe/1.txt"), get(4)).unwrap();
    }
    std::fs::write(d.join("print/1/depot/a.txt"), get(5)).unwrap();
    let repo = Repo::init(&dir.path().join("r")).unwrap();
    let _ = repo.import_p4(&d, &view, "main");
    let rep = repo.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
});
