# Jujutsu (jj) benchmark and feature comparison

Status: **measured once, one machine, 5 runs per cell**. Jujutsu (`jj`) is the closest design relative of allgit (operation log
with undo, change ids, a working copy that is a commit), so this document compares it with allgit (`vcs`) and git on the
same 100k-file datasets as the other benchmarks. Numbers are only what the harness printed; the last section lists what was not measured.

## What was used

| item | value |
|---|---|
| machine | AWS instance "a": AMD EPYC 7R32, 8 vCPU, 30 GiB RAM, ext4 on NVMe, Linux 7.0.0-1014-aws x86_64. Shared with other jobs: load1 was 1.0 to 9.7 at the sampling points during the runs (per-cell load is in the script output) |
| jj | **0.46.0** (`jj 0.46.0-7d382314f9420b4b8d932245b49c99492c812b9f`), official prebuilt `jj-v0.46.0-x86_64-unknown-linux-musl.tar.gz` from the jj-vcs/jj GitHub release; SHA256 `fce0271158e665ceb82dc66c5eb95b1728649094e9133074f3b5baece345fc08` equals the digest GitHub publishes for that asset (checked with `sha256sum -c`). Installed under `~/rt/jj/bin`, nothing system-wide |
| git | 2.43.0 |
| vcs | `vcs 0.1.0`, release build of origin/main at 28e2980 (a newer commit than the one behind the README table, so `vcs` cells here are not directly comparable with that table) |
| jj repo layout | `jj git init --no-colocate` (jj store only, no `.git` beside it) and `jj git clone --no-colocate`. jj 0.46 colocates by default; a colocated repo was **not** measured |
| config | `GIT_CONFIG_GLOBAL/SYSTEM=/dev/null`, `JJ_CONFIG=/dev/null`, identity from env, no fsmonitor/watchman for any tool |
| datasets | `datagen flat|deep|binary 100000`, seed 42, same generator and layouts as `scripts/bench_scale.sh` |
| script | `scripts/bench_jj.py <layout> 100000 5`; clone cells for flat and binary were re-run on their own (`CELLS=clone`) after the first run showed jj's clone was colocated (see caveats) |
| method | per cell: 5 runs, **interleaved** (inside every run each tool is visited in a rotating order, with untimed prep), warm cache, wall time and CPU time (user+sys of the child process tree, from `wait4`), load1 sampled before and after every cell |

### What each cell times

| workflow | git | vcs | jj |
|---|---|---|---|
| add | `git add .` into a fresh repo | `vcs add .` into a fresh repo | first snapshot: `jj status` in a fresh repo (jj has no staging area; any command snapshots and tracks every file) |
| commit | `git commit -q -m c` after `add` | `vcs commit -m c` after `add` | `jj commit -m c` after the snapshot above |
| status clean / 1% | `git status --porcelain` | `vcs status` | `jj status` (1%: every 100th file rewritten before each run, so each jj run snapshots 1,000 changed files) |
| log | `git log --oneline -n 50` | `vcs log -n 50` | `jj log --no-graph -n 50` |
| checkout (1% differ) | `git checkout main` from `side` | `vcs switch main` from `side` | `jj edit main` from `side` (bookmarks) |
| undo | `git reset --hard HEAD~1` (git has no operation undo; this is the nearest one-command equivalent, it also touches the working tree) | `vcs op undo` | `jj undo` |
| op log | `git reflog -n 20` (not equivalent, only the nearest read) | `vcs op log -n 20` | `jj op log --no-graph -n 20` |
| clone | `git clone --no-local <repo>` | `vcs clone <published dir> <dest>` (a prior `vcs publish` into a directory remote, untimed) | `jj git clone --no-colocate <repo>/.jj/repo/store/git <dest>` (after an untimed `jj git export`) |

Each repo has 50 commits on `main` (the 1% change plus 48 single-file commits); `side` points at the first commit.
For the undo cell, the untimed prep makes one single-file commit and the timed command undoes it.

## Results

Median wall ms (median CPU ms), 5 runs each. git / vcs / jj:

