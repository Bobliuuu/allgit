# The `WorktreeFs` boundary

Implements issue #119 (stage 3 preparation in [research/virtual-fs.md](research/virtual-fs.md)).

`vcs_repo::worktree_fs::WorktreeFs` is every operation that checkout and status perform on the working tree:
`read_dir`, `stat` (lstat), `read`, `read_link`, `write` (atomic replace), `symlink`, `remove`, `remove_dir`,
`create_dir_all`, `rename`. Paths are worktree-relative and `/`-separated.

* `RealFs` is the default and is what the code did before the trait existed (temp file + chmod + rename, never
  writing through a link, never removing the root or the process's current directory).
* `MemFs` is an in-memory fake with a logical clock (every mutation gets a fresh mtime/ctime). It has no directory
  identity, so the directory-listing cache is bypassed on it.
* `Repo::with_worktree_fs(Arc<dyn WorktreeFs>)` swaps the implementation. `.vcs/` state always stays on disk.
* A future virtual filesystem implements the trait; nothing else in checkout/status needs to change.

## What goes through the trait

checkout/transition (preflight stat checks, writes, deletes, empty-directory pruning), sparse set changes, the
unstaged check of `status` (`wt_state`), the untracked walk of `status` (`read_dir`, directory stamps) and ignore
file loading. Both implementations run the same contract test (`worktree_fs::tests`), and
`tests/memfs.rs` runs `zone add`, branch switch, status and sparse set changes with no working-tree file on disk.

## Not yet behind the trait

`add`/`add_dir` (staging reads), `restore`, `rm`, `mv`, merge conflict-marker writes and `resolve`, stash's work
tree read, `diff`, import/export. They still use `std::fs` on `Repo::worktree()`. Moving them is mechanical and
should be done one command at a time with the existing suite as the safety net.
