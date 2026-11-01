#![no_main]
use libfuzzer_sys::fuzz_target;
use vcs_repo::meta::{Refs, Workspace, Zones};
use vcs_repo::{MergeState, RebaseState, Repo};

// merge-state (`conflicts`), `rebase`, `stash`, zones, refs and workspace files.
fuzz_target!(|data: &[u8]| {
    let Some((sel, body)) = data.split_first() else { return };
    match sel % 6 {
        0 => {
            if let Ok(s) = MergeState::decode(body) {
                MergeState::decode(&s.encode()).expect("re-encoded merge state must decode");
            }
        }
        1 => {
            if let Ok(s) = RebaseState::decode(body) {
                RebaseState::decode(&s.encode()).expect("re-encoded rebase state must decode");
            }
        }
        2 => {
            if let Ok(s) = Zones::decode(body) {
                Zones::decode(&s.encode()).expect("re-encoded zones must decode");
            }
        }
        3 => {
            if let Ok(s) = Refs::decode(body) {
                Refs::decode(&s.encode()).expect("re-encoded refs must decode");
            }
        }
        4 => {
            if let Ok(s) = Workspace::decode(body) {
                Workspace::decode(&s.encode()).expect("re-encoded workspace must decode");
            }
        }
        _ => {
            let dir = tempfile::tempdir().unwrap();
            let repo = Repo::init(&dir.path().join("r")).unwrap();
            std::fs::write(dir.path().join("r/.vcs/stash"), body).unwrap();
            let _ = repo.stash_list();
        }
    }
});
