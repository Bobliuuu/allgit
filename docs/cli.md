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