| workflow | flat | deep | binary |
|---|---|---|---|
| add (git/vcs add ., jj first snapshot) | 30988 (9858) / 1278 (2391) / 153403 (118064) | 10272 (10213) / 2342 (3759) / 405602 (408685) | 18452 (18262) / 5462 (11223) / 112848 (113094) |
| commit (after add/snapshot) | 566 (725) / 397 (578) / 153 (550) | 13423 (13633) / 1173 (1843) / 1040 (4212) | 548 (710) / 450 (609) / 149 (548) |
| status (clean) | 113 (279) / 141 (617) / 96 (524) | 938 (1352) / 476 (2318) / 991 (4170) | 113 (281) / 141 (619) / 97 (525) |
| status (1% modified) | 120 (286) / 143 (624) / 706 (1113) | 958 (1372) / 467 (2287) / 2136 (5664) | 120 (288) / 142 (626) / 701 (1113) |
| log (50 commits) | 6 (5) / 10 (10) / 101 (529) | 6 (6) / 16 (15) / 998 (4189) | 6 (5) / 10 (10) / 101 (530) |
| checkout (1% differ) | 343 (506) / 541 (1019) / 382 (765) | 476 (890) / 579 (1504) / 1442 (4573) | 352 (521) / 540 (1019) / 380 (768) |
| undo last commit (git: reset --hard) | 268 (261) / 632 (1176) / 130 (545) | 571 (553) / 3087 (5158) / 1025 (4197) | 272 (265) / 633 (1179) / 131 (545) |
| op log / reflog, 20 entries | 4 (4) / 58 (158) / 99 (526) | 4 (4) / 98 (365) / 997 (4196) | 4 (4) / 57 (156) / 100 (527) |
| clone (local path) | 5274 (6336) / 3890 (11209) / 17357 (22119) | 10118 (9921) / 24455 (17453) / 36502 (40417) | 10495 (11601) / 8930 (17137) / 33906 (79673) |

Raw runs (wall ms):

**flat**

| cell | git | vcs | jj |
|---|---|---|---|
| add (git/vcs add ., jj first snapshot) | 11524 33615 35992 30988 8981 | 1252 1917 1278 1248 6437 | 131510 153403 158731 115973 296989 |
| commit (after add/snapshot) | 568 568 566 558 552 | 397 335 398 413 303 | 160 153 155 148 142 |
| status (clean) | 110 114 113 111 114 | 163 141 140 142 141 | 96 97 97 96 96 |
| status (1% modified) | 120 119 122 120 120 | 144 144 142 142 143 | 707 696 704 708 706 |
| log (50 commits) | 6 6 6 5 6 | 12 10 11 10 10 | 104 101 101 101 101 |
| checkout (1% differ) | 340 343 343 343 338 | 610 539 543 517 541 | 493 382 371 417 370 |
| undo last commit (git: reset --hard) | 295 268 268 267 268 | 666 631 634 632 630 | 131 130 129 130 129 |
| op log / reflog, 20 entries | 4 4 4 4 4 | 59 58 57 59 58 | 101 100 99 99 99 |
| clone (local path) | 5207 5354 5272 5274 5309 | 3890 3870 3828 4071 4603 | 18966 17357 17368 17262 17279 |

**deep**

| cell | git | vcs | jj |
|---|---|---|---|
| add (git/vcs add ., jj first snapshot) | 29361 10180 10272 10155 10312 | 25818 2331 2344 2342 2336 | 413082 402855 405602 408674 405304 |
| commit (after add/snapshot) | 13423 14101 13104 13497 13200 | 1695 1102 1173 1197 1091 | 1045 1026 1040 1046 1040 |
| status (clean) | 933 938 943 936 943 | 685 448 476 486 463 | 998 982 996 976 991 |
| status (1% modified) | 958 972 958 957 970 | 440 508 461 471 467 | 2134 2150 2147 2136 2131 |
| log (50 commits) | 6 6 6 6 6 | 16 16 16 15 14 | 998 998 994 992 998 |
| checkout (1% differ) | 476 482 472 443 488 | 560 579 559 620 614 | 1462 1442 1442 1400 1413 |
| undo last commit (git: reset --hard) | 582 573 564 570 571 | 3286 3086 3064 3101 3087 | 984 1018 1036 1025 1037 |
| op log / reflog, 20 entries | 4 4 4 4 4 | 96 98 98 99 98 | 996 998 998 997 984 |
| clone (local path) | 10118 10125 10027 11168 10036 | 24455 26356 25108 22125 22415 | 38832 36414 37353 36502 36375 |

