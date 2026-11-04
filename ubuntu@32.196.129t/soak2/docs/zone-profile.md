# Usage profile and automatic zone selection

Status: **first version, implemented** (`vcs-repo::zoneprofile`, `vcs-cli::zoneprofile_cmd`). Measurements and
their limits: [research/zone-profile-bench.md](research/zone-profile-bench.md).

`vcs sync --auto`, `vcs pull --auto` and `vcs clone --auto` / `--auto-profile FILE` choose which zones to
download from a **local profile of what you work on**, instead of "all zones" or a hand-written `--zone` list.
Chosen ("hot") zones are subscribed and their file contents downloaded first. Every other zone gets commits and
trees only (metadata), is **listed**, and can be fetched with one command. Nothing is dropped silently.

Everything runs on your machine. There is no model, no telemetry and no upload.

## The profile (`.vcs/profile`)

A text file, one line per event: `time-ms TAB kind TAB zone TAB directory`. Kinds: `r` read, `e` edit, `b` build.

What is recorded, automatically, after a command succeeds:

| Command | Event |
|---|---|
| `add`, `rm`, `mv`, `restore` | edit of the zone that owns each path (directory only) |
| `commit` | edit of every zone whose branch head moved |
| `status`, `diff`, `log` | read of the zone you are standing in |
| `zone checkout`, `zone add` | read of that zone |
| `vcs profile record [--path P] -- CMD ...` | build in the current zone, plus any word of `CMD` that names an existing path |
| `vcs profile note PATH...` | read (a hint for editors or build tools to call) |
| `vcs profile fetch ZONE` | read |

Privacy, by construction: the file holds a time, a kind, a **zone id** and a **directory prefix of at most 3
components**. It never holds file names, file contents, command lines, user names or the remote URL
(`privacy_dir` drops the file name and deeper components; there is a test that a secret file name never reaches the
file). It is not read by `publish`, `fetch`, `clone` or the server, and it is never sent anywhere. It is capped at
about 2000 events (the file is rewritten to the newest events past 128 KiB).

Controls:

* `vcs profile show`: where the file is, whether recording is on, counts per zone.
* `vcs profile off`: stop recording **and erase** the recorded events. `--auto` then uses hints only.
  `vcs profile on` resumes. `vcs profile clear` erases and keeps recording.
* `VCS_PROFILE=off` in the environment disables recording for that process.
* `--auto-profile FILE` imports another profile (for example an exported `.vcs/profile` of a teammate with your
  role) into this one, or reads a hints file; see below.

Recording costs one small `open(append)+write` per recorded command. Measured on the benchmark box:
see the research note (status with and without recording).

## The predictor

For every zone: `score = direct + dependencies + co-access + hint`.

* **direct**: sum over events of `weight(kind) * 2^(-age / half-life)` with weights read 1, build 1.5, edit 3 and a
  14-day half-life, divided by the largest zone's value (so 0..1). Frequency and recency both count.
* **dependencies**: from every used (or hinted, or pinned) zone, walk zone dependency edges up to 2 hops and give
  each reached zone `source_score * 0.3^hops`. Edges come from the manifests the zone graph already reads
  (`Cargo.toml` path dependencies, `package.json`, `go.mod`, `pyproject.toml`; see [zones-autodetect.md](zones-autodetect.md))
  plus `[deps]` in the hints. Only manifests of zones that can matter are downloaded (the used zones, then what they
  depend on), not all of them.
* **co-access**: events less than 30 minutes apart form a session; a zone that shared sessions with a used zone gets
  `0.4 * source_score * P(zone used | source used)` (smoothed by one pseudo-session).
* **hint**: `0.5` for each zone named by your `role` in the hints file.

Ties break on zone name, so the same inputs always give the same ranking.

Not done: co-change from commit history. A zone has its own commit chain, so "files that change together" across
zones is not visible in one history; co-access in your own sessions is used instead.

## Choosing and fetching

`--auto` fetches commits and trees of every remote zone first (small compared with file contents), reads the few
manifests it needs, ranks, then:

