# Zone auto-detection and minimal zone sets

Status: **first version, implemented** (`vcs-repo::zonedetect`, `vcs-repo::zonesplit`, `vcs-cli::autozone`).
Everything is opt-in, deterministic and local. There is no network call to any model in core. Nothing is
reshaped, moved or deleted unless you run an explicit command with `--yes`.

Two jobs:

1. **Find zone boundaries** in an existing repository: `vcs zone suggest`, then (optionally) `vcs zone apply`.
2. **Pull only the zones a task needs**: `vcs clone --for ...` and `vcs zone subscribe --for ...`, for an agent
   (AI or script) that needs a few files, or a person who wants what relates to them.

## `vcs zone suggest [--json]`

Read-only. Looks at the committed files of the root zone (the part not yet in a zone) and proposes path prefixes.

| Signal | What is read | Weight |
|---|---|---|
| Cargo workspace member | root `Cargo.toml` `[workspace] members` (globs like `crates/*`) | 0.85 |
| npm/yarn workspaces entry | `package.json` `"workspaces"` (array or `{packages}`) | 0.85 |
| Go module | a `go.mod` below the root | 0.80 |
| Python project | a `pyproject.toml` below the root | 0.70 |
| standalone Cargo package / package.json | manifest not listed by a workspace | 0.60 |
| Maven / Gradle module | `pom.xml`, `build.gradle(.kts)` | 0.60 |
| Bazel package | `BUILD`, `BUILD.bazel` | 0.50 |
| CODEOWNERS | a rule naming a plain directory (`/web/ @alice`) in `CODEOWNERS`, `.github/CODEOWNERS`, `docs/CODEOWNERS` | 0.50 |
| directory structure | a top-level directory, or a child of `crates/ packages/ apps/ services/ libs/ modules/ ...` | 0.30 |

Signals on the same path combine as `1 - prod(1 - w)`. Zones may not overlap, so overlapping candidates are
resolved by confidence (shorter path wins ties) and the dropped ones are listed in `notes`. Candidates under
`--min-files` (3) are dropped, and ones below `--min-confidence` (0.4) are hidden (counted in
`hidden_low_confidence`), so a plain `docs/` directory is not proposed unless you ask for it.
Dot-directories, `node_modules`, `target`, `vendor`, `dist`, `build` are never candidates.

With `--cochange [--max-commits N]` (default 500) the first-parent history of the root zone is read and, for
each pair of proposed zones, the share of commits touching both is reported (`jaccard_pct`; pairs with at least
3 shared commits and at least 30% are listed, groups with at least 50% are `clusters`). **Co-change only
annotates**: it never creates a zone, because a zone is one path prefix and files that change together can be in
unrelated directories. It reads commit trees, so it costs time on a long history.

Also reported: `depends_on` per proposal and `cycles` (proposed zones that depend on each other; consider keeping
them in one zone), `root_files_remaining`, and `notes` (unreadable manifests are listed, never guessed).

## `vcs zone apply <path|name> [--name N] [--yes]`

`apply` adopts one *current* suggestion (a path or proposed name from `suggest`; anything else is refused).
Without `--yes` it only prints the plan.

**Existing history is not rewritten, and nothing is copied.** `vcs zone create` already refuses a path that has
committed files ("tracked files exist there"), so there was no migration of populated directories before. `apply`
uses a new `Repo::zone_split` that makes exactly two commits:

* the root zone's checked-out branch gets one commit whose tree is the old tree minus the directory (parent = old
  head, so all earlier history, including the directory's, stays reachable in the root zone);
* the new zone's `main` gets a root commit holding the directory's tree. Trees and blobs are shared with the old
  commit, so no file data is copied or re-hashed.

Consequences, stated plainly:

* The new zone's history **starts at the split**. `vcs log --zone <z>` shows one commit; `blame` there does not
  follow into the root zone's older commits. Old history is still in the root zone.
* The working tree is not touched (files stay where they are).
* It refuses: a dirty root zone, a merge or rebase in progress, a root zone with a sparse set, a root zone with no
  commits, a path with no committed files, a path overlapping another zone.
