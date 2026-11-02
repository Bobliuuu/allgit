# Jujutsu and Sapling benchmark, per command (fresh run)

Status: **measured, one box, 100k files, 5 interleaved runs per cell (fewer where noted), warm cache.** This replaces the
earlier single-layout notes in [jujutsu-benchmark.md](jujutsu-benchmark.md) and [sapling-benchmark.md](sapling-benchmark.md)
(kept for history and for the feature comparison) with a re-run of every command on a current `origin/main` build.
Every number below is a median printed by `scripts/bench_jj_sl.py`; the raw per-run data is in
[jj-sapling-raw/](jj-sapling-raw/). Nothing was estimated.

Scoreboards (green = vcs WIN, amber = TIE, red = vcs LOSS, grey = N/A):

![vcs vs Jujutsu](../img/scoreboard-jj.svg)

![vcs vs Sapling](../img/scoreboard-sapling.svg)

## Setup

| item | value |
|---|---|
| machine | AWS box c: c6i.4xlarge, Intel Xeon Platinum 8375C, 16 vCPU, ext4, Linux 7.0.0-1014-aws x86_64. Shared box: only one benchmark at a time under `flock ~/bench.lock`, but other jobs and builds run outside the lock. load1 sampled before and after every cell is in the raw JSON (typical 1 to 5, a few cells up to ~12) |
| vcs | `vcs 0.1.0`, release build (`cargo build --release -p vcs-cli`) of `origin/main` at `34315b6` |
| jj | `jj 0.46.0-7d382314f9420b4b8d932245b49c99492c812b9f`, official prebuilt `jj-v0.46.0-x86_64-unknown-linux-musl.tar.gz`, SHA256 `fce0271158e665ceb82dc66c5eb95b1728649094e9133074f3b5baece345fc08`, user-space install under `~/rt-jjsl-tools/jj` |
| Sapling | `Sapling 0.2.20261028-123539+7415fb2f`, official prebuilt `sapling-...-linux-x64.tar.xz` (latest release at run time), SHA256 `9504b9514ed16dce3b5443811f7625742a363e66a44b316bcaad70a9d013b274`, user-space install under `~/rt-jjsl-tools/sl`, `sl init --git` (Git-backed store), no watchman, no EdenFS |
| git | 2.43.0 (used only for the bare Git remotes jj and sl push to) |
| jj layout | `jj git init --no-colocate` / `jj git clone --no-colocate` (jj store only). No fsmonitor |
| datasets | `datagen flat|deep|binary 100000`, seed 42 (same generator as `scripts/bench_scale.sh`) |
| config | `GIT_CONFIG_GLOBAL/SYSTEM=/dev/null`, `JJ_CONFIG=/dev/null`, identity from env or `--config ui.username`, no hooks |
| method | per cell: untimed prep, then the timed command; tools are visited in a rotating order inside every run (interleaved); wall ms and CPU ms (user+sys of the child tree, `wait4`) per run. Slow cells are cut to 3 runs when the first run (prep plus timed) exceeded 150 s (jj add/commit on deep and binary); `n` is in the tables |
| reproduce | `VCS_BIN=... DATAGEN=... JJ_BIN=... SL_BIN=... OUT=flat.json python3 -I scripts/bench_jj_sl.py flat 100000 5` for each layout, then `python3 scripts/jj_sl_scoreboard.py <dir with flat/deep/binary.json> docs/img tables.md` |

One-off smoke runs of the harness at 3,000 files (to debug commands) ran on the same box outside the lock; they are not in any number here.

## How to read the tables

- `ratio = other tool's median wall ms / vcs median wall ms`. Above 1 means vcs is faster.
- **TIE threshold: within 10 percent.** WIN: ratio >= 1.10. LOSS: ratio <= 0.91 (vcs slower by more than 10 percent). TIE: in between. The verdict uses wall time; CPU ms is shown in brackets.
- N/A: the command does not exist for vcs or for the other tool, or the command failed. N/A is never counted as a WIN.
- Each cell is at 100k files (flat = one directory, deep = nested directories, binary = binary blobs).

