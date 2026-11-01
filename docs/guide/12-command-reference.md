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
  init         Create a new repository (default: current directory)
  add          Stage files or directories (also stages deletions)
  restore      Discard working-tree changes (from the index) or unstage them (--staged, from HEAD)
  rm           Remove tracked files from the index and working tree
  mv           Move or rename a tracked file or directory (within one zone)
  commit       Commit staged changes in all subscribed zones atomically
  config       Get, set, list or unset settings (repo .vcs/config, user config, environment)
  status       Show staged, unstaged and untracked changes per zone
  diff         Show unstaged changes (or --staged)
  import-git   Import a `git fast-export` stream (file, or `-` for stdin) into the root zone
  import-p4    Import recorded Perforce output (fixture directory, see docs/import-p4.md) into the root zone. Checked once against real p4d output on a small history; see that document
  export-git   Write a zone branch as a Git fast-import stream (stdout, or --output)
  log          Show commit history of a zone branch
  gen          Generate shell completions or man pages
  branch       List branches of the current zone, or create one (at the checked-out head)
  stash        Set tracked changes aside (push, pop, list, drop); untracked files are left alone
  sparse       Materialize only some paths of a zone (set, add, remove, list, disable); the rest stays tracked, not deleted
  tag          List tags of the current zone, create one (lightweight, or annotated with -m), or delete one with -d
  show         Show a commit (default HEAD) with its diff against the first parent
  blame        Attribute each line of a file to a commit, following first parents
  bisect       Binary-search a zone's history for the commit that introduced a bug
  grep         Search tracked files (working tree, or a revision) for a regular expression
  describe     Name a commit after the nearest tag behind it
  archive      Write the files of a revision as a .tar or .zip archive
  rev-parse    Resolve revisions to commit ids
  ls-files     List tracked files
  cat-file     Show the type, size or content of an object
  shortlog     Summarize history by author
  switch       Switch branch (in the zone containing cwd, or --zone)
  merge        Merge a branch into the checked-out branch (fast-forward when possible)
  cherry-pick  Re-apply the change of one commit onto the checked-out branch
  revert       Create a commit that undoes one commit
  rebase       Replay the checked-out branch onto another branch, keeping each change id
  conflict     Inspect conflicts of an in-progress merge
  resolve      Resolve a conflicted path of a merge or rebase (ours = recorded left side, theirs = right)
  zone         Manage zones
  worktree     Linked worktrees over one object store (add, list, remove)
  snapshot     Manage snapshots (pinned sets of zone versions)
  remote       Manage the remote
  login        Save a server token for an http:// remote (via the credential helper)
  logout       Forget the saved token for a remote (default: the current remote)
  auth         Show authentication state
  credential   Credential helper protocol: key=value lines on stdin/stdout
  bundle       Create or verify an offline bundle of branches and zones
  clone        Clone a remote (or a bundle file) into a new directory (sparse with --zone)
  publish      Publish checked-out branches to the remote in one atomic transaction
  lock         Lock files on the remote so others cannot publish changes to them
  unlock       Release locks you hold (`--break` removes anyone's lock; unrestricted for now)
  locks        Show locks recorded on the remote (as of now; advisory until a validating writer exists)
  fetch        Download remote objects and update remote-tracking refs (local branches untouched)
  sync         Fetch, then fast-forward local branches where safe
  fsck         Verify all objects and graph invariants
  repack       Move loose objects into a pack (one pack + index under .vcs/objects/pack). With --format, existing packs are rewritten to that format too
  gc           Garbage collection. DESTRUCTIVE unless --dry-run: permanently deletes unreachable objects older than the grace period (local), or with --remote --yes on the remote
  op           Operation log: inspect, undo, restore (local branches and workspace only)
  help         Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs add`

```text
Stage files or directories (also stages deletions)

Usage: vcs add [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs commit`

```text
Commit staged changes in all subscribed zones atomically

Usage: vcs commit [OPTIONS]

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  Commit message (omit to write it in core.editor / $EDITOR)
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help               Print help
```

## `vcs config`

```text
Get, set, list or unset settings (repo .vcs/config, user config, environment)

Usage: vcs config [OPTIONS] <COMMAND>

Commands:
  get    Print the effective value of a key (exit 1 if unset)
  set    Set a key in the repository config (or the user config with --user)
  unset  Remove a key from the repository config (or the user config with --user)
  list   List effective settings and where each comes from
  help   Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

#### `vcs config set`

```text
Set a key in the repository config (or the user config with --user)

Usage: vcs config set [OPTIONS] <KEY> <VALUE>

Arguments:
  <KEY>    
  <VALUE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --user           
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

##### `vcs config unset`

```text
Remove a key from the repository config (or the user config with --user)

Usage: vcs config unset [OPTIONS] <KEY>

Arguments:
  <KEY>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --user           
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs status`

```text
Show staged, unstaged and untracked changes per zone

Usage: vcs status [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs diff`

```text
Show unstaged changes (or --staged)

Usage: vcs diff [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --staged         
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stat           Per-file added/deleted line counts instead of the patch
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
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
      --onto-origin      Reference commits that came from import-git by their original Git SHA (target = the origin repo)
  -h, --help             Print help
```

## `vcs log`

```text
Show commit history of a zone branch

Usage: vcs log [OPTIONS] [BRANCH] [-- <PATHS>...]

Arguments:
  [BRANCH]    Branch (default: the checked-out one)
  [PATHS]...  Only commits that change these paths (after `--`)

Options:
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>      Zone (default: the zone containing the current directory)
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --limit <LIMIT>    [default: 20]
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --oneline          One line per commit: short id and subject
      --graph            Draw the commit graph with ASCII lanes
      --author <AUTHOR>  Only commits whose author name or email contains this text (case-insensitive)
      --color <COLOR>    When to colour output [default: auto] [possible values: auto, always, never]
  -h, --help             Print help
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
  -h, --help           Print help
```

## `vcs branch`

```text
List branches of the current zone, or create one (at the checked-out head)

Usage: vcs branch [OPTIONS] [NAME]

Arguments:
  [NAME]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
  -d, --delete         Delete the named branch instead of creating it (refuses the checked-out branch)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --force          With --delete: delete even if its commits are not reachable from another branch or snapshot
  -h, --help           Print help
```

## `vcs stash`

```text
Set tracked changes aside (push, pop, list, drop); untracked files are left alone

Usage: vcs stash [OPTIONS] <COMMAND>

Commands:
  push  Save staged and unstaged changes of tracked files and reset to the branch heads
  pop   Apply a stash entry (staged) and drop it; on conflicts it is kept
  list  List stash entries, newest first
  drop  Forget a stash entry
  help  Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

### `vcs stash push`

```text
Save staged and unstaged changes of tracked files and reset to the branch heads

Usage: vcs stash push [OPTIONS]

Options:
  -C <DIR>                 Run as if vcs was started in <DIR> (applied before repository discovery)
  -m, --message <MESSAGE>  
      --json               Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>      PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

##### `vcs stash list`

```text
List stash entries, newest first

Usage: vcs stash list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

###### `vcs stash drop`

```text
Forget a stash entry

Usage: vcs stash drop [OPTIONS] [INDEX]

Arguments:
  [INDEX]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
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
      --signature <SIGNATURE>  Opaque signature text stored in an annotated tag (not verified)
  -d, --delete                 Delete the named tag
  -h, --help                   Print help
```

## `vcs show`

```text
Show a commit (default HEAD) with its diff against the first parent

Usage: vcs show [OPTIONS] [REV]

Arguments:
  [REV]  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs blame`

```text
Attribute each line of a file to a commit, following first parents

Usage: vcs blame [OPTIONS] <PATH>

Arguments:
  <PATH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --rev <REV>      Revision to blame at (default: HEAD of the file's zone)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs bisect`

```text
Binary-search a zone's history for the commit that introduced a bug

Usage: vcs bisect [OPTIONS] <COMMAND>

Commands:
  start  Start bisecting; optionally give the bad commit and good commits right away
  good   Mark revisions (default: the checked-out commit) as good
  bad    Mark revisions (default: the checked-out commit) as bad
  skip   Mark revisions (default: the checked-out commit) as untestable
  reset  End the bisection and return to the original branch
  run    Run a command at each step: exit 0 = good, 125 = skip, 1-127 = bad, above 127 aborts
  help   Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

######## `vcs bisect run`

```text
Run a command at each step: exit 0 = good, 125 = skip, 1-127 = bad, above 127 aborts

Usage: vcs bisect run [OPTIONS] <CMD>...

Arguments:
  <CMD>...  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -c, --count               Print only the number of matching lines per file
  -F, --fixed               Treat the pattern as a literal string
  -h, --help                Print help
```

## `vcs describe`

```text
Name a commit after the nearest tag behind it

Usage: vcs describe [OPTIONS] [REV]

Arguments:
  [REV]  Revision to describe (default HEAD)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --tags           Use lightweight tags too (default: annotated tags only)
      --always         Show the abbreviated commit id when no tag is found
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --zone <ZONE>    
  -h, --help           Print help
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
  -h, --help           Print help
```

## `vcs cat-file`

```text
Show the type, size or content of an object

Usage: vcs cat-file [OPTIONS] <OBJECT>

Arguments:
  <OBJECT>  An object id (64 hex digits), a revision, or `<rev>:<path>` for a file

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -t                   Print the object's type
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -s                   Print the object's size in bytes
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -p                   Print the content (file bytes; for a commit its header and message)
      --zone <ZONE>    
  -h, --help           Print help
```

## `vcs shortlog`

```text
Summarize history by author

Usage: vcs shortlog [OPTIONS] [BRANCH]

Arguments:
  [BRANCH]  Branch (default: the checked-out one)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
  -s, --summary        Only the number of commits per author
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
  -n, --numbered       Sort by number of commits instead of by name
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -e, --email          Show the author's email
      --zone <ZONE>    
  -h, --help           Print help
```

## `vcs switch`

```text
Switch branch (in the zone containing cwd, or --zone)

Usage: vcs switch [OPTIONS] <BRANCH>

Arguments:
  <BRANCH>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help
          Print help
```

## `vcs cherry-pick`

```text
Re-apply the change of one commit onto the checked-out branch

Usage: vcs cherry-pick [OPTIONS] <REV>

Arguments:
  <REV>  Commit id (or unique prefix), branch name or HEAD

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONE>    Zone (default: the zone containing the current directory)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  create    Create a zone owning a path prefix (zones may not overlap)
  list      
  show      
  branches  
  branch    Create a branch in a zone
  checkout  Check out a branch in a zone
  add       Subscribe to (and materialize) a zone
  remove    Unsubscribe from a zone (removes its files from the workspace)
  rename    Rename a zone (also with history: commits keep the original name as the zone's id)
  delete    Delete a zone definition (refuses a dirty zone; --force drops its branches and snapshot pins)
  help      Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

#### `vcs zone list`

```text
Usage: vcs zone list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs worktree`

```text
Linked worktrees over one object store (add, list, remove)

Usage: vcs worktree [OPTIONS] <COMMAND>

Commands:
  add     Create a linked worktree sharing this repository's objects and refs
  list    List worktrees and what each has checked out
  remove  Delete a linked worktree (refuses when it has uncommitted work unless --force)
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

##### `vcs snapshot list`

```text
Usage: vcs snapshot list [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs remote`

```text
Manage the remote

Usage: vcs remote [OPTIONS] <COMMAND>

Commands:
  set   Set the remote location (path or file:// URL)
  show  Show the configured remote
  help  Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs bundle`

```text
Create or verify an offline bundle of branches and zones

Usage: vcs bundle [OPTIONS] <COMMAND>

Commands:
  create  Write a bundle of branches or zones (default: every branch) to a file
  verify  Check a bundle's hashes, structure and completeness (needs no repository)
  help    Print this message or the help of the given subcommand(s)

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

#### `vcs bundle verify`

```text
Check a bundle's hashes, structure and completeness (needs no repository)

Usage: vcs bundle verify [OPTIONS] <FILE>

Arguments:
  <FILE>  

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>   Only download these zones (repeatable)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --stats          Print the number of remote requests and bytes transferred
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs publish`

```text
Publish checked-out branches to the remote in one atomic transaction

Usage: vcs publish [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --zone <ZONES>   
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --repair         First re-upload any object the remote lost beneath objects it still has (checks every reachable object)
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
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
  -h, --help                     Print help
```

## `vcs unlock`

```text
Release locks you hold (`--break` removes anyone's lock; unrestricted for now)

Usage: vcs unlock [OPTIONS] [PATHS]...

Arguments:
  [PATHS]...  

Options:
      --break          
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
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
  -h, --help           Print help
```

## `vcs sync`

```text
Fetch, then fast-forward local branches where safe

Usage: vcs sync [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs fsck`

```text
Verify all objects and graph invariants

Usage: vcs fsck [OPTIONS]

Options:
  -C <DIR>             Run as if vcs was started in <DIR> (applied before repository discovery)
      --json           Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>  PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help           Print help
```

## `vcs repack`

```text
Move loose objects into a pack (one pack + index under .vcs/objects/pack). With --format, existing packs are rewritten to that format too

Usage: vcs repack [OPTIONS]

Options:
      --auto             Only if loose objects exceed gc.autoPackLimit, and only those older than the grace period
  -C <DIR>               Run as if vcs was started in <DIR> (applied before repository discovery)
      --format <FORMAT>  Pack format: v2 (zstd, fanout index; the default for new packs) or v1 (rollback for older binaries)
      --json             Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>    PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
  -h, --help             Print help
```

## `vcs gc`

```text
Garbage collection. DESTRUCTIVE unless --dry-run: permanently deletes unreachable objects older than the grace period (local), or with --remote --yes on the remote

Usage: vcs gc [OPTIONS]

Options:
  -C <DIR>                       Run as if vcs was started in <DIR> (applied before repository discovery)
      --dry-run                  Report what would be deleted; delete nothing
      --grace-secs <GRACE_SECS>  Keep unreachable objects younger than this many seconds (default 14 days). A real sweep refuses values below the touch-on-dedup threshold (3.5 days) [default: 1209600]
      --json                     Machine-readable output (status, log, diff, branch, zone list, op log, conflict list); see docs/cli.md
      --ca-pem <FILE>            PEM file with the CA certificate(s) to trust for an https:// remote (same as config `remote.cainfo`)
      --remote                   Collect the REMOTE (protocol G1, docs/gc.md) instead of the local store. Needs --yes unless --dry-run. Directory (`path` / `file://`) remotes only: S3 and HTTP lack the touch / object-age / clock operations the protocol needs
      --yes                      Confirm a destructive REMOTE sweep (never needed for --dry-run or local gc)
      --hold-secs <HOLD_SECS>    Remote only: the publishers' maximum hold time in seconds. The grace period must exceed twice this (default 1 hour) [default: 3600]
  -h, --help                     Print help
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
  -h, --help           Print help
```
