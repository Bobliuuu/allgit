# Perforce to vcs, command by command

vcs is deliberately not Git- or Perforce-compatible at the storage or protocol level (that is what lets it be
fast). Instead, every everyday Perforce command has a one-to-one vcs equivalent so that Perforce muscle memory works.
This is a command map, not live sync with a Perforce server: no `p4` binary and no server is involved.
(To bring Perforce history in, see [import-p4.md](import-p4.md). The reverse direction, from each vcs command to its git and
p4 spelling, is [vcs-to-git-p4.md](vcs-to-git-p4.md).)

```bash
vcs p4 edit src/main.c              # runs the vcs form, prints "+ vcs ..." on stderr (-q hides it)
vcs p4 --explain submit -d "msg"    # prints the translation, runs nothing
vcs p4 --list                       # the whole map as tab separated text
vcs p4 -d ../other-workspace opened # -d DIR runs as if started in DIR
```

Every translated command is an ordinary vcs command, so you can drop the `p4` once you know the native name. When
there is no equivalent the command fails, says why, and prints the nearest native command. Where the meaning differs
a `note:` line says how (never silently). Code: `crates/vcs-cli/src/p4map.rs`.

## Vocabulary

| Perforce | vcs |
|---|---|
| depot path `//depot/dir/...` | repo path `dir` (trailing `...` means the directory). `//<zone>/rest` is `rest` inside that zone; any other first name (`//depot`) is the repository root |
| client / workspace | the directory (a worktree for more than one); `p4 -d DIR` selects it |
| client view | zone subscription plus sparse sets (`vcs zone`, `vcs sparse`) |
| default pending changelist | the index (staged files) |
| numbered pending changelists | a local ledger in `.vcs/p4-changes`: `vcs p4 change -m`, `-c N` on add/edit/delete/revert/reopen/submit (nothing is staged until `submit -c N`) |
| submitted changelist N | a commit. Ids are content hashes, so N exists only for submits made through `vcs p4 submit` in this repository (the ledger records N to commit); `@N` works for those and is refused for any other number |
| shelf (change N) | a named shelf on the remote (`vcs shelve <name>`) |
| label | a tag |
| stream / branch spec | a branch of a zone (history, not a mapping) |
| exclusive open (`+l`), `p4 lock` | lockable assets (`.vcsattributes`, `vcs lock`), see [locks.md](locks.md) |

## Status meanings

* **exact**: same effect and same name; arguments translate mechanically.
* **renamed**: same effect under another name or flags.
* **different-semantics**: translated, with a stated gap (see "Gaps").
* **missing**: no equivalent; fails with an explanation and the native command.

Output text is always vcs's own, never p4's tagged or marshalled format (`-G`, `-ztag` are refused).

## The map (57 rows: 6 exact, 21 renamed, 28 different-semantics, 2 missing)

