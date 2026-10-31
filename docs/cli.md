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
