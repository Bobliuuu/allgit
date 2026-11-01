# Linked worktrees

`vcs worktree add <path> [branch]`, `vcs worktree list`, `vcs worktree remove <path> [--force]`: several working
trees over one object store and one set of refs. Each has its own index, its own workspace (which zones are
subscribed and the branch checked out in each, i.e. the HEAD per zone), its own merge / rebase / cherry-pick state and
its own stash.

## On-disk layout

```
main/.vcs/                    the repository as before (a repo that never uses worktrees is byte-for-byte unchanged)
    objects refs zones remote remote-refs op_head lock ...     shared state
    index workspace stash conflicts rebase pick dircache       the main worktree's own state
    worktrees/<name>/path     registry: absolute path of each linked worktree
linked/.vcs/                  a real directory, not a pointer file
    format commondir worktree-name
    index workspace stash conflicts rebase pick dircache       per-worktree state only
```

`commondir` holds the absolute path of `main/.vcs`. `Repo` has two directories: `vcs` (per worktree) and `common`
(shared; equal to `vcs` in the main worktree and in every legacy repository, so nothing about old repositories
changes). Which file is shared is decided in one place (`Repo::state_path`) plus the explicit `common.join(...)` at the
write sites for `refs`, `zones`, `remote`, `remote-refs`, `op_head`, `lock` and the import / export maps.

Why a directory and not a one-line `.vcs` file like git's `gitdir:` link: every per-worktree reader and writer already
does `self.vcs.join(name)` and the index, dircache and rebase state need somewhere to live in the worktree anyway, so a
directory keeps that code unchanged and a pointer file would add an indirection for no benefit. Cost: `.vcs` in a
linked worktree is not a single file you can see at a glance; `commondir` plays that role. Opening a linked worktree
fails with a clear message when the main repository moved or the worktree was unregistered.

## Rules

- **Locking.** There is one lock, `main/.vcs/lock`, for every worktree. Ref updates, checkouts and `gc` are serialised
  across worktrees by it. This is coarser than per-worktree locks and deliberately so: refs are shared.
- **Branch exclusivity.** A branch of a zone is checked out in at most one worktree. `worktree add`, `checkout`,
  `zone add`, `snapshot checkout` refuse a branch another worktree has (checked under the shared lock before any file
  moves; `save_ws` re-checks as a backstop). `branch delete` refuses a branch any worktree has checked out.
- **`worktree add <path> [branch]`** checks out the root zone only (add other zones in the new worktree with
  `zone add`). The branch defaults to the directory name and is created at this worktree's root-zone head when it does
  not exist. The path must be new or an empty directory and may not be inside, or contain, another worktree.
- **`worktree remove`** refuses (without `--force`) staged, unstaged or untracked changes, an unfinished merge / rebase
  / cherry-pick, and stash entries (a stash lives only in its worktree). It deletes only a directory whose
  `worktree-name` and `commondir` prove it belongs to this repository. Branches and commits are never touched. The
  main worktree and the current one cannot be removed.
- **`op undo` / `op restore`** are refused while linked worktrees exist: they rewrite every branch at once and would
  move another worktree's checked-out branch under it.

## GC and fsck

Garbage collection roots are now the union over **every** worktree of: index entries, stash commits, an unfinished
rebase / cherry-pick, and merge state, on top of the shared refs, tags, snapshots, remote refs, operation log and
transactions (`gc.rs`: `worktree_roots`). A worktree that is registered but unreadable (directory deleted by hand, disk
unmounted) makes `gc` fail with an error naming it instead of guessing: deleting while a worktree's index is invisible
could lose its staged blobs. Run `vcs worktree remove --force <path>` to drop the registration.

`fsck` checks every worktree's index for missing blobs, reports a branch checked out in two worktrees (possible only by
editing state files by hand), and reports unreadable worktrees.

Tested in `crates/vcs-repo/tests/worktree.rs`: a blob that only another worktree's index reaches and the commits only
its stash reaches survive a real `gc_sweep` run from the main worktree, and become collectable once the worktree is
force-removed (the control); making `gc_roots` look at the main worktree only fails that test.

## Not done

- No `worktree prune` / `repair` / `move`; moving a worktree directory by hand breaks it (remove and re-add).
- Remote-tracking state, `fetch`, `publish` and `sync` act on the shared refs; `sync` fast-forwards only the branches
  checked out in the worktree it runs in.
- A detached or multi-zone `worktree add` (`--zone`) is not implemented.