* Collaborators: once you `publish`, a plain `vcs sync` fast-forwards their root zone, which removes the directory
  from their working tree until they `vcs zone add <name>` (and `vcs fetch --zone <name>`). Coordinate the split.
* Crash behaviour: the zone definition and workspace are written, then the refs (the linearization point). If the
  process dies in between, the new zone is subscribed but empty, its files show as staged additions of that zone and
  `vcs commit` reaches the intended state. Not fault-injected.
* `vcs op undo` of a split is **not supported** and not tested (like deleting a zone, the zone definition is not part
  of an operation view). `vcs zone delete --force` puts the paths back under the root zone's *future* commits only
  if you then re-add them; do not rely on it as an undo.

## Selecting a minimal zone set: `--for`, `--task`, `--for-owner`

```
vcs clone <url> <dir> --for <query>... [--task TEXT] [--for-owner USER] [--explain] [--dry-run] [--deep]
vcs zone subscribe     --for <query>... [--task TEXT] [--for-owner USER] [--explain] [--dry-run] [--deep]
```

`clone --for` cannot be combined with `--zone`. `zone subscribe` only adds subscriptions (fetching the zone first,
then `zone add`); it never unsubscribes or deletes anything and skips zones already subscribed.

How a query is resolved (first hit wins, per query):

1. an exact tracked file, or a directory (all zones holding files under it);
2. a zone name, display name, or zone path;
3. a symbol-ish name: file stem or module path (`parse_config`, `net::Client`, `pkg.mod`, `pkg/mod`);
4. a definition (`fn x`, `def x`, `class x`, `func x`, `struct x`, ...) in a code file, **only** when file content is
   available: always for `zone subscribe` on a local repository, and for remote selection only with
   `--search-content` (which downloads every code file to look; expensive, off by default).

A query that matches nothing is an error listing what failed. It is never widened to "everything"; `--partial`
continues with what resolved.

* `--task "text"`: offline keyword matching of the words in the text (stop words removed, camelCase split) against
  zone names, zone paths and file names. A *proposal*; the reasons name the matching words. Refused when nothing
  matches. Optional `--task-assist <cmd>` runs a command you name (30 s timeout) with
  `{"task":"...","zones":[{"name":"...","path":"..."}]}` on stdin and expects `{"zones":["name",...]}` on stdout; every
  name is validated against real zones (an unknown name is refused). It is off unless given. **The existing
  `vcs-assist-claude` binary speaks the merge-assist protocol, not this one, so it is not wired to this option**;
  any script can implement it.
* `--for-owner USER`: zones containing files whose CODEOWNERS rule (last matching rule wins) lists `USER`
  (`@user`, `user`, or an email), plus zones where `USER` authored at least 3 and at least 20% of the last 300
  first-parent commits (match by author name, email or email local part). Both are heuristics; authorship by a
  shared name or an unusual email is missed.
* **Dependency closure** (on by default, `--no-deps` to disable, `--max-depth N`): zones the selected zones need.
  From manifests: Cargo `path` dependencies and `workspace = true` through `[workspace.dependencies]`; package.json
  dependencies naming another package of the repository (and `file:` paths); go.mod `require` of another module
  of the repository and local `replace`; pyproject dependency names of another project and `path` sources.
  `--deep` additionally scans imports: Go (resolved through the module paths of go.mod files), Python
  (`import a` / `from a import` to a unique top-level package or module), JS/TS (relative paths and workspace
  package names). Not scanned: Rust `use`, Java/Kotlin/C/C++ includes, Bazel `deps`, Maven/Gradle dependencies.
  Cycles terminate; both zones are included.
* `--explain` prints, per zone, why it is included (which query matched, which manifest line or import pulled it
  in). `--json` always includes the reasons.
* `--dry-run`: prints files and bytes to be fetched, downloads no file content, creates and changes nothing.

### What the numbers are