| p4 | vcs | status | test (`crates/vcs-cli/tests/p4map.rs`) |
|---|---|---|---|
| `client -o / workspace -o` | `zone list; sparse list` | different-semantics | `client_view` |
| `clients / workspaces` | `worktree list` | different-semantics | `clients` |
| `depots` | `zone list` | renamed | `depots` |
| `sync` | `sync` | different-semantics | `sync` |
| `sync -f` | `sync; restore .` | different-semantics | `sync_f` |
| `sync -n` | `fetch` | different-semantics | `sync_n` |
| `sync path@rev / @label / @N` | `(computed) write the files of that commit into the workspace` | different-semantics | `sync_rev` |
| `edit` | `lock (lockable files); nothing otherwise` | different-semantics | `edit_plain` |
| `add` | `add` | exact | `add` |
| `delete` | `rm` | renamed | `delete` |
| `move / rename` | `mv` | renamed | `move` |
| `revert` | `restore --staged; restore` | renamed | `revert` |
| `revert -a` | `(nothing to do)` | different-semantics | `revert_a` |
| `diff` | `diff; diff --staged (filtered to the files)` | different-semantics | `diff` |
| `diff2` | `(computed) unified diff of two revisions` | renamed | `diff2` |
| `opened` | `status (without untracked)` | renamed | `opened` |
| `opened -a` | `locks` | different-semantics | `opened_a` |
| `reconcile [-a -e -d -n]` | `add (the selected files)` | renamed | `reconcile` |
| `status` | `reconcile -n` | renamed | `p4_status` |
| `submit [-d msg]` | `add (edits); commit -m; publish` | different-semantics | `submit` |
| `changes [-m -s -u -l]` | `log [-n --author --oneline]` | renamed | `changes` |
| `describe [-s]` | `show [--name-status]` | renamed | `describe` |
| `filelog` | `log --follow --name-status -- file` | renamed | `filelog` |
| `annotate` | `blame` | renamed | `annotate` |
| `files` | `ls-files --rev` | renamed | `files` |
| `fstat` | `log -n 1 --format ... -- file` | different-semantics | `fstat` |
| `print` | `cat-file -p rev:path` | renamed | `print` |
| `have` | `ls-files` | renamed | `have` |
| `integrate / merge` | `merge` | different-semantics | `integrate` |
| `copy source-branch` | `(computed) make the tracked files equal to that branch, staged` | different-semantics | `copy` |
| `resolve [-at -ay -af -n]` | `resolve --theirs/--ours/--mark; conflict list` | renamed | `resolve` |
| `shelve [-c name] [-d]` | `shelve [name] [--delete]` | different-semantics | `shelve` |
| `unshelve -s name` | `unshelve name` | different-semantics | `shelve` |
| `label / tag / labelsync` | `tag [-d]` | different-semantics | `label` |
| `labels` | `tag` | renamed | `labels` |
| `branch / stream` | `branch [-d]` | different-semantics | `branch` |
| `branches / streams` | `branch` | renamed | `branches` |
| `switch` | `switch` | exact | `switch` |
| `lock` | `lock --lease-secs` | different-semantics | `lock` |
| `unlock [-f]` | `unlock [--break]` | exact | `lock` |
| `locks` | `locks` | exact | `lock` |
| `where` | `(computed) zone path, worktree path` | renamed | `where` |
| `info` | `(computed) identity, root, remote` | different-semantics | `info` |
| `users` | `shortlog -s -e` | different-semantics | `users` |
| `login / logout` | `login / logout` | exact | `login` |
| `init / fetch / push / clone` | `init / fetch / publish / clone` | renamed | `push` |
| `grep` | `grep` | exact | `grep` |
| `clean` | `restore --staged .; restore .` | different-semantics | `clean` |
| `change / changelist` | `status` | different-semantics | `change` |
| `change -m desc / -i / -d N / -o N` | `(local ledger) numbered pending changelists` | different-semantics | `change_numbered` |
| `add / edit / delete / revert / opened -c N` | `(local ledger) open files in changelist N` | different-semantics | `numbered_open` |
| `reopen -c N` | `(local ledger) move files between changelists` | different-semantics | `reopen` |
| `submit -c N` | `add (the changelist's files); commit; publish` | different-semantics | `submit_numbered` |
| `describe N / @N` | `show <commit recorded for N>` | different-semantics | `changelist_numbers` |
| `verify` | `fsck` | renamed | `verify` |
| `obliterate / jobs / fix` | `(none)` | missing | `missing_cmds` |
| `protect / groups / group / user / passwd` | `(none: vcs-server tokens)` | missing | `protect_missing` |

`p4 sync`, `p4 submit`, `p4 shelve` and `p4 changes -s` need a remote for the parts that talk to one, as `vcs sync`,
`vcs publish`, `vcs shelve` do. Global p4 flags other than `-q` and `-d DIR` (`-p -u -c -P -G -x ...`) are refused: there
is no server, user or client to select; identity comes from `user.name` / `user.email`.

## Gaps (semantics that differ, stated)

* **Submit is two steps.** `p4 submit` is one atomic server step. `vcs p4 submit -d msg` stages your edits and
  deletions of tracked files (as p4 submits what is opened; new files must be `p4 add`ed first), commits locally,
  then publishes when a remote is configured. Without a remote it commits locally and says "local only". Files given
  on the command line restrict what is staged, but anything already staged is committed too. `-f -i -e -s -r` are refused.