**binary**

| cell | git | vcs | jj |
|---|---|---|---|
| add (git/vcs add ., jj first snapshot) | 18452 18086 19688 17984 318196 | 7462 5058 6739 5053 5462 | 106773 103545 114717 119767 112848 |
| commit (after add/snapshot) | 565 548 545 545 548 | 702 304 458 450 310 | 146 149 151 150 142 |
| status (clean) | 113 113 113 112 110 | 160 141 141 140 141 | 97 97 96 97 96 |
| status (1% modified) | 120 121 118 120 118 | 140 141 142 143 143 | 701 694 711 705 699 |
| log (50 commits) | 6 6 5 5 6 | 10 11 10 9 11 | 101 102 101 101 101 |
| checkout (1% differ) | 351 353 352 354 350 | 540 552 551 508 540 | 429 371 369 431 380 |
| undo last commit (git: reset --hard) | 295 272 274 272 272 | 651 632 634 630 633 | 130 132 131 132 129 |
| op log / reflog, 20 entries | 4 4 4 4 5 | 57 57 56 56 57 | 98 99 103 100 102 |
| clone (local path) | 10495 10525 10241 10574 10349 | 8969 9016 8747 8911 8930 | 34994 33571 33699 33915 33906 |


## Reading the numbers

* **The first jj snapshot is the expensive part.** jj has no `add`: its first command in a 100k-file tree hashes and writes every file.
  That took a median of 153 s (flat), 406 s (deep) and 113 s (binary), almost all of it single-threaded CPU (118, 409 and 113 s CPU).
  `git add .` took a median of 31 s, 10 s and 18 s, and `vcs add .` 1.3 s, 2.3 s and 5.5 s on this box. The git flat wall time is
  well above its CPU time (31 s wall vs 9.9 s CPU) and the runs ranged from 9 to 36 s, i.e. this cell had disk-write noise from neighbours; the
  first run of several vcs and git cells was also slower than later ones (cold cache). Medians, not minima, are shown.
* **After the snapshot jj is fast on a clean tree.** `jj status` on a clean flat or binary tree: 96 to 97 ms wall (but 525 ms CPU, it
  uses several threads), which is below git (113 ms) and vcs (141 ms). On the deep layout jj is 991 ms vs git 938 ms and vcs 476 ms.
* **A dirty tree costs jj more.** With 1% modified: jj 701 to 706 ms (flat, binary) and 2,136 ms (deep), against 120 to 143 ms for git and vcs on flat and binary, and 467 and 958 ms for vcs and git on deep.
  The jj cell includes writing the snapshot (a new commit and operation) for the 1,000 changed files, which git and vcs `status` do not do.
* **log and op log**: jj is 100 ms (flat, binary) and 1 s (deep) per command, because each jj command first checks the working copy
  for changes; git is 4 to 6 ms and vcs 10 to 16 ms (log) and 57 to 98 ms (op log). This is per-command overhead, not log traversal.
* **commit** (after the add/snapshot): jj 149 to 153 ms (flat, binary) and 1,040 ms (deep) versus git 548 to 566 ms (13,423 ms on deep) and vcs 397 to 450 ms (1,173 ms on deep). The cells are not the same work: the expensive jj step is in `add`. Compare sums: add + commit is about 153.6 s (jj), 31.6 s (git) and 1.7 s (vcs) on flat.
* **checkout (1% differ)**: flat and binary: git 343 to 352 ms, jj 380 to 382 ms, vcs 540 to 541 ms. Deep: git 476, vcs 579, jj 1,442 ms.
* **undo**: `jj undo` 130 ms (flat, binary) and 1,025 ms (deep); `vcs op undo` 632 ms (flat, binary) and 3,087 ms (deep); `git reset --hard HEAD~1` 268 to 272 ms and 571 ms. `vcs op undo` is the slowest here and is a clear target. The three do different work: jj `undo` restores the repo view and updates the working copy; git `reset --hard` rewrites files; vcs `op undo` restores local branches and workspace.
* **clone** (local, 100k files): flat git 5.3 s, vcs 3.9 s, jj 17.4 s. Binary git 10.5 s, vcs 8.9 s, jj 33.9 s. Deep git 10.1 s, vcs 24.5 s, jj 36.5 s. These clones are not the same kind of copy: git `--no-local` runs pack-objects, jj fetches from a git repo through its own git backend, vcs copies from a published directory remote.
* **disk (store directory only, `du -sk`, 4 KiB blocks)**, after the first commit / after 50 commits, KiB:

