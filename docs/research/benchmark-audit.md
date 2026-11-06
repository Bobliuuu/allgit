# Benchmark audit of the published performance claims

Audit date 2026-11-06, repository at `17989b5`. Scope: `README.md`, `docs/benchmarks.md`, `docs/scalability.md`, `docs/research/*`, `scripts/bench_*`,
`scripts/huge_*`, `scripts/command_scoreboard*.py`. Method: for every headline number, find the committed raw data, recompute it
(`python3 -I scripts/audit_headlines.py`, all pass), then read the harness for unfair comparisons.
Nothing was re-timed: the Mac was at load average 50 during the audit and a release build plus a 100k-file run could not be made crash-safe. Every re-timing
is listed under "Needs a remote run".

Result in one paragraph: **the arithmetic is honest** (every quoted median reproduces from the committed JSON/TSV; the scoreboard totals 136/68/14 re-render
exactly), but **four headline comparisons are not like for like** (1M-file clone, 1M-file add, 100k `status`, group-commit 512 writers), and the README quoted a
superseded run while linking the newer data. Items marked FIXED were corrected in the docs in this pass.

## Severity ranking

### 1. HIGH - 1M-file `clone` win (603 s vs 40.5 s) compares a loose-object copy with a packed clone  [caveat added]

Evidence: `scripts/huge_s1.sh:32` runs `git clone -q --no-hardlinks $D` of the working repo, and `git gc` only afterwards (`:34`). Git therefore copied about 2M
loose objects individually: `docs/research/data/hugescale-boxf/results.tsv` shows wall 603.11 s but user+sys 65 s (9.3x wall/CPU = I/O wait). vcs cloned from a
published, packed remote. The repository's own later run, `docs/research/monorepo-stress.md:369`, cloned the same size of tree (1M files) with git in **24 s** from a
local path, while vcs needed 177 s over HTTP (40.5 s local path in huge-scale). With a repacked git source the claimed 15x win probably reverses; it was never measured.
Same pattern, smaller: git `add` 215 s (CPU 59 s), `checkout` 46.5/28.6 s (CPU 4.8/4.7 s), `diff` 19.7 s (CPU 2.2 s) are I/O-bound on one gp3 volume with no cache state recorded.
Also: one run per cell, git always first, build `edd859b` is now far behind main, `gc` compares repack+delta (git) with an unreachable-object sweep (vcs).
Fix: README row and `huge-scale.md` now carry these caveats (FIXED). Real fix: run `scripts/bench_clone_fair.sh` (new) on a remote box.

### 2. HIGH - 100k `status` win compares vcs-with-a-background-daemon to git-without-fsmonitor  [caveat added]

Evidence: `docs/benchmarks.md:746-756`: since 2026-11-03 an index of at least 1 MB auto-starts a detached `vcs watch`, and later `status`/`diff` read its journal.
The scoreboard harness (`scripts/command_scoreboard.py:84`, CPU from `wait4` of the command only) neither sets `VCS_NO_AUTOWATCH`/`VCS_NO_FSMONITOR` nor counts the
daemon's CPU, and git runs with an empty config (no `core.fsmonitor`, no `core.untrackedCache`; `docs/research/command-scoreboard.md` "Not run"). The 2026-11-02 run
(before the autowatch default) had status 55 vs 65 ms, a CPU tie (1.04x); the 2026-11-04 run has 30.8 vs 100.2 ms and CPU 44.7 vs 235 ms. The scoreboard counts `status`, `status -s`, `status --porcelain` as three WINs.
Fix: README `status` row flagged (FIXED). Real fix: `scripts/bench_status_fair.sh` (new) gives git default / git fsmonitor+untrackedCache / vcs watcher off / vcs watcher on, and prints the daemon's CPU.

### 3. HIGH - README headline table quoted a superseded run while linking the newer data  [FIXED]

