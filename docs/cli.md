# CLI reference: machine-readable output

Read commands accept a global `--json` flag for tooling and scripts.

Rules:

- stdout is exactly one JSON object on one line, nothing else. Human text is never mixed in.
- Errors are written to stderr as `error: ...` with a nonzero exit status and empty stdout.
- Every document starts with `"version"` (integer, currently `1`) and `"command"`.
  `version` is bumped only when an existing field is removed or changes meaning;
  new fields may be added without a bump, so parsers must ignore unknown keys.
- Commands without a JSON form fail with `--json is not supported for this command`.
- Object ids are full hex strings. Timestamps are Unix seconds. A missing value is `null`.

## `vcs status --json`

```json
{"version":1,"command":"status",
 "zones":[{"zone":"root","zone_id":"root","path":"","branch":"main","head":"<id>|null",
           "staged":[{"path":"a.txt","kind":"added|modified|deleted"}],
           "unstaged":[{"path":"a.txt","kind":"modified"}]}],
 "untracked":["x"],"unsubscribed":["zone"],"unsupported":["link"],
 "merging":null,"rebasing":false,"clean":true}
```

`merging` is `null` or `{"zone","branch","theirs","conflicts":[<conflict>]}`.

## `vcs log --json [--zone Z] [-n N] [branch]`

```json
{"version":1,"command":"log","zone":"root",
 "commits":[{"id":"<id>","change_id":"<id>","zone":"root",
             "author":{"name":"","email":"","timestamp":0},"message":""}]}
```

## `vcs diff --json [--staged]` (also `vcs diff --stat`)

`--json` on `diff` always gives the per-file stat (the patch itself stays text-only).
`vcs diff --stat` prints the same numbers for humans.

```json
{"version":1,"command":"diff","staged":false,
 "files":[{"path":"a.txt","added":2,"deleted":1,"binary":false}],
 "added":2,"deleted":1}
```

## `vcs branch --json [--zone Z]`

Listing only; combining with a branch name is an error.

```json
{"version":1,"command":"branch","zone":"root",
 "branches":[{"name":"main","head":"<id>|null","current":true}]}
```

## `vcs zone list --json`

```json
{"version":1,"command":"zone-list",
 "zones":[{"name":"root","id":"root","path":"","subscribed":true,"branch":"main|null","branch_count":1}]}
```

## `vcs op log --json [-n N]`

```json
{"version":1,"command":"op-log",
 "operations":[{"id":"<id>","current":true,"description":"",
                "author":{"name":"","email":"","timestamp":0}}]}
```

## `vcs conflict list --json`

Fails (stderr, nonzero) when no merge is in progress.

```json
{"version":1,"command":"conflict-list",
 "conflicts":[{"path":"f.txt","kind":"add/add|modify/modify|delete/modify|mode|file/directory","resolved":false}]}
```

Golden tests for every schema: `crates/vcs-cli/tests/json.rs`.

## Tags, show and blame

All three work on one zone at a time, because commits belong to exactly one zone. The zone is
`--zone`, or the zone containing the current directory (for `blame`, the zone owning the path).

## Revisions

