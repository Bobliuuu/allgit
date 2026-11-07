# 12. Command reference

This page is generated from the binary. Do not edit it by hand. Regenerate it with
`scripts/gen_cli_reference.sh`. A test (`crates/vcs-cli/tests/cli_reference.rs`) fails when it is out of date.

Every command also accepts `-C <DIR>` and `--json`. `--json` works only for the commands listed in
[../cli.md](../cli.md); the others reply `--json is not supported for this command`.

```text
vcs 0.1.0
```

## `vcs`

```text
Native zone-aware version control

Usage: vcs [OPTIONS] <COMMAND>

Commands:
  init           Create a new repository (default: current directory)
  add            Stage files or directories (also stages deletions)
  restore        Discard working-tree changes (from the index) or unstage them (--staged, from HEAD)
  checkout       Choose hunks to take from the index or a revision into the working tree (`checkout -p`); branches: `vcs switch`
  mergetool      Run a merge tool on each conflicted file of a merge or rebase and mark the file resolved
  rm             Remove tracked files from the index and working tree
  mv             Move or rename a tracked file or directory (within one zone)
  commit         Commit staged changes in all subscribed zones atomically
  verify-commit  Check the Ed25519 signature of a commit against `signing.allowedSigners` (exit 1 unless good)
  verify-tag     Check the Ed25519 signature of an annotated tag (exit 1 unless good)
  config         Get, set, list or unset settings (repo .vcs/config, user config, environment)
  status         Show staged, unstaged and untracked changes per zone
  watch          Watch the working tree and journal changed paths so `status` can skip a full scan (runs until killed)
  diff           Show unstaged changes (or --staged)
  import-git     Import a `git fast-export` stream (file, or `-` for stdin) into the root zone
  import-p4      Import recorded Perforce output (fixture directory, see docs/import-p4.md) into the root zone. Checked once against real p4d output on a small history; see that document
  export-git     Write a zone branch as a Git fast-import stream (stdout, or --output)
  log            Show commit history of a zone branch
  gen            Generate shell completions or man pages
  branch         List branches of the current zone, or create one (at the checked-out head)
  stash          Set tracked changes aside (push, pop, list, drop); untracked files are left alone
  notes          Annotate commits after the fact (add, show, list, append, remove); notes are published and fetched with the branches
  shelve         Park your changes on the remote as a private shelf (the working tree is left alone); `--delete` removes one
  shelves        List your shelves on the remote (`--all`: also other people's that you may read)
  unshelve       Apply a shelf from the remote to the working tree (staged, like `stash pop`)
  p4             Run a Perforce command in its vcs form, one to one (`vcs p4 --list` shows the map, `--explain` the translation; docs/p4-to-vcs.md)
  sparse         Materialize only some paths of a zone (set, add, remove, list, disable); the rest stays tracked, not deleted
  tag            List tags of the current zone, create one (lightweight, or annotated with -m), or delete one with -d
  format-patch   Write commits as mail-format patch files (one per commit: author, date, message, diff)
  apply          Apply a unified diff (from `vcs diff`, `vcs show`, `vcs format-patch` or git) to the working tree
  am             Apply mail-format patches as commits, keeping author, date and message
  show           Show a commit (default HEAD) with its diff against the first parent
  blame          Attribute each line of a file to a commit, following first parents
  hook           List, install or run client-side hooks (.vcs/hooks/, see docs/hooks.md)
  bisect         Binary-search a zone's history for the commit that introduced a bug
  rerere         Reuse recorded conflict resolutions (rerere.enabled; docs/rerere.md)
  grep           Search tracked files (working tree, or a revision) for a regular expression
  describe       Name a commit after the nearest tag behind it
  archive        Write the files of a revision as a .tar or .zip archive
  rev-parse      Resolve revisions to commit ids
  ls-files       List tracked files
  cat-file       Show the type, size or content of an object
  rev-list       List commit ids of a revision range (A..B, A...B, ^A B, --all), newest first
  shortlog       Summarize history by author
  merge-base     Best common ancestor of two revisions
  diff-tree      List the files that differ between two revisions (A/D/M and path)
  reset          Move the checked-out branch of a zone to another commit (--soft, --mixed, --hard); acts on one zone
  rewrite        Rewrite a zone's history: keep/remove paths, change messages or authors, run a command on every tree (undo with `vcs op undo`)
  clean          Delete untracked files (-n lists, -f deletes)
  profile        Local usage profile that `sync --auto` / `pull --auto` / `clone --auto` predict zones from (docs/zone-profile.md)
  pull           Same as `sync` (fetch, then fast-forward); `--auto` chooses zones from your usage profile
  range-diff     Compare two commit ranges by patch: which commits were kept (=), changed (!), dropped (<) or added (>)
  show-branch    Show which commits each of several revisions has, as a matrix (git show-branch)
  git            Run a git command line as its vcs equivalent, e.g. `vcs git log --oneline` (table: docs/git-to-vcs.md; `--map` lists it)
  switch         Switch branch (in the zone containing cwd, or --zone); `--detach` checks out any revision without a branch
  merge          Merge a branch into the checked-out branch (fast-forward when possible)
  cherry-pick    Re-apply the change of one commit onto the checked-out branch
  cherry         List the commits of a branch with `+` (no equivalent patch upstream) or `-` (an upstream commit has the same patch)
  revert         Create a commit that undoes one commit
  rebase         Replay the checked-out branch onto another branch, keeping each change id
  conflict       Inspect conflicts of an in-progress merge
  resolve        Resolve a conflicted path of a merge or rebase (ours = recorded left side, theirs = right)
  zone           Manage zones
  worktree       Linked worktrees over one object store (add, list, remove)
  snapshot       Manage snapshots (pinned sets of zone versions)
  remote         Manage the remote
  login          Save a server token for an http:// remote (via the credential helper)
  logout         Forget the saved token for a remote (default: the current remote)
  auth           Show authentication state
  credential     Credential helper protocol: key=value lines on stdin/stdout
  bundle         Create or verify an offline bundle of branches and zones
  clone          Clone a remote (or a bundle file) into a new directory (sparse with --zone)
  publish        Publish checked-out branches to the remote in one atomic transaction
  lock           Lock files on the remote so others cannot publish changes to them
  unlock         Release locks you hold (`--break` removes anyone's lock; the server requires an admin token)
  locks          Show locks recorded on the remote (as of now; advisory until a validating writer exists)
  fetch          Download remote objects and update remote-tracking refs (local branches untouched)
  prefetch       Download the file contents of subscribed zones' heads now (to work offline later)
  sync           Fetch, then fast-forward local branches where safe (`--auto`: choose zones from your usage profile)
  fsck           Verify all objects and graph invariants
  repack         Move loose objects into a pack (one pack + index under .vcs/objects/pack). With --format, existing packs are rewritten to that format too
  gc             Garbage collection. DESTRUCTIVE unless --dry-run: permanently deletes unreachable objects older than the grace period (local), or with --remote --yes on the remote
  op             Operation log: inspect, undo, restore (local branches and workspace only)
  help           Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
  -V, --version        Print version
```

## `vcs init`

```text
Create a new repository (default: current directory)

Usage: vcs init [OPTIONS] [PATH]

Arguments:
  [PATH]  

Options:
  -b, --initial-branch <BRANCH>  Name of the first branch of the root zone (default `main`)
  -C <DIR>                       Run as if vcs was started in <DIR> (applied before repository discovery)
      --json                     Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>            PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks                 Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                     Print help
```

## `vcs add`

```text
Stage files or directories (also stages deletions)

Usage: vcs add [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --force-no-lock  Stage changes to lockable files (`.vcsattributes`) even without holding their lock
  -f, --force          Also stage files and directories that `.vcsignore` hides
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -p, --patch          Choose hunks to stage one by one (answers y/n/q/a/d/s/e/? on standard input; no path = everything)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs restore`

```text
Discard working-tree changes (from the index) or unstage them (--staged, from HEAD)

Usage: vcs restore [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --staged         Restore the index from HEAD instead of the working tree from the index
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -p, --patch          Choose hunks to discard (or, with --staged, unstage) one by one (answers on standard input)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs checkout`

```text
Choose hunks to take from the index or a revision into the working tree (`checkout -p`); branches: `vcs switch`

Usage: vcs checkout [OPTIONS] [REV] [-- <PATHS>...]

Arguments:
  [REV]       Revision to take the content from (default: the index)
  [PATHS]...  Paths (after `--`; default: everything)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -p, --patch          Discard hunks one by one: from the index into the working tree, or from <REV> into the index and the working tree. Required: `checkout` does nothing else (use `switch` and `restore`)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs mergetool`

```text
Run a merge tool on each conflicted file of a merge or rebase and mark the file resolved

Usage: vcs mergetool [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  Only these conflicted paths (default: all unresolved ones)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -t, --tool <TOOL>    Tool name (default: merge.tool); its command is mergetool.<name>.cmd, run with $BASE $LOCAL $REMOTE $MERGED
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -y, --no-prompt      Do not ask before starting the tool on each file
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs rm`

```text
Remove tracked files from the index and working tree

Usage: vcs rm [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --cached         Only stop tracking; keep the files
  -f, --force          Remove even files with uncommitted changes
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs mv`

```text
Move or rename a tracked file or directory (within one zone)

Usage: vcs mv [OPTIONS] <SRC> <DST>

Arguments:
  <SRC>  
  <DST>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -f, --force          Overwrite an existing destination
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs commit`

```text
Commit staged changes in all subscribed zones atomically

Usage: vcs commit [OPTIONS]

Options:
  -C <DIR>                     Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>      Commit message (omit to write it in core.editor / $EDITOR)
      --force-no-lock          Commit changes to lockable files (`.vcsattributes`) even without holding their lock
      --json                   Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -S, --gpg-sign [<KEY>]       Sign the commits with an Ed25519 key (path to an OpenSSH private key; default `user.signingkey`)
      --no-hooks               Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --no-sign                Do not sign even when `commit.sign` is true
      --author <NAME <EMAIL>>  Record this identity (`Name <email>`) as the commit's author instead of `user.name`/`user.email`
      --allow-empty            Commit the current zone even when nothing in it changed (like `git commit --allow-empty`)
  -h, --help                   Print help
```

## `vcs verify-commit`

```text
Check the Ed25519 signature of a commit against `signing.allowedSigners` (exit 1 unless good)

Usage: vcs verify-commit [OPTIONS] [REV]

Arguments:
  [REV]  Revision (default HEAD)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs verify-tag`

```text
Check the Ed25519 signature of an annotated tag (exit 1 unless good)

Usage: vcs verify-tag [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs config`