Evidence: README (before this pass) lines 37 and 273: add 538 vs 4,810 ms, status 65 vs 55 ms, switch 233 vs 179 ms. Those come from
`docs/research/data/command-scoreboard-prev/` (box E, 2026-11-02, `8da0be2`), but the "Source" link pointed to `data/command-scoreboard`, which holds the 2026-11-04 run
(`845610a`): add 5,383 vs 733 ms (n=3), commit 414 vs 240 ms, status 100 vs 30.8 ms, log 2.2 vs 2.9 ms, switch 252 vs 244 ms = **TIE** (README said 0.77, a win).
The README also described the table as "5 runs (3 for slowest)", box E, load1 1.5 to 2.1; the new run is 9 runs (3 for add/commit), box c, load gate 1.0.
Fix: table, headline row and provenance text replaced with the data it links to (FIXED). The "how fresh" column also said p4/walgit/jj/Sapling were from `8da0be2` or "not interleaved";
the data (`data/command-scoreboard/p4.json`, `data/walgit-final`, `data/jj-sl-final`) are all `845610a`, 2026-11-04, interleaved (FIXED).

### 4. HIGH - Group-commit headline (512 writers, 69.7 vs 27.3 publishes/s) has no raw data in the repo and ran on a loaded box

Evidence: the numbers appear only in `docs/scalability.md:25` (finding 6) and are hard-coded in `scripts/render_showcase_charts.py:88`; no JSON/TSV for them exists under
`docs/research/` and no script is named. The text itself says "box E load1 17-22" on a 16-vCPU machine. "Feature off" vs "on" uses the same binary, which is the right design, but the run cannot be re-derived.
Fix: README row now states the load and backend (FIXED). Needs: commit the raw output, or re-run (see below).

### 5. MEDIUM - README "215 to 350 pushes/s at 256 to 1024 writers" contradicts the data  [FIXED]

Evidence: `docs/research/many-writers-data/s3-batch-disjoint.jsonl`: 256 writers 214.3 (zones) / 347.6 (branches); **512 writers 157.5 / 157.6**; 1024 writers 279.9 / 266.3. The
doc's own table (`many-writers-s3.md:56,60`) shows 157, but its text (`:77`) and the README said 215 to 350. Also each writer pushed only twice (acked = 2N), so ramp-up dominates,
one run per cell, and the 512 dip is unexplained. The soak "177 `kill -9`, 2,254 acked files" (`many-writers-s3.md:145`) has no raw file in `many-writers-data/`.
Fix: README and `many-writers-s3.md` now say 157 to 348 and the 512 dip (FIXED). The soak data is missing and needs to be pulled from the box or the claim marked as unsupported.

### 6. MEDIUM - git comparison in the high-load matrix: loaded box, unrecorded build, different git version, different transport

Evidence: `highload-data/branches-groupcommit-pr452.json` meta: load average 11.6 to 14.6 at start (16 vCPU), `vcs_rev: "?"`. Header of `highload-matrix.md:3` says git 2.56 on box D, while the
scoreboards on box c say git 2.43.0 (`data/command-scoreboard/*.json`); `command-scoreboard.md:420` says 2.56.0 for the walgit pass on the same box c. git arm = `git daemon` (unauthenticated, no HTTP) vs
`vcs-server` over HTTP; the README states the local-disk loss for vcs honestly (git 222 vs vcs 85 pushes/s) and the `core.fsync=all` pairing is a good control. The headline "vcs S3 beats walgit 8 of 9" has min..max ranges recorded and is the best-supported item here.
Fix: caveat block added to `highload-matrix.md` (FIXED). Needs: confirm which boxes had which git, record `vcs_rev`.

### 7. MEDIUM - Scoreboard counts inflate unique scenarios; verdict rule differs between documents

