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
