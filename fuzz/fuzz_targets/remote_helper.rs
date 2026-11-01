#![no_main]
use libfuzzer_sys::fuzz_target;

#[allow(dead_code)]
#[path = "../../crates/vcs-cli/src/bin/git_remote_allgit.rs"]
mod helper;

// Fields separated by 0x1e: marks file, map file, pending file, then the command stream (the rest).
fuzz_target!(|data: &[u8]| {
    let mut it = data.splitn(4, |b| *b == 0x1e);
    let mut next = || it.next().unwrap_or(&[]);
    let (marks, map, pending, cmds) = (next(), next(), next(), next());
    let dir = tempfile::tempdir().unwrap();
    let repo = vcs_repo::Repo::init(&dir.path().join("r")).unwrap();
    let st = dir.path().join("st");
    std::fs::create_dir_all(&st).unwrap();
    std::fs::write(st.join("marks"), marks).unwrap();
    std::fs::write(st.join("map"), map).unwrap();
    std::fs::write(st.join("pending"), pending).unwrap();
    helper::fuzz_entry(repo, st, cmds);
});