### What each cell times

| cell | vcs | jj | sl |
|---|---|---|---|
| init | `vcs init` in a directory holding the dataset | `jj git init --no-colocate` | `sl init --git` |
| add/track | `vcs add .` (fresh repo) | first snapshot `jj status` (jj has no staging area; any command tracks every file) | `sl add .` |
| commit | `vcs commit -m c` after `add` | `jj commit -m c` after the snapshot above (the snapshot cost is in the **add/track** row, not here) | `sl commit -m c` after `add` |
| describe | N/A (`vcs describe` is git-describe, not message editing) | `jj describe -m xN` | `sl metaedit -m xN` |
| status clean / 1% | `vcs status` | `jj status` (1%: each run snapshots 1,000 rewritten files) | `sl status` |
| diff (1% modified) | `vcs diff` | `jj diff --git` | `sl diff --git` |
| log | `vcs log -n 50` | `jj log --no-graph -n 50` | `sl log -l 50 --template ...` |
| blame/annotate | `vcs blame <file>` | `jj file annotate <file>` | `sl annotate <file>` (one small file, 8 lines) |
| branch/bookmark | `vcs branch bmN` | `jj bookmark create bmN -r @-` | `sl bookmark bmN` |
| checkout/switch | `vcs switch main` from `side` (1% of files differ) | `jj edit main` | `sl goto main` |
| amend | N/A (no amend command in vcs) | `jj squash -u` after editing one file | `sl amend` after editing one file |
| undo | `vcs op undo` after a one-file commit | `jj undo` | `sl undo -f` |
| merge | `vcs merge fN` (one commit on `side`, disjoint file) | `jj new main fN` (a two-parent commit) | `sl merge fN` plus `sl commit` (sl leaves the merge uncommitted) |
| rebase | `vcs rebase main` on the checked-out branch | `jj rebase -b rN -d main` | `sl rebase -s rN -d main` |
| push | `vcs publish` (directory remote), 1 new commit | `jj git push --remote origin -b main` (bare Git repo) | `sl push --to main -r main origin` (bare Git repo) |
| clone | `vcs clone <published dir>` full history (50 commits plus pushes) | `jj git clone --no-colocate <bare repo>` | `sl clone --git <bare repo>` |
| fetch/pull | `vcs fetch` in a clone; untimed prep pushes 1 commit | `jj git fetch` | `sl pull` |
| gc | `vcs gc` (local, default 14-day grace) | `jj util gc` | N/A: `sl gc` prints "no longer supported" |

History: 1% of files modified and committed, then 48 single-file commits, so `main` has 50 commits on top of a base commit named `side`.

## Summary

- vs jj: 41 WIN, 4 TIE, 6 LOSS (51 cells where both tools ran; describe and amend are N/A).
- vs sl: 47 WIN, 0 TIE, 1 LOSS (48 cells; gc, describe and amend are N/A).
- Every LOSS is listed below. None of them was investigated for a root cause in this run; the explanations are hypotheses and are marked as such.

## All LOSSES (vcs slower than the other tool by more than 10 percent)

| command | layout | vs | vcs ms | other ms | vcs slower by |
|---|---|---|---|---|---|
| commit (all files) | flat | jj | 102 | 82 | 1.24x |
| commit (all files) | deep | jj | 544 | 268 | 2.03x |
| commit (all files) | binary | jj | 177 | 95 | 1.87x |
| diff (1% modified) | flat | jj | 643 | 550 | 1.17x |
| diff (1% modified) | binary | jj | 815 | 563 | 1.45x |
| clone (local remote, full history) | deep | jj | 25,562 | 14,973 | 1.71x |
| diff (1% modified) | binary | sl | 815 | 626 | 1.30x |

TIEs vs jj: status (clean) on flat, deep and binary (1.04 to 1.08), undo on binary (1.05). Details are in the table.

## Notes and caveats (read before quoting any number)