* **Numbered changelists are a local ledger.** `vcs p4 change -m <desc>` (or `change -i` with a spec on stdin; `Change:` and
  `Description:` are read, `Files:` is ignored) creates changelist N in `.vcs/p4-changes`. `-c N` on `add`, `edit`, `delete`,
  `revert`, `opened`, `reopen` records files in it; nothing is staged until `submit -c N`, which stages exactly those files,
  commits with the changelist description (or `-d`), publishes when a remote is set, and records N to commit. It refuses when
  the index holds other files (they would be committed too), when the changelist is empty or has no description, and when files
  are named on the command line. `change -d N` deletes an empty changelist; `change -o N` and `describe N` show a pending one;
  `changes -s pending` lists them. The default changelist stays the index, and `reconcile`/`submit` skip files that a numbered
  changelist holds. The ledger belongs to one repository (worktree directory): numbers are not shared with clones, not global,
  never reused, and a file is open in at most one changelist. `move`, `lock`, `unlock` and other commands still refuse `-c N`.
  `delete -c N` removes the file from disk straight away (as p4) and `revert -c N` brings it back.
* **`@N` and `#N`.** `@N` names the commit that `vcs p4 submit` recorded as Change N in this repository (every `p4 submit`
  gets the next number, so `describe 3`, `print file@3`, `diff2 a@3 a@4`, `sync @3` work for those). Any other N is refused.
  A multi-zone commit records one zone's commit (the root zone's when it changed). `#N` (file revision numbers) stays refused:
  vcs versions whole commits, and per-file counts differ across merges and renames. Commit id prefixes, tags and branch names
  after `@` still work everywhere. Do not use an all-digit id prefix of six digits or fewer: it reads as a changelist number.
* **`sync path@rev`, `sync @label`, `sync @N`** write the files of that commit (tag, branch, id prefix or recorded number)
  into the workspace and delete tracked files the commit lacks, in the zones where the revision exists. HEAD does not move,
  so the files show as edits; `vcs restore .` returns. It refuses over local changes. `-f` is not combined with a revision.
* **`copy <branch>`** (also `-S //stream`, optional target that must be the checked-out branch) makes the tracked files of the
  zones where the branch exists equal to the branch and stages the result (files only the target has are deleted); `-n` previews.
  It records no integration, so a later `merge` sees plain edits. Whole branch only. It refuses over local changes.
* **Edit is a no-op for ordinary files.** vcs files stay writable and edits are found by content. `p4 edit` only checks
  that the file exists, except for lockable files, where it takes the exclusive lock (a lease, default 8 hours,
  `VCS_P4_LOCK_LEASE_SECS` to change; repeat to renew). `p4 lock` does the same for any file. p4 locks do not expire.
* **Revert** discards changes and unstages; a reverted add stays on disk (as p4). `revert -a` finds nothing, because
  vcs never stages a file equal to HEAD.
* **Diff** shows the staged diff then the unstaged diff; a file with both appears twice. Whitespace flags are not mapped.
* **Sync** is `vcs sync` (fetch plus fast-forward of every subscribed zone); paths are ignored (use sparse sets),
  `-n` is `fetch` because there is no dry run, `-f` rewrites files from the index; syncing to `@rev` is described above.
* **Integrate / merge** merges a whole branch into the checked-out branch of a zone; there is no file-level integrate,
  no preview and no reverse; `copy` is described above.
* **Resolve**: p4 "theirs" is the merged-in branch (`--theirs`), "yours" the checked-out one (`--ours`). `-am`/`-as` list
  what is still conflicting (vcs already merged what it could); `-af` marks the file as it is on disk; there is no interactive resolve.
* **Labels** are tags on a whole commit of a zone, so labelling a subset of files is refused.
* **Streams / branch specs** are branches; creation is at the checked-out head.
* **fstat** gives only `depotFile`, `headChange`, `headTime`, `headAuthor`. **have** gives paths without revisions.
  **users** lists commit authors, not accounts. **opened -a** shows the locks of everyone.
* **clean** restores tracked files but does not delete untracked ones (vcs has no such command).
* **Missing by design**: `obliterate` (history is immutable; `vcs gc` removes unreachable data), jobs/fixes,
  permissions tables (`protect`, `groups`; server tokens and roles live on vcs-server, see [server.md](server.md)).

Semantics of the Perforce side come from the real p4d runs in [research/perforce-benchmark.md](research/perforce-benchmark.md)
(for example that `reconcile` asks the server what the workspace has, and that a submit is one numbered server step).
Not tested against a real p4 client in this repository: the map is checked by running the vcs side only.

## Tests

`crates/vcs-cli/tests/p4map.rs` runs one case per row above in a fresh temp world (alice's repo with a directory
remote and bob's clone), plus `--explain` checks, and a test that every row names an existing case and appears in this page.
Unit tests of the flag and path parsing are in `p4map.rs`.
