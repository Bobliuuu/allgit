# Client-side hooks

Hooks are executables in `.vcs/hooks/` that `vcs` runs at fixed points. They are local to one machine and one
repository: **never cloned, fetched, bundled or imported**, and never taken from a remote.

```
vcs hook list                         which hooks are installed and whether they would run
vcs hook install <name> <file> [-f]   copy <file> to .vcs/hooks/<name> with mode 755
vcs hook run <name> [args...]         run one by hand with the documented environment (ignores --no-hooks)
```

## Hook points

| Hook | When | Arguments / stdin | Failure |
|---|---|---|---|
| `pre-commit` | `vcs commit`, before the message editor opens | none | aborts |
| `commit-msg` | after the message is known, before the commit | `$1` = path of a file holding the message; the hook may rewrite it, the file's final content is the message | aborts (an emptied message aborts too) |
| `post-commit` | after a successful commit | none | warning only |
| `pre-publish` | `vcs publish`, before anything is sent | stdin: one line per branch that will change on the remote | aborts |
| `post-publish` | after a successful publish | stdin: the updates that were applied | warning only |
| `pre-merge` | `vcs merge`, before merging | `$1` = branch being merged | aborts |
| `post-merge` | after a merge or fast-forward that changed something (not when conflicts stop it) | `$1` = branch merged | warning only |
| `post-checkout` | after `vcs switch` / `vcs zone checkout` | `$1` = previous head, `$2` = new head (commit ids, empty if none) | warning only |
| `pre-rebase` | `vcs rebase <onto>` (not `--continue`) | `$1` = branch rebased onto | aborts |

Publish stdin lines: `<zone> <branch> <old-id> <new-id>`; `old-id` is 64 zeros when the branch is new on the remote.
`pre-publish` lists the checked-out branches whose head differs from the remote-tracking head, so it can include a
branch the transaction turns out not to change.

A non-zero exit from a gating hook (`pre-*`, `commit-msg`) aborts the operation, prints the hook's stderr and exits
non-zero. A hook killed by a signal or that cannot be executed counts as failed. Failures of `post-*` hooks print a
warning and do not change the exit status. A hook's stdout goes to vcs's stdout; on success its stderr is passed through.

## Environment

Hooks run with the working directory at the worktree root and a **scrubbed environment**: only `PATH HOME USER LOGNAME
LANG LC_ALL LC_CTYPE TERM TMPDIR TZ` are inherited, plus the variables named in config `hooks.env` (comma-separated).
Tokens such as `VCS_TOKEN` or `ANTHROPIC_API_KEY` are not passed unless you list them. vcs sets:

| Variable | Meaning |
|---|---|
| `VCS_HOOK` | hook name |
| `VCS_DIR` | the worktree's own `.vcs` directory |
| `VCS_COMMON_DIR` | the shared `.vcs` directory (same as `VCS_DIR` outside linked worktrees) |
| `VCS_ZONE` | zone of the current directory (root zone for publish and `hook run`) |
| `VCS_BRANCH` | branch checked out in that zone |

## Switches and limits

* `--no-hooks` (global flag) or `VCS_NO_HOOKS=1` skips every hook. `vcs hook run` ignores both.
* `hooks.timeout` (config, env `VCS_HOOKS_TIMEOUT`): seconds, default 60. A hook that exceeds it, and every process in
  its process group, is killed with SIGKILL, and the operation fails (gating hook) or warns.
* A hook file, or the `hooks` directory, that is group- or world-writable is **never run**: gating hooks abort with
  "refusing to run hook ...", post hooks warn. Fix with `chmod go-w`.
* A hook without the execute bit is skipped with a warning. Unix only; on Windows hooks are not run.

## Worktrees

Hooks are looked up in the **common** `.vcs/hooks/` (the main worktree's), exactly like refs and objects, so every
linked worktree runs the same hooks. A `hooks` directory inside a linked worktree's own `.vcs` is ignored. Decision
rationale: hooks enforce repository policy (lint, message format), and one set avoids silent divergence.

## Security

* The directory is never written by clone, fetch, merge, `import-git`, `import-p4`, `bundle` fetch/clone or the git
  remote helper. A tree entry named `.vcs` (any case or filesystem alias, at any depth) is rejected by name
  validation (`vcs_objects::names`), so a hostile tree containing `.vcs/hooks/pre-commit` cannot land in the metadata
  directory. Tests: `crates/vcs-cli/tests/hooks.rs` (`hostile_trees_cannot_install_hooks`,
  `clone_and_bundle_never_bring_hooks`).
* Hook names are a fixed list; `install` cannot write outside the hooks directory.
* A hook is code you installed yourself and runs with your privileges. The checks above stop another local user (or a
  group-writable checkout) from swapping a hook; they do not sandbox the hook.
* Not covered: the git remote helper and `import-p4` have no dedicated hook tests; they rely on the same name
  validation as `import-git` (tested).