Evidence: `scripts/command_scoreboard_render.py` counts one verdict per doc-table row, so `git init`/`git init proj`, `git add .`/`git add -A`, `git status`/`-s`/`--porcelain`, `git switch --detach`/`git checkout --detach` count 2 to 3 times. Counting each git scenario once gives **55 WIN / 51 TIE / 6 LOSS (112 cells)**, not 64 / 59 / 8 (131). README says "rows sharing a scenario count once per row", but the headline "136 wins" is stated without it.
Also `box-harvest.md:8` uses a 0.90/1.10 band while the scoreboard uses 0.95/1.05; 93 of 244 git rows are NOT-MEASURED and 44 MISSING (stated in the README table, not in the totals sentence).
Fix: unique counts added to README (FIXED). Optional: render both counts in the generated summary.

### 8. MEDIUM - Real-S3 numbers quoted are the best-case cells

Evidence: `data/real-s3/std100k.json`: cold slice clone 341 ms (fresh history) vs git blobless 456 ms, but after the benchmark's push series the same clone is 1,492 ms vs 635 ms (Express 3,641 ms vs 629 ms). Warm fetch 264 ms vs git 231 to 245 ms is a loss/tie and 3-file push p50 232 ms vs 83 ms is a loss; the README does state these. 3 rounds per cell; credentials were from a different account so traffic used the public endpoint and Express was cross-AZ. The git baseline is `git://` on the same box (flatters git, stated).
Fix: post-push cells added to README (FIXED).

### 9. MEDIUM - "warm" is slower than "cold" in the walgit remote-workflow table

Evidence: `data/walgit-full-boxc/res-flat-100000.json`: vcs full clone warm 2,174 ms vs cold 1,241 ms; git warm 4,023 vs cold 3,654. The README table prints both without comment. Either the labels mean something other than cache temperature (warm = existing client store, more work?) or the harness has an ordering effect. `scripts/walgit_bench/walgit_full.py` should be read to define them. Also that table is build `84f3fa2`, 3 reps, git 2.56.0; the 24-0-0 walgit scoreboard count comes from a different run (`data/walgit-final`, `845610a`, 5 reps).
Fix: none made (needs the harness author). Do not use warm-vs-cold as a statement about caching.

### 10. MEDIUM - Zone-profile bench: debug binary, self-fulfilling profile, flattering baseline

Evidence: `zone-profile-bench.md:9-14`: vcs ran `target/debug` ("optimized dev profile"), load1 30.6 at start, persona profile seeded to edit exactly `z07` and read `z08`,`z21`, so the predictor choosing them is by construction; git bytes are `.git` growth, not wire bytes; the incremental-sync baseline (49 MB, 13.8 s) is a bug fixed afterwards and "exaggerates the size of the win" (the doc says so). README's headline row "37 MB vs 189.5 MB full clone" compares against vcs's own full clone; against git partial+sparse it is a tie (39.6 MB), which the README prose states. vcs LOSES incremental sync to git (0.78 vs 0.29 s) and makes 2.5x the requests; both stated.
Fix: none needed beyond what is stated; re-run with a release build.

### 11. LOW - Mixed git versions, machines and loads inside `docs/benchmarks.md`

Evidence: sections use git 2.34.1 (shared 30-core box, load1 up to 36, vcs cells run at higher load than git cells), 2.43.0, 2.52.0 (Mac M2 Pro, load 14-35 with 20 agents) and 2.56.0. The early tables show vcs 3 to 6x slower than git, later ones show it faster; the file reads as a chronological log and a skimming reader can quote either.
Fix: audit banner added at the top of `docs/benchmarks.md` (FIXED).

### 12. LOW - Stale statements after code changes  [FIXED]

