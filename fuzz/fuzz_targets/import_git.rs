#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_repo::Repo;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repo::init(&dir.path().join("r")).unwrap();
    let _ = repo.import_git(data);
    // whatever was accepted must leave a consistent repository
    let rep = repo.fsck().unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
});
