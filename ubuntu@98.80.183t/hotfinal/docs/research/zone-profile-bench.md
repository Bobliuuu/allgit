# Zone profile benchmark: `--auto` sync and clone

Feature: [../zone-profile.md](../zone-profile.md). Script: `scripts/bench_zoneprofile.py` (tables: `scripts/render_zoneprofile_bench.py`).
Raw data: `docs/research/data/zoneprofile-bench.json` (system run) and `docs/research/data/zoneprofile-sim.md` (simulation).
Tie rule used throughout: within about 10 percent, or overlapping ranges, is a tie.

## Setup (system benchmark)

* Box: AWS c6i.4xlarge (box C), 16 vCPU, Xeon 8375C. The whole timed run held `~/bench.lock`. **load1 was 30.6 when it started and 1.9 when it ended**
  (other agents' build jobs outside the lock were still running at the start), so absolute wall times, especially early ones, carry noise;
  CPU seconds are given too. Ranges below are min..max of 3 runs.
* vcs: this PR's binary (`target/debug`, the repo's optimized dev profile), `vcs-server` on 127.0.0.1 (no network latency, so wall time
  understates what a real link costs and favours designs with more requests). git 2.43.0 over `file://` of a bare repo with
  `uploadpack.allowFilter`.
* Data: synthetic monorepo, 50 zones, 100,051 files, 2,000 files per zone (about 1 to 5 KB of generated word text each, plus a `Cargo.toml` per
  zone with 0 to 3 path dependencies on earlier zones). Remote size: vcs 189.5 MB, git bare 187.8 MB. Not a real codebase: content
  compresses like word salad, directory shapes are uniform.
* Persona profile (a seeded `--auto-profile` file): 5 days of sessions that edit and build `z07` and read `z08` and `z21`. Default budget:
  8 new zones, no byte limit. The predictor chose `z07 z08 z21` plus `z01 z15 z03 z06` (their dependencies) plus root: 7 added zones.
* git comparison: `git clone --filter=blob:none --no-checkout --sparse`, then `sparse-checkout set --cone` on the same 7 zone directories, then `checkout`.
  That is what a user would do by hand after knowing which zones; it has no profile or prediction. git bytes are the growth of `.git` on disk
  (compressed pack data), not wire bytes, and git requests were not counted (not measured).
* Incremental sync: 1000 files changed (100 in each of 10 zones: the 3 persona zones and 7 others), published, then each client updated.

## Results

### Initial clone

| scenario | wall s | cpu s | requests | MB downloaded | MB on disk |
|---|---|---|---|---|---|
| vcs full (all 50 zones) | 2.87 [2.85..2.90] | 4.88 [4.79..4.91] | 28 | 189.5 | 622.5 |
| vcs `--auto-profile` (7 zones + root) | 1.16 [1.14..1.18] | 1.34 [1.34..1.38] | 71 | 37.0 | 95.2 |
| git full | 7.80 [7.27..7.83] | 6.70 | n/a | n/a | 197.4 (.git) |
| git partial + sparse, same zones | 3.68 [3.59..3.74] | 7.71 | n/a | n/a | 39.6 (.git) |

* `--auto` downloads 19.5 percent of the bytes of a full vcs clone and is faster in wall and CPU: a clear win over full vcs clone.
* It makes **2.5 times more requests (71 vs 28)**. Stage breakdown from one run: root clone 8 requests / 8.0 MB, commits and trees of all 50
  zones 5 / 3.5 MB, planning 16 / 0.8 KB, contents of the chosen zones 42 / 25.6 MB, total 71 requests / 37.0 MB. Over a real network with
  latency per request this cost would show; here (loopback) it does not.
* Against git partial clone + sparse checkout of the same zones: bytes are a **tie** (37.0 vs 39.6 MB; different measures), vcs `--auto` is faster
  in wall (1.16 vs 3.68 s) and in CPU (1.34 vs 7.71 s). git has no automatic choice of the zones; this comparison gives git the answer.

### Incremental sync (1000 changed files in 10 zones; 3 of them subscribed by `--auto`)

| scenario | wall s | cpu s | requests | MB downloaded |
|---|---|---|---|---|
| vcs sync, full clone (50 zones subscribed) | 13.76 [13.45..17.48] | 13.27 | 65 [55..75] | 49.0 |
| vcs sync --auto | 0.78 [0.72..1.39] | 0.70 | 21 | 2.0 [2.0..3.5] |
| git pull, full clone | 0.39 [0.39..1.07] | 0.53 | n/a | 2.4 (.git growth) |
| git pull, partial + sparse | 0.29 [0.25..1.91] | 0.29 | n/a | 1.1 (.git growth) |

