# Zone ontology: a machine-readable map of the repository

Status: **implemented** (`crates/vcs-cli/src/ontology.rs`, tests in `crates/vcs-cli/tests/ontology.rs`).

The point of vcs is that a repository is *zonal*: it is a set of zones (path prefixes with their own
history, branches, subscriptions and locks). The ontology exposes that structure in one JSON document an
agent can navigate without reading raw objects or guessing from directory names.

Two read-only commands:

| Command | Answers |
|---|---|
| `vcs zone describe [ZONE] [--history N] [--transactions N] [--path P]... [--fields K,..] --json` | what each zone is, who owns it, what it depends on, how it is subscribed, what changed recently |
| `vcs zone graph [--deep] --json` | the dependency graph: edges with reasons, direct and transitive closures both ways, cycles, path index |

`--fields purpose,history` keeps only those keys per zone (plus `id`, `name`) to cut output for agents; the schema's per-zone `required` list applies to unfiltered output.

### Output formats (`--format`)

`compact` is the default for every ontology command; `full` is the documented schema in `zone-ontology.schema.json`; `brief` is `compact` with commit entries
reduced to id, author, time and subject. Compact drops: null, empty-list and false-by-default keys (absent means null/empty/false), the redundant `id` when it equals `name`/`zone`,
`change_id`, `parents`, author e-mails, dependency `reasons`, operation `branch_moves` and (for `zone graph`) `edges`, and it shortens 64-hex ids to 12 (a unique prefix every command accepts).
Measured on the benchmark fixture (9 zones): `zone describe` 26.9 KB full, 13.4 KB compact (before the history/transaction defaults went from 5/10 to 3/3); `zone graph` 3.4 KB full, 1.6 KB compact.

### More commands

* `vcs zone explain PATH [--json]`: one call for a path: owner zone and zone-relative path, whether it exists in the head tree, purpose, CODEOWNERS of the path, direct and transitive dependencies and dependents of the zone, `impacted` (owner plus every transitive dependent) and the last commit touching the path.
* `vcs zone changes A..B [--json]`: per zone, the files that changed between two repository states, in one call. A and B are commit ids of any zone, operation ids, dates, or `HEAD` (B); `..B` starts at the beginning. A commit names the state of the whole repository right after the first operation (transaction) that made it a branch head, so the comparison does not depend on commit timestamps; a date selects the newest operation not newer than it. Each zone is compared at its checked-out branch; files are the net difference (`added`/`modified`/`deleted`, repo-relative paths) with a commit count.
* `vcs log --all-zones` (existing) now also takes `-G REGEX` (a commit that adds or removes a line matching the regex, like `git log -G`) next to `-S`, `--grep`, `--author`, `--since` and `-- paths`: one call searches every zone.
* `zone describe --since DATE` and `--limit N` (alias of `--history`).

Without `--json` both print a short human summary. Schema: [`zone-ontology.schema.json`](zone-ontology.schema.json)
(`$defs.describe`, `$defs.graph`; id `vcs.zone-ontology/1`). Rules are those of [cli.md](cli.md): one JSON object on
one line, `version` is bumped only when a field disappears or changes meaning, parsers ignore unknown keys,
`null` means the repository does not say.

## Nothing is invented

The ontology stores nothing. Every field is computed when you ask, from state vcs already has:

| Field | Derived from |
|---|---|
| `id`, `name`, `path`, `is_root` | the `zones` file (name is the display name, id is permanent) |
| `path_ownership` | zone prefixes; the root zone owns the rest, `excluded_prefixes` lists what it gave away; `tracked_files` counts files in the head tree |
| `owner_of` (`--path`) | the same longest-prefix rule `add`/`commit` use |
| `kind` | which manifest sits at the zone root: `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `pom.xml` |
| `purpose` | first of: `description` in `Cargo.toml` / `pyproject.toml` / `package.json`, then the first paragraph line (else heading) of `README.md`. `{text, source}`; **`null` when the zone says nothing about itself**. No summarizing, no model |
| `owners.codeowners` | last matching rule of the repository's `CODEOWNERS` (`CODEOWNERS`, `.github/CODEOWNERS`, `docs/CODEOWNERS`), read from the committed head trees |
| `owners.top_authors` | commit authorship over the newest 200 commits of the zone's branch |
| `dependencies` | the same analysis as `vcs zone subscribe --for`: Cargo path/workspace deps, package.json deps, go.mod `require`/`replace`, pyproject deps; `graph --deep` also scans Go/Python/JS/TS imports. Each edge carries human-readable `reasons`. Unreadable manifests are listed in `notes`, never guessed |
| `subscription` | the workspace file: subscribed or not, checked-out branch, sparse paths (`vcs sparse`) |
| `branches` | refs |
| `history` | the zone's branch head, a commit count (capped at 1000, `capped: true` then), and the newest `--history N` (alias `--limit`, default 3) commits, optionally only `--since DATE` each with its changed files (zone-relative paths, `added`/`modified`/`deleted`, first parent diff, capped at 100 files per commit) |
| `locks.held_locally` | the locks this checkout believes it holds (`lockable-held`). Remote lock state needs a network call, so it is not included: use `vcs locks` |
| `transactions` | the operation log: each entry lists the zones whose branch heads it moved (`branch_moves` with from/to), `zones_touched`, and subscription changes. A multi-zone `vcs commit` is one entry touching several zones, which is how "which zones does this change touch" is answered |

## How an agent should use it

1. `vcs zone graph --json`: the whole map in one call (zone names, paths, dependency closure, `path_index`).
2. `vcs zone describe --path <file> --json`: which zone owns a file, plus everything about every zone.
3. `vcs zone describe <zone> --history 20 --json`: the recent commits of one zone with changed files.
4. To build target T: `graph.nodes[T].depends_on_transitive` is the set of zones to subscribe (`vcs zone subscribe --for T` does it).
5. To judge a merge: zones with no path in common and no edge between them cannot interfere.

## Limits

* Dependencies come from manifests (and, with `--deep`, import statements). Dynamic or build-system-only
  dependencies are not seen.
* `purpose` is only as good as the README/manifest; an undocumented zone has `purpose: null`.
* Zones whose head commit or trees are not present locally (lazy clones) are reported in `notes` and have empty content-derived fields.
* Remote locks and published (remote) transactions are not read; only the local operation log is.
* Computing the document flattens every zone head tree, so cost grows with repository size. It is a
  navigation aid, not a hot-path command.