Wherever a `<rev>` is accepted it can be `HEAD` (the zone's checked-out branch), a tag, a local
branch, or a hex commit-id prefix of at least 4 digits that is unambiguous among the zone's
commits reachable from its branches and tags. Any of these may be followed by `~N` (N first
parents back) and/or `^` (one first parent back). Tags win over branches of the same name.

## `vcs tag`

```
vcs tag                               list tags of the zone
vcs tag <name> [<rev>]                lightweight tag: a ref straight to a commit
vcs tag <name> [<rev>] -m <message> [--signature <text>]
                                      annotated tag: a stored tag object
vcs tag -d <name>                     delete
```

Tags are per zone (`engine/v1` and `root/v1` are different tags). Names follow branch-name rules.
Creating a tag whose name exists, tagging an unknown revision or a zone with no commits, and
deleting a missing tag are refused and change nothing.

An annotated tag is an object of kind `Tag` (7): zone, name, target commit, tagger, message, and
a `signature` byte string (at most 4096 bytes). The signature is stored verbatim and is **not**
verified; there is no key handling yet. Tag refs live in the `refs` file in a trailing section that
is only written when at least one tag exists, so repositories without tags keep their bytes.
Tag refs and the objects they name are garbage-collection roots and are checked by `fsck`.

Every tag create and delete is recorded in the operation log (`vcs op log`). The operation view
(branch heads and subscriptions) does not contain tags, so `vcs op undo` does not move them.
Tags are not published to or fetched from the remote yet.

Deleting a zone with tags needs `--force`, like branches and snapshot pins.

## `vcs show [<rev>]`

Prints the commit (id, change id, zone, parents, author, date, message) and a unified diff against
the first parent; a root commit is diffed against the empty tree. For an annotated tag the tag's
tagger and message come first. Paths in the diff are repository-relative. A mode-only change is
reported as `mode changed`.

## `vcs blame <path> [--rev <rev>]`

Attributes every line of a text file to the commit that introduced it, walking only **first
parents** from `<rev>` (default: the zone's checked-out head). Each line is aligned with the same
line diff used by `vcs diff`; a line that is unchanged between a commit and its first parent is
passed down to the parent, otherwise it belongs to that commit. A line that arrived through the
second parent of a merge is therefore attributed to the merge commit. Output rows are
`<short commit id> (<line no>) <text>`. Binary files, directories and paths missing at the
revision are refused.
# Log options, completions and man pages

## `vcs log`

```
vcs log [--zone Z] [-n N] [--oneline] [--graph] [--author TEXT] [--color auto|always|never] [BRANCH] [-- PATH...]
```

| Option | Effect |
|---|---|
| `-n N` | At most N commits (default 20). Applied after `--author` and path filtering. |
| `--oneline` | `<short id> <subject>` per commit. |
| `--graph` | ASCII lanes (`*`, `\|`, `\`, `/`). Commits are ordered children before parents. |
| `--author TEXT` | Keep commits whose author name or email contains TEXT (case-insensitive). |
| `-- PATH...` | Keep commits whose content under any PATH differs from every parent (a root commit counts if it contains any). Paths are relative to the current directory. |
| `--color` | `auto` colours only when stdout is a terminal and `NO_COLOR` is unset or empty. |

Notes and limits:

* With `--graph` and a filter, only edges between shown commits are drawn; skipped commits are not
  bridged (git's history rewriting is not implemented).
* Filtering walks the zone's whole first-reachable history and flattens each commit's tree, so it is
  linear in history size times tree size (see `docs/research.md` for acceleration ideas).
* A path filter looks at one zone: the zone containing the current directory, or `--zone`.

## Shell completions and man pages

```
vcs gen completions bash > ~/.local/share/bash-completion/completions/vcs
vcs gen completions zsh  > "${fpath[1]}/_vcs"
vcs gen completions fish > ~/.config/fish/completions/vcs.fish
vcs gen man                       # vcs(1) on stdout
vcs gen man --dir ./man           # vcs.1 plus one vcs-<command>.1 per command
```

Both are generated from the same clap definition as `--help`, so they cannot drift from the binary.
Completions also support `elvish` and `powershell`.

# Everyday commands

## `vcs bisect`

Binary search over one zone's first-parent history for the commit that introduced a problem.

```
vcs bisect start [<bad> [<good>...]] [--zone Z]
vcs bisect good|bad|skip [<rev>...]     # default: the commit currently checked out
vcs bisect run <cmd> [args...]          # a single argument is run with `sh -c`
vcs bisect reset
```

- Candidates are checked out on the branch `bisect/work` of the zone, with the same safety as
  `vcs switch`: the command refuses while the zone has uncommitted changes or a merge in
  progress. `reset` switches back to the original branch and deletes `bisect/work`.
- State is `.vcs/bisect`, replaced atomically after every mark, so an interrupted run resumes
  with the next `good`/`bad`/`skip` (or `bisect run` again).
- Only the first-parent chain of the first bad commit is searched; a good commit must lie on it.
  Merged-in side branches are not searched.
- `run` exit codes follow git: 0 good, 125 skip, 1-127 (other than 125) bad, above 127 aborts
  and leaves the bisection in place. If only skipped commits remain, the candidates for the
  first bad commit are listed.

## `vcs grep [-n -i -w -l -c -F] <pattern> [<rev>] [-- <paths>...]`

Search tracked files with a regular expression (Rust `regex` syntax; `-F` for a literal).
Without `<rev>` the tracked files of the working tree are read from disk (unstaged edits are
searched, untracked and ignored files never are). With `<rev>` the files of that revision are
read from the object store, in every subscribed zone where it resolves; chunked large files are
reassembled. Binary files (a NUL in the first 8000 bytes) and symlinks are skipped. Paths are
repository-relative and sorted; `--` limits the search to files or directories. Files are
searched in parallel. Output is `path:text`, `path:line:text` with `-n`, file names with `-l`,
`path:count` with `-c`. Exit status 1 when nothing matches.

## `vcs describe [<rev>] [--tags] [--always] [--zone Z]`

Name a commit after the nearest tag behind it: `v1` when the commit is tagged, else
`v1-<n>-g<short id>` where `n` counts the commits reachable from the revision but not from the
tag. Annotated tags only unless `--tags`; `--always` falls back to the short id instead of
failing. The nearest tag is the first one met walking parents breadth-first.

## `vcs archive [<rev>] -o <file.tar|file.zip> [--prefix DIR/] [--format tar|zip]`

Write the files of a revision (every subscribed zone in which it resolves) as an archive. The
format follows the extension, or `--format` (needed when writing to stdout). Both writers are
built in: tar is POSIX ustar (paths up to 255 bytes, link targets up to 100), zip uses the
stored method without zip64. Symlinks stay symlinks, executable files get mode 0755 and others
0644. Output is reproducible: entries are in path order, owner 0, and every mtime is the newest
commit time of the revision, so the same revision always gives the same bytes.

## Plumbing: `rev-parse`, `ls-files`, `cat-file`, `shortlog`

All four accept `--json` (one line, `"version": 1`, `"command": <name>`).

- `vcs rev-parse [--short] [--zone Z] <rev>...` prints commit ids; `--show-toplevel` prints the
  working tree root. JSON: `{"zone", "ids": [...]}` (or `{"toplevel"}`).
- `vcs ls-files [-s] [--rev R] [<paths>...]` lists tracked files (or those of a revision);
  `-s` adds `<mode> <id>` (100644, 100755, 120000). JSON: `{"files": [{"path", "mode", "id"}]}`.
- `vcs cat-file (-t|-s|-p) <object>` where the object is a 64-digit id, a revision (a commit), or
  `<rev>:<path>` (a file). `-p` prints file bytes, or a commit's zone, tree, parents, author and
  message. JSON: `{"id", "type", "size", "content"}` (content with `-p`, text only).
- `vcs shortlog [-s] [-n] [-e] [<branch>] [--zone Z]` groups the zone's reachable history by
  author name (`-e`: name and email). JSON: `{"zone", "authors": [{"author", "count", "subjects"}]}`.