```text
Get, set, list or unset settings (repo .vcs/config, user config, environment)

Usage: vcs config [OPTIONS] <COMMAND>

Commands:
  get    Print the effective value of a key (exit 1 if unset)
  set    Set a key in the repository config (or the user config with --user, the machine-wide one with --system)
  unset  Remove a key from the repository config (or the user config with --user, the machine-wide one with --system)
  list   List effective settings and where each comes from
  help   Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs config get`

```text
Print the effective value of a key (exit 1 if unset)

Usage: vcs config get [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs config set`

```text
Set a key in the repository config (or the user config with --user, the machine-wide one with --system)

Usage: vcs config set [OPTIONS] <KEY> <VALUE>

Arguments:
  <KEY>    
  <VALUE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --user           
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --system         The machine-wide config (`$VCS_SYSTEM_CONFIG`, else /etc/vcs/config; usually needs root)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs config unset`

```text
Remove a key from the repository config (or the user config with --user, the machine-wide one with --system)

Usage: vcs config unset [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --user           
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --system         The machine-wide config
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs config list`

```text
List effective settings and where each comes from

Usage: vcs config list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs status`

```text
Show staged, unstaged and untracked changes per zone

Usage: vcs status [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --ignored        Also list untracked files and directories that `.vcsignore` hides
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs watch`

```text
Watch the working tree and journal changed paths so `status` can skip a full scan (runs until killed)

Usage: vcs watch [OPTIONS]

Options:
  -C <DIR>                     Run as if vcs was started in <DIR> (applied before repository discovery)
      --idle-exit <IDLE_EXIT>  Stop after this many seconds without a `status` asking for the journal (set when a command starts the watcher itself)
      --daemonize              Start the watcher in the background (own session, output to .vcs/fsmon/watchd.log) and return once it is journaling
      --json                   Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks               Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                   Print help
```

## `vcs diff`

```text
Show unstaged changes (or --staged)

Usage: vcs diff [OPTIONS]

Options:
  -C <DIR>                          Run as if vcs was started in <DIR> (applied before repository discovery)
      --staged                      
      --json                        Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stat                        Per-file added/deleted line counts instead of the patch
      --ca-pem <FILE>               PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --numstat                     Machine-readable counts: `added<TAB>deleted<TAB>path` per file (`-` for both on a binary file)
      --exit-code                   Exit with status 1 when there are differences, 0 when there are none
      --no-hooks                    Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --name-only                   Only the names of changed files
      --name-status                 Names with a status letter: A added, D deleted, M modified, R<score> renamed, C<score> copied
      --word-diff                   Show changed words inline as [-old-]{+new+} instead of changed lines
      --color [<WHEN>]              Colour the output: auto (terminal and no NO_COLOR), always, never. Bare `--color` forces colour unless NO_COLOR is set; `--color=always` overrides NO_COLOR
      --color-moved                 Colour lines that were moved (deleted in one place, added in another of the same diff) differently; only visible with colour
      --diff-algorithm <ALGORITHM>  Diff algorithm: myers (default), patience or histogram [possible values: myers, patience, histogram]
  -U, --unified <N>                 Lines of context around each change (default 3)
  -w, --ignore-all-space            Ignore all whitespace when comparing lines
  -M, --find-renames                Detect renamed files (the short -C of git is taken by the global --chdir; use --find-copies)
      --find-copies                 Detect copies of modified files as well as renames
      --rename-threshold <PERCENT>  Minimum similarity in percent for rename and copy detection [default: 50]
  -h, --help                        Print help
```

## `vcs import-git`

```text
Import a `git fast-export` stream (file, or `-` for stdin) into the root zone

Usage: vcs import-git [OPTIONS] <STREAM>

Arguments:
  <STREAM>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs import-p4`

```text
Import recorded Perforce output (fixture directory, see docs/import-p4.md) into the root zone. Checked once against real p4d output on a small history; see that document

Usage: vcs import-p4 [OPTIONS] --view <VIEW> <DIR>

Arguments:
  <DIR>  

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --view <VIEW>      View file mapping depot paths to zone paths
      --branch <BRANCH>  Branch of the root zone to import onto [default: main]
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

## `vcs export-git`

```text
Write a zone branch as a Git fast-import stream (stdout, or --output)

Usage: vcs export-git [OPTIONS] [BRANCH]

Arguments:
  [BRANCH]  Branch (default: main)

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>      Zone (default: the zone containing the current directory)
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -o, --output <OUTPUT>  
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --full             Export the whole branch again and reset the export table (default: only commits not yet exported)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --onto-origin      Reference commits that came from import-git by their original Git SHA (target = the origin repo)
  -h, --help             Print help
```

## `vcs log`

```text
Show commit history of a zone branch

Usage: vcs log [OPTIONS] [BRANCH] [REVS]... [-- <PATHS>...]

Arguments:
  [BRANCH]    Branch or revision range (default: the checked-out branch): `A`, `A..B`, `A...B`, `^A`
  [REVS]...   More revisions (`vcs log main ^dev`, see also --not and --all)
  [PATHS]...  Only commits that change these paths (after `--`)

Options:
  -C <DIR>                          Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>                 Zone (default: the zone containing the current directory)
      --json                        Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --limit <LIMIT>               [default: 20]
      --ca-pem <FILE>               PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --oneline                     One line per commit: short id and subject
      --graph                       Draw the commit graph with ASCII lanes
      --no-hooks                    Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --author <AUTHOR>             Only commits whose author name or email contains this text (case-insensitive)
      --color <COLOR>               When to colour output [default: auto] [possible values: auto, always, never]
      --since <DATE>                Only commits at or after this date (UTC; see `vcs log --help` placeholders in docs/guide)
      --until <DATE>                Only commits at or before this date (a date without time includes that whole day)
      --grep <REGEX>                Only commits whose message matches this pattern: a POSIX basic regular expression like `git log --grep` (so `+ ? | ( ) { }` are literal; `\+ \? \| \( \)` are the operators), or see -E / -F
  -F, --fixed-strings               Treat `--grep` / `-G` patterns as plain strings (`+`, `(`, `.` are literal)
  -E, --extended-regexp             Treat `--grep` / `-G` patterns as extended regular expressions (`+ ? | ( )` are operators)
  -i, --regexp-ignore-case          Match `--grep` / `-G` patterns case-insensitively
      --merges                      Only merge commits (two or more parents)
      --no-merges                   Skip merge commits
      --first-parent                Follow only the first parent of each commit
  -S <STRING>                       Only commits that change the number of occurrences of this string in some file (pickaxe)
  -G <REGEX>                        Only commits that add or remove a line matching this regular expression in some file (like `git log -G`)
      --format <TEMPLATE>           Template per commit. Placeholders: %H %h %P %p %an %ae %at %ad %s %b %B %C %z %n %%
      --follow                      With one path: the history of that file across renames (first-parent walk)
      --name-status                 List changed files with a status letter under each commit
      --name-only                   List changed file names under each commit
  -M, --find-renames                Detect renames in --name-status / --name-only listings
      --find-copies                 Detect copies of modified files too
      --rename-threshold <PERCENT>  Minimum similarity in percent for rename and copy detection (also used by --follow) [default: 50]
      --all                         Start from every branch and tag of the zone (and a detached HEAD), not only the checked-out branch
      --all-zones                   Like --all for every zone of the repository; commits of all zones interleave newest first
      --not <REV>                   Leave out the history of this revision (repeatable; the same as a `^REV` argument)
      --reverse                     Show the selected commits oldest first (after `-n` picked them)
      --show-signature              Show the signature verdict of each commit (against `signing.allowedSigners`)
  -h, --help                        Print help
```

## `vcs gen`

```text
Generate shell completions or man pages

Usage: vcs gen [OPTIONS] <COMMAND>

Commands:
  completions  Print a shell completion script to stdout (bash, zsh, fish, elvish, powershell)
  man          Print the vcs(1) man page, or with --dir write one page per command
  help         Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs gen completions`

```text
Print a shell completion script to stdout (bash, zsh, fish, elvish, powershell)

Usage: vcs gen completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs gen man`

```text
Print the vcs(1) man page, or with --dir write one page per command