* README said the lazy tag table (#481) was "pending merge" and "the delta log is not built"; `docs/scalability.md:27,50` and `git log` (`0dd0687`, #527) say it is built (opt-in via feature gate, default from 50,000 tags). The delta-log numbers are macOS, dev profile opt-level 1, load 100 to 240, single runs.
* README: "2.3 GiB" peak RSS; the data is 2,310,224 KiB = 2.2 GiB. `huge-scale.md` said vcs was faster on every op but one; `init` (0.17 vs 0.02 s) and status-1% (2.06 vs 1.83 s, a 12% loss) are also slower.
* The 1M-file numbers are build `edd859b`; the rest of the claims are `845610a`; main has moved on (e.g. `17989b5` fsck batching). Re-run before quoting against current code.

### 13. LOW - Soak/chaos "0 acknowledged files lost" has low statistical power

Evidence: `box-harvest-data/soak-chaos/chaos.log` (157 kills, one per 5.4 s) and `verify.json` (245 acked files in 14 min, about 0.3 files/s). Few writes are in flight at any kill. The doc discloses this, plus the sampler bug and no kill during `gc`. The monorepo-stress runs (5,038 to 7,223 acked, 8 kills per run) are far stronger evidence. The README row is accurate but reads stronger than the data.

### 14. LOW - Peering: 3 vs 129 to 513 origin requests relies on a 30 s manifest TTL

Evidence: `docs/peering.md:116,120,124`: with ttl 0 the peer sends 98 to 418 requests; with ttl 30 s only 3. A 30 s TTL means clones can see a manifest up to 30 s stale (heads, locks); the README names the TTL but not this trade-off. Latency is a tie on loopback (stated).

## What was checked and found sound

* Every number in the README headline table reproduces from the committed raw data (`scripts/audit_headlines.py`): 1M-file TSV, scoreboard JSON medians, walgit table, real-S3 medians, soak counts.
* Scoreboard re-render: `scripts/command_scoreboard_render.py` reproduces 64/59/8, 17/7/6, 24/0/0 exactly (the 14 losses are the git 8 + p4 6 rows).
* Verdict rule was fixed before looking, uses a 5% band plus MAD noise and a 2 ms floor; interleaved random order, load gate below 1.0, medians with min/max in the JSON.
* The README discloses most losses (no-op `add` at 1M, local-disk pushes, blame, fsck memory, whole-tree clone over HTTP) and says "not measured" for cold cache, fsmonitor and WAN.

## Needs a remote run (not run here; no cloud resources launched)

| what | why | how |
|---|---|---|
| fair 1M-file clone (git repacked, `file://`, cold and warm vs vcs) | finding 1 | `scripts/bench_clone_fair.sh` on a c6i.4xlarge or larger, about 25 GB |
| `status`/`diff` with git fsmonitor+untrackedCache and vcs watcher off, plus daemon CPU | finding 2 | `scripts/bench_status_fair.sh <dir> 9` |
| group commit at 512 writers on a quiet box, raw output committed | finding 4 | `scripts/highload_bench/highload.py` plus the `vcs-server` flag for batching; record `vcs_rev` and load |
| rerun of 1M-file add/checkout/diff with `iostat` and cache state recorded, current main | findings 1, 12 | `scripts/huge_s1.sh` |
| pull the 177-kill S3 soak raw data off the box | finding 5 | box that ran `many-writers-s3` |
| harness read of what "warm"/"cold" mean in `walgit_full.py` | finding 9 | code read, then rerun if wrong |
| release-build zone-profile bench | finding 10 | `scripts/bench_zoneprofile.py` |
| 100k status/add/commit on a second hardware class (laptop, small VM) | `diff` uses 16 cores (51.9 s user for 3.7 s wall) | any |

Not re-run locally on purpose: any build (`target/` has debug only), any 100k-file generation, anything touching disk beyond a few JSON reads, on a Mac at load average 50.

## Added in this pass

* `scripts/audit_headlines.py`: recomputes the headline numbers from the raw data and fails on a disagreement.
* `scripts/bench_clone_fair.sh`, `scripts/bench_status_fair.sh`: the two missing fair-pair harnesses. They are syntax-checked and refuse to run off Linux, but have **not** been executed.