1. Always kept, never counted against the budget: the root zone and zones you already subscribe to.
2. Pinned zones (hints), then zones in rank order with `score >= 0.05`, until the budget is used:
   `--budget-zones N` (default: the hints' `max_zones`, else **8** new zones) and `--budget-bytes SIZE` (stored
   bytes of the zone's head; default unlimited). A zone that would exceed the byte budget is skipped and smaller
   ones may still fit.
3. Hot zones are subscribed one by one, hottest first, each with its files fetched in one batch. A zone that fails
   (no `main` branch, a path in the way) moves to the skipped list with the error.
4. `sync` then fast-forwards all subscribed zones as usual. Subscriptions are never removed.

Everything not hot is **skipped** and printed with its size, file count, score and the reason: `no evidence in the
profile`, `over the zone budget (N)`, `over the byte budget (...)`, `excluded by hints`, `no main branch on the
remote`, or the error. Skipped zones keep their commits and trees locally, so `vcs log --zone Z`, `vcs profile
skipped` and zone queries work offline; only file contents are missing.

Get skipped zones with one command:

```text
vcs profile fetch billing legacy        # fetch (if needed) and subscribe, contents included
vcs profile fetch web --with-deps       # also the zones web depends on
vcs profile skipped                     # list unsubscribed zones
```

`--explain` prints, per zone, the terms of its score. `--dry-run` prints the plan and downloads no file contents
(it still fetches commits and trees and reads manifests). `--stats` prints requests and bytes per stage (clone,
metadata, planning, contents, sync). Text output only; `--json` is not implemented for these commands.

On-demand is explicit: nothing fetches a skipped zone behind your back. A command that needs a missing zone's
files fails or sees nothing there; the fix is the one command above. (A transparent fetch-on-access hook is not
implemented.)

```text
vcs clone URL dir --auto-profile my.profile     # or --auto (hints only)
vcs sync --auto [--budget-zones 6] [--budget-bytes 500M] [--explain] [--stats]
vcs pull --auto                                  # same as sync
vcs clone ... --auto --dry-run --explain         # plan in a temporary directory, nothing kept
```

A clone with `--auto` first clones the root zone alone, then plans. It sets `VCS_SMALL_PACK_BYTES=0` for itself
(unless you set it) because a normal clone reads packs under 1 MiB whole, which would download every zone of a
small remote.

## Team hints

Two files are read, both optional: `.vcs-profile-hints.toml` at the worktree root (**checked in**, so a team shares
it; it lives at the root, not in `.vcs/`, because `.vcs/` is never tracked) and `.vcs/profile-hints.toml` (local).
`--auto-profile FILE` can also pass a hints file.

```toml
[profile]
role = "frontend"        # which [role.*] applies to you
half_life_days = 14
max_zones = 6            # new zones per run
max_bytes = 500000000

[role.frontend]
zones = ["web", "ui"]    # prior for people with this role

[role.backend]
zones = ["api", "billing"]

[deps]                   # extra dependency edges (from = ["to", ...])
web = ["design-tokens"]

[pin]
zones = ["tools"]        # always chosen, exempt from the budgets

[exclude]
zones = ["legacy"]       # never chosen automatically (still fetchable by hand)
```

Zone names may be display names or ids. Unknown keys are errors (a typo must not silently change what you pull);
unknown zone names are reported as notes and ignored. With no profile at all, the role, pins and their
dependencies decide.

## Limits and honest caveats

* The profile is per repository and per machine. A fresh clone has no history; seed it with `--auto-profile`.
* The accuracy of the prediction was only measured on **simulated** sessions with an access model written for the
  benchmark, not on real developers; see the research note.
* Dependency edges are manifest-based. Source-level imports (`--deep` in `clone --for`) are not scanned.
* Planning costs requests: it downloads commits and trees of all zones plus a few manifests, and `sync --auto`
  does a normal fetch after that. For small remotes that is more requests than a plain clone; see the research
  note for the measured sizes.
* `--auto` plans from the remote's `main` branch of each zone.
* Profile recording runs inside the `vcs` process for the commands in the table; it adds a repository discovery and one
  file append to them (`VCS_PROFILE=off` skips it).
