# vcs to git and Perforce: the reverse table

The other two pages go from a git or Perforce command to vcs ([git-to-vcs.md](git-to-vcs.md), [p4-to-vcs.md](p4-to-vcs.md)).
This page goes the other way: for each `vcs` command, what you would have typed in git and in Perforce. Use it when you know
vcs and talk to someone on git or p4, or when you read a vcs command and want to know what it replaces.

How to read it:

* **git** and **p4** columns hold the real command spelling. `vcs git <git-args>` and `vcs p4 <p4-args>` run those
  spellings directly; the forward pages carry the exact status (exact, renamed, different, missing) and the tests.
  `none` means the tool has no equivalent command.
* This is a command map, not a promise of identical behaviour. Where the meaning differs, the note says how. Object ids,
  pack format and protocols are not compatible with either tool (see [interop.md](interop.md)).
* Perforce has no local history, so most history commands map to a server query and a few vcs commands (`stash`, `bisect`,
  `rebase`) have no p4 equivalent at all.
* vcs says **zone** where it names a path-prefix with its own branches and history inside one repository. git has
  submodules (separate repositories) and p4 has depot subtrees or streams; neither is the same thing.

## Start and configure

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs init` | `git init` | none (the server holds the depot) | every vcs repository has a working tree (no bare form) |
| `vcs clone URL dir` | `git clone URL dir` | `p4 clone -p URL -d dir` (vcs spelling) | remotes: path, file://, http(s)://, s3:// |
| `vcs config get/set/list/unset` | `git config` | none (`p4 set` / env) | `vcs p4 info` prints identity, root, remote |
| `vcs remote set/show` | `git remote add/set-url/-v` | none | vcs has exactly one remote |
| `vcs login` / `vcs logout` / `vcs auth` / `vcs credential` | credential helpers | `p4 login` / `p4 logout` | tokens are for a vcs-server remote |
| `vcs gen` | none | none | shell completions and man pages |
| `vcs hook` | `.git/hooks`, `git hook run` | triggers (server side, not mapped) | client-side hooks only |

## Everyday work

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs status` | `git status` | `p4 opened`, `p4 status`, `p4 reconcile -n` | vcs lists staged, unstaged and untracked per zone |
| `vcs watch` | `git fsmonitor--daemon run` | (the p4 server) | journals changed paths so `status` and `diff` skip the full scan; started on its own for big repositories, see benchmarks.md |
| `vcs add f` | `git add f` | `p4 add f` (new), `p4 edit f` then nothing | an edit is found by content; `p4 edit` is a no-op except for lockable files |
| `vcs rm f` | `git rm f` | `p4 delete f` | |
| `vcs mv a b` | `git mv a b` | `p4 move a b` | within one zone |
| `vcs restore f` | `git restore f` | `p4 revert f` (also unstages) | |
| `vcs restore --staged f` | `git restore --staged f` | `p4 revert f` (the nearest) | |
| `vcs diff` / `--staged` | `git diff` / `--staged` | `p4 diff` | |
| `vcs commit -m msg` | `git commit -m msg` | `p4 submit -d msg` (also publishes) | `vcs p4 submit` = add edits, commit, publish |
| `vcs clean -n/-f` | `git clean -n/-f` | `p4 clean` (restores; `vcs p4 clean` does not delete untracked files) | |
| `vcs stash push/pop/list/drop` | `git stash` | none (`p4 shelve` is the nearest, but it parks on the server) | |
| `vcs shelve name` / `vcs shelves` / `vcs unshelve name` | none (a pushed WIP branch) | `p4 shelve -c`, `p4 changes -s shelved`, `p4 unshelve -s` | shelves are named and live on the remote, see [shelves.md](shelves.md) |
| `vcs reset` | `git reset` | none | acts on one zone |
| `vcs op log/undo/restore` | `git reflog` plus `git reset` | none | operation log of local branches and workspace |