* `sync --auto` beats a full vcs sync by a wide margin in all of time, requests and bytes (it only updates 3 zones' contents).
* **git is ahead of vcs here**: git partial pull (0.29 s, 1.1 MB) and git full pull (0.39 s, 2.4 MB) beat vcs `--auto` (0.78 s, 2.0 MB) in wall time
  by more than 10 percent with non-overlapping medians; bytes vs the full-clone git pull are a tie (2.0 vs 2.4), vs partial git about 1.8 times more.
* The full vcs sync downloaded 49 MB for 1000 changed files (about 3 MB of changed content). I did not find out why; it is existing behaviour of
  `vcs sync` on a clone that holds every zone, measured with the same binary and not changed by this PR. It is the baseline `--auto` is compared with,
  so it exaggerates the size of the win; the fairer comparison for bytes is the git rows.

### On-demand fetch of a skipped zone (`z02`, 2,001 files)

vcs `profile fetch z02`: 0.10 s, 11 requests, 8.3 MB downloaded (stored size of the zone is 3.5 MiB; the extra is unexplained here). git
`sparse-checkout add`: 0.30 s, 3.9 MB. vcs is faster in wall time on loopback and downloads about twice the bytes (not a tie). One run each.

### Cost of recording

`vcs status` on the 100k-file clone, 40 interleaved runs each: median 1317.7 ms with recording, 1316.2 ms with `VCS_PROFILE=off`.
Difference 1.5 ms, 0.1 percent: a tie. (status itself is slow here because the binary is the dev profile on a loaded box; the point is the difference.)

## Prediction hit rate (simulated sessions only)

`crates/vcs-repo/tests/zoneprofile_sim.rs`, 200 seeded developers, 50 zones, a random dependency graph (0 to 3 deps per zone), 60 days, the
first 10 as training; each later day is predicted from the earlier days; hit rate = share of the zones touched that day that were in the hot set
(the budget of k zones). Access model **written by me, not measured from real developers**: each developer has 3 home zones (one replaced every
20 days), a session has 4 to 8 events, 65 percent in the home zones, 17 percent on dependencies of the home zones, 18 percent in random zones.

k = 6 hot zones (mean of per-developer hit rate; p10..p90):

| predictor | dependency accesses recorded | dependency accesses not recorded |
|---|---|---|
| random | 12.2% (9.1..15.1) | 12.2% (9.1..15.1) |
| recency only | 61.6% (54.8..68.7) | 60.2% (54.9..67.0) |
| frequency only | 63.7% (58.2..70.2) | 61.0% (55.7..67.5) |
| ours without dependency expansion | 64.2% (58.6..70.7) | 60.8% (55.9..66.7) |
| ours (default) | 63.3% (56.8..70.9) | 64.1% (58.5..70.7) |

Reading, with the tie rule:

* Every profile-based predictor beats random by about 5 times: clear.
* Recency, frequency and ours are **ties** when dependency accesses are recorded (all within 10 percent, ranges overlap). The decay and
  co-access terms did not show a measurable gain over plain frequency in this model.
* When the profile does not see dependency accesses (the compiler reading them, which is the usual case for `vcs profile record -- cargo build`),
  ours is 64.1 vs 61.0 percent for frequency at k = 6: still a tie by the rule (5 percent, overlapping ranges). At k = 8 (69.5 vs 63.3, about 10 percent)
  and k = 12 (76.3 vs 67.4, 13 percent) the dependency expansion starts to separate from the rest; see `data/zoneprofile-sim.md`.
* The default dependency weight (0.3) was picked after looking at these same simulations (0.15, 0.3, 0.45 and 0.6 were compared), so the
  default is slightly tuned to this model.
* Not measured: hit rate on real developer histories; the metadata-only tier (the hit counts only zones with contents local).

## What was not measured

* Real network latency or bandwidth; real S3; an https remote; concurrent clients.
* git request counts; git wire bytes (only on-disk growth).
* Hit rate on real usage. Memory use. The profile file's effect on commands other than `status`.
* Larger monorepos than 100k files or smaller packs than the ones here: `--auto` requests grow with the number of zones whose manifests are read
  and with the pack count; on a 12-zone, 2,400-file test the full clone needed 4 to 7 requests and `--auto` 31 (see `crates/vcs-cli/tests/zoneprofile.rs`, bytes were
  lower, requests higher).
