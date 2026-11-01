//! Optional `git`-named binary (cargo feature `git-shim`, off by default). It runs `vcs git <args>`
//! using the `vcs` binary next to it, so `git status` in a vcs repository behaves as `vcs status`.
//! See docs/git-to-vcs.md. No git binary is involved.
use std::os::unix::process::CommandExt;

fn main() {
    let me = std::env::current_exe().expect("own path");
    let vcs = me.with_file_name("vcs");
    let err = std::process::Command::new(&vcs).arg("git").args(std::env::args_os().skip(1)).exec();
    eprintln!("git (vcs shim): cannot run {}: {err}", vcs.display());
    std::process::exit(127);
}