## History and inspection

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs log` | `git log` | `p4 changes` | `vcs p4 changes` prints commit ids, plus the numbers it assigned itself |
| `vcs show rev` | `git show rev` | `p4 describe rev` | `describe N` also takes a number recorded by `vcs p4 submit` |
| `vcs blame f` | `git blame f` | `p4 annotate f` | |
| `vcs cat-file -p rev:path` | `git cat-file -p rev:path`, `git show rev:path` | `p4 print path@rev` | |
| `vcs ls-files` | `git ls-files` | `p4 have`, `p4 files` | no per-file revision numbers |
| `vcs ls-files --rev R` | `git ls-tree -r --name-only R` | `p4 files path@R` | |
| `vcs grep` | `git grep` | `p4 grep -e` | Rust regex syntax |
| `vcs rev-parse` | `git rev-parse` | none | 64-hex ids |
| `vcs rev-list` | `git rev-list` | `p4 changes` | |
| `vcs shortlog` | `git shortlog` | `p4 users` (accounts, not authors) | |
| `vcs merge-base a b` | `git merge-base a b` | none | |
| `vcs diff-tree A B` | `git diff-tree -r A B` | `p4 diff2` | |
| `vcs describe` | `git describe` | none | names a commit after the nearest tag |
| `vcs archive` | `git archive` | none | tar or zip of a revision |
| `vcs bisect` | `git bisect` | none | |
| `vcs fsck` | `git fsck` | `p4 verify` | |

## Branches, merging, tags

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs branch [name]` | `git branch` | `p4 branch`, `p4 branches`, `p4 stream` | a branch is a line of history, not a mapping |
| `vcs switch b` | `git switch b` | `p4 switch` | a dirty zone is refused |
| `vcs merge b` | `git merge b` | `p4 merge`, `p4 integrate` | whole branches, no file-level integrate |
| `vcs merge b` after a conflict, `vcs conflict`, `vcs resolve` | `git merge`, `git mergetool` | `p4 resolve` (`-at`, `-ay`, `-af`) | theirs/ours follow the p4 map: see p4-to-vcs.md |
| `vcs cherry-pick c` | `git cherry-pick c` | none (`p4 integrate` of one change) | |
| `vcs revert c` | `git revert c` | none (`p4 undo`, not mapped) | |
| `vcs rebase b` | `git rebase b` | none | keeps each change id |
| `vcs tag [-d] name` | `git tag` | `p4 label`, `p4 labels`, `p4 labelsync` | a label is a whole-commit tag |
| `vcs verify-tag` / `vcs verify-commit` | `git tag -v` / `git verify-commit` | none | Ed25519 keys, not GPG |
| a numbered `vcs p4` changelist (`vcs p4 change`, `-c N`) | none | `p4 change`, `p4 reopen -c N`, `p4 submit -c N` | local ledger, not a global counter |
| `vcs p4 copy branch` | none | `p4 copy` | whole branch into the checked-out branch |
| `vcs p4 sync path@rev` | `git checkout rev -- path` | `p4 sync path@rev` | files only, HEAD does not move |

## Sharing

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs fetch` | `git fetch` | `p4 sync -n` (nearest) | local branches stay untouched |
| `vcs sync` | `git pull --ff-only` | `p4 sync` | fetch plus fast-forward; paths ignored |
| `vcs publish` | `git push` | `p4 submit` (the publishing half) | all checked-out branches, one atomic transaction |
| `vcs prefetch` | none | none | warm file contents for offline work |
| `vcs lock` / `vcs unlock` / `vcs locks` | `git lfs lock` / `unlock` / `locks` | `p4 lock` / `p4 unlock` / `p4 locks`, `+l` filetype | leases, see [locks.md](locks.md) |
| `vcs bundle create/verify` | `git bundle` | none | vcs bundle format |
| `vcs format-patch`, `vcs apply`, `vcs am` | `git format-patch`, `git apply`, `git am` | none | |
| `vcs worktree add/list/remove` | `git worktree` | `p4 clients` / `p4 client` (a workspace) | |

## Beyond git and p4

| vcs | git | p4 | note |
|---|---|---|---|
| `vcs zone ...` | submodules (nearest, but separate repositories) | depots / stream paths / client view | a path-prefix with its own branches, in one repository |
| `vcs sparse set/add/remove/list` | `git sparse-checkout` | client view | per zone |
| `vcs snapshot ...` | none (a superproject commit pinning submodules) | a label over a depot path | a pinned set of zone versions |
| `vcs repack`, `vcs gc` | `git repack`, `git gc` | `p4 obliterate` is NOT the equivalent: history is immutable, gc removes unreachable data only | `vcs gc` is destructive unless `--dry-run` |
| `vcs import-git`, `vcs export-git` | `git fast-import`, `git fast-export` | none | see [import-git.md](import-git.md) |
| `vcs import-p4` | none | reads recorded p4 output, see [import-p4.md](import-p4.md) | |
| `vcs git ...`, `vcs p4 ...` | the git spelling | the p4 spelling | translation front-ends, same process, no git or p4 binary |
| `vcs help` | `git help` | `p4 help` | |

Server-side p4 commands (`protect`, `groups`, `user`, `passwd`, `jobs`, `fix`, `obliterate`) have no vcs command; the forward
page says why for each.