Usage: vcs gen man [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --dir <DIR>      Write vcs.1 and vcs-<command>.1 into this directory instead of printing
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs branch`

```text
List branches of the current zone, or create one (at the checked-out head)

Usage: vcs branch [OPTIONS] [NAME] [START]

Arguments:
  [NAME]   
  [START]  Start point (any revision) of the new branch; with -m/-M: the new name

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
  -d, --delete         Delete the named branch instead of creating it (refuses the checked-out branch)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --force          With --delete: delete even if its commits are not reachable from another branch or snapshot
      --at <COMMIT>    Create the branch at this commit (full id or prefix) instead of the checked-out head
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -m, --move           Rename a branch: `-m <old> <new>`, or `-m <new>` for the checked-out one
  -M, --force-move     Like -m but replace an existing branch of that name (never the checked-out one)
  -r, --remotes        List the remote-tracking branches of the zone (`origin/<branch>`, as of the last fetch or publish)
  -h, --help           Print help
```

## `vcs stash`

```text
Set tracked changes aside (push, pop, list, drop); untracked files are left alone

Usage: vcs stash [OPTIONS] <COMMAND>

Commands:
  push   Save staged and unstaged changes of tracked files and reset to the branch heads
  pop    Apply a stash entry (staged) and drop it; on conflicts it is kept
  apply  Apply a stash entry (staged) and keep it
  list   List stash entries, newest first
  drop   Forget a stash entry
  help   Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs stash push`

```text
Save staged and unstaged changes of tracked files and reset to the branch heads

Usage: vcs stash push [OPTIONS] [-- <PATHS>...]

Arguments:
  [PATHS]...  With -p: only these paths

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -u, --include-untracked  Also set aside untracked files (not ignored ones): they are added first, so `pop` brings them back staged
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -p, --patch              Choose hunks to stash one by one (answers on standard input); the rest stays in the working tree
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help               Print help
```

#### `vcs stash pop`

```text
Apply a stash entry (staged) and drop it; on conflicts it is kept

Usage: vcs stash pop [OPTIONS] [INDEX]

Arguments:
  [INDEX]  Entry number from `stash list` (default: newest)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs stash apply`

```text
Apply a stash entry (staged) and keep it

Usage: vcs stash apply [OPTIONS] [INDEX]

Arguments:
  [INDEX]  Entry number from `stash list` (default: newest)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs stash list`

```text
List stash entries, newest first

Usage: vcs stash list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs stash drop`

```text
Forget a stash entry

Usage: vcs stash drop [OPTIONS] [INDEX]

Arguments:
  [INDEX]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs notes`

```text
Annotate commits after the fact (add, show, list, append, remove); notes are published and fetched with the branches

Usage: vcs notes [OPTIONS] [COMMAND]

Commands:
  list    List notes as `<note blob> <annotated commit>`, or print the note blob id of one commit
  add     Attach a note to a commit (default HEAD); refuses an existing note unless -f
  show    Print the note on a commit (default HEAD); exit 1 when it has none
  append  Add text to the note on a commit, after a blank line (creates the note when there is none)
  remove  Remove the note from each commit (default HEAD)
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone whose commits are annotated (default: the zone of the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs notes list`

```text
List notes as `<note blob> <annotated commit>`, or print the note blob id of one commit

Usage: vcs notes list [OPTIONS] [REV]

Arguments:
  [REV]  Commit to look up (default: every note)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone whose commits are annotated (default: the zone of the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs notes add`

```text
Attach a note to a commit (default HEAD); refuses an existing note unless -f

Usage: vcs notes add [OPTIONS] [REV]

Arguments:
  [REV]  

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  Note text (repeat for several paragraphs, separated by a blank line)
      --zone <ZONE>        Zone whose commits are annotated (default: the zone of the current directory)
  -F, --file <FILE>        Read the note text from a file (`-` for standard input)
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -f, --force              Replace an existing note
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help               Print help
```

##### `vcs notes show`

```text
Print the note on a commit (default HEAD); exit 1 when it has none

Usage: vcs notes show [OPTIONS] [REV]

Arguments:
  [REV]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone whose commits are annotated (default: the zone of the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs notes append`

```text
Add text to the note on a commit, after a blank line (creates the note when there is none)

Usage: vcs notes append [OPTIONS] [REV]

Arguments:
  [REV]  

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  Note text (repeat for several paragraphs, separated by a blank line)
      --zone <ZONE>        Zone whose commits are annotated (default: the zone of the current directory)
  -F, --file <FILE>        Read the note text from a file (`-` for standard input)
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help               Print help
```

####### `vcs notes remove`

```text
Remove the note from each commit (default HEAD)

Usage: vcs notes remove [OPTIONS] [REVS]...

Arguments:
  [REVS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone whose commits are annotated (default: the zone of the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs shelve`

```text
Park your changes on the remote as a private shelf (the working tree is left alone); `--delete` removes one

Usage: vcs shelve [OPTIONS] [NAME]

Arguments:
  [NAME]  Shelf name (default: shelf-<unix time>); with --delete, `name` or `principal/name`

Options:
  -C <DIR>                  Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>   Description of the shelf
      --json                Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --zone <ZONE>         Shelve only this zone (default: every subscribed zone with changes)
      --ca-pem <FILE>       PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --expires <DURATION>  Let `vcs gc --remote` delete the shelf after this long (e.g. 90m, 12h, 30d, 2w)
      --delete              Delete the shelf instead of creating it (owner or admin)
      --no-hooks            Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                Print help
```

## `vcs shelves`

```text
List your shelves on the remote (`--all`: also other people's that you may read)

Usage: vcs shelves [OPTIONS]

Options:
      --all            Also list shelves of other principals that you are allowed to read
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs unshelve`

```text
Apply a shelf from the remote to the working tree (staged, like `stash pop`)

Usage: vcs unshelve [OPTIONS] <SHELF>

Arguments:
  <SHELF>  `principal/name` as printed by `vcs shelves --all` (a bare `name` is one of yours)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --drop           Delete the shelf from the remote after it applied cleanly (like `stash pop`); a conflicted apply keeps it
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs p4`

```text
p4 command	vcs command	status	test
client -o / workspace -o	zone list; sparse list	different-semantics	client_view
clients / workspaces	worktree list	different-semantics	clients
depots	zone list	renamed	depots
sync	sync	different-semantics	sync
sync -f	sync; restore .	different-semantics	sync_f
sync -n	fetch	different-semantics	sync_n
sync path@rev / @label / @N	(computed) write the files of that commit into the workspace	different-semantics	sync_rev
edit	lock (lockable files); nothing otherwise	different-semantics	edit_plain
add	add	exact	add
delete	rm	renamed	delete
move / rename	mv	renamed	move
revert	restore --staged; restore	renamed	revert
revert -a	(nothing to do)	different-semantics	revert_a
diff	diff; diff --staged (filtered to the files)	different-semantics	diff
diff2	(computed) unified diff of two revisions	renamed	diff2
opened	status (without untracked)	renamed	opened
opened -a	locks	different-semantics	opened_a
reconcile [-a -e -d -n]	add (the selected files)	renamed	reconcile
status	reconcile -n	renamed	p4_status
submit [-d msg]	add (edits); commit -m; publish	different-semantics	submit
changes [-m -s -u -l]	log [-n --author --oneline]	renamed	changes
describe [-s]	show [--name-status]	renamed	describe
filelog	log --follow --name-status -- file	renamed	filelog
annotate	blame	renamed	annotate
files	ls-files --rev	renamed	files
fstat	log -n 1 --format ... -- file	different-semantics	fstat
print	cat-file -p rev:path	renamed	print
have	ls-files	renamed	have
integrate / merge	merge	different-semantics	integrate
copy source-branch	(computed) make the tracked files equal to that branch, staged	different-semantics	copy
resolve [-at -ay -af -n]	resolve --theirs/--ours/--mark; conflict list	renamed	resolve
shelve [-c name] [-d]	shelve [name] [--delete]	different-semantics	shelve
unshelve -s name	unshelve name	different-semantics	shelve
label / tag / labelsync	tag [-d]	different-semantics	label
labels	tag	renamed	labels
branch / stream	branch [-d]	different-semantics	branch
branches / streams	branch	renamed	branches
switch	switch	exact	switch
lock	lock --lease-secs	different-semantics	lock
unlock [-f]	unlock [--break]	exact	lock
locks	locks	exact	lock
where	(computed) zone path, worktree path	renamed	where
info	(computed) identity, root, remote	different-semantics	info
users	shortlog -s -e	different-semantics	users
login / logout	login / logout	exact	login
init / fetch / push / clone	init / fetch / publish / clone	renamed	push
grep	grep	exact	grep
clean	restore --staged .; restore .	different-semantics	clean
change / changelist	status	different-semantics	change
change -m desc / -i / -d N / -o N	(local ledger) numbered pending changelists	different-semantics	change_numbered
add / edit / delete / revert / opened -c N	(local ledger) open files in changelist N	different-semantics	numbered_open
reopen -c N	(local ledger) move files between changelists	different-semantics	reopen
submit -c N	add (the changelist's files); commit; publish	different-semantics	submit_numbered
describe N / @N	show <commit recorded for N>	different-semantics	changelist_numbers
verify	fsck	renamed	verify
obliterate / jobs / fix	(none)	missing	missing_cmds
protect / groups / group / user / passwd	(none: vcs-server tokens)	missing	protect_missing
```

## `vcs sparse`

```text
Materialize only some paths of a zone (set, add, remove, list, disable); the rest stays tracked, not deleted

Usage: vcs sparse [OPTIONS] <COMMAND>

Commands:
  set      Materialize only these paths (directories or files) of the zone; fetches just their blobs
  add      Also materialize these paths
  remove   Stop materializing these listed paths (files leave the working tree, stay tracked)
  list     Show the sparse sets of this workspace
  disable  Materialize the whole zone again
  help     Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs sparse set`

```text
Materialize only these paths (directories or files) of the zone; fetches just their blobs

Usage: vcs sparse set [OPTIONS] <ZONE> [PATHS]...

Arguments:
  <ZONE>      
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs sparse add`

```text
Also materialize these paths

Usage: vcs sparse add [OPTIONS] <ZONE> [PATHS]...

Arguments:
  <ZONE>      
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs sparse remove`

```text
Stop materializing these listed paths (files leave the working tree, stay tracked)

Usage: vcs sparse remove [OPTIONS] <ZONE> [PATHS]...

Arguments:
  <ZONE>      
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs sparse list`

```text
Show the sparse sets of this workspace

Usage: vcs sparse list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs sparse disable`

```text
Materialize the whole zone again

Usage: vcs sparse disable [OPTIONS] <ZONE>

Arguments:
  <ZONE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs tag`

```text
List tags of the current zone, create one (lightweight, or annotated with -m), or delete one with -d

Usage: vcs tag [OPTIONS] [NAME] [REV]

Arguments:
  [NAME]  
  [REV]   Revision to tag (default: HEAD): a tag, branch, commit id prefix, optionally with ~N or ^

Options:
  -C <DIR>                     Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>            
      --json                   Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -m, --message <MESSAGE>      Make an annotated tag with this message
      --ca-pem <FILE>          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --signature <SIGNATURE>  Opaque signature text stored in an annotated tag (never verified; use --sign for a real signature)
      --no-hooks               Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -s, --sign                   Sign the annotated tag with `user.signingkey` (Ed25519; needs -m)
  -d, --delete                 Delete the named tag
  -f, --force                  Replace the tag when it already exists (the old tag is only dropped once the new target resolves)
  -h, --help                   Print help
```

## `vcs format-patch`

```text
Write commits as mail-format patch files (one per commit: author, date, message, diff)

Usage: vcs format-patch [OPTIONS] [REV]

Arguments:
  [REV]  Commits to turn into patches: `A..B` (commits of B not in A), `A...B` (commits of either not in the other), or a revision (commits on the branch not in it)

Options:
  -C <DIR>                      Run as if vcs was started in <DIR> (applied before repository discovery)
  -n, --count <N>               The last N commits of the branch instead
      --json                    Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --root                    Include root commits (all history up to the branch tip)
      --ca-pem <FILE>           PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -o, --output-directory <DIR>  Directory for the patch files (default: the current directory)
      --no-hooks                Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --stdout                  Write all patches to standard output as one mailbox
      --start-number <N>        Number the first patch from N (default 1) [default: 1]
      --no-numbered             Never put `n/m` in the subject
      --subject-prefix <TEXT>   Subject prefix inside the brackets (default PATCH) [default: PATCH]
      --cover-letter            Write a `0000-cover-letter.patch` first: a subject and blurb to fill in, the shortlog and the diffstat of the series; the patches are then always numbered n/N
      --zone <ZONE>             Zone (default: the zone containing the current directory)
  -h, --help                    Print help
```

## `vcs apply`

```text
Apply a unified diff (from `vcs diff`, `vcs show`, `vcs format-patch` or git) to the working tree

Usage: vcs apply [OPTIONS] [FILES]...

Arguments:
  [FILES]...  Patch files (default: standard input; `-` is standard input)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --check          Check that the patch applies; change nothing
      --index          Also stage the changes (the files must match the index)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --cached         Apply to the index only: check against the staged content, stage the result, leave the working tree alone
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -R, --reverse        Apply the patch backwards
  -p <N>               Strip N leading path components (default 1)
  -3, --3way           Three-way application: a hunk that does not fit is merged against its own pre-image; overlapping edits leave conflict markers in the file
  -v, --verbose        Print the files that were (or would be) changed
      --stat           Show the per-file added and deleted line counts of the patch and apply nothing
  -h, --help           Print help
```

## `vcs am`

```text
Apply mail-format patches as commits, keeping author, date and message

Usage: vcs am [OPTIONS] [FILES]...

Arguments:
  [FILES]...  Mailbox files, such as those `format-patch` wrote (default: standard input)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --continue       Commit the files you fixed and staged for the stopped patch, then go on
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --skip           Drop the stopped patch and go on
      --abort          Go back to the branch heads from before `vcs am`
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -3, --3way           Three-way application: a hunk that does not fit is merged against its own pre-image; the run stops on overlapping edits with conflict markers (resolve, `vcs add`, `vcs am --continue`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs show`

```text
Show a commit (default HEAD) with its diff against the first parent

Usage: vcs show [OPTIONS] [REV]

Arguments:
  [REV]  

Options:
  -C <DIR>                          Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>                 
      --json                        Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stat                        Per-file added/deleted line counts instead of the patch
      --ca-pem <FILE>               PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --numstat                     Machine-readable counts: `added<TAB>deleted<TAB>path` per file (`-` for both on a binary file)
      --exit-code                   Exit with status 1 when there are differences, 0 when there are none
      --no-hooks                    Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --name-only                   Only the names of changed files
      --name-status                 Names with a status letter: A added, D deleted, M modified, R<score> renamed, C<score> copied
      --word-diff                   Show changed words inline as [-old-]{+new+} instead of changed lines
      --color [<WHEN>]              Colour the output: auto (terminal and no NO_COLOR), always, never. Bare `--color` forces colour unless NO_COLOR is set; `--color=always` overrides NO_COLOR
      --color-moved                 Colour lines that were moved (deleted in one place, added in another of the same diff) differently; only visible with colour
      --diff-algorithm <ALGORITHM>  Diff algorithm: myers (default), patience or histogram [possible values: myers, patience, histogram]
  -U, --unified <N>                 Lines of context around each change (default 3)
  -w, --ignore-all-space            Ignore all whitespace when comparing lines
  -M, --find-renames                Detect renamed files (the short -C of git is taken by the global --chdir; use --find-copies)
      --find-copies                 Detect copies of modified files as well as renames
      --rename-threshold <PERCENT>  Minimum similarity in percent for rename and copy detection [default: 50]
  -h, --help                        Print help
```

## `vcs blame`

```text
Attribute each line of a file to a commit, following first parents

Usage: vcs blame [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
  -C <DIR>                          Run as if vcs was started in <DIR> (applied before repository discovery)
      --rev <REV>                   Revision to blame at (default: HEAD of the file's zone)
      --json                        Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -M, --find-renames                Follow the file across renames (a file new in a commit is matched with files that commit removed)
      --ca-pem <FILE>               PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --rename-threshold <PERCENT>  Minimum similarity in percent for -M [default: 50]
  -L, --lines <RANGE>               Only these lines: `START,END`, `START,+COUNT`, `START,-COUNT`, `START` (to the end), `,END` or `START,`
      --no-hooks                    Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                        Print help
```

## `vcs hook`

```text
List, install or run client-side hooks (.vcs/hooks/, see docs/hooks.md)

Usage: vcs hook [OPTIONS] <COMMAND>

Commands:
  list     Show which hooks are installed and whether they would run
  run      Run a hook by hand with the documented environment (ignores --no-hooks); extra arguments are passed through
  install  Copy an executable into .vcs/hooks/<name> (mode 755)
  help     Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs hook list`

```text
Show which hooks are installed and whether they would run

Usage: vcs hook list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs hook run`

```text
Run a hook by hand with the documented environment (ignores --no-hooks); extra arguments are passed through

Usage: vcs hook run [OPTIONS] <NAME> [ARGS]...

Arguments:
  <NAME>     
  [ARGS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs hook install`

```text
Copy an executable into .vcs/hooks/<name> (mode 755)

Usage: vcs hook install [OPTIONS] <NAME> <FILE>

Arguments:
  <NAME>  
  <FILE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -f, --force          Replace an installed hook
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs bisect`

```text
Binary-search a zone's history for the commit that introduced a bug

Usage: vcs bisect [OPTIONS] <COMMAND>

Commands:
  start   Start bisecting; optionally give the bad commit and good commits right away
  good    Mark revisions (default: the checked-out commit) as good
  bad     Mark revisions (default: the checked-out commit) as bad
  skip    Mark revisions (default: the checked-out commit) as untestable
  reset   End the bisection and return to the original branch
  log     Print what has been marked so far, in the order it was marked (feed it to `replay`)
  replay  Redo the marks of a saved `bisect log` on a fresh bisection
  run     Run a command at each step: exit 0 = good, 125 = skip, 1-127 = bad, above 127 aborts
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs bisect start`

```text
Start bisecting; optionally give the bad commit and good commits right away

Usage: vcs bisect start [OPTIONS] [REVS]...

Arguments:
  [REVS]...  Bad revision, then any number of good revisions

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs bisect good`

```text
Mark revisions (default: the checked-out commit) as good

Usage: vcs bisect good [OPTIONS] [REVS]...

Arguments:
  [REVS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs bisect bad`

```text
Mark revisions (default: the checked-out commit) as bad

Usage: vcs bisect bad [OPTIONS] [REVS]...

Arguments:
  [REVS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs bisect skip`

```text
Mark revisions (default: the checked-out commit) as untestable

Usage: vcs bisect skip [OPTIONS] [REVS]...

Arguments:
  [REVS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs bisect reset`

```text
End the bisection and return to the original branch

Usage: vcs bisect reset [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######## `vcs bisect log`

```text
Print what has been marked so far, in the order it was marked (feed it to `replay`)

Usage: vcs bisect log [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######### `vcs bisect replay`

```text
Redo the marks of a saved `bisect log` on a fresh bisection

Usage: vcs bisect replay [OPTIONS] <FILE>

Arguments:
  <FILE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

########## `vcs bisect run`

```text
Run a command at each step: exit 0 = good, 125 = skip, 1-127 = bad, above 127 aborts

Usage: vcs bisect run [OPTIONS] <CMD>...

Arguments:
  <CMD>...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs rerere`

```text
Reuse recorded conflict resolutions (rerere.enabled; docs/rerere.md)

Usage: vcs rerere [OPTIONS] [COMMAND]

Commands:
  status     List the paths of the conflict in progress that rerere is tracking
  remaining  List the conflicted paths rerere did not resolve (still need your edit)
  diff       Show what you changed in the tracked files since the conflict was written
  forget     Forget the recorded resolution for these paths of the conflict in progress, so the next one is recorded afresh
  clear      Drop the unresolved preimages of the conflict in progress and stop tracking it
  gc         Delete resolutions unused for gc.rerereResolved days (60) and conflicts unresolved for gc.rerereUnresolved days (15)
  help       Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs rerere status`

```text
List the paths of the conflict in progress that rerere is tracking

Usage: vcs rerere status [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs rerere remaining`

```text
List the conflicted paths rerere did not resolve (still need your edit)

Usage: vcs rerere remaining [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs rerere diff`

```text
Show what you changed in the tracked files since the conflict was written

Usage: vcs rerere diff [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs rerere forget`

```text
Forget the recorded resolution for these paths of the conflict in progress, so the next one is recorded afresh

Usage: vcs rerere forget [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs rerere clear`

```text
Drop the unresolved preimages of the conflict in progress and stop tracking it

Usage: vcs rerere clear [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######## `vcs rerere gc`

```text
Delete resolutions unused for gc.rerereResolved days (60) and conflicts unresolved for gc.rerereUnresolved days (15)

Usage: vcs rerere gc [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs grep`

```text
Search tracked files (working tree, or a revision) for a regular expression

Usage: vcs grep [OPTIONS] <PATTERN> [REV] [-- <PATHS>...]

Arguments:
  <PATTERN>   Regular expression (Rust regex syntax); use -F for a literal string
  [REV]       Revision to search instead of the working tree
  [PATHS]...  Limit the search to these paths (after `--`)

Options:
  -C <DIR>                  Run as if vcs was started in <DIR> (applied before repository discovery)
  -n, --line-number         Prefix each line with its line number
  -i, --ignore-case         Ignore case
      --json                Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>       PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -w, --word                Match whole words only
  -l, --files-with-matches  Print only the names of files with a match
      --no-hooks            Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -c, --count               Print only the number of matching lines per file
  -F, --fixed               Treat the pattern as a literal string
      --cached              Search the staged content (the index) instead of the files on disk
  -h, --help                Print help
```

## `vcs describe`

```text
Name a commit after the nearest tag behind it

Usage: vcs describe [OPTIONS] [REV]

Arguments:
  [REV]  Revision to describe (default HEAD)

Options:
  -C <DIR>              Run as if vcs was started in <DIR> (applied before repository discovery)
      --tags            Use lightweight tags too (default: annotated tags only)
      --always          Show the abbreviated commit id when no tag is found
      --json            Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>   PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --zone <ZONE>     
      --dirty[=<MARK>]  Append MARK (default `-dirty`) when tracked files differ from HEAD (staged or unstaged); HEAD only
      --no-hooks        Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help            Print help
```

## `vcs archive`

```text
Write the files of a revision as a .tar or .zip archive

Usage: vcs archive [OPTIONS] [REV]

Arguments:
  [REV]  Revision to archive (default HEAD)

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
  -o, --output <OUTPUT>  Output file; the format follows the extension (.tar or .zip)
      --format <FORMAT>  Format when writing to stdout or when the extension is unknown: tar or zip
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --prefix <PREFIX>  Prepend this directory prefix to every path (e.g. `proj-1.0/`)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

## `vcs rev-parse`

```text
Resolve revisions to commit ids

Usage: vcs rev-parse [OPTIONS] [REVS]...

Arguments:
  [REVS]...  Revisions to resolve (HEAD, a branch, a tag, a commit id prefix, with ~N or ^)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --short          Abbreviate the ids
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --show-toplevel  Print the absolute path of the working tree root instead
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --git-dir        Print the directory that holds this worktree's state (`.vcs`; relative at the repository root, else absolute)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --zone <ZONE>    Zone (default: the zone containing the current directory)
  -h, --help           Print help
```

## `vcs ls-files`

```text
List tracked files

Usage: vcs ls-files [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  Only these paths (files or directories)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -s, --stage          Show mode, object id and path
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --rev <REV>      List the files of this revision instead of the working tree's tracked files
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --depth <N>      With --rev: list only the entries directly inside the root of the revision (`1`), directories as mode 040000 with their tree id, like `git ls-tree <rev>`; reads one tree
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -o, --others         List the untracked files instead (one per file, not collapsed to directories); files matched by `.vcsignore` are left out, like `git ls-files -o --exclude-standard`
  -h, --help           Print help
```

## `vcs cat-file`

```text
Show the type, size or content of an object

Usage: vcs cat-file [OPTIONS] [OBJECT]

Arguments:
  [OBJECT]  An object id (64 hex digits), a revision, or `<rev>:<path>` for a file [default: ""]

Options:
      --batch          Read object names from standard input, one per line, and print `<id> <type> <size>`, the content and a newline for each (`<name> missing` for an unknown one)
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --batch-check    Like --batch but only the `<id> <type> <size>` line
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -t                   Print the object's type
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -s                   Print the object's size in bytes
  -p                   Print the content (file bytes; for a commit its header and message)
      --zone <ZONE>    
  -h, --help           Print help
```

## `vcs rev-list`

```text
List commit ids of a revision range (A..B, A...B, ^A B, --all), newest first

Usage: vcs rev-list [OPTIONS] [REVS]...

Arguments:
  [REVS]...  Revisions: `A`, `^A`, `A..B`, `A...B` (default: HEAD)

Options:
      --all            Also start from every branch and tag of the zone (and a detached HEAD)
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --not <REV>      Leave out the history of this revision (repeatable; same as `^REV`)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --count          Print only the number of commits
  -n, --max-count <N>  At most this many commits
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --reverse        Oldest first (after the limit)
      --zone <ZONE>    Zone (default: the zone containing the current directory)
  -h, --help           Print help
```

## `vcs shortlog`

```text
Summarize history by author

Usage: vcs shortlog [OPTIONS] [BRANCH]

Arguments:
  [BRANCH]  Branch, or a revision range such as `v1..main` (default: the checked-out branch)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -s, --summary        Only the number of commits per author
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --numbered       Sort by number of commits instead of by name
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -e, --email          Show the author's email
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --zone <ZONE>    
  -h, --help           Print help
```

## `vcs merge-base`

```text
Best common ancestor of two revisions

Usage: vcs merge-base [OPTIONS] <A> <B>

Arguments:
  <A>  First revision
  <B>  Second revision

Options:
      --all            Print every best common ancestor, not just the first
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --zone <ZONE>    Zone (default: the zone containing the current directory)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs diff-tree`

```text
List the files that differ between two revisions (A/D/M and path)

Usage: vcs diff-tree [OPTIONS] <A> <B>

Arguments:
  <A>  Older revision
  <B>  Newer revision

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone (default: the zone containing the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs reset`

```text
Move the checked-out branch of a zone to another commit (--soft, --mixed, --hard); acts on one zone

Usage: vcs reset [OPTIONS] [REV] [-- <PATHS>...]

Arguments:
  [REV]       Revision to move to (default HEAD): a branch, tag, commit id prefix, with ~N or ^
  [PATHS]...  Only unstage these paths (index entries return to HEAD); the head does not move

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --soft           Move only the branch head; keep the index and the files (changes show as staged)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --mixed          Move the head and reset the index to it; keep the files (the default)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --hard           Move the head and reset the index AND the files of the zone; uncommitted work in the zone is lost
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --zone <ZONE>    Zone (default: the zone containing the current directory)
  -h, --help           Print help
```

## `vcs rewrite`

```text
Rewrite a zone's history: keep/remove paths, change messages or authors, run a command on every tree (undo with `vcs op undo`)

Usage: vcs rewrite [OPTIONS]

Options:
  -C <DIR>                         Run as if vcs was started in <DIR> (applied before repository discovery)
      --subdirectory-filter <DIR>  Make this directory the root of every commit; commits without it vanish (filter-repo --subdirectory-filter)
      --json                       Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --keep <PATH>                Keep only this file or directory (repeatable; filter-repo --path)
      --ca-pem <FILE>              PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --remove <PATH>              Remove this file or directory from every commit (repeatable; filter-repo --path with --invert-paths)
      --msg-filter <CMD>           Shell command: old message on stdin, new message on stdout
      --no-hooks                   Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --env-filter <SCRIPT>        Shell script that may set GIT_AUTHOR_NAME / GIT_AUTHOR_EMAIL / GIT_AUTHOR_DATE ('@SECONDS'); GIT_COMMITTER_* are readable but changes to them are ignored (a commit has one signature)
      --tree-filter <CMD>          Shell command run in a checkout of each commit's tree (SLOW: one checkout and one re-hash per commit)
      --tag-name-filter <CMD>      Shell command: tag name on stdin, new tag name on stdout (`cat` keeps the names; tags are moved either way)
      --all-branches               Rewrite every branch of the zone, not only the checked-out one
      --keep-empty                 Keep commits that the filters left empty (default: drop them)
  -f, --force                      Rewrite although the changed commits are already published (held by origin as last fetched)
  -n, --dry-run                    Show what would change; move nothing
      --zone <ZONE>                Zone (default: the zone containing the current directory)
  -h, --help                       Print help
```

## `vcs clean`

```text
Delete untracked files (-n lists, -f deletes)

Usage: vcs clean [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  Only below these paths

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -f, --force          Delete (required unless --dry-run, like git's clean.requireForce)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --dry-run        Only list what would be deleted
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -d                   Also delete untracked directories (here: untracked files inside them are always listed, so this only removes the emptied directories)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -x                   Also delete files matched by `.vcsignore` (ignored directories need -d as well)
  -h, --help           Print help
```

## `vcs profile`

```text
Local usage profile that `sync --auto` / `pull --auto` / `clone --auto` predict zones from (docs/zone-profile.md)

Usage: vcs profile [OPTIONS] <COMMAND>

Commands:
  show     Show what is recorded: counts per zone and where the file is
  on       Start recording again after `off`
  off      Stop recording and erase what was recorded (hints still work)
  clear    Erase recorded events, keep recording
  record   Run a command and record it as build activity in the current zone (and in any zone whose directory the command line names). The command's exit status is passed through
  note     Record that you read these paths (a hint for editors and build tools)
  rank     Rank the zones for this repository from the profile (offline) and print the plan
  skipped  List zones that are not subscribed, with how to fetch them
  fetch    Download the contents of zones now (commits and trees too if missing) and subscribe
  help     Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs profile show`

```text
Show what is recorded: counts per zone and where the file is

Usage: vcs profile show [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs profile on`

```text
Start recording again after `off`

Usage: vcs profile on [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs profile off`

```text
Stop recording and erase what was recorded (hints still work)

Usage: vcs profile off [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs profile clear`

```text
Erase recorded events, keep recording

Usage: vcs profile clear [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs profile record`

```text
Run a command and record it as build activity in the current zone (and in any zone whose directory the command line names). The command's exit status is passed through

Usage: vcs profile record [OPTIONS] <CMD>...

Arguments:
  <CMD>...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --path <PATHS>   Also record these paths as used (repeatable)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######## `vcs profile note`

```text
Record that you read these paths (a hint for editors and build tools)

Usage: vcs profile note [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######### `vcs profile rank`

```text
Rank the zones for this repository from the profile (offline) and print the plan

Usage: vcs profile rank [OPTIONS]

Options:
      --budget-zones <N>     Add at most this many zones (default: the hints' max_zones, else 8). Zones already subscribed and the root zone are always kept and do not count
  -C <DIR>                   Run as if vcs was started in <DIR> (applied before repository discovery)
      --budget-bytes <SIZE>  Add at most this many stored bytes of file contents (e.g. 200M, 2G; default: no limit)
      --json                 Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>        PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --explain              Show why each zone is ranked where it is
      --no-hooks             Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                 Print help
```

########## `vcs profile skipped`

```text
List zones that are not subscribed, with how to fetch them

Usage: vcs profile skipped [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

########### `vcs profile fetch`

```text
Download the contents of zones now (commits and trees too if missing) and subscribe

Usage: vcs profile fetch [OPTIONS] [ZONES]...

Arguments:
  [ZONES]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --with-deps      Also the zones they depend on
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stats          Print the number of remote requests and bytes transferred
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs pull`

```text
Same as `sync` (fetch, then fast-forward); `--auto` chooses zones from your usage profile

Usage: vcs pull [OPTIONS]

Options:
      --auto                 Choose which zones to download from your local usage profile (see `vcs profile`); the other zones get commits and trees only and are listed, fetchable with `vcs profile fetch`
  -C <DIR>                   Run as if vcs was started in <DIR> (applied before repository discovery)
      --budget-zones <N>     Add at most this many zones (default: the hints' max_zones, else 8). Zones already subscribed and the root zone are always kept and do not count
      --json                 Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --budget-bytes <SIZE>  Add at most this many stored bytes of file contents (e.g. 200M, 2G; default: no limit)
      --ca-pem <FILE>        PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --auto-profile <FILE>  Seed the prediction with a profile or hints file (repeatable; with --auto)
      --no-hooks             Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --explain              With --auto: show why each zone was chosen or skipped
      --dry-run              With --auto: print the plan, download no file contents, change nothing but zone metadata
      --stats                Print the number of remote requests and bytes transferred
  -h, --help                 Print help
```

## `vcs range-diff`

```text
Compare two commit ranges by patch: which commits were kept (=), changed (!), dropped (<) or added (>)

Usage: vcs range-diff [OPTIONS] <RANGES>...

Arguments:
  <RANGES>...  `OLD_BASE..OLD_TIP NEW_BASE..NEW_TIP`, or `BASE OLD_TIP NEW_TIP`, or `A...B` (= `B..A A..B`)

Options:
  -C <DIR>
          Run as if vcs was started in <DIR> (applied before repository discovery)
      --creation-factor <CREATION_FACTOR>
          How much a commit that has no partner is penalised, in percent of its patch size (higher pairs more commits) [default: 60]
      --json
          Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -s, --no-patch
          List the pairs only, without the diff between the two patches
      --ca-pem <FILE>
          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --zone <ZONE>
          Zone (default: the zone containing the current directory)
      --no-hooks
          Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help
          Print help
```

## `vcs show-branch`

```text
Show which commits each of several revisions has, as a matrix (git show-branch)

Usage: vcs show-branch [OPTIONS] [REVS]...

Arguments:
  [REVS]...  Revisions to compare (default: every local branch of the zone)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --list           Show the heads only, no commit rows
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --more <N>       Go N commits past the first common ancestor
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --merge-base     Print the best common ancestor(s) of the revisions instead of the matrix
      --independent    Print the revisions that are not an ancestor of another one
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --current        Also include the checked-out branch
      --sha1-name      Name commits by short id instead of `branch~N`
      --no-name        Leave the `[name]` out of the rows
      --zone <ZONE>    Zone (default: the zone containing the current directory)
  -h, --help           Print help
```

## `vcs git`

```text
Run a git command line as its vcs equivalent, e.g. `vcs git log --oneline` (table: docs/git-to-vcs.md; `--map` lists it)

Usage: vcs git [OPTIONS] [GIT_ARGS]...

Arguments:
  [GIT_ARGS]...  A git command line, e.g. `vcs git log --oneline -5`

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs switch`

```text
Switch branch (in the zone containing cwd, or --zone); `--detach` checks out any revision without a branch

Usage: vcs switch [OPTIONS] <BRANCH>

Arguments:
  <BRANCH>  Branch (with --detach: any revision: tag, commit id prefix, HEAD~2, branch)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
  -c, --create         Create the branch (at the checked-out head) and switch to it
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -d, --detach         Check the revision out as a detached HEAD of the zone (commits made there belong to no branch)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs merge`

```text
Merge a branch into the checked-out branch (fast-forward when possible)

Usage: vcs merge [OPTIONS] [BRANCH]

Arguments:
  [BRANCH]  Branch to merge (not needed with --abort)

Options:
  -C <DIR>
          Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>
          Zone (default: the zone containing the current directory)
      --json
          Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --remote
          Merge the remote-tracking branch (as last fetched) instead of a local one
      --abort
          Abandon the in-progress merge and restore the pre-merge state
      --ca-pem <FILE>
          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks
          Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --no-structural
          Do not use the structural Rust merge (only matters when built with `--features structural`)
      --renames
          Detect renamed/moved files and merge the other side's edits into them (off by default)
      --assist <COMMAND>
          After a conflicted merge, ask this command (run via the shell) for a resolution of each text conflict; answers are saved as proposals, never applied (see `vcs conflict proposals`)
      --assist-timeout <ASSIST_TIMEOUT>
          Seconds the assistant gets per conflict [default: 30]
      --no-assist
          Do not run the assistant even if the `merge.assist` setting is configured
      --no-ff
          Always create a merge commit, even when a fast-forward is possible
      --squash
          Stage the merged result without a merge commit; the branch does not move (then `vcs commit`)
      --ff-only
          Refuse unless the merge is a fast-forward (or already up to date)
  -m, --message <MESSAGE>
          Message for the merge commit
  -h, --help
          Print help
```

## `vcs cherry-pick`

```text
Re-apply the change of one commit onto the checked-out branch

Usage: vcs cherry-pick [OPTIONS] <REVS>...

Arguments:
  <REVS>...  Commit id (or unique prefix), branch name, HEAD, a range `A..B` (the commits of B that A lacks, oldest first) or `A...B` (the commits of either that the other lacks); several allowed

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --mainline <PARENT>  Mainline parent (1-based) for a merge commit: take its change relative to that parent
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --zone <ZONE>        Zone (default: the zone containing the current directory)
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help               Print help
```

## `vcs cherry`

```text
List the commits of a branch with `+` (no equivalent patch upstream) or `-` (an upstream commit has the same patch)

Usage: vcs cherry [OPTIONS] <UPSTREAM> [HEAD]

Arguments:
  <UPSTREAM>  Branch or revision to compare against
  [HEAD]      Commits to list (default HEAD)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -v, --verbose        Print the subject after each id
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --zone <ZONE>    Zone (default: the zone containing the current directory)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs revert`

```text
Create a commit that undoes one commit

Usage: vcs revert [OPTIONS] <REV>

Arguments:
  <REV>  Commit id (or unique prefix), branch name or HEAD

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone (default: the zone containing the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --no-commit      Leave the undoing change staged in the index and working tree instead of committing it
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs rebase`

```text
Replay the checked-out branch onto another branch, keeping each change id

Usage: vcs rebase [OPTIONS] [ONTO]

Arguments:
  [ONTO]  Branch to rebase onto (not needed with --continue/--abort)

Options:
  -C <DIR>
          Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>
          Zone (default: the zone containing the current directory)
      --continue
          Commit the resolved conflicts and carry on
      --json
          Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --abort
          Abandon the in-progress rebase and restore the original branch
      --ca-pem <FILE>
          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks
          Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --skip
          Drop the commit the rebase is stopped on (conflicts) and carry on
  -i, --interactive
          Edit the todo list (pick/reword/edit/squash/fixup/drop/exec, reorder) in $GIT_SEQUENCE_EDITOR / $VCS_SEQUENCE_EDITOR / core.editor / $EDITOR; `onto` may be any revision such as HEAD~3
      --autosquash
          With -i: move `fixup! X` / `squash! X` commits next to X and fold them
  -x, --exec <COMMAND>
          Run this shell command after each replayed commit (repeatable); a failure stops the rebase
      --onto <NEWBASE>
          Replay onto NEWBASE (any revision) the commits that the positional upstream lacks (`git rebase --onto`)
      --root
          With --onto: replay every commit of the branch, root commit included (no upstream)
  -r, --rebase-merges[=<MODE>]
          Recreate the merge commits of the range instead of refusing it (`git rebase --rebase-merges`). MODE `no-rebase-cousins` (default) keeps commits that do not descend from the merge base where they are; `rebase-cousins` replays them too. Not combinable with -i or -x
      --no-structural
          Do not use the structural Rust merge (only matters when built with `--features structural`)
      --renames
          Detect renamed/moved files and merge the other side's edits into them (off by default)
      --assist <COMMAND>
          After a conflicted step, ask this command for a resolution of each text conflict (saved as proposals, never applied; see `vcs conflict proposals`)
      --assist-timeout <ASSIST_TIMEOUT>
          Seconds the assistant gets per conflict [default: 30]
      --no-assist
          Do not run the assistant even if the `merge.assist` setting is configured
  -h, --help
          Print help
```

## `vcs conflict`

```text
Inspect conflicts of an in-progress merge

Usage: vcs conflict [OPTIONS] <COMMAND>

Commands:
  list       List conflicted paths and whether they are resolved
  proposals  Show the assistant's saved proposals as diffs from our side
  show       Show one conflict (ids and a diff3 rendering for text)
  help       Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs conflict list`

```text
List conflicted paths and whether they are resolved

Usage: vcs conflict list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs conflict proposals`

```text
Show the assistant's saved proposals as diffs from our side

Usage: vcs conflict proposals [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs conflict show`

```text
Show one conflict (ids and a diff3 rendering for text)

Usage: vcs conflict show [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs resolve`

```text
Resolve a conflicted path of a merge or rebase (ours = recorded left side, theirs = right)

Usage: vcs resolve [OPTIONS] <--ours|--theirs|--base|--file <FILE>|--mark|--check|--proposal> [PATH]

Arguments:
  [PATH]  Conflicted path (not used with --check)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --ours           Take our side (merge: current branch; rebase: the branch being rebased onto)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --theirs         Take their side
      --base           Take the common ancestor's version
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --file <FILE>    Take the content of this file
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --mark           Accept the hand-edited working-tree file
      --proposal       Apply the assistant's saved proposal for this path (see `vcs merge --assist`)
      --check          Fail while conflicts are unresolved or conflict markers remain in resolved files
  -h, --help           Print help
```

## `vcs zone`

```text
Manage zones

Usage: vcs zone [OPTIONS] <COMMAND>

Commands:
  create     Create a zone owning a path prefix (zones may not overlap)
  list       
  show       
  branches   
  branch     Create a branch in a zone
  checkout   Check out a branch in a zone
  add        Subscribe to (and materialize) a zone
  remove     Unsubscribe from a zone (removes its files from the workspace)
  rename     Rename a zone (also with history: commits keep the original name as the zone's id)
  delete     Delete a zone definition (refuses a dirty zone; --force drops its branches and snapshot pins)
  suggest    Propose zone boundaries from manifests, ownership files, directories (read-only; see docs/zones-autodetect.md)
  apply      Turn one suggestion into a zone (plan only without --yes; history is not rewritten)
  subscribe  Subscribe to the minimal zone set for a path, symbol, task or person, plus their dependencies
  describe   Machine-readable description of zones: purpose, owners, dependencies, subscriptions, locks, history (docs/zone-ontology.md)
  graph      Zone dependency graph with reasons, transitive closures and path index (docs/zone-ontology.md)
  explain    One call: owner zone of a path, its owners, dependencies, dependents and impacted zones (docs/zone-ontology.md)
  changes    Per zone, the files that changed between two commits or dates (`A..B`), in one call
  help       Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs zone create`

```text
Create a zone owning a path prefix (zones may not overlap)

Usage: vcs zone create [OPTIONS] --path <PATH> <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --path <PATH>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs zone list`

```text
Usage: vcs zone list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs zone show`

```text
Usage: vcs zone show [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs zone branches`

```text
Usage: vcs zone branches [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

####### `vcs zone branch`

```text
Create a branch in a zone

Usage: vcs zone branch [OPTIONS] <ZONE> <BRANCH>

Arguments:
  <ZONE>    
  <BRANCH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######## `vcs zone checkout`

```text
Check out a branch in a zone

Usage: vcs zone checkout [OPTIONS] <ZONE> <BRANCH>

Arguments:
  <ZONE>    
  <BRANCH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

######### `vcs zone add`

```text
Subscribe to (and materialize) a zone

Usage: vcs zone add [OPTIONS] <NAME> [BRANCH]

Arguments:
  <NAME>    
  [BRANCH]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

########## `vcs zone remove`

```text
Unsubscribe from a zone (removes its files from the workspace)

Usage: vcs zone remove [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

########### `vcs zone rename`

```text
Rename a zone (also with history: commits keep the original name as the zone's id)

Usage: vcs zone rename [OPTIONS] <OLD> <NEW>

Arguments:
  <OLD>  
  <NEW>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

############ `vcs zone delete`

```text
Delete a zone definition (refuses a dirty zone; --force drops its branches and snapshot pins)

Usage: vcs zone delete [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --force          
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

############# `vcs zone suggest`

```text
Propose zone boundaries from manifests, ownership files, directories (read-only; see docs/zones-autodetect.md)

Usage: vcs zone suggest [OPTIONS]

Options:
  -C <DIR>
          Run as if vcs was started in <DIR> (applied before repository discovery)
      --min-confidence <MIN_CONFIDENCE>
          Hide proposals below this confidence (0..1) [default: 0.4]
      --json
          Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --min-files <MIN_FILES>
          Ignore candidates with fewer files than this [default: 3]
      --ca-pem <FILE>
          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --cochange
          Also analyse history: which proposed zones change in the same commits (reads commit trees)
      --max-commits <MAX_COMMITS>
          How many commits of the root zone `--cochange` looks at [default: 500]
      --no-hooks
          Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help
          Print help
```

############## `vcs zone apply`

```text
Turn one suggestion into a zone (plan only without --yes; history is not rewritten)

Usage: vcs zone apply [OPTIONS] <TARGET>...

Arguments:
  <TARGET>...  Path or proposed name of a suggestion from `vcs zone suggest`; several make all the zones in one step (one status check, one new root commit, one write of the repository state)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --name <NAME>    Zone name to use instead of the proposed one (only with a single target)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --yes            Actually do it. Without this only the plan is printed
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

############### `vcs zone subscribe`

```text
Subscribe to the minimal zone set for a path, symbol, task or person, plus their dependencies

Usage: vcs zone subscribe [OPTIONS]

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
      --for <PATH|SYMBOL>  Pull what is needed for this: a file, a directory, a zone name, or a symbol-ish name (`parse_config`, `net::Client`, `pkg.mod`). Repeatable. Plus the zones they depend on
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --task <TEXT>        Describe a task in words; zones are chosen by offline keyword matching (a proposal)
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --for-owner <USER>   Pull the zones a person relates to (CODEOWNERS entries, and commit authorship)
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --task-assist <CMD>  Ask this command to propose zones for `--task` (JSON on stdin/stdout, see docs). Off unless given
      --explain            Show why each zone is included
      --dry-run            Only print the plan with files and bytes; download no file content and change nothing
      --no-deps            Do not add the zones the selection depends on
      --max-depth <N>      Follow dependencies at most this many hops
      --deep               Also scan source imports (Go, Python, JS/TS), downloading the source of selected zones to do so
      --search-content     Search file contents for definitions of symbol-ish queries (remote: downloads source files)
      --partial            Continue when some `--for` queries match nothing (default: refuse)
  -h, --help               Print help
```

################ `vcs zone describe`

```text
Machine-readable description of zones: purpose, owners, dependencies, subscriptions, locks, history (docs/zone-ontology.md)

Usage: vcs zone describe [OPTIONS] [ZONE]

Arguments:
  [ZONE]  Describe only this zone (display name or id); default: every zone

Options:
  -C <DIR>                           Run as if vcs was started in <DIR> (applied before repository discovery)
      --history <HISTORY>            Recent commits listed per zone (each with its changed files) [default: 3]
      --json                         Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --since <DATE>                 Only commits at or after this date (`YYYY-MM-DD`, Unix seconds, `N days ago`, ...)
      --ca-pem <FILE>                PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --format <FORMAT>              Output size: compact (default), brief, or full (the documented schema) [default: compact] [possible values: compact, full, brief]
      --no-hooks                     Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --transactions <TRANSACTIONS>  Recent operations (local transaction log) listed [default: 3]
      --path <PATH>                  Report which zone owns this repo-relative path (repeatable)
      --fields <KEYS>                Keep only these keys of each zone object (comma separated, e.g. `purpose,history`); `id` and `name` are always kept. Smaller output for agents; the schema's per-zone `required` list then does not apply
  -h, --help                         Print help
```

################# `vcs zone graph`

```text
Zone dependency graph with reasons, transitive closures and path index (docs/zone-ontology.md)

Usage: vcs zone graph [OPTIONS]

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --format <FORMAT>  Output size: compact (default, no `edges`), or full (with edges and reasons) [default: compact] [possible values: compact, full, brief]
      --deep             Also scan Go, Python and JS/TS imports (reads source files; slower)
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

################## `vcs zone explain`

```text
One call: owner zone of a path, its owners, dependencies, dependents and impacted zones (docs/zone-ontology.md)

Usage: vcs zone explain [OPTIONS] <PATH>

Arguments:
  <PATH>  Repo-relative path (file or directory; need not exist)

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --format <FORMAT>  [default: compact] [possible values: compact, full, brief]
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

################### `vcs zone changes`

```text
Per zone, the files that changed between two commits or dates (`A..B`), in one call

Usage: vcs zone changes [OPTIONS] <RANGE>

Arguments:
  <RANGE>  `A..B`: what changed in every zone after A up to and including B. A and B are commit ids (any zone), operation ids, dates, or `HEAD` (B only). `..B` starts from the beginning

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --format <FORMAT>  [default: compact] [possible values: compact, full, brief]
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

## `vcs worktree`

```text
Linked worktrees over one object store (add, list, remove)

Usage: vcs worktree [OPTIONS] <COMMAND>

Commands:
  add     Create a linked worktree sharing this repository's objects and refs
  list    List worktrees and what each has checked out
  remove  Delete a linked worktree (refuses when it has uncommitted work unless --force)
  prune   Forget linked worktrees whose directory no longer exists (never deletes a directory)
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs worktree add`

```text
Create a linked worktree sharing this repository's objects and refs

Usage: vcs worktree add [OPTIONS] <PATH> [BRANCH]

Arguments:
  <PATH>    
  [BRANCH]  Root-zone branch to check out (default: the directory name; created here if new)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs worktree list`

```text
List worktrees and what each has checked out

Usage: vcs worktree list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs worktree remove`

```text
Delete a linked worktree (refuses when it has uncommitted work unless --force)

Usage: vcs worktree remove [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --force          
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs worktree prune`

```text
Forget linked worktrees whose directory no longer exists (never deletes a directory)

Usage: vcs worktree prune [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -n, --dry-run        Only list what would be forgotten
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs snapshot`

```text
Manage snapshots (pinned sets of zone versions)

Usage: vcs snapshot [OPTIONS] <COMMAND>

Commands:
  create    
  show      
  list      
  checkout  Restore every subscribed zone to its pinned commit (on branch snapshot/<name>); refuses dirty zones
  help      Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs snapshot create`

```text
Usage: vcs snapshot create [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  [default: ""]
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks           Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help               Print help
```

#### `vcs snapshot show`

```text
Usage: vcs snapshot show [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs snapshot list`

```text
Usage: vcs snapshot list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs snapshot checkout`

```text
Restore every subscribed zone to its pinned commit (on branch snapshot/<name>); refuses dirty zones

Usage: vcs snapshot checkout [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs remote`

```text
Manage the remote

Usage: vcs remote [OPTIONS] <COMMAND>

Commands:
  set         Set the remote location (path or file:// URL)
  show        Show the configured remote
  remove      Forget the remote and its remote-tracking refs (local branches and history are kept)
  verify      Walk and hash everything reachable from the remote's manifest; exit 1 if anything is missing or corrupt
  backup      Copy the remote into DEST (a directory or other remote), holding a GC safe point; the manifest is written last, after verification
  restore     Restore a remote or backup FROM into a fresh, empty store TO; verified before its manifest is written
  safe-point  Show the last destructive gc (the GC safe point) and any backup holds on the remote
  help        Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs remote set`

```text
Set the remote location (path or file:// URL)

Usage: vcs remote set [OPTIONS] <URL>

Arguments:
  <URL>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs remote show`

```text
Show the configured remote

Usage: vcs remote show [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs remote remove`

```text
Forget the remote and its remote-tracking refs (local branches and history are kept)

Usage: vcs remote remove [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs remote verify`

```text
Walk and hash everything reachable from the remote's manifest; exit 1 if anything is missing or corrupt

Usage: vcs remote verify [OPTIONS] [URL]

Arguments:
  [URL]  Remote or backup to check (default: the configured remote)

Options:
  -C <DIR>                   Run as if vcs was started in <DIR> (applied before repository discovery)
      --manifest <MANIFEST>  Check this saved manifest file instead of the remote's current one (is an old manifest still restorable after gc?)
      --deep                 Also read every pack end to end and every loose object, reachable or not
      --json                 Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>        PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks             Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                 Print help
```

####### `vcs remote backup`

```text
Copy the remote into DEST (a directory or other remote), holding a GC safe point; the manifest is written last, after verification

Usage: vcs remote backup [OPTIONS] <DEST>

Arguments:
  <DEST>  Where the backup goes; an existing backup is updated incrementally

Options:
  -C <DIR>                     Run as if vcs was started in <DIR> (applied before repository discovery)
      --from <FROM>            Remote to back up (default: the configured remote)
      --hold-secs <HOLD_SECS>  Lifetime of the GC safe point in seconds (renewed while the backup runs; a killed backup frees it after this) [default: 3600]
      --json                   Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hold                Do not take a GC safe point (only if nothing runs `vcs gc --remote` meanwhile)
      --no-deep                Skip reading every pack end to end before installing the manifest
      --no-hooks               Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                   Print help
```

######## `vcs remote restore`

```text
Restore a remote or backup FROM into a fresh, empty store TO; verified before its manifest is written

Usage: vcs remote restore [OPTIONS] <FROM> <TO>

Arguments:
  <FROM>  
  <TO>    

Options:
  -C <DIR>                     Run as if vcs was started in <DIR> (applied before repository discovery)
      --hold-secs <HOLD_SECS>  Lifetime of the GC safe point on FROM in seconds [default: 3600]
      --json                   Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --no-hold                Do not take a GC safe point on FROM
      --ca-pem <FILE>          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-deep                Skip reading every pack end to end before installing the manifest
      --no-hooks               Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                   Print help
```

######### `vcs remote safe-point`

```text
Show the last destructive gc (the GC safe point) and any backup holds on the remote

Usage: vcs remote safe-point [OPTIONS] [URL]

Arguments:
  [URL]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs login`

```text
Save a server token for an http:// remote (via the credential helper)

Usage: vcs login [OPTIONS] <REMOTE_URL>

Arguments:
  <REMOTE_URL>  Remote URL (http://host:port)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --token-stdin    Read the token from stdin instead of prompting
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs logout`

```text
Forget the saved token for a remote (default: the current remote)

Usage: vcs logout [OPTIONS] [REMOTE_URL]

Arguments:
  [REMOTE_URL]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs auth`

```text
Show authentication state

Usage: vcs auth [OPTIONS] <COMMAND>

Commands:
  status  Show whether a token is available for the remote and who the server says you are
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs auth status`

```text
Show whether a token is available for the remote and who the server says you are

Usage: vcs auth status [OPTIONS] [REMOTE_URL]

Arguments:
  [REMOTE_URL]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs credential`

```text
Credential helper protocol: key=value lines on stdin/stdout

Usage: vcs credential [OPTIONS] <COMMAND>

Commands:
  get    Print host= and token= for the host read from stdin (nothing if unknown)
  store  Save the host= and token= read from stdin
  erase  Forget the credential for the host read from stdin
  help   Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs credential get`

```text
Print host= and token= for the host read from stdin (nothing if unknown)

Usage: vcs credential get [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs credential store`

```text
Save the host= and token= read from stdin

Usage: vcs credential store [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs credential erase`

```text
Forget the credential for the host read from stdin

Usage: vcs credential erase [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs bundle`

```text
Create or verify an offline bundle of branches and zones

Usage: vcs bundle [OPTIONS] <COMMAND>

Commands:
  create  Write a bundle of branches or zones (default: every branch) to a file
  verify  Check a bundle's header, refs, lengths and prerequisites in about a millisecond (like `git bundle verify`). --full also hashes the whole file and decodes every object
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs bundle create`

```text
Write a bundle of branches or zones (default: every branch) to a file

Usage: vcs bundle create [OPTIONS] <FILE> [SELECTORS]...

Arguments:
  <FILE>          
  [SELECTORS]...  `zone` (all its branches), `zone/branch`, or a root-zone branch name

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --since <SINCE>  Leave out everything reachable from this revision; it becomes a prerequisite
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs bundle verify`

```text
Check a bundle's header, refs, lengths and prerequisites in about a millisecond (like `git bundle verify`). --full also hashes the whole file and decodes every object

Usage: vcs bundle verify [OPTIONS] <FILE>

Arguments:
  <FILE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --full           Hash the whole file, decode every object and check the object graph (what `fetch` and `clone` always do); the default does not read the pack body
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs clone`

```text
Clone a remote (or a bundle file) into a new directory (sparse with --zone)

Usage: vcs clone [OPTIONS] <URL> <DIR>

Arguments:
  <URL>  
  <DIR>  

Options:
  -C <DIR>                   Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>         Only download these zones (repeatable)
      --json                 Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stats                Print the number of remote requests and bytes transferred
      --ca-pem <FILE>        PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --for <PATH|SYMBOL>    Pull what is needed for this: a file, a directory, a zone name, or a symbol-ish name (`parse_config`, `net::Client`, `pkg.mod`). Repeatable. Plus the zones they depend on
      --no-hooks             Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --task <TEXT>          Describe a task in words; zones are chosen by offline keyword matching (a proposal)
      --for-owner <USER>     Pull the zones a person relates to (CODEOWNERS entries, and commit authorship)
      --task-assist <CMD>    Ask this command to propose zones for `--task` (JSON on stdin/stdout, see docs). Off unless given
      --explain              Show why each zone is included
      --dry-run              Only print the plan with files and bytes; download no file content and change nothing
      --no-deps              Do not add the zones the selection depends on
      --max-depth <N>        Follow dependencies at most this many hops
      --deep                 Also scan source imports (Go, Python, JS/TS), downloading the source of selected zones to do so
      --search-content       Search file contents for definitions of symbol-ish queries (remote: downloads source files)
      --partial              Continue when some `--for` queries match nothing (default: refuse)
      --auto                 Choose the zones to download from hints (`.vcs-profile-hints.toml`) and `--auto-profile` files; `--explain` and `--dry-run` work as for `--for`
      --auto-profile <FILE>  A profile (exported `.vcs/profile`) or hints file to predict from; implies --auto. Repeatable
      --budget-zones <N>     Add at most this many zones (default: the hints' max_zones, else 8). Zones already subscribed and the root zone are always kept and do not count
      --budget-bytes <SIZE>  Add at most this many stored bytes of file contents (e.g. 200M, 2G; default: no limit)
  -h, --help                 Print help
```

## `vcs publish`

```text
Publish checked-out branches to the remote in one atomic transaction

Usage: vcs publish [OPTIONS] [BRANCHES]...

Arguments:
  [BRANCHES]...  Publish only these branches (default: every checked-out branch)

Options:
  -C <DIR>
          Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>
          
      --json
          Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --repair
          First re-upload any object the remote lost beneath objects it still has (checks every reachable object)
      --ca-pem <FILE>
          PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --force
          Replace a remote branch that is not an ancestor of yours, but only if it is still the head you last fetched (a lease). Needs the branch name(s)
      --force-with-lease[=<BRANCH[:ID]>]
          Like --force; `=BRANCH` leases that branch, `=BRANCH:ID` leases it against the remote head starting with ID
      --no-hooks
          Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --force-without-lease
          Replace the remote head whatever it is when the publish reads it (still one atomic compare-and-swap, but not checked against what you last fetched)
      --delete <BRANCH>
          Delete this remote branch (only if it is the head you last fetched; repeatable)
      --tags
          Publish only the tags, move no branch (`git push --tags`)
      --prune
          Also delete every remote branch of a subscribed zone that has no local branch (`git push --prune`); each deletion needs the head you last fetched (or your lease) and is audited
      --mirror
          Make the remote match this repo: force every checked-out branch (leased, like --force-with-lease), prune remote branches with no local one, and publish tags (`git push --mirror`)
      --auto-merge[=<N>]
          When the remote branch moved, fetch, merge it in (only if the merge is clean) and publish again, up to N times with a short randomized wait (default 20; env VCS_PUBLISH_AUTOMERGE=N also turns it on)
  -h, --help
          Print help
```

## `vcs lock`

```text
Lock files on the remote so others cannot publish changes to them

Usage: vcs lock [OPTIONS] --lease-secs <LEASE_SECS> [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>                       Run as if vcs was started in <DIR> (applied before repository discovery)
      --lease-secs <LEASE_SECS>  Lease length in seconds (no default: choose per your team's workflow)
      --json                     Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --reason <REASON>          [default: ""]
      --ca-pem <FILE>            PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks                 Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help                     Print help
```

## `vcs unlock`

```text
Release locks you hold (`--break` removes anyone's lock; the server requires an admin token)

Usage: vcs unlock [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
      --break          
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs locks`

```text
Show locks recorded on the remote (as of now; advisory until a validating writer exists)

Usage: vcs locks [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs fetch`

```text
Download remote objects and update remote-tracking refs (local branches untouched)

Usage: vcs fetch [OPTIONS] [SOURCE]

Arguments:
  [SOURCE]  Fetch from this bundle file instead of the remote

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>   
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stats          Print the number of remote requests and bytes transferred
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs prefetch`

```text
Download the file contents of subscribed zones' heads now (to work offline later)

Usage: vcs prefetch [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>   
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stats          Print the number of remote requests and bytes transferred
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs sync`

```text
Fetch, then fast-forward local branches where safe (`--auto`: choose zones from your usage profile)

Usage: vcs sync [OPTIONS]

Options:
      --auto                 Choose which zones to download from your local usage profile (see `vcs profile`); the other zones get commits and trees only and are listed, fetchable with `vcs profile fetch`
  -C <DIR>                   Run as if vcs was started in <DIR> (applied before repository discovery)
      --budget-zones <N>     Add at most this many zones (default: the hints' max_zones, else 8). Zones already subscribed and the root zone are always kept and do not count
      --json                 Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --budget-bytes <SIZE>  Add at most this many stored bytes of file contents (e.g. 200M, 2G; default: no limit)
      --ca-pem <FILE>        PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --auto-profile <FILE>  Seed the prediction with a profile or hints file (repeatable; with --auto)
      --no-hooks             Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --explain              With --auto: show why each zone was chosen or skipped
      --dry-run              With --auto: print the plan, download no file contents, change nothing but zone metadata
      --stats                Print the number of remote requests and bytes transferred
  -h, --help                 Print help
```

## `vcs fsck`

```text
Verify all objects and graph invariants

Usage: vcs fsck [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

## `vcs repack`

```text
Move loose objects into a pack (one pack + index under .vcs/objects/pack). With --format, existing packs are rewritten to that format too

Usage: vcs repack [OPTIONS]

Options:
      --auto             Only if loose objects exceed gc.autoPackLimit, and only those older than the grace period
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --format <FORMAT>  Pack format: v2 (zstd, fanout index; the default for new packs), v1 (rollback for older binaries), v3 (independently compressed blocks, for compacted packs) or v4 (v2 plus delta entries; local only, opt-in)
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-compact       Do not merge small packs afterwards (by default packs of similar size are merged geometrically, with commits and trees first and file content grouped by path)
      --no-hooks         Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help             Print help
```

## `vcs gc`

```text
Garbage collection. DESTRUCTIVE unless --dry-run: permanently deletes unreachable objects older than the grace period (local), or with --remote --yes on the remote

Usage: vcs gc [OPTIONS]

Options:
  -C <DIR>                         Run as if vcs was started in <DIR> (applied before repository discovery)
      --dry-run                    Report what would be deleted; delete nothing
      --grace-secs <GRACE_SECS>    Keep unreachable objects younger than this many seconds (default 14 days). A real sweep refuses values below the touch-on-dedup threshold (3.5 days) [default: 1209600]
      --json                       Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>              PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --remote                     Collect the REMOTE (protocol G1, docs/gc.md) instead of the local store. Needs --yes unless --dry-run. Directory (`path` / `file://`) remotes only: S3 and HTTP lack the touch / object-age / clock operations the protocol needs
      --no-hooks                   Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
      --yes                        Confirm a destructive REMOTE sweep (never needed for --dry-run or local gc)
      --hold-secs <HOLD_SECS>      Remote only: the publishers' maximum hold time in seconds. The grace period must exceed twice this (default 1 hour) [default: 3600]
      --compact                    With --remote: instead of sweeping, merge the remote's many small packs into fewer (tiered; new pack and index first, old ones deleted after; commits and trees first, file content grouped by path). Needs --yes unless --dry-run. See docs/gc.md
      --pack-format <PACK_FORMAT>  With --compact: pack format of the merged pack. v3 (the default) stores independently compressed blocks in path order; v2 or v1 is the rollback that every release reads (also `VCS_PACK_FORMAT=v2`). Packs already written are never rewritten by choosing this
      --block-kib <BLOCK_KIB>      With --compact and v3: uncompressed KiB per block (default 4096). Bigger blocks compress better; reading one object fetches its whole compressed block
  -h, --help                       Print help
```

## `vcs op`

```text
Operation log: inspect, undo, restore (local branches and workspace only)

Usage: vcs op [OPTIONS] <COMMAND>

Commands:
  log      List operations, newest first
  show     Show one operation and its view
  undo     Restore the view before the current operation (recorded as a new operation)
  restore  Restore the view as it was after the given operation (recorded as a new operation)
  help     Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

### `vcs op log`

```text
List operations, newest first

Usage: vcs op log [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -n, --limit <LIMIT>  [default: 20]
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

#### `vcs op show`

```text
Show one operation and its view

Usage: vcs op show [OPTIONS] <ID>

Arguments:
  <ID>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

##### `vcs op undo`

```text
Restore the view before the current operation (recorded as a new operation)

Usage: vcs op undo [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```

###### `vcs op restore`

```text
Restore the view as it was after the given operation (recorded as a new operation)

Usage: vcs op restore [OPTIONS] <ID>

Arguments:
  <ID>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --no-hooks       Do not run client-side hooks (same as VCS_NO_HOOKS=1); see docs/hooks.md
  -h, --help           Print help
```