- **jj commit is not comparable to vcs commit.** jj has no staging area: its `add/track` cell is the first working-copy snapshot and costs 95 s (flat), 428 s (deep, 3 runs) and 98 s (binary); the later `jj commit` only rewrites a commit that already exists. vcs pays for the add in `add` and then commits staged state. Read the add/track and commit rows together: the sum add+commit is where vcs is far ahead (about 0.7 s vs 95 s on flat). The commit LOSS rows are real for the command as timed, but they move cost between the two rows.
- **diff** LOSSes: `vcs diff` has CPU time about equal to wall time (643 ms wall, 657 ms CPU on flat), while jj's CPU exceeds its wall (550 ms wall, 802 ms CPU), which suggests jj spreads work across threads. That is a hypothesis from the CPU columns, not a profile.
- **clone (deep) LOSS vs jj** and the clone cells in general compare different transports: vcs clones its own published directory format, jj and sl clone a bare Git repository. The sl deep clone shows 78.7 s wall but 13.7 s CPU; load1 rose from 3.9 to 11.7 during that cell, so its wall time is inflated by other jobs on the shared box (it is a WIN either way, since vcs took 25.6 s).
- **Sapling is run without watchman or EdenFS**, which is how Sapling is normally deployed at scale; its status/diff/commit numbers here are for its plain Git-backed working copy. They say nothing about the Meta-internal setup.
- **jj** was run non-colocated and without fsmonitor; a colocated or watchman-backed jj was not measured.
- **Cut runs:** `add/track` and `commit` on the deep layout have n=3 (jj and sl first run exceeded 150 s with prep). With 3 runs the median is the middle run.
- **N/A rows:** `describe` (vcs has no message-edit command; `vcs describe` is the git-describe analogue), `amend` (vcs has no amend command), `gc` for sl (`sl gc` prints "no longer supported"). The jj/sl absolute numbers for these are in the tables.
- **undo**: jj `undo` and vcs `op undo` are operation-log undos; `sl undo -f` is Sapling's own undo of the last commit, not an op log.
- **merge/rebase** are single-commit, non-conflicting. Conflicting merges, large histories and `jj`'s automatic descendant rebasing were not measured.
- **blame** is on a small file (8 lines, a handful of commits); long-history blame was not measured.
- **push/fetch** use local bare repositories or directory remotes; no network, no S3, no SSH.
- **describe** was re-run on its own (`CELLS=describe`, the first run reused one message for every run and sl's `metaedit` rejected the repeat); the table uses the re-run, merged by `scripts/jj_sl_scoreboard.py`'s `desc-*.json` rule.
- The box is shared. The lock serialises benchmarks but not builds or other jobs; check each cell's load1 in the raw JSON before relying on a close call.
- Not run: git itself in this harness (ratios are against vcs only), the 10k size, other machines, repeated sessions on different days.

## Per-command results

### vcs vs Jujutsu (jj)

| command | layout | jj wall ms (cpu) | vcs wall ms (cpu) | ratio jj/vcs | result | runs |
|---|---|---|---|---|---|---|
| init (empty dir) | flat | 27 (11) | 15 (4) | 1.85 | WIN | 5/5 |
| init (empty dir) | deep | 30 (11) | 15 (4) | 2.01 | WIN | 5/5 |
| init (empty dir) | binary | 29 (11) | 14 (4) | 1.97 | WIN | 5/5 |
| add/track (all files) | flat | 94,761 (96,028) | 588 (1,816) | 161.18 | WIN | 5/5 |
| add/track (all files) | deep | 428,123 (429,350) | 1,307 (2,529) | 327.55 | WIN | 3/3 |
| add/track (all files) | binary | 97,500 (99,020) | 2,754 (4,723) | 35.40 | WIN | 5/5 |
| commit (all files) | flat | 82 (412) | 102 (92) | 0.81 | LOSS | 5/5 |
| commit (all files) | deep | 268 (2,101) | 544 (530) | 0.49 | LOSS | 3/3 |
| commit (all files) | binary | 95 (421) | 177 (96) | 0.53 | LOSS | 5/5 |
| status (clean) | flat | 51 (397) | 49 (202) | 1.04 | TIE | 5/5 |
| status (clean) | deep | 222 (2,063) | 206 (887) | 1.08 | TIE | 5/5 |
| status (clean) | binary | 51 (400) | 49 (208) | 1.04 | TIE | 5/5 |
| status (1% modified) | flat | 507 (756) | 50 (212) | 10.12 | WIN | 5/5 |
| status (1% modified) | deep | 983 (2,432) | 212 (901) | 4.64 | WIN | 5/5 |
| status (1% modified) | binary | 550 (769) | 51 (221) | 10.70 | WIN | 5/5 |
| diff (1% modified) | flat | 550 (802) | 643 (657) | 0.85 | LOSS | 5/5 |
| diff (1% modified) | deep | 974 (2,489) | 759 (790) | 1.28 | WIN | 5/5 |
| diff (1% modified) | binary | 563 (808) | 815 (829) | 0.69 | LOSS | 5/5 |
| log (50 commits) | flat | 54 (403) | 4 (4) | 13.43 | WIN | 5/5 |
| log (50 commits) | deep | 230 (2,094) | 5 (5) | 48.80 | WIN | 5/5 |
| log (50 commits) | binary | 56 (409) | 4 (4) | 12.71 | WIN | 5/5 |
| blame/annotate (1 file) | flat | 67 (413) | 9 (9) | 7.38 | WIN | 5/5 |
| blame/annotate (1 file) | deep | 230 (2,091) | 7 (7) | 31.74 | WIN | 5/5 |
| blame/annotate (1 file) | binary | 69 (419) | 10 (9) | 7.17 | WIN | 5/5 |
| describe/metaedit (message) | flat | 90 (421) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| describe/metaedit (message) | deep | 266 (2,108) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| describe/metaedit (message) | binary | 79 (417) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| branch/bookmark (create) | flat | 80 (414) | 16 (4) | 5.15 | WIN | 5/5 |
| branch/bookmark (create) | deep | 251 (2,104) | 10 (4) | 24.99 | WIN | 5/5 |
| branch/bookmark (create) | binary | 78 (420) | 12 (4) | 6.28 | WIN | 5/5 |
| checkout/switch (1% differ) | flat | 239 (563) | 152 (306) | 1.57 | WIN | 5/5 |
| checkout/switch (1% differ) | deep | 480 (2,320) | 170 (411) | 2.83 | WIN | 5/5 |
| checkout/switch (1% differ) | binary | 244 (562) | 161 (311) | 1.52 | WIN | 5/5 |
| amend (1 file) | flat | 95 (416) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| amend (1 file) | deep | 260 (2,118) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| amend (1 file) | binary | 95 (418) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| undo (last commit) | flat | 73 (415) | 66 (200) | 1.11 | WIN | 5/5 |
| undo (last commit) | deep | 250 (2,112) | 91 (306) | 2.75 | WIN | 5/5 |
| undo (last commit) | binary | 74 (417) | 70 (208) | 1.05 | TIE | 5/5 |
| merge (disjoint files) | flat | 252 (559) | 98 (241) | 2.58 | WIN | 5/5 |
| merge (disjoint files) | deep | 477 (2,298) | 122 (371) | 3.89 | WIN | 5/5 |
| merge (disjoint files) | binary | 248 (562) | 101 (250) | 2.47 | WIN | 5/5 |
| rebase (1 commit onto main) | flat | 250 (560) | 41 (189) | 6.06 | WIN | 5/5 |
| rebase (1 commit onto main) | deep | 470 (2,295) | 58 (289) | 8.10 | WIN | 5/5 |
| rebase (1 commit onto main) | binary | 250 (567) | 40 (191) | 6.27 | WIN | 5/5 |
| push (1 new commit) | flat | 124 (465) | 13 (6) | 9.90 | WIN | 5/5 |
| push (1 new commit) | deep | 390 (2,244) | 13 (6) | 29.91 | WIN | 5/5 |
| push (1 new commit) | binary | 125 (470) | 14 (6) | 9.26 | WIN | 5/5 |
| clone (local remote, full history) | flat | 5,013 (5,104) | 1,060 (2,707) | 4.73 | WIN | 5/5 |
| clone (local remote, full history) | deep | 14,973 (7,424) | 25,562 (5,538) | 0.59 | LOSS | 5/5 |
| clone (local remote, full history) | binary | 9,463 (9,058) | 4,164 (3,979) | 2.27 | WIN | 5/5 |
| fetch/pull (1 new commit) | flat | 126 (465) | 23 (19) | 5.53 | WIN | 5/5 |
| fetch/pull (1 new commit) | deep | 397 (2,247) | 32 (30) | 12.26 | WIN | 5/5 |
| fetch/pull (1 new commit) | binary | 126 (463) | 22 (18) | 5.81 | WIN | 5/5 |
| gc (local) | flat | 2,478 (2,868) | 1,134 (1,158) | 2.18 | WIN | 5/5 |
| gc (local) | deep | 4,339 (7,145) | 1,791 (1,849) | 2.42 | WIN | 5/5 |
| gc (local) | binary | 5,920 (4,444) | 1,625 (1,649) | 3.64 | WIN | 5/5 |

Totals vs jj: 41 WIN, 4 TIE, 6 LOSS.

### vcs vs Sapling (sl)

| command | layout | sl wall ms (cpu) | vcs wall ms (cpu) | ratio sl/vcs | result | runs |
|---|---|---|---|---|---|---|
| init (empty dir) | flat | 229 (221) | 15 (4) | 15.53 | WIN | 5/5 |
| init (empty dir) | deep | 240 (231) | 15 (4) | 16.22 | WIN | 5/5 |
| init (empty dir) | binary | 237 (229) | 14 (4) | 16.40 | WIN | 5/5 |
| add/track (all files) | flat | 6,134 (6,478) | 588 (1,816) | 10.43 | WIN | 5/5 |
| add/track (all files) | deep | 8,069 (9,494) | 1,307 (2,529) | 6.17 | WIN | 3/3 |
| add/track (all files) | binary | 6,078 (6,429) | 2,754 (4,723) | 2.21 | WIN | 5/5 |
| commit (all files) | flat | 13,341 (13,949) | 102 (92) | 130.85 | WIN | 5/5 |
| commit (all files) | deep | 21,250 (24,297) | 544 (530) | 39.05 | WIN | 3/3 |
| commit (all files) | binary | 22,365 (22,207) | 177 (96) | 126.34 | WIN | 5/5 |
| status (clean) | flat | 216 (557) | 49 (202) | 4.41 | WIN | 5/5 |
| status (clean) | deep | 526 (1,991) | 206 (887) | 2.56 | WIN | 5/5 |
| status (clean) | binary | 210 (562) | 49 (208) | 4.27 | WIN | 5/5 |
| status (1% modified) | flat | 222 (588) | 50 (212) | 4.44 | WIN | 5/5 |
| status (1% modified) | deep | 543 (1,997) | 212 (901) | 2.56 | WIN | 5/5 |
| status (1% modified) | binary | 223 (574) | 51 (221) | 4.34 | WIN | 5/5 |
| diff (1% modified) | flat | 779 (1,123) | 643 (657) | 1.21 | WIN | 5/5 |
| diff (1% modified) | deep | 1,087 (2,490) | 759 (790) | 1.43 | WIN | 5/5 |
| diff (1% modified) | binary | 626 (971) | 815 (829) | 0.77 | LOSS | 5/5 |
| log (50 commits) | flat | 248 (248) | 4 (4) | 61.14 | WIN | 5/5 |
| log (50 commits) | deep | 246 (246) | 5 (5) | 52.07 | WIN | 5/5 |
| log (50 commits) | binary | 251 (251) | 4 (4) | 56.93 | WIN | 5/5 |
| blame/annotate (1 file) | flat | 227 (228) | 9 (9) | 24.85 | WIN | 5/5 |
| blame/annotate (1 file) | deep | 223 (224) | 7 (7) | 30.83 | WIN | 5/5 |
| blame/annotate (1 file) | binary | 231 (232) | 10 (9) | 24.13 | WIN | 5/5 |
| describe/metaedit (message) | flat | 255 (243) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| describe/metaedit (message) | deep | 244 (234) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| describe/metaedit (message) | binary | 249 (240) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| branch/bookmark (create) | flat | 231 (226) | 16 (4) | 14.91 | WIN | 5/5 |
| branch/bookmark (create) | deep | 233 (228) | 10 (4) | 23.23 | WIN | 5/5 |
| branch/bookmark (create) | binary | 236 (229) | 12 (4) | 19.01 | WIN | 5/5 |
| checkout/switch (1% differ) | flat | 440 (817) | 152 (306) | 2.90 | WIN | 5/5 |
| checkout/switch (1% differ) | deep | 797 (2,396) | 170 (411) | 4.70 | WIN | 5/5 |
| checkout/switch (1% differ) | binary | 450 (851) | 161 (311) | 2.80 | WIN | 5/5 |
| amend (1 file) | flat | 662 (1,458) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| amend (1 file) | deep | 1,247 (4,478) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| amend (1 file) | binary | 669 (1,434) | N/A | N/A (vcs has no equivalent command) | N/A | 5/- |
| undo (last commit) | flat | 610 (1,244) | 66 (200) | 9.31 | WIN | 5/5 |
| undo (last commit) | deep | 1,165 (3,870) | 91 (306) | 12.82 | WIN | 5/5 |
| undo (last commit) | binary | 622 (1,273) | 70 (208) | 8.91 | WIN | 5/5 |
| merge (disjoint files) | flat | 1,061 (2,067) | 98 (241) | 10.88 | WIN | 5/5 |
| merge (disjoint files) | deep | 1,934 (6,131) | 122 (371) | 15.80 | WIN | 5/5 |
| merge (disjoint files) | binary | 1,094 (2,098) | 101 (250) | 10.86 | WIN | 5/5 |
| rebase (1 commit onto main) | flat | 874 (1,990) | 41 (189) | 21.15 | WIN | 5/5 |
| rebase (1 commit onto main) | deep | 1,653 (6,172) | 58 (289) | 28.50 | WIN | 5/5 |
| rebase (1 commit onto main) | binary | 880 (2,005) | 40 (191) | 22.10 | WIN | 5/5 |
| push (1 new commit) | flat | 279 (281) | 13 (6) | 22.32 | WIN | 5/5 |
| push (1 new commit) | deep | 355 (357) | 13 (6) | 27.22 | WIN | 5/5 |
| push (1 new commit) | binary | 278 (279) | 14 (6) | 20.51 | WIN | 5/5 |
| clone (local remote, full history) | flat | 2,931 (5,218) | 1,060 (2,707) | 2.77 | WIN | 5/5 |
| clone (local remote, full history) | deep | 78,734 (13,738) | 25,562 (5,538) | 3.08 | WIN | 5/5 |
| clone (local remote, full history) | binary | 9,598 (10,922) | 4,164 (3,979) | 2.31 | WIN | 5/5 |
| fetch/pull (1 new commit) | flat | 284 (281) | 23 (19) | 12.52 | WIN | 5/5 |
| fetch/pull (1 new commit) | deep | 379 (375) | 32 (30) | 11.69 | WIN | 5/5 |
| fetch/pull (1 new commit) | binary | 292 (287) | 22 (18) | 13.49 | WIN | 5/5 |
| gc (local) | flat | N/A | 1,134 (1,158) | N/A (no sl result) | N/A | -/5 |
| gc (local) | deep | N/A | 1,791 (1,849) | N/A (no sl result) | N/A | -/5 |
| gc (local) | binary | N/A | 1,625 (1,649) | N/A (no sl result) | N/A | -/5 |

Totals vs sl: 47 WIN, 0 TIE, 1 LOSS.