| layout | git | vcs | jj |
|---|---|---|---|
| flat | 51,412 / 62,204 | 74,948 / 83,368 | 417,180 / 430,604 |
| deep | 62,980 / 87,724 | 84,268 / 94,532 | 770,212 / 907,020 |
| binary | 414,972 / 425,764 | 420,884 / 429,300 | 817,180 / 830,548 |

  jj's store (`.jj`, including its git backing store) is 2x to 12x the size of git's and vcs's. I did not look inside it; loose
  per-object files rounded to 4 KiB blocks would fit the size (git had packed its objects automatically by that point), but that is an inference, not something I checked.
  `jj util gc` was not run. The "end" sizes after the undo/op history runs are within 1% of the 50-commit sizes for all tools.

## Qualitative comparison

| | Jujutsu 0.46 | allgit |
|---|---|---|
| Working copy | is a commit, snapshotted by every command; no staging area | explicit `add` (index) then `commit`; stat-cache `add`; no auto-snapshot |
| Operation log | every operation is recorded, `jj op log`, `jj undo`, `jj op restore` cover the whole repo view (bookmarks, working copy, rewritten commits) | `vcs op log / show / undo / restore`, **local branches and workspace only**, undone state is recorded as a new operation ([operations.md](../operations.md)) |
| Conflicts | first-class: stored inside commits, a conflicted commit can be committed and resolved later, rebases never stop halfway | merge leaves an in-progress conflict state in the working tree, `resolve` / `--abort`; the structured Conflict object is "designed" in [object-model.md](../object-model.md), not stored in commits |
| Change ids | stable change ids across rewrites, automatic rebase of descendants | `change_id` field on commits kept by `rebase` (linear ranges only); no automatic descendant rebase |
| Branches | bookmarks, no current branch | normal branches, `switch` |
| Storage | git backend (this benchmark) or native; git-compatible remotes | own object format (BLAKE3), zones, packs, CAS manifest remotes; git via import/export and a remote helper |
| Monorepo features | none comparable measured here | zones (sparse subscribe, atomic multi-zone commit), lazy blob fetch, locks (enforced behind `vcs-server`) |
| Maturity | widely used, active upstream | early, single-team, Windows never run |

The jj column is from jj's documented behaviour and the commands used above; I did not test its conflict handling,
rebase or remotes in this study. The allgit column is from this repo's README and docs.

## What was not measured

* A **colocated** jj repo (jj 0.46's default); watchman/fsmonitor for jj; jj's native (non-git) backend.
* Cold-cache runs; multiple machines; networked remotes (clone is local only); `jj git push/fetch`, `jj rebase`, conflicts, `jj util gc` and git gc/repack effects.
* Whether the cause of jj's slow first snapshot is hashing, compression or object writing (no profile was taken).
* Why git's flat `add` has wall time well above CPU time (disk contention is likely, not confirmed); why the vcs `add` is much quicker than in the README table (it is a newer commit; not investigated).
* The `vcs` publish step used before the clone cell, and the undo cells' equivalence (see table above), are not like-for-like.
* Memory use, and any dataset other than datagen seed 42 at 100,000 files.

## Caveats and history of this run

* The first attempt ran on AWS instance "b", which was supposed to be quiet but had load1 13 to 32 and 45 to 59% iowait from other jobs. Its first cell produced a `git add` of 905 s in one run, so that attempt was killed and discarded. All numbers above are from instance "a".
* The first full pass on "a" used `jj git clone` with its default colocated layout, which put the git store in `.git`, outside the measured `.jj`. Flat and binary clone cells were re-run with `--no-colocate`; the deep layout already used the fixed script. Every other cell of flat and binary comes from the first pass.
* The wall-clock timer includes process start. `jj` and `git` are single binaries; the `vcs` release binary was built on instance "b" and copied.
* jj tools use several threads for `status`, so CPU time can exceed wall time; compare both columns.