A remote selection reads commits and trees (and the few manifest files) into a **throwaway** store to plan; the
clone then runs normally, so metadata for the selected zones is fetched twice. `planning_cost` reports the requests
and bytes spent planning; `clone --stats` totals both. Sizes come from the remote's pack indexes (the stored entry
length, `bytes_kind: "stored_estimate"`); loose remote objects count as 0 and are noted; for a local repository
sizes are content sizes (`"content"`). Treat bytes as an estimate of the download, not a promise (see the
measurement below for how close it was).

When every query can be mapped to a zone from the remote manifest alone (a zone name, or a path inside a zone or
with a `/`), only the root zone and the selected zones are loaded, and dependency zones as they are discovered. The
rest of the repository is then **not measured**: `files_all`/`bytes_all` are `null` and unselected zones show
`null` sizes. Symbol, `--task`, `--for-owner`, `--search-content` and `--deep` queries load every zone's trees.

## Machine contract (`--json`)

One JSON object on one line on stdout; errors go to stderr with a non-zero exit status and no JSON.
`version` is 1; fields may be added, never removed or redefined without a version bump. Percentages are integers
0..100 (there are no floats).

`vcs --json zone suggest`

```
{"version":1,"command":"zone suggest","history_rewritten":false,
 "total_files":N,"root_files_remaining":N,"hidden_low_confidence":N,
 "zones":[{"name","path","files","bytes":N|null,"confidence_pct",
           "signals":[{"kind","detail","weight_pct"}],"depends_on":[name],"coupled_with":[name]}],
 "cycles":[[name]],"coupling":[{"a","b","commits_both","commits_either","jaccard_pct"}],"clusters":[[name]],"notes":[str]}
```

`vcs --json clone ... --for ...` (`"command":"clone"`) and `vcs --json zone subscribe ...` (`"zone subscribe"`)

```
{"version":1,"command":"clone","dry_run":bool,"source":"remote"|"local","bytes_kind":"stored_estimate"|"content",
 "zones":[{"name","id","path","direct":bool,"depth":N,"reasons":[str],"files":N|null,"bytes":N|null}],
 "not_selected":[{"name","id","path","files":N|null,"bytes":N|null}],
 "totals":{"files_selected","bytes_selected","bytes_complete":bool,"files_all":N|null,"bytes_all":N|null},
 "unresolved":[{"query","why"}],"notes":[str],"planning_cost":{"requests","bytes"}}
```

`vcs --json zone apply <target> [--yes]` -> `{"version":1,"command":"zone apply","applied":bool,"zone","path","files",
"root_commit":id|null,"zone_commit":id|null,"history_rewritten":false}`.

Without `--dry-run`, `clone` and `zone subscribe` print the plan document and then do the work (so `--json` output
is the plan that was executed; a failure after that is an error on stderr). An agent should call with `--dry-run`
first when it wants to decide, then re-run without it. `zones[].id` is what `vcs zone add` and `vcs fetch --zone`
accept.

## Measurement

`scripts/bench_zones_autodetect.sh [files] [runs]` generates the monorepo, runs suggest and apply on it, publishes
to a directory remote and alternates full clone and `clone --for` (see the script header). Results of the run made
for this document are in the pull request description; they are byte counts from `CountingBackend`, which are
deterministic, plus wall times that are only meaningful with the load noted there.

## Limits and not done

* Manifest parsers are small and tolerant, not full TOML/Starlark/XML parsers: multi-line inline tables in Cargo
  dependencies, Cargo `[patch]`, `go.work`, Poetry groups, Maven/Gradle dependencies and Bazel `deps` are not read.
* Rust imports and C-family includes are not scanned; a Rust crate that depends on another one only through a
  `path` dependency in a manifest is found, one that is wired another way is not.
* `zone apply` is one directory at a time and each call walks the whole root tree (about 170 ms per call at 4,000
  files; see the benchmark for 100,000). There is no batch apply and no preview of what collaborators will see.
* Zone history after a split starts at the split (no history migration).
* Task mapping is keyword matching; a task whose words appear in no zone or file name is refused.
* The planning pass fetches the commits and trees of the selected zones twice (once to plan, once for the clone).
* Selection cannot name a branch other than `main` (a zone without `main` uses its first branch, with a note).
