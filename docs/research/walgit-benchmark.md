# walgit vs vcs vs plain git on remote workflows, full matrix re-run (speed first)

Status: measured 2026-11-01. **Machine: Lambda box (`lambda-final`, AMD EPYC 7J13, 30 cores, kernel 6.8.0-1046-nvidia, ext4), shared with other agents: load1 at the start of each dataset is 10.5 (flat), 19.7 (deep), 30.4 (binary) and went up to 27 during runs. vcs build: origin/main `6ef5547` built at 15:14 UTC, i.e. BEFORE #343-#353.** git 2.56.0. walgit `80e9a20b29e2` (tobi/walgit, release, all roles, auth none, TLS off). RustFS 1.0.4, single node, loopback. Nothing here is a statement about AWS S3 or a real network.

This supersedes the earlier run (kept at the bottom as "Earlier run"), which used a vcs build older than the clone/fetch fixes (#320, #322, #334), pack v2 default, v3/v4, tiered compaction and the lazy index.

**Not run (stated up front):** the 100k datasets are NOT in this document (the 100k part of this matrix was stopped on request; flat 100k is being run on a quieter box with the current build by another agent). Cloud S3, TLS, authenticated modes, concurrent clients, multiple walgit instances, LFS. Only 10k files per layout, 5 reps, 5 fresh remotes for the initial push. `vcs fsck` after repack was run on the repacked copies (all passed, flagged in the maintenance tables if not).

## 2026-11-01 (evening): quiet box, current vcs build, flat 100k files

**Machine: AWS c6i.4xlarge ("box c"), Intel Xeon Platinum 8375C, 16 vCPU, kernel 7.0.0-1014-aws, dedicated to benchmarks (all timing under `flock ~/bench.lock`), load1 1.33 at start and 1.19 at end of the timing pass (load is recorded per dataset, not per cell).** Object store: local RustFS 1.0.4, loopback (real S3 credentials were expired; nothing here says anything about AWS S3 or a real network). vcs: origin/main `84f3fa2` (release build). walgit: tobi/walgit `80e9a20b29e2` (release, same commit as the Lambda run). **git 2.56.0** (see note below). Driver: `scripts/walgit_bench/walgit_full.py`, 3 reps, 3 fresh remotes for the initial push, all arms interleaved against one RustFS, then a 1-rep counting pass. Raw JSON and logs: `docs/research/data/walgit-full-boxc/`.

Labels: `vcs default` = no settings; `vcs best-documented` = `VCS_PACK_FORMAT=v3`; `vcs fastest-config` = `VCS_PACK_FORMAT=v1` (raw packs), the fastest documented pack setting from the earlier run, not a tuned build.

**Scope, stated plainly.** Only flat 100k was run on box c. Deep 100k and binary 100k were NOT run. The 10k results for all three layouts are the earlier Lambda-box run below (Lambda box, load ~20, vcs build from 15:14 UTC, before #343-#353); they are not comparable cell by cell to this section (different box, different vcs build, 10k vs 100k).

**Honest notes on how this run was made.**
* box c has git 2.43.0 from apt. walgit-server runs git with `pack.allowPackReuse=multi`, which git 2.43.0 rejects ("bad boolean config value 'multi'"), so the first attempt failed at the first walgit clone. I copied the git 2.56.0 binary and its `git-core` from the Lambda box into `~/rt/walgitc-env/git256/` and put it first on PATH for the whole run, so **every git/walgit arm here uses git 2.56.0**, as in the Lambda run. box c's own git 2.43.0 was not benchmarked. No system package was installed.
* A first attempt also failed because `VCS_AUTHOR_NAME/EMAIL` were not set on box c; the harness now sets them.
* Interleaved per rep as in the Lambda run, but only 3 reps: read the min-max, not just the medians. Ratios use a 5% tie band, not a significance test.
* "vcs fastest-config" is just pack v1; no other speed knob was tested (`VCS_REMOTE_PARALLEL` is a documented knob, default 8; not varied here).

### Result in four lines (flat 100k, box c)
* **Speed (wall and CPU): vcs beats walgit in all 36 wall/CPU cells for all three vcs columns except ties (default [35 win, 1 tie, 0 LOSS]; best-doc [35, 1, 0]; fastest [34, 2, 0]).** Initial push 788 ms vs 8398 ms (about 11x); full clone warm 2174 vs 3392 ms, cold 1241 vs 3735 ms; zone clone 414 vs 814 ms (warm); cold fetch of 1 commit 218 vs 1206 ms.
* **vs plain git (`file://`, a lower bound with no HTTP):** vcs is faster in every wall cell except warm fetch of 1 commit (git 84 ms (72-434) vs vcs default 85, best-doc 99, fastest 100 ms) and the same after maintenance (git 50 ms vs vcs 54 / 53 / 58): git is ahead by about 10% to 20% there on medians, with wide min-max.
* **Where walgit still beats vcs: object-store request counts, server memory, store traffic and space, not time.** Details in the three loss lists below. Warm full clone is 4 store GETs for walgit against 28 (default) or 14 (pack v3) for vcs-server.
* **Space (secondary):** vcs client store is 78.2 MiB against 51.0 for git and walgit (1.5x); vcs remote store 46.6 MiB (pack v3: 45.6, raw v1: 66.8) against 42.1 walgit and 42.5 git. walgit's store after its forced maintenance is 127.4 MiB (3x its pre-maintenance size) because old packs are not removed in the same pass.

### Flat 100k tables and scoreboards

### Cells where walgit or git still beats vcs, one list

Speed (wall or CPU): none against walgit (0 LOSS in 36 cells; ties: see Scoreboard 1). Against plain git: warm `fetch 1 commit` and warm `after maint: fetch 1 commit`, wall, for vcs best-doc and fastest (default is a tie in the first; 54 vs 50 ms after maint). All other wall cells are vcs wins vs git.
Object-store requests, memory, traffic, space (secondary): the lists under Scoreboards 1 (request rows), 2 and 3 below; in short walgit/git win on warm-clone requests (4 vs 14 to 31), 1-file push requests (7 vs 12-13), warm store traffic (0 MiB, local cache), full-clone server RSS for pack v3 (104 vs 76 MiB), client store size (git/walgit 51 MiB vs vcs 78 MiB), and remote store size after the initial push (walgit 42.1 vs vcs 45.6 to 66.8 MiB).

### Tables (generated by `report_full.py`)


#### flat 100000 files (57.2 MiB, g0 = 10000 files), reps 3, init reps 3, load1 start 1.33, end 1.19

Wall ms, median (min-max). CPU = client + front-server + RustFS CPU seconds (sum of medians). srv RSS = peak VmHWM of walgit-server / vcs-server in MiB.

| op | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| initial push | 6554 (6453-6824); cpu 10.54s | 8398 (7683-10159); cpu 11.07s; rss 145 | 788 (787-800); cpu 1.97s; rss 67 | 750 (700-751); cpu 0.98s; rss 67 | 871 (844-1422); cpu 0.87s; rss 67 |
| 1-file push | 46 (46-51); cpu 0.04s | 93 (80-105); cpu 0.05s; rss 125 | 40 (38-41); cpu 0.02s; rss 12 | 38 (37-43); cpu 0.02s; rss 12 | 44 (42-54); cpu 0.02s; rss 12 |
| full clone, warm | 4023 (3389-4326); cpu 3.96s | 3392 (3257-3484); cpu 3.68s; rss 76 | 2174 (1966-3062); cpu 3.49s; rss 57 | 2329 (1283-3654); cpu 3.24s; rss 104 | 2521 (2143-3596); cpu 3.04s; rss 80 |
| full clone, cold | 3654 (3626-3697); cpu 3.90s | 3735 (3702-3774); cpu 3.83s; rss 146 | 1241 (1225-1254); cpu 3.24s; rss 45 | 1309 (1306-1350); cpu 3.01s; rss 82 | 1323 (1252-1633); cpu 2.88s; rss 63 |
| partial/zone clone, warm | 1266 (969-1387); cpu 0.98s | 814 (796-1269); cpu 0.87s; rss 126 | 414 (286-618); cpu 0.51s; rss 57 | 421 (352-605); cpu 0.51s; rss 64 | 310 (257-551); cpu 0.44s; rss 72 |
| partial/zone clone, cold | 1387 (1357-1398); cpu 0.86s | 1471 (1459-1484); cpu 1.16s; rss 141 | 392 (392-417); cpu 0.47s; rss 13 | 534 (524-569); cpu 0.46s; rss 22 | 409 (409-450); cpu 0.41s; rss 16 |
| fetch 1 commit, warm | 84 (72-434); cpu 0.07s | 101 (58-565); cpu 0.07s; rss 125 | 85 (44-142); cpu 0.03s; rss 21 | 99 (97-100); cpu 0.02s; rss 12 | 100 (96-101); cpu 0.04s; rss 16 |
| fetch 1 commit, cold | 710 (522-717); cpu 0.08s | 1206 (1026-1210); cpu 0.46s; rss 162 | 218 (215-224); cpu 0.03s; rss 4 | 199 (188-232); cpu 0.03s; rss 4 | 222 (205-229); cpu 0.04s; rss 4 |
| no-op fetch, warm | 14 (14-94); cpu 0.01s | 19 (19-21); cpu 0.02s; rss 59 | 5 (5-113); cpu 0.00s; rss 4 | 6 (5-24); cpu 0.00s; rss 4 | 5 (5-25); cpu 0.00s; rss 4 |
| no-op fetch, cold | 187 (131-194); cpu 0.02s | 383 (372-390); cpu 0.15s; rss 135 | 122 (104-126); cpu 0.01s; rss 4 | 121 (119-123); cpu 0.01s; rss 4 | 120 (118-121); cpu 0.01s; rss 4 |
| after maint: full clone, warm | 3178 (3116-4281); cpu 3.43s | 3174 (3172-4200); cpu 3.48s; rss 137 | 1358 (1101-3444); cpu 3.20s; rss 49 | 2195 (1182-3349); cpu 3.04s; rss 104 | 2163 (1110-3355); cpu 2.95s; rss 66 |
| after maint: full clone, cold | 3211 (3193-3388); cpu 3.39s | 3676 (3667-3725); cpu 3.79s; rss 153 | 1386 (1205-2035); cpu 3.35s; rss 43 | 1358 (1357-1502); cpu 3.06s; rss 82 | 1753 (1278-1812); cpu 2.91s; rss 61 |
| after maint: partial/zone clone, warm | 727 (724-1043); cpu 0.74s | 788 (786-789); cpu 0.81s; rss 124 | 268 (240-376); cpu 0.44s; rss 55 | 333 (332-388); cpu 0.44s; rss 67 | 245 (236-264); cpu 0.40s; rss 71 |
| after maint: partial/zone clone, cold | 1327 (1313-1332); cpu 0.76s | 1419 (1409-1435); cpu 1.17s; rss 159 | 408 (381-418); cpu 0.46s; rss 13 | 534 (498-536); cpu 0.49s; rss 22 | 406 (403-425); cpu 0.40s; rss 16 |
| after maint: fetch 1 commit, warm | 50 (49-562); cpu 0.05s | 56 (56-128); cpu 0.05s; rss 135 | 54 (37-106); cpu 0.02s; rss 19 | 53 (37-97); cpu 0.02s; rss 12 | 58 (42-106); cpu 0.03s; rss 24 |
| after maint: fetch 1 commit, cold | 720 (703-724); cpu 0.08s | 797 (784-799); cpu 0.45s; rss 166 | 219 (195-230); cpu 0.03s; rss 4 | 228 (220-231); cpu 0.03s; rss 4 | 233 (225-237); cpu 0.04s; rss 4 |
| after maint: no-op fetch, warm | 18 (16-96); cpu 0.02s | 20 (19-21); cpu 0.02s; rss 57 | 6 (5-106); cpu 0.00s; rss 4 | 6 (5-23); cpu 0.00s; rss 4 | 6 (5-26); cpu 0.00s; rss 4 |
| after maint: no-op fetch, cold | 198 (145-200); cpu 0.02s | 373 (365-374); cpu 0.15s; rss 143 | 121 (93-122); cpu 0.01s; rss 4 | 119 (115-120); cpu 0.01s; rss 4 | 122 (118-123); cpu 0.01s; rss 4 |

Sizes (MiB; du = allocated blocks, app = apparent bytes). Side effect only: speed is the primary goal.

| size | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| remote store after initial push (du / app) | 42.5 / 42.4 | 42.1 / 42.0 | 46.6 / 46.5 | 45.6 / 45.6 | 66.8 / 66.8 |
| remote store after maintenance (du / app) | 43.0 / 42.9 | 127.4 / 126.9 | 46.7 / 46.6 | 45.8 / 45.6 | 67.0 / 66.9 |
| full clone: client store (.git / .vcs) | 51.0 | 51.0 | 78.2 | 78.2 | 78.2 |
| partial clone: files / client store | 10000 / 15.8 | 10000 / 15.8 | 10000 / 11.7 | 10000 / 11.7 | 10000 / 11.7 |

Maintenance (time ms / total CPU s / result size MiB):

| arm | remote maintenance | client maintenance variants |
|---|---|---|
| git (local bare) | gc_default: 1053 ms / 0.8 s; gc_aggressive: 9452 ms / 92.3 s -> 43.0; before: 42.9, after: 43.0 | before 51.2; gc: 532 ms / 51.0; gc_aggressive: 9511 ms / 51.0 |
| walgit | ops_compact_force_base: 13313 ms / 96.2 s; after maintainer settle: 84.9; before: 84.9, after: 127.4 | before 51.3; gc: 524 ms / 51.0; gc_aggressive: 9446 ms / 51.0 |
| vcs default | compact: 371 ms / 0.1 s; before: 46.8, after: 46.7 | before 78.4; repack_default: 35 ms / 78.4; repack_v2: 20 ms / 78.4; repack_v3: 440 ms / 57.2; repack_v4: 1223 ms / 57.7 |
| vcs best-documented (pack v3) | compact: 412 ms / 0.1 s; before: 45.9, after: 45.8 | before 78.4; repack_v3: 405 ms / 57.2; repack_v4: 1225 ms / 57.7 |
| vcs fastest-config (pack v1, raw) | compact: 706 ms / 0.1 s; before: 67.1, after: 67.0 | before 78.4; repack_v1: 437 ms / 78.3 |

Object-store requests / payload bytes (counting proxy, 1 run per op; GET+PUT+other, MiB up/down):

| op | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|
| initial push | 11 (6/5/0) 42.1 / 0.0 | 6 (2/4/0) 46.5 / 0.0 | 6 (2/4/0) 45.6 / 0.0 | 6 (2/4/0) 66.8 / 0.0 |
| 1-file push | 7 (3/4/0) 0.0 / 0.0 | 13 (8/4/1) 0.0 / 4.3 | 13 (8/4/1) 0.0 / 4.3 | 12 (7/4/1) 0.0 / 0.0 |
| full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 28 (28/0/0) 0.0 / 46.5 | 14 (14/0/0) 0.0 / 45.6 | 29 (29/0/0) 0.0 / 66.8 |
| full clone, cold | 13 (13/0/0) 0.0 / 42.0 | 28 (28/0/0) 0.0 / 46.5 | 14 (14/0/0) 0.0 / 45.6 | 29 (29/0/0) 0.0 / 66.8 |
| partial/zone clone, warm | 7 (7/0/0) 0.0 / 0.0 | 24 (24/0/0) 0.0 / 11.8 | 18 (10/6/2) 0.0 / 12.9 | 25 (25/0/0) 0.0 / 14.8 |
| partial/zone clone, cold | 16 (16/0/0) 0.0 / 42.0 | 24 (24/0/0) 0.0 / 11.8 | 8 (8/0/0) 0.0 / 12.9 | 25 (25/0/0) 0.0 / 14.8 |
| fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| fetch 1 commit, cold | 19 (19/0/0) 0.0 / 42.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| no-op fetch, warm | 2 (2/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| no-op fetch, cold | 16 (16/0/0) 0.0 / 10.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 31 (30/0/1) 0.0 / 46.5 | 14 (14/0/0) 0.0 / 45.6 | 29 (29/0/0) 0.0 / 66.8 |
| after maint: full clone, cold | 22 (20/0/2) 0.0 / 42.4 | 27 (27/0/0) 0.0 / 46.5 | 14 (14/0/0) 0.0 / 45.6 | 29 (29/0/0) 0.0 / 66.8 |
| after maint: partial/zone clone, warm | 7 (7/0/0) 0.0 / 0.0 | 24 (24/0/0) 0.0 / 11.8 | 8 (8/0/0) 0.0 / 12.9 | 25 (25/0/0) 0.0 / 14.8 |
| after maint: partial/zone clone, cold | 25 (23/0/2) 0.0 / 42.4 | 24 (24/0/0) 0.0 / 11.8 | 8 (8/0/0) 0.0 / 12.9 | 25 (25/0/0) 0.0 / 14.8 |
| after maint: fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: fetch 1 commit, cold | 28 (26/0/2) 0.0 / 42.4 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, warm | 2 (2/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, cold | 25 (23/0/2) 0.0 / 10.4 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |

walgit idle (maintainer loop, one repo, 65 s): 21 store requests; vcs-server idle: 0 by construction (stateless; not separately counted).

#### Scoreboard 1: speed (wall, CPU, store requests)

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| flat 100000 | initial push wall ms | 8398 | 788 | 0.09x | win | 750 | 0.09x | win | 871 | 0.10x | win |
| flat 100000 | initial push total CPU s | 11.07 | 1.97 | 0.18x | win | 0.98 | 0.09x | win | 0.87 | 0.08x | win |
| flat 100000 | 1-file push wall ms | 93.29 | 40.21 | 0.43x | win | 38.43 | 0.41x | win | 44.06 | 0.47x | win |
| flat 100000 | 1-file push total CPU s | 0.05 | 0.02 | 0.46x | win | 0.02 | 0.47x | win | 0.02 | 0.46x | win |
| flat 100000 | full clone, warm wall ms | 3392 | 2174 | 0.64x | win | 2329 | 0.69x | win | 2521 | 0.74x | win |
| flat 100000 | full clone, warm total CPU s | 3.68 | 3.49 | 0.95x | win | 3.24 | 0.88x | win | 3.04 | 0.83x | win |
| flat 100000 | full clone, cold wall ms | 3735 | 1241 | 0.33x | win | 1309 | 0.35x | win | 1323 | 0.35x | win |
| flat 100000 | full clone, cold total CPU s | 3.83 | 3.24 | 0.85x | win | 3.01 | 0.79x | win | 2.88 | 0.75x | win |
| flat 100000 | partial/zone clone, warm wall ms | 814 | 414 | 0.51x | win | 421 | 0.52x | win | 310 | 0.38x | win |
| flat 100000 | partial/zone clone, warm total CPU s | 0.87 | 0.51 | 0.59x | win | 0.51 | 0.59x | win | 0.44 | 0.51x | win |
| flat 100000 | partial/zone clone, cold wall ms | 1471 | 392 | 0.27x | win | 534 | 0.36x | win | 409 | 0.28x | win |
| flat 100000 | partial/zone clone, cold total CPU s | 1.16 | 0.47 | 0.41x | win | 0.46 | 0.39x | win | 0.41 | 0.35x | win |
| flat 100000 | fetch 1 commit, warm wall ms | 101 | 84.72 | 0.84x | win | 98.98 | 0.98x | tie | 99.62 | 0.99x | tie |
| flat 100000 | fetch 1 commit, warm total CPU s | 0.07 | 0.03 | 0.43x | win | 0.02 | 0.32x | win | 0.04 | 0.52x | win |
| flat 100000 | fetch 1 commit, cold wall ms | 1206 | 218 | 0.18x | win | 199 | 0.16x | win | 222 | 0.18x | win |
| flat 100000 | fetch 1 commit, cold total CPU s | 0.46 | 0.03 | 0.07x | win | 0.03 | 0.07x | win | 0.04 | 0.08x | win |
| flat 100000 | no-op fetch, warm wall ms | 19.08 | 5.32 | 0.28x | win | 5.84 | 0.31x | win | 5.24 | 0.27x | win |
| flat 100000 | no-op fetch, warm total CPU s | 0.02 | 0.00 | 0.17x | win | 0.00 | 0.17x | win | 0.00 | 0.16x | win |
| flat 100000 | no-op fetch, cold wall ms | 383 | 122 | 0.32x | win | 121 | 0.32x | win | 120 | 0.31x | win |
| flat 100000 | no-op fetch, cold total CPU s | 0.15 | 0.01 | 0.05x | win | 0.01 | 0.05x | win | 0.01 | 0.05x | win |
| flat 100000 | after maint: full clone, warm wall ms | 3174 | 1358 | 0.43x | win | 2195 | 0.69x | win | 2163 | 0.68x | win |
| flat 100000 | after maint: full clone, warm total CPU s | 3.48 | 3.20 | 0.92x | win | 3.04 | 0.87x | win | 2.95 | 0.85x | win |
| flat 100000 | after maint: full clone, cold wall ms | 3676 | 1386 | 0.38x | win | 1358 | 0.37x | win | 1753 | 0.48x | win |
| flat 100000 | after maint: full clone, cold total CPU s | 3.79 | 3.35 | 0.88x | win | 3.06 | 0.81x | win | 2.91 | 0.77x | win |
| flat 100000 | after maint: partial/zone clone, warm wall ms | 788 | 268 | 0.34x | win | 333 | 0.42x | win | 245 | 0.31x | win |
| flat 100000 | after maint: partial/zone clone, warm total CPU s | 0.81 | 0.44 | 0.54x | win | 0.44 | 0.54x | win | 0.40 | 0.49x | win |
| flat 100000 | after maint: partial/zone clone, cold wall ms | 1419 | 408 | 0.29x | win | 534 | 0.38x | win | 406 | 0.29x | win |
| flat 100000 | after maint: partial/zone clone, cold total CPU s | 1.17 | 0.46 | 0.39x | win | 0.49 | 0.42x | win | 0.40 | 0.35x | win |
| flat 100000 | after maint: fetch 1 commit, warm wall ms | 56.31 | 53.78 | 0.95x | tie | 52.75 | 0.94x | win | 57.58 | 1.02x | tie |
| flat 100000 | after maint: fetch 1 commit, warm total CPU s | 0.05 | 0.02 | 0.36x | win | 0.02 | 0.36x | win | 0.03 | 0.63x | win |
| flat 100000 | after maint: fetch 1 commit, cold wall ms | 797 | 219 | 0.27x | win | 228 | 0.29x | win | 233 | 0.29x | win |
| flat 100000 | after maint: fetch 1 commit, cold total CPU s | 0.45 | 0.03 | 0.07x | win | 0.03 | 0.07x | win | 0.04 | 0.08x | win |
| flat 100000 | after maint: no-op fetch, warm wall ms | 20.36 | 6.31 | 0.31x | win | 5.61 | 0.28x | win | 5.65 | 0.28x | win |
| flat 100000 | after maint: no-op fetch, warm total CPU s | 0.02 | 0.00 | 0.20x | win | 0.00 | 0.18x | win | 0.00 | 0.18x | win |
| flat 100000 | after maint: no-op fetch, cold wall ms | 373 | 121 | 0.33x | win | 119 | 0.32x | win | 122 | 0.33x | win |
| flat 100000 | after maint: no-op fetch, cold total CPU s | 0.15 | 0.01 | 0.05x | win | 0.01 | 0.05x | win | 0.01 | 0.05x | win |
| flat 100000 | initial push store requests | 11.00 | 6.00 | 0.55x | win | 6.00 | 0.55x | win | 6.00 | 0.55x | win |
| flat 100000 | 1-file push store requests | 7.00 | 13.00 | 1.86x | LOSS | 13.00 | 1.86x | LOSS | 12.00 | 1.71x | LOSS |
| flat 100000 | full clone, warm store requests | 4.00 | 28.00 | 7.00x | LOSS | 14.00 | 3.50x | LOSS | 29.00 | 7.25x | LOSS |
| flat 100000 | full clone, cold store requests | 13.00 | 28.00 | 2.15x | LOSS | 14.00 | 1.08x | LOSS | 29.00 | 2.23x | LOSS |
| flat 100000 | partial/zone clone, warm store requests | 7.00 | 24.00 | 3.43x | LOSS | 18.00 | 2.57x | LOSS | 25.00 | 3.57x | LOSS |
| flat 100000 | partial/zone clone, cold store requests | 16.00 | 24.00 | 1.50x | LOSS | 8.00 | 0.50x | win | 25.00 | 1.56x | LOSS |
| flat 100000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| flat 100000 | fetch 1 commit, cold store requests | 19.00 | 5.00 | 0.26x | win | 4.00 | 0.21x | win | 5.00 | 0.26x | win |
| flat 100000 | no-op fetch, warm store requests | 2.00 | 1.00 | 0.50x | win | 1.00 | 0.50x | win | 1.00 | 0.50x | win |
| flat 100000 | no-op fetch, cold store requests | 16.00 | 1.00 | 0.06x | win | 1.00 | 0.06x | win | 1.00 | 0.06x | win |
| flat 100000 | after maint: full clone, warm store requests | 4.00 | 31.00 | 7.75x | LOSS | 14.00 | 3.50x | LOSS | 29.00 | 7.25x | LOSS |
| flat 100000 | after maint: full clone, cold store requests | 22.00 | 27.00 | 1.23x | LOSS | 14.00 | 0.64x | win | 29.00 | 1.32x | LOSS |
| flat 100000 | after maint: partial/zone clone, warm store requests | 7.00 | 24.00 | 3.43x | LOSS | 8.00 | 1.14x | LOSS | 25.00 | 3.57x | LOSS |
| flat 100000 | after maint: partial/zone clone, cold store requests | 25.00 | 24.00 | 0.96x | tie | 8.00 | 0.32x | win | 25.00 | 1.00x | tie |
| flat 100000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| flat 100000 | after maint: fetch 1 commit, cold store requests | 28.00 | 5.00 | 0.18x | win | 4.00 | 0.14x | win | 5.00 | 0.18x | win |
| flat 100000 | after maint: no-op fetch, warm store requests | 2.00 | 1.00 | 0.50x | win | 1.00 | 0.50x | win | 1.00 | 0.50x | win |
| flat 100000 | after maint: no-op fetch, cold store requests | 25.00 | 1.00 | 0.04x | win | 1.00 | 0.04x | win | 1.00 | 0.04x | win |

Totals over 54 cells (win / tie / LOSS): vcs default [42, 2, 10]; vcs best-doc [45, 3, 6]; vcs fastest [41, 3, 10].

##### Cells where walgit still beats vcs (scoreboard 1: speed (wall, cpu, store requests))

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| flat 100000 | 1-file push store requests | 7.00 | 13.00 | 13.00 | 12.00 | default, best-doc, fastest |
| flat 100000 | full clone, warm store requests | 4.00 | 28.00 | 14.00 | 29.00 | default, best-doc, fastest |
| flat 100000 | full clone, cold store requests | 13.00 | 28.00 | 14.00 | 29.00 | default, best-doc, fastest |
| flat 100000 | partial/zone clone, warm store requests | 7.00 | 24.00 | 18.00 | 25.00 | default, best-doc, fastest |
| flat 100000 | partial/zone clone, cold store requests | 16.00 | 24.00 | 8.00 | 25.00 | default, fastest |
| flat 100000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| flat 100000 | after maint: full clone, warm store requests | 4.00 | 31.00 | 14.00 | 29.00 | default, best-doc, fastest |
| flat 100000 | after maint: full clone, cold store requests | 22.00 | 27.00 | 14.00 | 29.00 | default, fastest |
| flat 100000 | after maint: partial/zone clone, warm store requests | 7.00 | 24.00 | 8.00 | 25.00 | default, best-doc, fastest |
| flat 100000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |

#### Scoreboard 2: server memory and object-store traffic

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| flat 100000 | initial push server RSS MiB | 145 | 67.39 | 0.46x | win | 67.43 | 0.46x | win | 67.45 | 0.46x | win |
| flat 100000 | 1-file push server RSS MiB | 125 | 12.46 | 0.10x | win | 12.49 | 0.10x | win | 12.21 | 0.10x | win |
| flat 100000 | full clone, warm server RSS MiB | 75.55 | 57.04 | 0.76x | win | 104 | 1.37x | LOSS | 79.82 | 1.06x | LOSS |
| flat 100000 | full clone, cold server RSS MiB | 146 | 45.04 | 0.31x | win | 82.20 | 0.56x | win | 62.95 | 0.43x | win |
| flat 100000 | partial/zone clone, warm server RSS MiB | 126 | 57.04 | 0.45x | win | 64.48 | 0.51x | win | 72.26 | 0.57x | win |
| flat 100000 | partial/zone clone, cold server RSS MiB | 141 | 13.38 | 0.09x | win | 22.14 | 0.16x | win | 15.93 | 0.11x | win |
| flat 100000 | fetch 1 commit, warm server RSS MiB | 125 | 21.40 | 0.17x | win | 12.50 | 0.10x | win | 15.99 | 0.13x | win |
| flat 100000 | fetch 1 commit, cold server RSS MiB | 162 | 3.61 | 0.02x | win | 3.67 | 0.02x | win | 3.67 | 0.02x | win |
| flat 100000 | no-op fetch, warm server RSS MiB | 58.84 | 3.65 | 0.06x | win | 3.61 | 0.06x | win | 3.59 | 0.06x | win |
| flat 100000 | no-op fetch, cold server RSS MiB | 135 | 3.60 | 0.03x | win | 3.60 | 0.03x | win | 3.59 | 0.03x | win |
| flat 100000 | after maint: full clone, warm server RSS MiB | 137 | 49.33 | 0.36x | win | 104 | 0.76x | win | 66.09 | 0.48x | win |
| flat 100000 | after maint: full clone, cold server RSS MiB | 153 | 43.20 | 0.28x | win | 82.14 | 0.54x | win | 61.49 | 0.40x | win |
| flat 100000 | after maint: partial/zone clone, warm server RSS MiB | 124 | 54.95 | 0.44x | win | 66.66 | 0.54x | win | 70.79 | 0.57x | win |
| flat 100000 | after maint: partial/zone clone, cold server RSS MiB | 159 | 13.33 | 0.08x | win | 22.12 | 0.14x | win | 15.86 | 0.10x | win |
| flat 100000 | after maint: fetch 1 commit, warm server RSS MiB | 135 | 18.55 | 0.14x | win | 12.39 | 0.09x | win | 23.73 | 0.18x | win |
| flat 100000 | after maint: fetch 1 commit, cold server RSS MiB | 166 | 3.66 | 0.02x | win | 3.62 | 0.02x | win | 3.67 | 0.02x | win |
| flat 100000 | after maint: no-op fetch, warm server RSS MiB | 56.84 | 3.66 | 0.06x | win | 3.60 | 0.06x | win | 3.66 | 0.06x | win |
| flat 100000 | after maint: no-op fetch, cold server RSS MiB | 143 | 3.57 | 0.02x | win | 3.58 | 0.03x | win | 3.56 | 0.02x | win |
| flat 100000 | initial push store traffic up+down MiB | 42.07 | 46.52 | 1.11x | LOSS | 45.56 | 1.08x | LOSS | 66.77 | 1.59x | LOSS |
| flat 100000 | 1-file push store traffic up+down MiB | 0.01 | 4.26 | 508.12x | LOSS | 4.27 | 508.93x | LOSS | 0.01 | 1.43x | LOSS |
| flat 100000 | full clone, warm store traffic up+down MiB | 0.00 | 46.53 | inf | LOSS | 45.57 | inf | LOSS | 66.77 | inf | LOSS |
| flat 100000 | full clone, cold store traffic up+down MiB | 42.02 | 46.53 | 1.11x | LOSS | 45.57 | 1.08x | LOSS | 66.77 | 1.59x | LOSS |
| flat 100000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 11.78 | inf | LOSS | 12.91 | inf | LOSS | 14.76 | inf | LOSS |
| flat 100000 | partial/zone clone, cold store traffic up+down MiB | 42.02 | 11.78 | 0.28x | win | 12.91 | 0.31x | win | 14.76 | 0.35x | win |
| flat 100000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| flat 100000 | fetch 1 commit, cold store traffic up+down MiB | 42.04 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| flat 100000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| flat 100000 | no-op fetch, cold store traffic up+down MiB | 10.04 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| flat 100000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 46.54 | inf | LOSS | 45.59 | inf | LOSS | 66.79 | inf | LOSS |
| flat 100000 | after maint: full clone, cold store traffic up+down MiB | 42.41 | 46.54 | 1.10x | LOSS | 45.59 | 1.07x | LOSS | 66.79 | 1.57x | LOSS |
| flat 100000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 11.78 | inf | LOSS | 12.91 | inf | LOSS | 14.76 | inf | LOSS |
| flat 100000 | after maint: partial/zone clone, cold store traffic up+down MiB | 42.41 | 11.78 | 0.28x | win | 12.91 | 0.30x | win | 14.76 | 0.35x | win |
| flat 100000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| flat 100000 | after maint: fetch 1 commit, cold store traffic up+down MiB | 42.42 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| flat 100000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| flat 100000 | after maint: no-op fetch, cold store traffic up+down MiB | 10.42 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |

Totals over 36 cells (win / tie / LOSS): vcs default [24, 0, 12]; vcs best-doc [23, 0, 13]; vcs fastest [23, 0, 13].

##### Cells where walgit still beats vcs (scoreboard 2: server memory and object-store traffic)

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| flat 100000 | full clone, warm server RSS MiB | 75.55 | 57.04 | 104 | 79.82 | best-doc, fastest |
| flat 100000 | initial push store traffic up+down MiB | 42.07 | 46.52 | 45.56 | 66.77 | default, best-doc, fastest |
| flat 100000 | 1-file push store traffic up+down MiB | 0.01 | 4.26 | 4.27 | 0.01 | default, best-doc, fastest |
| flat 100000 | full clone, warm store traffic up+down MiB | 0.00 | 46.53 | 45.57 | 66.77 | default, best-doc, fastest |
| flat 100000 | full clone, cold store traffic up+down MiB | 42.02 | 46.53 | 45.57 | 66.77 | default, best-doc, fastest |
| flat 100000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 11.78 | 12.91 | 14.76 | default, best-doc, fastest |
| flat 100000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| flat 100000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| flat 100000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 46.54 | 45.59 | 66.79 | default, best-doc, fastest |
| flat 100000 | after maint: full clone, cold store traffic up+down MiB | 42.41 | 46.54 | 45.59 | 66.79 | default, best-doc, fastest |
| flat 100000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 11.78 | 12.91 | 14.76 | default, best-doc, fastest |
| flat 100000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| flat 100000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |

#### Scoreboard 3: space (secondary)

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| flat 100000 | remote store after initial push MiB (du) | 42.07 | 46.57 | 1.11x | LOSS | 45.62 | 1.08x | LOSS | 66.82 | 1.59x | LOSS |
| flat 100000 | remote store after maintenance MiB (du) | 127 | 46.71 | 0.37x | win | 45.77 | 0.36x | win | 66.98 | 0.53x | win |
| flat 100000 | full-clone client store MiB (du) | 51.01 | 78.24 | 1.53x | LOSS | 78.24 | 1.53x | LOSS | 78.24 | 1.53x | LOSS |
| flat 100000 | partial-clone client store MiB (du) | 15.83 | 11.71 | 0.74x | win | 11.71 | 0.74x | win | 11.71 | 0.74x | win |

Totals over 4 cells (win / tie / LOSS): vcs default [2, 0, 2]; vcs best-doc [2, 0, 2]; vcs fastest [2, 0, 2].

##### Cells where walgit still beats vcs (scoreboard 3: space (secondary))

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| flat 100000 | remote store after initial push MiB (du) | 42.07 | 46.57 | 45.62 | 66.82 | default, best-doc, fastest |
| flat 100000 | full-clone client store MiB (du) | 51.01 | 78.24 | 78.24 | 78.24 | default, best-doc, fastest |

---

## Headline (10k files, all three layouts, 5 reps, medians)

* **Wall time: vcs is faster than walgit in 54 of 54 wall cells for vcs default and for the fastest-config column, and in 52 of 54 for best-documented (pack v3: 1 LOSS, 1 tie)**, over 3 layouts x 18 operations (including the "after maintenance" re-runs). Initial push is about 7x to 10x faster (flat 231 ms vs 2386 ms, binary 576 vs 4191), 1-file push 1.3x to 2x faster, full clone ~2x faster (flat 350 vs 700 ms), fetch of one commit and no-op fetch 1.3x to 5x faster (cold fetch on flat: 212 ms walgit vs 64 ms vcs). Plain `git` over `file://` (no HTTP, no object store; a lower bound, not a like-for-like server) is also beaten by vcs on initial push (up to 10x) and full clone (~2x), and about equal on fetch.
* **Where walgit still wins on speed (details in "Cells where walgit still beats vcs")**: (1) total CPU (client + server + RustFS) of a warm full clone on all layouts: walgit is 7% to 17% lower than vcs default (8 CPU LOSS cells for default, 6 for best-documented, 4 for fastest, out of 54 each) (binary 1.26 s vs 1.53 s, deep 2.13 vs 2.45, flat 0.71 vs 0.76 default); (2) object-store request counts for clone and warm fetch (24 of 54 request cells lost by vcs default, 7 by best-documented v3, 26 by fastest): walgit serves from its local cache, so a warm full clone is 4 store GETs where vcs-server needs 26-28 (10-14 with pack v3); (3) one wall cell: after maintenance, partial/zone clone warm on binary with vcs best-documented (pack v3): 261 ms vs walgit 240 ms.
* **Pack v3 (the "best documented" column) is not the fastest setting.** It reduces store requests (10 vs 27 for a full clone) and size, but costs wall time on partial/zone clones (flat 140 vs 103 ms, binary 294 vs 157 ms warm; reading one object fetches its 4 MiB block) and ~15% on full clone. Default (v2) and the fastest-config column are quicker for clones.
* **The "fastest-config" column (`VCS_PACK_FORMAT=v1`, raw uncompressed packs, client and server) did not beat the default** by more than noise on most cells (it is best on zone clone cold: flat 191 vs 309 ms, partial warm 97 vs 103 ms) and costs space (flat remote 7.0 MiB vs 4.9). No cache-size or mmap knob exists in the v1 CLI/server; "fastest" here means the fastest documented pack setting we could select, not a tuned build. HYPOTHESIS, not tested: more speed needs code changes, not settings.
* **Space (secondary)**: see Scoreboard 3. vcs client stores are 1.4x to 1.5x larger than walgit/git on flat and deep full clones (flat 8.1 vs 5.3 MiB, deep 11.3 vs 7.9). Remote store after the initial push is 9% to 22% larger than walgit on flat/deep (equal on binary), 1.5x to 1.6x larger with the raw-pack setting. walgit's store AFTER its maintenance (forced compaction with base rebuild) is 2x to 3x larger than before, because the old packs are not removed in the same pass (binary 120 MiB vs 40 MiB, flat 9.1 vs 4.3); vcs compaction shrinks the store slightly.

## Setup

| Item | Value |
|---|---|
| Driver | `scripts/walgit_bench/walgit_full.py` (+ `run_all.sh`, `report_full.py`, `s3count_proxy.py`); raw JSON in `docs/research/data/walgit-full/` (`res-*` timing, `cnt-*` counting pass) |
| Interleaving | all five arms run at the same time against one RustFS; for every operation and every rep the arm order rotates, so load drift hits all arms |
| Arms | `git` (bare repo on local disk, `file://`), `walgit` (git over HTTP), `vcs default` (no settings), `vcs best-documented` (`VCS_PACK_FORMAT=v3` on client and server; compaction `--pack-format v3`; `repack --format v3/v4` for client stores), `vcs fastest-config` (`VCS_PACK_FORMAT=v1`, raw packs everywhere) |
| Datasets | `datagen flat|deep|binary 10000`, seed 42, regrouped into g0..g9; history: initial commit + 50 one-file commits; g0 = 1000 files (10%) |
| Metrics | wall ms (client command); CPU = client-tree CPU (rusage of the command and all descendants) + front-server CPU (walgit-server incl. reaped git children / vcs-server) + RustFS CPU, summed medians; server RSS = peak VmHWM of walgit-server / vcs-server (reset before each op; git children of walgit not included); remote store = `du` of the RustFS directory of that repo (`--apparent-size` too); client store = `du` of `.git` / `.vcs`; store requests/bytes from a counting proxy in a separate 1-rep pass |
| Warm / cold | warm = nothing; cold = front server restarted (walgit: cache directory wiped, restarted with `roles=["serve"]` so the maintainer does not re-materialise; vcs-server restarted) AND `drop_caches` before the command |
| Partial clone | git/walgit: `--filter=blob:none --sparse` + `sparse-checkout set g0` (full history, lazy blobs); vcs: `clone --zone g0` (zone history only). Not the same thing |
| Maintenance | git: `git gc` (and `gc --aggressive` on a copy); walgit: wait for its maintainer loop (default config, 60 s interval) to go quiet, then `POST /o/init0/api/ops/compact?force=1&base=1`; vcs: `vcs gc --remote --compact --yes` straight on the S3 prefix (the HTTP server has no maintenance endpoint) with the arm's pack format. Client side on copies of the full clone: git/walgit `git gc`, `gc --aggressive`; vcs `repack` default / `--format v2|v3|v4` (best-documented: v3, v4; fastest: v1). All reads/clones/fetches were then measured again ("after maint") |
| Counting proxy | walgit's maintainer loop runs during the counting pass too (idle: 11 store requests in 65 s with one repo; vcs-server idle is 0 by construction) |

Caveats: shared, loaded box (load1 10 to 30, see above): read the min-max in the tables, not only medians; the interleaving removes drift between arms, not noise inside an arm. `git` over `file://` has no network/HTTP cost and is a lower bound. One RustFS node on local disk: request counts are portable, wall times are not predictive of S3. On this box walgit's cold server pulls the whole pack set (4.2 MiB flat) on first use, which is a cost vcs-server does not have because it keeps no state (but vcs-server then re-reads the store on every clone, see the request tables). Cold server CPU for walgit includes git subprocesses. Scoreboard thresholds: `win`/`LOSS` only if the medians differ by more than 5%, otherwise `tie`; this is a rule of thumb, not a significance test. The `RSS` numbers for walgit exclude its child `git` processes, so they under-state walgit.

## Results per dataset


### binary 10000 files (39.1 MiB, g0 = 1000 files), reps 5, init reps 5, load1 start 30.36, end 18.62

Wall ms, median (min-max). CPU = client + front-server + RustFS CPU seconds (sum of medians). srv RSS = peak VmHWM of walgit-server / vcs-server in MiB.

| op | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| initial push | 3640 (3495-3879); cpu 7.39s | 4191 (4036-4413); cpu 7.93s; rss 100 | 576 (556-679); cpu 0.81s; rss 67 | 617 (532-714); cpu 0.82s; rss 67 | 590 (516-780); cpu 0.59s; rss 67 |
| 1-file push | 50 (37-104); cpu 0.03s | 113 (102-124); cpu 0.07s; rss 93 | 77 (70-104); cpu 0.03s; rss 3 | 80 (72-140); cpu 0.03s; rss 3 | 73 (60-82); cpu 0.03s; rss 3 |
| full clone, warm | 1427 (1225-1879); cpu 1.23s | 1597 (1261-2050); cpu 1.26s; rss 87 | 675 (588-877); cpu 1.53s; rss 43 | 779 (680-836); cpu 1.55s; rss 123 | 617 (543-714); cpu 1.49s; rss 46 |
| full clone, cold | 1121 (977-1582); cpu 1.26s | 1257 (1089-2277); cpu 1.57s; rss 148 | 531 (523-712); cpu 1.62s; rss 40 | 647 (621-699); cpu 1.65s; rss 78 | 555 (530-593); cpu 1.63s; rss 40 |
| partial/zone clone, warm | 228 (202-265); cpu 0.24s | 291 (220-370); cpu 0.28s; rss 114 | 157 (130-191); cpu 0.23s; rss 35 | 294 (236-362); cpu 0.28s; rss 39 | 164 (118-221); cpu 0.23s; rss 39 |
| partial/zone clone, cold | 277 (260-321); cpu 0.26s | 493 (431-720); cpu 0.55s; rss 145 | 216 (189-313); cpu 0.26s; rss 8 | 357 (317-779); cpu 0.31s; rss 20 | 193 (184-282); cpu 0.24s; rss 8 |
| fetch 1 commit, warm | 28 (26-87); cpu 0.03s | 40 (39-89); cpu 0.04s; rss 195 | 25 (24-30); cpu 0.02s; rss 8 | 25 (23-30); cpu 0.02s; rss 5 | 25 (23-27); cpu 0.01s; rss 8 |
| fetch 1 commit, cold | 75 (68-91); cpu 0.05s | 281 (279-354); cpu 0.38s; rss 162 | 58 (51-71); cpu 0.03s; rss 3 | 63 (53-76); cpu 0.03s; rss 3 | 61 (58-97); cpu 0.04s; rss 3 |
| no-op fetch, warm | 13 (12-26); cpu 0.01s | 22 (20-41); cpu 0.02s; rss 300 | 10 (9-14); cpu 0.01s; rss 3 | 10 (8-11); cpu 0.01s; rss 3 | 10 (8-13); cpu 0.01s; rss 3 |
| no-op fetch, cold | 33 (24-42); cpu 0.02s | 111 (89-171); cpu 0.15s; rss 101 | 29 (26-53); cpu 0.01s; rss 3 | 27 (25-29); cpu 0.01s; rss 3 | 32 (25-40); cpu 0.01s; rss 3 |
| after maint: full clone, warm | 1042 (858-1063); cpu 1.14s | 987 (865-1230); cpu 1.24s; rss 157 | 503 (489-773); cpu 1.48s; rss 47 | 608 (557-1147); cpu 1.48s; rss 85 | 493 (460-570); cpu 1.47s; rss 43 |
| after maint: full clone, cold | 965 (877-1532); cpu 1.17s | 1390 (1179-1540); cpu 1.54s; rss 143 | 580 (526-747); cpu 1.63s; rss 43 | 624 (595-728); cpu 1.59s; rss 76 | 549 (509-749); cpu 1.57s; rss 40 |
| after maint: partial/zone clone, warm | 200 (182-257); cpu 0.21s | 240 (216-322); cpu 0.25s; rss 177 | 134 (120-205); cpu 0.21s; rss 41 | 261 (235-313); cpu 0.26s; rss 23 | 132 (126-146); cpu 0.21s; rss 43 |
| after maint: partial/zone clone, cold | 237 (222-290); cpu 0.23s | 440 (435-477); cpu 0.55s; rss 141 | 154 (151-161); cpu 0.22s; rss 8 | 290 (287-365); cpu 0.28s; rss 20 | 153 (147-172); cpu 0.22s; rss 8 |
| after maint: fetch 1 commit, warm | 27 (27-54); cpu 0.03s | 42 (38-65); cpu 0.04s; rss 172 | 28 (24-40); cpu 0.02s; rss 9 | 33 (26-54); cpu 0.01s; rss 5 | 29 (23-44); cpu 0.02s; rss 8 |
| after maint: fetch 1 commit, cold | 79 (70-128); cpu 0.05s | 332 (325-340); cpu 0.39s; rss 160 | 72 (61-79); cpu 0.04s; rss 3 | 67 (58-90); cpu 0.04s; rss 3 | 70 (58-75); cpu 0.04s; rss 3 |
| after maint: no-op fetch, warm | 16 (15-27); cpu 0.01s | 26 (21-54); cpu 0.05s; rss 250 | 13 (12-34); cpu 0.02s; rss 4 | 14 (9-22); cpu 0.01s; rss 3 | 16 (10-24); cpu 0.01s; rss 4 |
| after maint: no-op fetch, cold | 31 (30-36); cpu 0.02s | 124 (100-135); cpu 0.15s; rss 88 | 51 (38-65); cpu 0.01s; rss 3 | 50 (37-58); cpu 0.01s; rss 3 | 43 (33-59); cpu 0.02s; rss 3 |

Sizes (MiB; du = allocated blocks, app = apparent bytes). Side effect only: speed is the primary goal.

| size | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| remote store after initial push (du / app) | 39.9 / 39.8 | 39.8 / 39.8 | 40.3 / 40.3 | 40.3 / 40.3 | 40.5 / 40.5 |
| remote store after maintenance (du / app) | 40.0 / 39.9 | 120.0 / 119.8 | 40.6 / 40.5 | 40.6 / 40.5 | 40.7 / 40.7 |
| full clone: client store (.git / .vcs) | 40.8 | 40.8 | 41.5 | 41.5 | 41.5 |
| partial clone: files / client store | 1000 / 5.3 | 1000 / 5.3 | 1000 / 4.9 | 1000 / 4.9 | 1000 / 4.9 |

Maintenance (time ms / total CPU s / result size MiB):

| arm | remote maintenance | client maintenance variants |
|---|---|---|
| git (local bare) | gc_default: 509 ms / 0.5 s; gc_aggressive: 4205 ms / 66.7 s -> 40.0; before: 40.5, after: 40.0 | before 41.2; gc: 538 ms / 40.8; gc_aggressive: 4295 ms / 40.8 |
| walgit | ops_compact_force_base: 6579 ms / 69.5 s; after maintainer settle: 80.1; before: 80.1, after: 120.0 | before 41.2; gc: 381 ms / 40.8; gc_aggressive: 3878 ms / 40.8 |
| vcs default | compact: 1521 ms / 0.5 s; before: 40.8, after: 40.6 | before 41.8; repack_default: 24 ms / 41.7; repack_v2: 18 ms / 41.7; repack_v3: 427 ms / 41.6; repack_v4: 1600 ms / 41.3 |
| vcs best-documented (pack v3) | compact: 1386 ms / 0.5 s; before: 40.8, after: 40.6 | before 41.8; repack_v3: 258 ms / 41.6; repack_v4: 1438 ms / 41.3 |
| vcs fastest-config (pack v1, raw) | compact: 1911 ms / 0.5 s; before: 40.9, after: 40.7 | before 41.8; repack_v1: 265 ms / 41.7 |

Object-store requests / payload bytes (counting proxy, 1 run per op; GET+PUT+other, MiB up/down):

| op | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|
| initial push | 11 (6/5/0) 39.8 / 0.0 | 6 (2/4/0) 40.3 / 0.0 | 6 (2/4/0) 40.3 / 0.0 | 6 (2/4/0) 40.4 / 0.0 |
| 1-file push | 7 (3/4/0) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 |
| full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 28 (28/0/0) 0.0 / 40.3 | 14 (14/0/0) 0.0 / 40.3 | 28 (28/0/0) 0.0 / 40.4 |
| full clone, cold | 13 (13/0/0) 0.0 / 39.7 | 28 (28/0/0) 0.0 / 40.3 | 14 (14/0/0) 0.0 / 40.3 | 28 (28/0/0) 0.0 / 40.4 |
| partial/zone clone, warm | 8 (8/0/0) 0.0 / 0.0 | 24 (24/0/0) 0.0 / 4.9 | 7 (7/0/0) 0.0 / 8.8 | 24 (24/0/0) 0.0 / 5.0 |
| partial/zone clone, cold | 16 (16/0/0) 0.0 / 39.7 | 24 (24/0/0) 0.0 / 4.9 | 7 (7/0/0) 0.0 / 8.8 | 24 (24/0/0) 0.0 / 5.0 |
| fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| fetch 1 commit, cold | 19 (19/0/0) 0.0 / 39.8 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| no-op fetch, warm | 3 (3/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| no-op fetch, cold | 15 (15/0/0) 0.0 / 0.3 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 27 (27/0/0) 0.0 / 40.3 | 14 (14/0/0) 0.0 / 40.3 | 28 (28/0/0) 0.0 / 40.5 |
| after maint: full clone, cold | 22 (20/0/2) 0.0 / 39.8 | 27 (27/0/0) 0.0 / 40.3 | 14 (14/0/0) 0.0 / 40.3 | 28 (28/0/0) 0.0 / 40.5 |
| after maint: partial/zone clone, warm | 7 (7/0/0) 0.0 / 0.0 | 24 (24/0/0) 0.0 / 4.9 | 7 (7/0/0) 0.0 / 8.8 | 24 (24/0/0) 0.0 / 5.0 |
| after maint: partial/zone clone, cold | 25 (23/0/2) 0.0 / 39.8 | 24 (24/0/0) 0.0 / 4.9 | 7 (7/0/0) 0.0 / 8.8 | 24 (24/0/0) 0.0 / 5.0 |
| after maint: fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: fetch 1 commit, cold | 28 (26/0/2) 0.0 / 39.8 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, warm | 3 (3/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, cold | 24 (22/0/2) 0.0 / 0.6 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |

walgit idle (maintainer loop, one repo, 65 s): 11 store requests; vcs-server idle: 0 by construction (stateless; not separately counted).

### deep 10000 files (5.7 MiB, g0 = 1000 files), reps 5, init reps 5, load1 start 19.69, end 27.00

Wall ms, median (min-max). CPU = client + front-server + RustFS CPU seconds (sum of medians). srv RSS = peak VmHWM of walgit-server / vcs-server in MiB.

| op | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| initial push | 3906 (3376-5151); cpu 3.53s | 5811 (3603-6262); cpu 3.88s; rss 96 | 456 (310-528); cpu 0.52s; rss 11 | 377 (323-622); cpu 0.33s; rss 11 | 345 (322-423); cpu 0.29s; rss 11 |
| 1-file push | 164 (160-224); cpu 0.17s | 163 (104-207); cpu 0.10s; rss 106 | 104 (65-351); cpu 0.03s; rss 3 | 85 (76-146); cpu 0.03s; rss 4 | 72 (50-174); cpu 0.03s; rss 3 |
| full clone, warm | 2586 (2197-3405); cpu 2.20s | 2842 (2251-5336); cpu 2.13s; rss 82 | 1350 (1051-1828); cpu 2.45s; rss 16 | 1347 (1250-1498); cpu 2.40s; rss 23 | 1287 (1003-1905); cpu 2.47s; rss 17 |
| full clone, cold | 3284 (2944-4166); cpu 2.43s | 3632 (2305-5893); cpu 2.45s; rss 105 | 1361 (1039-1947); cpu 2.58s; rss 11 | 1564 (1119-1758); cpu 2.60s; rss 16 | 1424 (1040-1683); cpu 2.57s; rss 12 |
| partial/zone clone, warm | 819 (678-1162); cpu 0.77s | 912 (610-1189); cpu 0.69s; rss 132 | 384 (262-439); cpu 0.33s; rss 16 | 326 (287-431); cpu 0.37s; rss 25 | 363 (270-477); cpu 0.34s; rss 15 |
| partial/zone clone, cold | 835 (624-1040); cpu 0.72s | 1069 (858-1163); cpu 0.96s; rss 105 | 416 (329-431); cpu 0.39s; rss 7 | 423 (340-601); cpu 0.40s; rss 8 | 336 (312-847); cpu 0.37s; rss 7 |
| fetch 1 commit, warm | 88 (61-107); cpu 0.07s | 87 (69-265); cpu 0.08s; rss 155 | 53 (44-58); cpu 0.02s; rss 7 | 56 (48-73); cpu 0.03s; rss 10 | 80 (43-107); cpu 0.03s; rss 7 |
| fetch 1 commit, cold | 119 (114-158); cpu 0.09s | 467 (401-821); cpu 0.40s; rss 111 | 125 (93-776); cpu 0.05s; rss 3 | 131 (104-343); cpu 0.05s; rss 3 | 108 (92-163); cpu 0.05s; rss 4 |
| no-op fetch, warm | 25 (21-55); cpu 0.02s | 33 (31-66); cpu 0.09s; rss 85 | 20 (12-48); cpu 0.01s; rss 3 | 13 (13-20); cpu 0.01s; rss 4 | 13 (11-23); cpu 0.01s; rss 4 |
| no-op fetch, cold | 45 (39-51); cpu 0.03s | 115 (101-126); cpu 0.15s; rss 107 | 41 (37-54); cpu 0.01s; rss 3 | 49 (37-64); cpu 0.02s; rss 3 | 60 (33-67); cpu 0.02s; rss 3 |
| after maint: full clone, warm | 2404 (2146-3701); cpu 2.08s | 3180 (2111-3558); cpu 2.20s; rss 126 | 1157 (1140-1418); cpu 2.48s; rss 16 | 1378 (1141-2317); cpu 2.45s; rss 17 | 1237 (1038-1548); cpu 2.42s; rss 17 |
| after maint: full clone, cold | 2084 (1898-2554); cpu 2.09s | 2239 (2166-2685); cpu 2.40s; rss 116 | 1078 (1001-1101); cpu 2.54s; rss 11 | 1097 (1069-1154); cpu 2.49s; rss 11 | 990 (982-1214); cpu 2.52s; rss 12 |
| after maint: partial/zone clone, warm | 535 (475-580); cpu 0.62s | 599 (525-624); cpu 0.65s; rss 146 | 234 (218-261); cpu 0.32s; rss 13 | 285 (244-303); cpu 0.34s; rss 10 | 248 (203-312); cpu 0.31s; rss 16 |
| after maint: partial/zone clone, cold | 527 (510-626); cpu 0.64s | 774 (741-881); cpu 0.89s; rss 120 | 282 (248-323); cpu 0.37s; rss 7 | 275 (269-419); cpu 0.38s; rss 7 | 279 (261-439); cpu 0.37s; rss 7 |
| after maint: fetch 1 commit, warm | 73 (67-84); cpu 0.07s | 91 (83-107); cpu 0.09s; rss 153 | 42 (33-79); cpu 0.03s; rss 7 | 55 (42-88); cpu 0.03s; rss 4 | 56 (47-67); cpu 0.02s; rss 7 |
| after maint: fetch 1 commit, cold | 128 (112-176); cpu 0.09s | 388 (358-425); cpu 0.39s; rss 131 | 80 (75-110); cpu 0.04s; rss 3 | 84 (78-87); cpu 0.04s; rss 3 | 94 (82-121); cpu 0.04s; rss 3 |
| after maint: no-op fetch, warm | 20 (19-44); cpu 0.02s | 30 (28-41); cpu 0.03s; rss 134 | 9 (9-18); cpu 0.01s; rss 3 | 10 (9-10); cpu 0.01s; rss 3 | 9 (9-15); cpu 0.01s; rss 3 |
| after maint: no-op fetch, cold | 40 (38-60); cpu 0.03s | 105 (81-111); cpu 0.16s; rss 115 | 33 (28-36); cpu 0.01s; rss 3 | 30 (28-31); cpu 0.01s; rss 3 | 33 (28-55); cpu 0.01s; rss 3 |

Sizes (MiB; du = allocated blocks, app = apparent bytes). Side effect only: speed is the primary goal.

| size | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| remote store after initial push (du / app) | 6.2 / 6.2 | 6.0 / 6.0 | 7.0 / 7.0 | 7.4 / 7.3 | 8.9 / 8.9 |
| remote store after maintenance (du / app) | 6.4 / 6.4 | 12.7 / 12.5 | 7.1 / 7.1 | 7.5 / 7.5 | 9.0 / 9.0 |
| full clone: client store (.git / .vcs) | 7.9 | 7.9 | 11.3 | 11.3 | 11.3 |
| partial clone: files / client store | 1000 / 4.4 | 1000 / 4.4 | 1000 / 2.6 | 1000 / 2.6 | 1000 / 2.6 |

Maintenance (time ms / total CPU s / result size MiB):

| arm | remote maintenance | client maintenance variants |
|---|---|---|
| git (local bare) | gc_default: 586 ms / 0.3 s; gc_aggressive: 726 ms / 6.6 s -> 6.4; before: 7.4, after: 6.4 | before 8.8; gc: 499 ms / 8.0; gc_aggressive: 1083 ms / 8.0 |
| walgit | ops_compact_force_base: 1799 ms / 7.8 s; after maintainer settle: 12.7; before: 12.7, after: 12.7 | before 8.8; gc: 620 ms / 8.0; gc_aggressive: 1020 ms / 8.0 |
| vcs default | compact: 1323 ms / 0.5 s; before: 7.3, after: 7.1 | before 12.1; repack_default: 58 ms / 11.7; repack_v2: 62 ms / 11.7; repack_v3: 315 ms / 10.1; repack_v4: 662 ms / 9.8 |
| vcs best-documented (pack v3) | compact: 2134 ms / 0.5 s; before: 7.7, after: 7.5 | before 12.1; repack_v3: 227 ms / 10.1; repack_v4: 618 ms / 9.8 |
| vcs fastest-config (pack v1, raw) | compact: 1934 ms / 0.5 s; before: 9.2, after: 9.0 | before 12.1; repack_v1: 148 ms / 11.7 |

Object-store requests / payload bytes (counting proxy, 1 run per op; GET+PUT+other, MiB up/down):

| op | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|
| initial push | 20 (12/7/1) 6.0 / 0.0 | 6 (2/4/0) 6.9 / 0.0 | 6 (2/4/0) 7.3 / 0.0 | 6 (2/4/0) 8.8 / 0.0 |
| 1-file push | 7 (3/4/0) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 |
| full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 27 (27/0/0) 0.0 / 6.9 | 10 (10/0/0) 0.0 / 7.3 | 28 (28/0/0) 0.0 / 8.8 |
| full clone, cold | 12 (12/0/0) 0.0 / 6.0 | 27 (27/0/0) 0.0 / 6.9 | 10 (10/0/0) 0.0 / 7.3 | 28 (28/0/0) 0.0 / 8.8 |
| partial/zone clone, warm | 10 (8/1/1) 0.0 / 0.0 | 10 (10/0/0) 0.0 / 3.4 | 7 (7/0/0) 0.0 / 7.3 | 11 (11/0/0) 0.0 / 3.6 |
| partial/zone clone, cold | 15 (15/0/0) 0.0 / 6.0 | 10 (10/0/0) 0.0 / 3.4 | 7 (7/0/0) 0.0 / 7.3 | 11 (11/0/0) 0.0 / 3.6 |
| fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| fetch 1 commit, cold | 18 (18/0/0) 0.0 / 6.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| no-op fetch, warm | 2 (2/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 2 (1/1/0) 0.0 / 0.0 | 3 (2/1/0) 0.0 / 0.0 |
| no-op fetch, cold | 15 (15/0/0) 0.0 / 1.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 26 (26/0/0) 0.0 / 6.9 | 10 (10/0/0) 0.0 / 7.3 | 28 (28/0/0) 0.0 / 8.8 |
| after maint: full clone, cold | 23 (20/0/3) 0.0 / 6.1 | 26 (26/0/0) 0.0 / 6.9 | 10 (10/0/0) 0.0 / 7.3 | 28 (28/0/0) 0.0 / 8.8 |
| after maint: partial/zone clone, warm | 7 (7/0/0) 0.0 / 0.0 | 10 (10/0/0) 0.0 / 3.4 | 7 (7/0/0) 0.0 / 7.3 | 11 (11/0/0) 0.0 / 3.6 |
| after maint: partial/zone clone, cold | 26 (23/0/3) 0.0 / 6.1 | 10 (10/0/0) 0.0 / 3.4 | 7 (7/0/0) 0.0 / 7.3 | 11 (11/0/0) 0.0 / 3.6 |
| after maint: fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: fetch 1 commit, cold | 29 (26/0/3) 0.0 / 6.1 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, warm | 5 (3/1/1) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, cold | 22 (19/0/3) 0.0 / 0.3 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |

walgit idle (maintainer loop, one repo, 65 s): 9 store requests; vcs-server idle: 0 by construction (stateless; not separately counted).

### flat 10000 files (5.7 MiB, g0 = 1000 files), reps 5, init reps 5, load1 start 10.51, end 16.52

Wall ms, median (min-max). CPU = client + front-server + RustFS CPU seconds (sum of medians). srv RSS = peak VmHWM of walgit-server / vcs-server in MiB.

| op | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| initial push | 2232 (2064-2460); cpu 2.28s | 2386 (2315-3819); cpu 2.44s; rss 86 | 231 (184-559); cpu 0.27s; rss 11 | 211 (172-540); cpu 0.19s; rss 11 | 300 (168-367); cpu 0.17s; rss 11 |
| 1-file push | 38 (33-163); cpu 0.03s | 156 (103-315); cpu 0.08s; rss 92 | 88 (65-325); cpu 0.03s; rss 4 | 78 (54-138); cpu 0.03s; rss 3 | 86 (71-176); cpu 0.03s; rss 3 |
| full clone, warm | 703 (638-744); cpu 0.77s | 700 (676-997); cpu 0.71s; rss 89 | 350 (307-637); cpu 0.76s; rss 10 | 410 (367-510); cpu 0.71s; rss 21 | 354 (298-1097); cpu 0.71s; rss 10 |
| full clone, cold | 660 (643-791); cpu 0.78s | 775 (757-828); cpu 0.84s; rss 98 | 315 (314-341); cpu 0.75s; rss 8 | 414 (379-436); cpu 0.73s; rss 12 | 320 (313-338); cpu 0.71s; rss 10 |
| partial/zone clone, warm | 164 (161-206); cpu 0.17s | 187 (181-243); cpu 0.18s; rss 136 | 103 (91-108); cpu 0.14s; rss 10 | 140 (137-165); cpu 0.16s; rss 21 | 97 (90-117); cpu 0.11s; rss 10 |
| partial/zone clone, cold | 253 (202-454); cpu 0.22s | 471 (336-890); cpu 0.38s; rss 102 | 309 (164-477); cpu 0.15s; rss 5 | 307 (199-620); cpu 0.20s; rss 9 | 191 (163-407); cpu 0.15s; rss 5 |
| fetch 1 commit, warm | 32 (25-39); cpu 0.03s | 44 (39-100); cpu 0.03s; rss 149 | 33 (29-205); cpu 0.02s; rss 5 | 31 (24-37); cpu 0.02s; rss 5 | 32 (28-57); cpu 0.02s; rss 5 |
| fetch 1 commit, cold | 65 (61-99); cpu 0.04s | 212 (191-257); cpu 0.24s; rss 109 | 64 (60-77); cpu 0.04s; rss 3 | 70 (66-83); cpu 0.04s; rss 3 | 70 (65-84); cpu 0.04s; rss 3 |
| no-op fetch, warm | 16 (14-38); cpu 0.01s | 26 (21-47); cpu 0.02s; rss 138 | 14 (10-21); cpu 0.01s; rss 3 | 14 (10-16); cpu 0.01s; rss 3 | 11 (9-21); cpu 0.01s; rss 3 |
| no-op fetch, cold | 35 (33-39); cpu 0.02s | 101 (98-118); cpu 0.17s; rss 103 | 30 (28-32); cpu 0.02s; rss 3 | 31 (29-34); cpu 0.02s; rss 3 | 27 (26-29); cpu 0.01s; rss 3 |
| after maint: full clone, warm | 650 (639-746); cpu 0.68s | 678 (662-736); cpu 0.70s; rss 140 | 317 (301-353); cpu 0.74s; rss 8 | 381 (356-409); cpu 0.70s; rss 12 | 315 (302-316); cpu 0.66s; rss 10 |
| after maint: full clone, cold | 690 (677-1276); cpu 0.71s | 961 (806-1512); cpu 0.87s; rss 105 | 380 (320-500); cpu 0.79s; rss 8 | 574 (405-737); cpu 0.79s; rss 12 | 442 (381-598); cpu 0.73s; rss 9 |
| after maint: partial/zone clone, warm | 173 (160-196); cpu 0.16s | 196 (190-246); cpu 0.20s; rss 148 | 113 (105-149); cpu 0.13s; rss 8 | 184 (163-199); cpu 0.17s; rss 14 | 141 (113-217); cpu 0.14s; rss 12 |
| after maint: partial/zone clone, cold | 212 (181-238); cpu 0.17s | 356 (351-382); cpu 0.35s; rss 109 | 169 (168-199); cpu 0.13s; rss 4 | 212 (204-221); cpu 0.17s; rss 9 | 173 (160-237); cpu 0.15s; rss 5 |
| after maint: fetch 1 commit, warm | 32 (27-77); cpu 0.03s | 49 (39-160); cpu 0.04s; rss 170 | 43 (27-76); cpu 0.02s; rss 4 | 36 (28-72); cpu 0.02s; rss 5 | 29 (27-70); cpu 0.02s; rss 6 |
| after maint: fetch 1 commit, cold | 67 (54-88); cpu 0.04s | 234 (222-489); cpu 0.25s; rss 116 | 76 (55-196); cpu 0.04s; rss 3 | 92 (65-128); cpu 0.03s; rss 3 | 69 (65-83); cpu 0.04s; rss 3 |
| after maint: no-op fetch, warm | 14 (13-30); cpu 0.01s | 25 (23-89); cpu 0.02s; rss 135 | 20 (11-28); cpu 0.01s; rss 3 | 10 (10-18); cpu 0.01s; rss 3 | 11 (10-34); cpu 0.01s; rss 3 |
| after maint: no-op fetch, cold | 34 (31-47); cpu 0.02s | 105 (103-150); cpu 0.16s; rss 107 | 33 (28-38); cpu 0.02s; rss 3 | 44 (29-60); cpu 0.02s; rss 3 | 31 (25-52); cpu 0.01s; rss 3 |

Sizes (MiB; du = allocated blocks, app = apparent bytes). Side effect only: speed is the primary goal.

| size | git (local bare) | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|---|
| remote store after initial push (du / app) | 4.4 / 4.3 | 4.3 / 4.3 | 4.9 / 4.9 | 4.6 / 4.6 | 7.0 / 6.9 |
| remote store after maintenance (du / app) | 4.5 / 4.4 | 9.1 / 8.9 | 5.1 / 5.0 | 4.8 / 4.8 | 7.2 / 7.1 |
| full clone: client store (.git / .vcs) | 5.3 | 5.3 | 8.1 | 8.1 | 8.1 |
| partial clone: files / client store | 1000 / 1.8 | 1000 / 1.8 | 1000 / 1.6 | 1000 / 1.6 | 1000 / 1.5 |

Maintenance (time ms / total CPU s / result size MiB):

| arm | remote maintenance | client maintenance variants |
|---|---|---|
| git (local bare) | gc_default: 220 ms / 0.2 s; gc_aggressive: 521 ms / 6.4 s -> 4.5; before: 4.9, after: 4.5 | before 5.7; gc: 139 ms / 5.3; gc_aggressive: 523 ms / 5.3 |
| walgit | ops_compact_force_base: 1648 ms / 6.9 s; after maintainer settle: 9.1; before: 9.1, after: 9.1 | before 5.7; gc: 145 ms / 5.3; gc_aggressive: 558 ms / 5.3 |
| vcs default | compact: 964 ms / 0.4 s; before: 5.3, after: 5.1 | before 8.5; repack_default: 12 ms / 8.3; repack_v2: 10 ms / 8.3; repack_v3: 112 ms / 6.0; repack_v4: 413 ms / 6.1 |
| vcs best-documented (pack v3) | compact: 1063 ms / 0.4 s; before: 5.0, after: 4.8 | before 8.5; repack_v3: 108 ms / 6.0; repack_v4: 421 ms / 6.1 |
| vcs fastest-config (pack v1, raw) | compact: 1027 ms / 0.4 s; before: 7.4, after: 7.2 | before 8.4; repack_v1: 83 ms / 8.3 |

Object-store requests / payload bytes (counting proxy, 1 run per op; GET+PUT+other, MiB up/down):

| op | walgit | vcs default | vcs best-documented (pack v3) | vcs fastest-config (pack v1, raw) |
|---|---|---|---|---|
| initial push | 15 (9/5/1) 4.2 / 0.0 | 6 (2/4/0) 4.8 / 0.0 | 6 (2/4/0) 4.6 / 0.0 | 6 (2/4/0) 6.9 / 0.0 |
| 1-file push | 7 (3/4/0) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 | 7 (2/4/1) 0.0 / 0.0 |
| full clone, warm | 4 (4/0/0) 0.0 / 0.0 | 27 (27/0/0) 0.0 / 4.8 | 10 (10/0/0) 0.0 / 4.6 | 28 (28/0/0) 0.0 / 6.9 |
| full clone, cold | 12 (12/0/0) 0.0 / 4.2 | 27 (27/0/0) 0.0 / 4.8 | 10 (10/0/0) 0.0 / 4.6 | 28 (28/0/0) 0.0 / 6.9 |
| partial/zone clone, warm | 8 (8/0/0) 0.0 / 0.0 | 10 (10/0/0) 0.0 / 1.4 | 7 (7/0/0) 0.0 / 3.4 | 11 (11/0/0) 0.0 / 1.7 |
| partial/zone clone, cold | 15 (15/0/0) 0.0 / 4.2 | 10 (10/0/0) 0.0 / 1.4 | 7 (7/0/0) 0.0 / 3.4 | 11 (11/0/0) 0.0 / 1.7 |
| fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| fetch 1 commit, cold | 18 (18/0/0) 0.0 / 4.2 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| no-op fetch, warm | 3 (3/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| no-op fetch, cold | 16 (16/0/0) 0.0 / 4.2 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: full clone, warm | 5 (5/0/0) 0.0 / 0.0 | 26 (26/0/0) 0.0 / 4.8 | 10 (10/0/0) 0.0 / 4.6 | 28 (28/0/0) 0.0 / 6.9 |
| after maint: full clone, cold | 23 (20/0/3) 0.0 / 4.3 | 26 (26/0/0) 0.0 / 4.8 | 10 (10/0/0) 0.0 / 4.6 | 28 (28/0/0) 0.0 / 6.9 |
| after maint: partial/zone clone, warm | 7 (7/0/0) 0.0 / 0.0 | 10 (10/0/0) 0.0 / 1.4 | 7 (7/0/0) 0.0 / 3.4 | 11 (11/0/0) 0.0 / 1.7 |
| after maint: partial/zone clone, cold | 26 (23/0/3) 0.0 / 4.3 | 10 (10/0/0) 0.0 / 1.4 | 7 (7/0/0) 0.0 / 3.4 | 11 (11/0/0) 0.0 / 1.7 |
| after maint: fetch 1 commit, warm | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: fetch 1 commit, cold | 29 (26/0/3) 0.0 / 4.3 | 5 (5/0/0) 0.0 / 0.0 | 4 (4/0/0) 0.0 / 0.0 | 5 (5/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, warm | 3 (3/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |
| after maint: no-op fetch, cold | 20 (19/0/1) 0.0 / 0.3 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 | 1 (1/0/0) 0.0 / 0.0 |

walgit idle (maintainer loop, one repo, 65 s): 11 store requests; vcs-server idle: 0 by construction (stateless; not separately counted).


## Scoreboard 1: speed (wall, CPU, store requests)

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| binary 10000 | initial push wall ms | 4191 | 576 | 0.14x | win | 617 | 0.15x | win | 590 | 0.14x | win |
| binary 10000 | initial push total CPU s | 7.93 | 0.81 | 0.10x | win | 0.82 | 0.10x | win | 0.59 | 0.07x | win |
| binary 10000 | 1-file push wall ms | 113 | 76.85 | 0.68x | win | 79.61 | 0.70x | win | 73.44 | 0.65x | win |
| binary 10000 | 1-file push total CPU s | 0.07 | 0.03 | 0.37x | win | 0.03 | 0.38x | win | 0.03 | 0.36x | win |
| binary 10000 | full clone, warm wall ms | 1597 | 675 | 0.42x | win | 779 | 0.49x | win | 617 | 0.39x | win |
| binary 10000 | full clone, warm total CPU s | 1.26 | 1.53 | 1.21x | LOSS | 1.55 | 1.23x | LOSS | 1.49 | 1.18x | LOSS |
| binary 10000 | full clone, cold wall ms | 1257 | 531 | 0.42x | win | 647 | 0.51x | win | 555 | 0.44x | win |
| binary 10000 | full clone, cold total CPU s | 1.57 | 1.62 | 1.03x | tie | 1.65 | 1.05x | LOSS | 1.63 | 1.04x | tie |
| binary 10000 | partial/zone clone, warm wall ms | 291 | 157 | 0.54x | win | 294 | 1.01x | tie | 164 | 0.56x | win |
| binary 10000 | partial/zone clone, warm total CPU s | 0.28 | 0.23 | 0.82x | win | 0.28 | 0.97x | tie | 0.23 | 0.81x | win |
| binary 10000 | partial/zone clone, cold wall ms | 493 | 216 | 0.44x | win | 357 | 0.72x | win | 193 | 0.39x | win |
| binary 10000 | partial/zone clone, cold total CPU s | 0.55 | 0.26 | 0.47x | win | 0.31 | 0.55x | win | 0.24 | 0.44x | win |
| binary 10000 | fetch 1 commit, warm wall ms | 40.08 | 25.46 | 0.64x | win | 25.13 | 0.63x | win | 24.89 | 0.62x | win |
| binary 10000 | fetch 1 commit, warm total CPU s | 0.04 | 0.02 | 0.55x | win | 0.02 | 0.54x | win | 0.01 | 0.31x | win |
| binary 10000 | fetch 1 commit, cold wall ms | 281 | 57.71 | 0.21x | win | 62.66 | 0.22x | win | 60.99 | 0.22x | win |
| binary 10000 | fetch 1 commit, cold total CPU s | 0.38 | 0.03 | 0.08x | win | 0.03 | 0.09x | win | 0.04 | 0.11x | win |
| binary 10000 | no-op fetch, warm wall ms | 21.63 | 10.44 | 0.48x | win | 10.37 | 0.48x | win | 10.29 | 0.48x | win |
| binary 10000 | no-op fetch, warm total CPU s | 0.02 | 0.01 | 0.27x | win | 0.01 | 0.28x | win | 0.01 | 0.30x | win |
| binary 10000 | no-op fetch, cold wall ms | 111 | 28.57 | 0.26x | win | 27.19 | 0.24x | win | 32.39 | 0.29x | win |
| binary 10000 | no-op fetch, cold total CPU s | 0.15 | 0.01 | 0.09x | win | 0.01 | 0.09x | win | 0.01 | 0.09x | win |
| binary 10000 | after maint: full clone, warm wall ms | 987 | 503 | 0.51x | win | 608 | 0.62x | win | 493 | 0.50x | win |
| binary 10000 | after maint: full clone, warm total CPU s | 1.24 | 1.48 | 1.20x | LOSS | 1.48 | 1.20x | LOSS | 1.47 | 1.19x | LOSS |
| binary 10000 | after maint: full clone, cold wall ms | 1390 | 580 | 0.42x | win | 624 | 0.45x | win | 549 | 0.39x | win |
| binary 10000 | after maint: full clone, cold total CPU s | 1.54 | 1.63 | 1.06x | LOSS | 1.59 | 1.04x | tie | 1.57 | 1.02x | tie |
| binary 10000 | after maint: partial/zone clone, warm wall ms | 240 | 134 | 0.56x | win | 261 | 1.09x | LOSS | 132 | 0.55x | win |
| binary 10000 | after maint: partial/zone clone, warm total CPU s | 0.25 | 0.21 | 0.82x | win | 0.26 | 1.04x | tie | 0.21 | 0.82x | win |
| binary 10000 | after maint: partial/zone clone, cold wall ms | 440 | 154 | 0.35x | win | 290 | 0.66x | win | 153 | 0.35x | win |
| binary 10000 | after maint: partial/zone clone, cold total CPU s | 0.55 | 0.22 | 0.41x | win | 0.28 | 0.50x | win | 0.22 | 0.41x | win |
| binary 10000 | after maint: fetch 1 commit, warm wall ms | 41.61 | 28.12 | 0.68x | win | 33.28 | 0.80x | win | 28.93 | 0.70x | win |
| binary 10000 | after maint: fetch 1 commit, warm total CPU s | 0.04 | 0.02 | 0.54x | win | 0.01 | 0.29x | win | 0.02 | 0.55x | win |
| binary 10000 | after maint: fetch 1 commit, cold wall ms | 332 | 71.77 | 0.22x | win | 66.59 | 0.20x | win | 70.29 | 0.21x | win |
| binary 10000 | after maint: fetch 1 commit, cold total CPU s | 0.39 | 0.04 | 0.11x | win | 0.04 | 0.11x | win | 0.04 | 0.11x | win |
| binary 10000 | after maint: no-op fetch, warm wall ms | 26.37 | 13.15 | 0.50x | win | 13.58 | 0.52x | win | 15.63 | 0.59x | win |
| binary 10000 | after maint: no-op fetch, warm total CPU s | 0.05 | 0.02 | 0.31x | win | 0.01 | 0.12x | win | 0.01 | 0.13x | win |
| binary 10000 | after maint: no-op fetch, cold wall ms | 124 | 50.52 | 0.41x | win | 49.96 | 0.40x | win | 43.15 | 0.35x | win |
| binary 10000 | after maint: no-op fetch, cold total CPU s | 0.15 | 0.01 | 0.10x | win | 0.01 | 0.10x | win | 0.02 | 0.10x | win |
| binary 10000 | initial push store requests | 11.00 | 6.00 | 0.55x | win | 6.00 | 0.55x | win | 6.00 | 0.55x | win |
| binary 10000 | 1-file push store requests | 7.00 | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie |
| binary 10000 | full clone, warm store requests | 4.00 | 28.00 | 7.00x | LOSS | 14.00 | 3.50x | LOSS | 28.00 | 7.00x | LOSS |
| binary 10000 | full clone, cold store requests | 13.00 | 28.00 | 2.15x | LOSS | 14.00 | 1.08x | LOSS | 28.00 | 2.15x | LOSS |
| binary 10000 | partial/zone clone, warm store requests | 8.00 | 24.00 | 3.00x | LOSS | 7.00 | 0.88x | win | 24.00 | 3.00x | LOSS |
| binary 10000 | partial/zone clone, cold store requests | 16.00 | 24.00 | 1.50x | LOSS | 7.00 | 0.44x | win | 24.00 | 1.50x | LOSS |
| binary 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| binary 10000 | fetch 1 commit, cold store requests | 19.00 | 5.00 | 0.26x | win | 4.00 | 0.21x | win | 5.00 | 0.26x | win |
| binary 10000 | no-op fetch, warm store requests | 3.00 | 1.00 | 0.33x | win | 1.00 | 0.33x | win | 1.00 | 0.33x | win |
| binary 10000 | no-op fetch, cold store requests | 15.00 | 1.00 | 0.07x | win | 1.00 | 0.07x | win | 1.00 | 0.07x | win |
| binary 10000 | after maint: full clone, warm store requests | 4.00 | 27.00 | 6.75x | LOSS | 14.00 | 3.50x | LOSS | 28.00 | 7.00x | LOSS |
| binary 10000 | after maint: full clone, cold store requests | 22.00 | 27.00 | 1.23x | LOSS | 14.00 | 0.64x | win | 28.00 | 1.27x | LOSS |
| binary 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 24.00 | 3.43x | LOSS | 7.00 | 1.00x | tie | 24.00 | 3.43x | LOSS |
| binary 10000 | after maint: partial/zone clone, cold store requests | 25.00 | 24.00 | 0.96x | tie | 7.00 | 0.28x | win | 24.00 | 0.96x | tie |
| binary 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| binary 10000 | after maint: fetch 1 commit, cold store requests | 28.00 | 5.00 | 0.18x | win | 4.00 | 0.14x | win | 5.00 | 0.18x | win |
| binary 10000 | after maint: no-op fetch, warm store requests | 3.00 | 1.00 | 0.33x | win | 1.00 | 0.33x | win | 1.00 | 0.33x | win |
| binary 10000 | after maint: no-op fetch, cold store requests | 24.00 | 1.00 | 0.04x | win | 1.00 | 0.04x | win | 1.00 | 0.04x | win |
| deep 10000 | initial push wall ms | 5811 | 456 | 0.08x | win | 377 | 0.06x | win | 345 | 0.06x | win |
| deep 10000 | initial push total CPU s | 3.88 | 0.52 | 0.13x | win | 0.33 | 0.09x | win | 0.29 | 0.07x | win |
| deep 10000 | 1-file push wall ms | 163 | 104 | 0.64x | win | 84.85 | 0.52x | win | 71.75 | 0.44x | win |
| deep 10000 | 1-file push total CPU s | 0.10 | 0.03 | 0.29x | win | 0.03 | 0.29x | win | 0.03 | 0.28x | win |
| deep 10000 | full clone, warm wall ms | 2842 | 1350 | 0.47x | win | 1347 | 0.47x | win | 1287 | 0.45x | win |
| deep 10000 | full clone, warm total CPU s | 2.13 | 2.45 | 1.15x | LOSS | 2.40 | 1.13x | LOSS | 2.47 | 1.16x | LOSS |
| deep 10000 | full clone, cold wall ms | 3632 | 1361 | 0.37x | win | 1564 | 0.43x | win | 1424 | 0.39x | win |
| deep 10000 | full clone, cold total CPU s | 2.45 | 2.58 | 1.06x | LOSS | 2.60 | 1.06x | LOSS | 2.57 | 1.05x | tie |
| deep 10000 | partial/zone clone, warm wall ms | 912 | 384 | 0.42x | win | 326 | 0.36x | win | 363 | 0.40x | win |
| deep 10000 | partial/zone clone, warm total CPU s | 0.69 | 0.33 | 0.48x | win | 0.37 | 0.53x | win | 0.34 | 0.49x | win |
| deep 10000 | partial/zone clone, cold wall ms | 1069 | 416 | 0.39x | win | 423 | 0.40x | win | 336 | 0.31x | win |
| deep 10000 | partial/zone clone, cold total CPU s | 0.96 | 0.39 | 0.40x | win | 0.40 | 0.42x | win | 0.37 | 0.39x | win |
| deep 10000 | fetch 1 commit, warm wall ms | 86.80 | 52.69 | 0.61x | win | 55.61 | 0.64x | win | 80.42 | 0.93x | win |
| deep 10000 | fetch 1 commit, warm total CPU s | 0.08 | 0.02 | 0.23x | win | 0.03 | 0.37x | win | 0.03 | 0.37x | win |
| deep 10000 | fetch 1 commit, cold wall ms | 467 | 125 | 0.27x | win | 131 | 0.28x | win | 108 | 0.23x | win |
| deep 10000 | fetch 1 commit, cold total CPU s | 0.40 | 0.05 | 0.13x | win | 0.05 | 0.13x | win | 0.05 | 0.13x | win |
| deep 10000 | no-op fetch, warm wall ms | 33.36 | 19.98 | 0.60x | win | 13.18 | 0.40x | win | 13.50 | 0.40x | win |
| deep 10000 | no-op fetch, warm total CPU s | 0.09 | 0.01 | 0.08x | win | 0.01 | 0.07x | win | 0.01 | 0.07x | win |
| deep 10000 | no-op fetch, cold wall ms | 115 | 41.31 | 0.36x | win | 48.58 | 0.42x | win | 59.58 | 0.52x | win |
| deep 10000 | no-op fetch, cold total CPU s | 0.15 | 0.01 | 0.10x | win | 0.02 | 0.10x | win | 0.02 | 0.10x | win |
| deep 10000 | after maint: full clone, warm wall ms | 3180 | 1157 | 0.36x | win | 1378 | 0.43x | win | 1237 | 0.39x | win |
| deep 10000 | after maint: full clone, warm total CPU s | 2.20 | 2.48 | 1.12x | LOSS | 2.45 | 1.11x | LOSS | 2.42 | 1.10x | LOSS |
| deep 10000 | after maint: full clone, cold wall ms | 2239 | 1078 | 0.48x | win | 1097 | 0.49x | win | 990 | 0.44x | win |
| deep 10000 | after maint: full clone, cold total CPU s | 2.40 | 2.54 | 1.06x | LOSS | 2.49 | 1.04x | tie | 2.52 | 1.05x | tie |
| deep 10000 | after maint: partial/zone clone, warm wall ms | 599 | 234 | 0.39x | win | 285 | 0.48x | win | 248 | 0.41x | win |
| deep 10000 | after maint: partial/zone clone, warm total CPU s | 0.65 | 0.32 | 0.49x | win | 0.34 | 0.52x | win | 0.31 | 0.48x | win |
| deep 10000 | after maint: partial/zone clone, cold wall ms | 774 | 282 | 0.36x | win | 275 | 0.36x | win | 279 | 0.36x | win |
| deep 10000 | after maint: partial/zone clone, cold total CPU s | 0.89 | 0.37 | 0.41x | win | 0.38 | 0.42x | win | 0.37 | 0.41x | win |
| deep 10000 | after maint: fetch 1 commit, warm wall ms | 91.23 | 42.02 | 0.46x | win | 54.57 | 0.60x | win | 56.04 | 0.61x | win |
| deep 10000 | after maint: fetch 1 commit, warm total CPU s | 0.09 | 0.03 | 0.32x | win | 0.03 | 0.31x | win | 0.02 | 0.23x | win |
| deep 10000 | after maint: fetch 1 commit, cold wall ms | 388 | 79.80 | 0.21x | win | 83.82 | 0.22x | win | 94.08 | 0.24x | win |
| deep 10000 | after maint: fetch 1 commit, cold total CPU s | 0.39 | 0.04 | 0.10x | win | 0.04 | 0.11x | win | 0.04 | 0.11x | win |
| deep 10000 | after maint: no-op fetch, warm wall ms | 29.56 | 9.38 | 0.32x | win | 9.70 | 0.33x | win | 9.42 | 0.32x | win |
| deep 10000 | after maint: no-op fetch, warm total CPU s | 0.03 | 0.01 | 0.24x | win | 0.01 | 0.22x | win | 0.01 | 0.23x | win |
| deep 10000 | after maint: no-op fetch, cold wall ms | 105 | 32.71 | 0.31x | win | 30.30 | 0.29x | win | 32.75 | 0.31x | win |
| deep 10000 | after maint: no-op fetch, cold total CPU s | 0.16 | 0.01 | 0.09x | win | 0.01 | 0.09x | win | 0.01 | 0.09x | win |
| deep 10000 | initial push store requests | 20.00 | 6.00 | 0.30x | win | 6.00 | 0.30x | win | 6.00 | 0.30x | win |
| deep 10000 | 1-file push store requests | 7.00 | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie |
| deep 10000 | full clone, warm store requests | 4.00 | 27.00 | 6.75x | LOSS | 10.00 | 2.50x | LOSS | 28.00 | 7.00x | LOSS |
| deep 10000 | full clone, cold store requests | 12.00 | 27.00 | 2.25x | LOSS | 10.00 | 0.83x | win | 28.00 | 2.33x | LOSS |
| deep 10000 | partial/zone clone, warm store requests | 10.00 | 10.00 | 1.00x | tie | 7.00 | 0.70x | win | 11.00 | 1.10x | LOSS |
| deep 10000 | partial/zone clone, cold store requests | 15.00 | 10.00 | 0.67x | win | 7.00 | 0.47x | win | 11.00 | 0.73x | win |
| deep 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| deep 10000 | fetch 1 commit, cold store requests | 18.00 | 5.00 | 0.28x | win | 4.00 | 0.22x | win | 5.00 | 0.28x | win |
| deep 10000 | no-op fetch, warm store requests | 2.00 | 1.00 | 0.50x | win | 2.00 | 1.00x | tie | 3.00 | 1.50x | LOSS |
| deep 10000 | no-op fetch, cold store requests | 15.00 | 1.00 | 0.07x | win | 1.00 | 0.07x | win | 1.00 | 0.07x | win |
| deep 10000 | after maint: full clone, warm store requests | 4.00 | 26.00 | 6.50x | LOSS | 10.00 | 2.50x | LOSS | 28.00 | 7.00x | LOSS |
| deep 10000 | after maint: full clone, cold store requests | 23.00 | 26.00 | 1.13x | LOSS | 10.00 | 0.43x | win | 28.00 | 1.22x | LOSS |
| deep 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 10.00 | 1.43x | LOSS | 7.00 | 1.00x | tie | 11.00 | 1.57x | LOSS |
| deep 10000 | after maint: partial/zone clone, cold store requests | 26.00 | 10.00 | 0.38x | win | 7.00 | 0.27x | win | 11.00 | 0.42x | win |
| deep 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| deep 10000 | after maint: fetch 1 commit, cold store requests | 29.00 | 5.00 | 0.17x | win | 4.00 | 0.14x | win | 5.00 | 0.17x | win |
| deep 10000 | after maint: no-op fetch, warm store requests | 5.00 | 1.00 | 0.20x | win | 1.00 | 0.20x | win | 1.00 | 0.20x | win |
| deep 10000 | after maint: no-op fetch, cold store requests | 22.00 | 1.00 | 0.05x | win | 1.00 | 0.05x | win | 1.00 | 0.05x | win |
| flat 10000 | initial push wall ms | 2386 | 231 | 0.10x | win | 211 | 0.09x | win | 300 | 0.13x | win |
| flat 10000 | initial push total CPU s | 2.44 | 0.27 | 0.11x | win | 0.19 | 0.08x | win | 0.17 | 0.07x | win |
| flat 10000 | 1-file push wall ms | 156 | 88.30 | 0.56x | win | 77.79 | 0.50x | win | 85.87 | 0.55x | win |
| flat 10000 | 1-file push total CPU s | 0.08 | 0.03 | 0.33x | win | 0.03 | 0.33x | win | 0.03 | 0.33x | win |
| flat 10000 | full clone, warm wall ms | 700 | 350 | 0.50x | win | 410 | 0.59x | win | 354 | 0.51x | win |
| flat 10000 | full clone, warm total CPU s | 0.71 | 0.76 | 1.07x | LOSS | 0.71 | 1.00x | tie | 0.71 | 0.99x | tie |
| flat 10000 | full clone, cold wall ms | 775 | 315 | 0.41x | win | 414 | 0.53x | win | 320 | 0.41x | win |
| flat 10000 | full clone, cold total CPU s | 0.84 | 0.75 | 0.90x | win | 0.73 | 0.87x | win | 0.71 | 0.84x | win |
| flat 10000 | partial/zone clone, warm wall ms | 187 | 103 | 0.55x | win | 140 | 0.75x | win | 97.05 | 0.52x | win |
| flat 10000 | partial/zone clone, warm total CPU s | 0.18 | 0.14 | 0.78x | win | 0.16 | 0.89x | win | 0.11 | 0.63x | win |
| flat 10000 | partial/zone clone, cold wall ms | 471 | 309 | 0.66x | win | 307 | 0.65x | win | 191 | 0.41x | win |
| flat 10000 | partial/zone clone, cold total CPU s | 0.38 | 0.15 | 0.39x | win | 0.20 | 0.53x | win | 0.15 | 0.40x | win |
| flat 10000 | fetch 1 commit, warm wall ms | 43.85 | 32.68 | 0.75x | win | 30.58 | 0.70x | win | 31.76 | 0.72x | win |
| flat 10000 | fetch 1 commit, warm total CPU s | 0.03 | 0.02 | 0.76x | win | 0.02 | 0.75x | win | 0.02 | 0.79x | win |
| flat 10000 | fetch 1 commit, cold wall ms | 212 | 63.77 | 0.30x | win | 69.85 | 0.33x | win | 69.93 | 0.33x | win |
| flat 10000 | fetch 1 commit, cold total CPU s | 0.24 | 0.04 | 0.18x | win | 0.04 | 0.17x | win | 0.04 | 0.18x | win |
| flat 10000 | no-op fetch, warm wall ms | 26.04 | 14.42 | 0.55x | win | 14.03 | 0.54x | win | 10.73 | 0.41x | win |
| flat 10000 | no-op fetch, warm total CPU s | 0.02 | 0.01 | 0.28x | win | 0.01 | 0.27x | win | 0.01 | 0.27x | win |
| flat 10000 | no-op fetch, cold wall ms | 101 | 30.39 | 0.30x | win | 31.43 | 0.31x | win | 27.48 | 0.27x | win |
| flat 10000 | no-op fetch, cold total CPU s | 0.17 | 0.02 | 0.09x | win | 0.02 | 0.11x | win | 0.01 | 0.08x | win |
| flat 10000 | after maint: full clone, warm wall ms | 678 | 317 | 0.47x | win | 381 | 0.56x | win | 315 | 0.46x | win |
| flat 10000 | after maint: full clone, warm total CPU s | 0.70 | 0.74 | 1.05x | tie | 0.70 | 0.99x | tie | 0.66 | 0.94x | win |
| flat 10000 | after maint: full clone, cold wall ms | 961 | 380 | 0.40x | win | 574 | 0.60x | win | 442 | 0.46x | win |
| flat 10000 | after maint: full clone, cold total CPU s | 0.87 | 0.79 | 0.90x | win | 0.79 | 0.91x | win | 0.73 | 0.84x | win |
| flat 10000 | after maint: partial/zone clone, warm wall ms | 196 | 113 | 0.58x | win | 184 | 0.94x | win | 141 | 0.72x | win |
| flat 10000 | after maint: partial/zone clone, warm total CPU s | 0.20 | 0.13 | 0.68x | win | 0.17 | 0.85x | win | 0.14 | 0.73x | win |
| flat 10000 | after maint: partial/zone clone, cold wall ms | 356 | 169 | 0.47x | win | 212 | 0.59x | win | 173 | 0.49x | win |
| flat 10000 | after maint: partial/zone clone, cold total CPU s | 0.35 | 0.13 | 0.38x | win | 0.17 | 0.50x | win | 0.15 | 0.43x | win |
| flat 10000 | after maint: fetch 1 commit, warm wall ms | 49.32 | 42.68 | 0.87x | win | 35.75 | 0.72x | win | 28.97 | 0.59x | win |
| flat 10000 | after maint: fetch 1 commit, warm total CPU s | 0.04 | 0.02 | 0.52x | win | 0.02 | 0.54x | win | 0.02 | 0.54x | win |
| flat 10000 | after maint: fetch 1 commit, cold wall ms | 234 | 76.18 | 0.32x | win | 91.65 | 0.39x | win | 68.79 | 0.29x | win |
| flat 10000 | after maint: fetch 1 commit, cold total CPU s | 0.25 | 0.04 | 0.17x | win | 0.03 | 0.13x | win | 0.04 | 0.17x | win |
| flat 10000 | after maint: no-op fetch, warm wall ms | 25.21 | 19.56 | 0.78x | win | 10.41 | 0.41x | win | 11.08 | 0.44x | win |
| flat 10000 | after maint: no-op fetch, warm total CPU s | 0.02 | 0.01 | 0.28x | win | 0.01 | 0.27x | win | 0.01 | 0.26x | win |
| flat 10000 | after maint: no-op fetch, cold wall ms | 105 | 32.93 | 0.32x | win | 44.14 | 0.42x | win | 30.58 | 0.29x | win |
| flat 10000 | after maint: no-op fetch, cold total CPU s | 0.16 | 0.02 | 0.15x | win | 0.02 | 0.09x | win | 0.01 | 0.09x | win |
| flat 10000 | initial push store requests | 15.00 | 6.00 | 0.40x | win | 6.00 | 0.40x | win | 6.00 | 0.40x | win |
| flat 10000 | 1-file push store requests | 7.00 | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie | 7.00 | 1.00x | tie |
| flat 10000 | full clone, warm store requests | 4.00 | 27.00 | 6.75x | LOSS | 10.00 | 2.50x | LOSS | 28.00 | 7.00x | LOSS |
| flat 10000 | full clone, cold store requests | 12.00 | 27.00 | 2.25x | LOSS | 10.00 | 0.83x | win | 28.00 | 2.33x | LOSS |
| flat 10000 | partial/zone clone, warm store requests | 8.00 | 10.00 | 1.25x | LOSS | 7.00 | 0.88x | win | 11.00 | 1.38x | LOSS |
| flat 10000 | partial/zone clone, cold store requests | 15.00 | 10.00 | 0.67x | win | 7.00 | 0.47x | win | 11.00 | 0.73x | win |
| flat 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| flat 10000 | fetch 1 commit, cold store requests | 18.00 | 5.00 | 0.28x | win | 4.00 | 0.22x | win | 5.00 | 0.28x | win |
| flat 10000 | no-op fetch, warm store requests | 3.00 | 1.00 | 0.33x | win | 1.00 | 0.33x | win | 1.00 | 0.33x | win |
| flat 10000 | no-op fetch, cold store requests | 16.00 | 1.00 | 0.06x | win | 1.00 | 0.06x | win | 1.00 | 0.06x | win |
| flat 10000 | after maint: full clone, warm store requests | 5.00 | 26.00 | 5.20x | LOSS | 10.00 | 2.00x | LOSS | 28.00 | 5.60x | LOSS |
| flat 10000 | after maint: full clone, cold store requests | 23.00 | 26.00 | 1.13x | LOSS | 10.00 | 0.43x | win | 28.00 | 1.22x | LOSS |
| flat 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 10.00 | 1.43x | LOSS | 7.00 | 1.00x | tie | 11.00 | 1.57x | LOSS |
| flat 10000 | after maint: partial/zone clone, cold store requests | 26.00 | 10.00 | 0.38x | win | 7.00 | 0.27x | win | 11.00 | 0.42x | win |
| flat 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 1.25x | LOSS | 4.00 | 1.00x | tie | 5.00 | 1.25x | LOSS |
| flat 10000 | after maint: fetch 1 commit, cold store requests | 29.00 | 5.00 | 0.17x | win | 4.00 | 0.14x | win | 5.00 | 0.17x | win |
| flat 10000 | after maint: no-op fetch, warm store requests | 3.00 | 1.00 | 0.33x | win | 1.00 | 0.33x | win | 1.00 | 0.33x | win |
| flat 10000 | after maint: no-op fetch, cold store requests | 20.00 | 1.00 | 0.05x | win | 1.00 | 0.05x | win | 1.00 | 0.05x | win |

Totals over 162 cells (win / tie / LOSS): vcs default [123, 7, 32]; vcs best-doc [128, 20, 14]; vcs fastest [123, 9, 30].

### Cells where walgit still beats vcs (scoreboard 1: speed (wall, cpu, store requests))

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| binary 10000 | full clone, warm total CPU s | 1.26 | 1.53 | 1.55 | 1.49 | default, best-doc, fastest |
| binary 10000 | full clone, cold total CPU s | 1.57 | 1.62 | 1.65 | 1.63 | best-doc |
| binary 10000 | after maint: full clone, warm total CPU s | 1.24 | 1.48 | 1.48 | 1.47 | default, best-doc, fastest |
| binary 10000 | after maint: full clone, cold total CPU s | 1.54 | 1.63 | 1.59 | 1.57 | default |
| binary 10000 | after maint: partial/zone clone, warm wall ms | 240 | 134 | 261 | 132 | best-doc |
| binary 10000 | full clone, warm store requests | 4.00 | 28.00 | 14.00 | 28.00 | default, best-doc, fastest |
| binary 10000 | full clone, cold store requests | 13.00 | 28.00 | 14.00 | 28.00 | default, best-doc, fastest |
| binary 10000 | partial/zone clone, warm store requests | 8.00 | 24.00 | 7.00 | 24.00 | default, fastest |
| binary 10000 | partial/zone clone, cold store requests | 16.00 | 24.00 | 7.00 | 24.00 | default, fastest |
| binary 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| binary 10000 | after maint: full clone, warm store requests | 4.00 | 27.00 | 14.00 | 28.00 | default, best-doc, fastest |
| binary 10000 | after maint: full clone, cold store requests | 22.00 | 27.00 | 14.00 | 28.00 | default, fastest |
| binary 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 24.00 | 7.00 | 24.00 | default, fastest |
| binary 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| deep 10000 | full clone, warm total CPU s | 2.13 | 2.45 | 2.40 | 2.47 | default, best-doc, fastest |
| deep 10000 | full clone, cold total CPU s | 2.45 | 2.58 | 2.60 | 2.57 | default, best-doc |
| deep 10000 | after maint: full clone, warm total CPU s | 2.20 | 2.48 | 2.45 | 2.42 | default, best-doc, fastest |
| deep 10000 | after maint: full clone, cold total CPU s | 2.40 | 2.54 | 2.49 | 2.52 | default |
| deep 10000 | full clone, warm store requests | 4.00 | 27.00 | 10.00 | 28.00 | default, best-doc, fastest |
| deep 10000 | full clone, cold store requests | 12.00 | 27.00 | 10.00 | 28.00 | default, fastest |
| deep 10000 | partial/zone clone, warm store requests | 10.00 | 10.00 | 7.00 | 11.00 | fastest |
| deep 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| deep 10000 | no-op fetch, warm store requests | 2.00 | 1.00 | 2.00 | 3.00 | fastest |
| deep 10000 | after maint: full clone, warm store requests | 4.00 | 26.00 | 10.00 | 28.00 | default, best-doc, fastest |
| deep 10000 | after maint: full clone, cold store requests | 23.00 | 26.00 | 10.00 | 28.00 | default, fastest |
| deep 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 10.00 | 7.00 | 11.00 | default, fastest |
| deep 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| flat 10000 | full clone, warm total CPU s | 0.71 | 0.76 | 0.71 | 0.71 | default |
| flat 10000 | full clone, warm store requests | 4.00 | 27.00 | 10.00 | 28.00 | default, best-doc, fastest |
| flat 10000 | full clone, cold store requests | 12.00 | 27.00 | 10.00 | 28.00 | default, fastest |
| flat 10000 | partial/zone clone, warm store requests | 8.00 | 10.00 | 7.00 | 11.00 | default, fastest |
| flat 10000 | fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |
| flat 10000 | after maint: full clone, warm store requests | 5.00 | 26.00 | 10.00 | 28.00 | default, best-doc, fastest |
| flat 10000 | after maint: full clone, cold store requests | 23.00 | 26.00 | 10.00 | 28.00 | default, fastest |
| flat 10000 | after maint: partial/zone clone, warm store requests | 7.00 | 10.00 | 7.00 | 11.00 | default, fastest |
| flat 10000 | after maint: fetch 1 commit, warm store requests | 4.00 | 5.00 | 4.00 | 5.00 | default, fastest |

## Scoreboard 2: server memory and object-store traffic

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| binary 10000 | initial push server RSS MiB | 99.52 | 66.80 | 0.67x | win | 66.80 | 0.67x | win | 66.56 | 0.67x | win |
| binary 10000 | 1-file push server RSS MiB | 92.98 | 3.48 | 0.04x | win | 3.48 | 0.04x | win | 3.47 | 0.04x | win |
| binary 10000 | full clone, warm server RSS MiB | 86.84 | 43.21 | 0.50x | win | 123 | 1.41x | LOSS | 45.73 | 0.53x | win |
| binary 10000 | full clone, cold server RSS MiB | 148 | 39.95 | 0.27x | win | 77.93 | 0.53x | win | 40.20 | 0.27x | win |
| binary 10000 | partial/zone clone, warm server RSS MiB | 114 | 34.86 | 0.31x | win | 38.70 | 0.34x | win | 39.16 | 0.34x | win |
| binary 10000 | partial/zone clone, cold server RSS MiB | 145 | 7.99 | 0.06x | win | 20.04 | 0.14x | win | 8.36 | 0.06x | win |
| binary 10000 | fetch 1 commit, warm server RSS MiB | 195 | 7.87 | 0.04x | win | 4.93 | 0.03x | win | 8.24 | 0.04x | win |
| binary 10000 | fetch 1 commit, cold server RSS MiB | 162 | 3.41 | 0.02x | win | 3.39 | 0.02x | win | 3.39 | 0.02x | win |
| binary 10000 | no-op fetch, warm server RSS MiB | 300 | 3.41 | 0.01x | win | 3.35 | 0.01x | win | 3.39 | 0.01x | win |
| binary 10000 | no-op fetch, cold server RSS MiB | 101 | 3.36 | 0.03x | win | 3.31 | 0.03x | win | 3.34 | 0.03x | win |
| binary 10000 | after maint: full clone, warm server RSS MiB | 157 | 47.24 | 0.30x | win | 84.82 | 0.54x | win | 42.94 | 0.27x | win |
| binary 10000 | after maint: full clone, cold server RSS MiB | 143 | 42.55 | 0.30x | win | 76.43 | 0.53x | win | 40.47 | 0.28x | win |
| binary 10000 | after maint: partial/zone clone, warm server RSS MiB | 177 | 40.95 | 0.23x | win | 22.81 | 0.13x | win | 42.93 | 0.24x | win |
| binary 10000 | after maint: partial/zone clone, cold server RSS MiB | 141 | 8.38 | 0.06x | win | 19.79 | 0.14x | win | 8.32 | 0.06x | win |
| binary 10000 | after maint: fetch 1 commit, warm server RSS MiB | 172 | 8.54 | 0.05x | win | 4.53 | 0.03x | win | 7.84 | 0.05x | win |
| binary 10000 | after maint: fetch 1 commit, cold server RSS MiB | 160 | 3.41 | 0.02x | win | 3.41 | 0.02x | win | 3.48 | 0.02x | win |
| binary 10000 | after maint: no-op fetch, warm server RSS MiB | 250 | 3.51 | 0.01x | win | 3.40 | 0.01x | win | 3.52 | 0.01x | win |
| binary 10000 | after maint: no-op fetch, cold server RSS MiB | 88.48 | 3.38 | 0.04x | win | 3.38 | 0.04x | win | 3.37 | 0.04x | win |
| binary 10000 | initial push store traffic up+down MiB | 39.78 | 40.28 | 1.01x | tie | 40.28 | 1.01x | tie | 40.42 | 1.02x | tie |
| binary 10000 | 1-file push store traffic up+down MiB | 0.01 | 0.01 | 1.04x | tie | 0.01 | 1.05x | tie | 0.01 | 1.04x | tie |
| binary 10000 | full clone, warm store traffic up+down MiB | 0.00 | 40.29 | inf | LOSS | 40.29 | inf | LOSS | 40.43 | inf | LOSS |
| binary 10000 | full clone, cold store traffic up+down MiB | 39.74 | 40.29 | 1.01x | tie | 40.29 | 1.01x | tie | 40.43 | 1.02x | tie |
| binary 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 4.90 | inf | LOSS | 8.82 | inf | LOSS | 5.04 | inf | LOSS |
| binary 10000 | partial/zone clone, cold store traffic up+down MiB | 39.74 | 4.90 | 0.12x | win | 8.82 | 0.22x | win | 5.04 | 0.13x | win |
| binary 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| binary 10000 | fetch 1 commit, cold store traffic up+down MiB | 39.75 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| binary 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| binary 10000 | no-op fetch, cold store traffic up+down MiB | 0.31 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| binary 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 40.31 | inf | LOSS | 40.31 | inf | LOSS | 40.45 | inf | LOSS |
| binary 10000 | after maint: full clone, cold store traffic up+down MiB | 39.77 | 40.31 | 1.01x | tie | 40.31 | 1.01x | tie | 40.45 | 1.02x | tie |
| binary 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 4.90 | inf | LOSS | 8.82 | inf | LOSS | 5.04 | inf | LOSS |
| binary 10000 | after maint: partial/zone clone, cold store traffic up+down MiB | 39.77 | 4.90 | 0.12x | win | 8.82 | 0.22x | win | 5.04 | 0.13x | win |
| binary 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| binary 10000 | after maint: fetch 1 commit, cold store traffic up+down MiB | 39.79 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| binary 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| binary 10000 | after maint: no-op fetch, cold store traffic up+down MiB | 0.59 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| deep 10000 | initial push server RSS MiB | 95.81 | 10.78 | 0.11x | win | 10.78 | 0.11x | win | 11.02 | 0.11x | win |
| deep 10000 | 1-file push server RSS MiB | 106 | 3.46 | 0.03x | win | 3.51 | 0.03x | win | 3.47 | 0.03x | win |
| deep 10000 | full clone, warm server RSS MiB | 81.96 | 15.89 | 0.19x | win | 23.46 | 0.29x | win | 17.18 | 0.21x | win |
| deep 10000 | full clone, cold server RSS MiB | 105 | 10.52 | 0.10x | win | 16.29 | 0.16x | win | 12.23 | 0.12x | win |
| deep 10000 | partial/zone clone, warm server RSS MiB | 132 | 15.95 | 0.12x | win | 24.65 | 0.19x | win | 14.79 | 0.11x | win |
| deep 10000 | partial/zone clone, cold server RSS MiB | 105 | 7.00 | 0.07x | win | 7.63 | 0.07x | win | 7.28 | 0.07x | win |
| deep 10000 | fetch 1 commit, warm server RSS MiB | 155 | 7.14 | 0.05x | win | 10.07 | 0.06x | win | 7.41 | 0.05x | win |
| deep 10000 | fetch 1 commit, cold server RSS MiB | 111 | 3.42 | 0.03x | win | 3.45 | 0.03x | win | 3.51 | 0.03x | win |
| deep 10000 | no-op fetch, warm server RSS MiB | 84.69 | 3.42 | 0.04x | win | 3.52 | 0.04x | win | 3.52 | 0.04x | win |
| deep 10000 | no-op fetch, cold server RSS MiB | 107 | 3.35 | 0.03x | win | 3.32 | 0.03x | win | 3.33 | 0.03x | win |
| deep 10000 | after maint: full clone, warm server RSS MiB | 126 | 15.80 | 0.13x | win | 16.97 | 0.14x | win | 17.17 | 0.14x | win |
| deep 10000 | after maint: full clone, cold server RSS MiB | 116 | 10.52 | 0.09x | win | 10.64 | 0.09x | win | 11.93 | 0.10x | win |
| deep 10000 | after maint: partial/zone clone, warm server RSS MiB | 146 | 13.49 | 0.09x | win | 9.74 | 0.07x | win | 15.56 | 0.11x | win |
| deep 10000 | after maint: partial/zone clone, cold server RSS MiB | 120 | 6.80 | 0.06x | win | 6.67 | 0.06x | win | 7.20 | 0.06x | win |
| deep 10000 | after maint: fetch 1 commit, warm server RSS MiB | 153 | 6.98 | 0.05x | win | 3.71 | 0.02x | win | 7.32 | 0.05x | win |
| deep 10000 | after maint: fetch 1 commit, cold server RSS MiB | 131 | 3.42 | 0.03x | win | 3.41 | 0.03x | win | 3.41 | 0.03x | win |
| deep 10000 | after maint: no-op fetch, warm server RSS MiB | 134 | 3.38 | 0.03x | win | 3.47 | 0.03x | win | 3.41 | 0.03x | win |
| deep 10000 | after maint: no-op fetch, cold server RSS MiB | 115 | 3.34 | 0.03x | win | 3.34 | 0.03x | win | 3.37 | 0.03x | win |
| deep 10000 | initial push store traffic up+down MiB | 5.99 | 6.94 | 1.16x | LOSS | 7.31 | 1.22x | LOSS | 8.83 | 1.48x | LOSS |
| deep 10000 | 1-file push store traffic up+down MiB | 0.00 | 0.00 | 0.94x | win | 0.00 | 0.96x | tie | 0.00 | 0.72x | win |
| deep 10000 | full clone, warm store traffic up+down MiB | 0.00 | 6.94 | inf | LOSS | 7.32 | inf | LOSS | 8.83 | inf | LOSS |
| deep 10000 | full clone, cold store traffic up+down MiB | 5.98 | 6.94 | 1.16x | LOSS | 7.32 | 1.22x | LOSS | 8.83 | 1.48x | LOSS |
| deep 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 3.44 | 35762.46x | LOSS | 7.32 | 75950.03x | LOSS | 3.60 | 37412.73x | LOSS |
| deep 10000 | partial/zone clone, cold store traffic up+down MiB | 5.98 | 3.44 | 0.58x | win | 7.32 | 1.22x | LOSS | 3.60 | 0.60x | win |
| deep 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| deep 10000 | fetch 1 commit, cold store traffic up+down MiB | 5.99 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.00 | 0.00x | win |
| deep 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| deep 10000 | no-op fetch, cold store traffic up+down MiB | 0.98 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| deep 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 6.94 | inf | LOSS | 7.32 | inf | LOSS | 8.84 | inf | LOSS |
| deep 10000 | after maint: full clone, cold store traffic up+down MiB | 6.13 | 6.94 | 1.13x | LOSS | 7.32 | 1.19x | LOSS | 8.84 | 1.44x | LOSS |
| deep 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 3.45 | inf | LOSS | 7.32 | inf | LOSS | 3.60 | inf | LOSS |
| deep 10000 | after maint: partial/zone clone, cold store traffic up+down MiB | 6.13 | 3.45 | 0.56x | win | 7.32 | 1.19x | LOSS | 3.60 | 0.59x | win |
| deep 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.01 | inf | LOSS | 0.00 | inf | LOSS |
| deep 10000 | after maint: fetch 1 commit, cold store traffic up+down MiB | 6.14 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.00 | 0.00x | win |
| deep 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 6.55x | LOSS | 0.00 | 6.55x | LOSS | 0.00 | 6.55x | LOSS |
| deep 10000 | after maint: no-op fetch, cold store traffic up+down MiB | 0.32 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| flat 10000 | initial push server RSS MiB | 85.68 | 10.78 | 0.13x | win | 10.78 | 0.13x | win | 10.78 | 0.13x | win |
| flat 10000 | 1-file push server RSS MiB | 91.86 | 3.51 | 0.04x | win | 3.48 | 0.04x | win | 3.49 | 0.04x | win |
| flat 10000 | full clone, warm server RSS MiB | 89.41 | 10.36 | 0.12x | win | 21.11 | 0.24x | win | 10.23 | 0.11x | win |
| flat 10000 | full clone, cold server RSS MiB | 98.15 | 8.23 | 0.08x | win | 11.60 | 0.12x | win | 9.89 | 0.10x | win |
| flat 10000 | partial/zone clone, warm server RSS MiB | 136 | 9.73 | 0.07x | win | 20.52 | 0.15x | win | 9.88 | 0.07x | win |
| flat 10000 | partial/zone clone, cold server RSS MiB | 102 | 4.89 | 0.05x | win | 8.83 | 0.09x | win | 5.13 | 0.05x | win |
| flat 10000 | fetch 1 commit, warm server RSS MiB | 149 | 5.25 | 0.04x | win | 4.58 | 0.03x | win | 5.29 | 0.04x | win |
| flat 10000 | fetch 1 commit, cold server RSS MiB | 109 | 3.46 | 0.03x | win | 3.41 | 0.03x | win | 3.43 | 0.03x | win |
| flat 10000 | no-op fetch, warm server RSS MiB | 138 | 3.46 | 0.03x | win | 3.41 | 0.02x | win | 3.49 | 0.03x | win |
| flat 10000 | no-op fetch, cold server RSS MiB | 103 | 3.38 | 0.03x | win | 3.33 | 0.03x | win | 3.33 | 0.03x | win |
| flat 10000 | after maint: full clone, warm server RSS MiB | 140 | 8.39 | 0.06x | win | 11.99 | 0.09x | win | 9.64 | 0.07x | win |
| flat 10000 | after maint: full clone, cold server RSS MiB | 105 | 8.27 | 0.08x | win | 11.59 | 0.11x | win | 9.34 | 0.09x | win |
| flat 10000 | after maint: partial/zone clone, warm server RSS MiB | 148 | 8.00 | 0.05x | win | 14.04 | 0.09x | win | 11.82 | 0.08x | win |
| flat 10000 | after maint: partial/zone clone, cold server RSS MiB | 109 | 4.07 | 0.04x | win | 9.02 | 0.08x | win | 5.39 | 0.05x | win |
| flat 10000 | after maint: fetch 1 commit, warm server RSS MiB | 170 | 4.04 | 0.02x | win | 4.79 | 0.03x | win | 5.56 | 0.03x | win |
| flat 10000 | after maint: fetch 1 commit, cold server RSS MiB | 116 | 3.36 | 0.03x | win | 3.36 | 0.03x | win | 3.45 | 0.03x | win |
| flat 10000 | after maint: no-op fetch, warm server RSS MiB | 135 | 3.36 | 0.02x | win | 3.36 | 0.02x | win | 3.37 | 0.02x | win |
| flat 10000 | after maint: no-op fetch, cold server RSS MiB | 107 | 3.38 | 0.03x | win | 3.36 | 0.03x | win | 3.33 | 0.03x | win |
| flat 10000 | initial push store traffic up+down MiB | 4.23 | 4.83 | 1.14x | LOSS | 4.58 | 1.08x | LOSS | 6.90 | 1.63x | LOSS |
| flat 10000 | 1-file push store traffic up+down MiB | 0.01 | 0.01 | 1.07x | LOSS | 0.01 | 1.08x | LOSS | 0.01 | 1.09x | LOSS |
| flat 10000 | full clone, warm store traffic up+down MiB | 0.00 | 4.84 | inf | LOSS | 4.59 | inf | LOSS | 6.91 | inf | LOSS |
| flat 10000 | full clone, cold store traffic up+down MiB | 4.22 | 4.84 | 1.15x | LOSS | 4.59 | 1.09x | LOSS | 6.91 | 1.64x | LOSS |
| flat 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 1.36 | inf | LOSS | 3.42 | inf | LOSS | 1.70 | inf | LOSS |
| flat 10000 | partial/zone clone, cold store traffic up+down MiB | 4.22 | 1.36 | 0.32x | win | 3.42 | 0.81x | win | 1.70 | 0.40x | win |
| flat 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| flat 10000 | fetch 1 commit, cold store traffic up+down MiB | 4.23 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| flat 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| flat 10000 | no-op fetch, cold store traffic up+down MiB | 4.23 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |
| flat 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 4.85 | inf | LOSS | 4.59 | inf | LOSS | 6.92 | inf | LOSS |
| flat 10000 | after maint: full clone, cold store traffic up+down MiB | 4.27 | 4.85 | 1.14x | LOSS | 4.59 | 1.08x | LOSS | 6.92 | 1.62x | LOSS |
| flat 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 1.36 | inf | LOSS | 3.42 | inf | LOSS | 1.70 | inf | LOSS |
| flat 10000 | after maint: partial/zone clone, cold store traffic up+down MiB | 4.27 | 1.36 | 0.32x | win | 3.42 | 0.80x | win | 1.70 | 0.40x | win |
| flat 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | inf | LOSS | 0.01 | inf | LOSS | 0.01 | inf | LOSS |
| flat 10000 | after maint: fetch 1 commit, cold store traffic up+down MiB | 4.28 | 0.01 | 0.00x | win | 0.01 | 0.00x | win | 0.01 | 0.00x | win |
| flat 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | inf | LOSS | 0.00 | inf | LOSS | 0.00 | inf | LOSS |
| flat 10000 | after maint: no-op fetch, cold store traffic up+down MiB | 0.29 | 0.00 | 0.00x | win | 0.00 | 0.00x | win | 0.00 | 0.00x | win |

Totals over 108 cells (win / tie / LOSS): vcs default [73, 4, 31]; vcs best-doc [69, 5, 34]; vcs fastest [73, 4, 31].

### Cells where walgit still beats vcs (scoreboard 2: server memory and object-store traffic)

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| binary 10000 | full clone, warm server RSS MiB | 86.84 | 43.21 | 123 | 45.73 | best-doc |
| binary 10000 | full clone, warm store traffic up+down MiB | 0.00 | 40.29 | 40.29 | 40.43 | default, best-doc, fastest |
| binary 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 4.90 | 8.82 | 5.04 | default, best-doc, fastest |
| binary 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| binary 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| binary 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 40.31 | 40.31 | 40.45 | default, best-doc, fastest |
| binary 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 4.90 | 8.82 | 5.04 | default, best-doc, fastest |
| binary 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| binary 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| deep 10000 | initial push store traffic up+down MiB | 5.99 | 6.94 | 7.31 | 8.83 | default, best-doc, fastest |
| deep 10000 | full clone, warm store traffic up+down MiB | 0.00 | 6.94 | 7.32 | 8.83 | default, best-doc, fastest |
| deep 10000 | full clone, cold store traffic up+down MiB | 5.98 | 6.94 | 7.32 | 8.83 | default, best-doc, fastest |
| deep 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 3.44 | 7.32 | 3.60 | default, best-doc, fastest |
| deep 10000 | partial/zone clone, cold store traffic up+down MiB | 5.98 | 3.44 | 7.32 | 3.60 | best-doc |
| deep 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| deep 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| deep 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 6.94 | 7.32 | 8.84 | default, best-doc, fastest |
| deep 10000 | after maint: full clone, cold store traffic up+down MiB | 6.13 | 6.94 | 7.32 | 8.84 | default, best-doc, fastest |
| deep 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 3.45 | 7.32 | 3.60 | default, best-doc, fastest |
| deep 10000 | after maint: partial/zone clone, cold store traffic up+down MiB | 6.13 | 3.45 | 7.32 | 3.60 | best-doc |
| deep 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.00 | 0.01 | 0.00 | default, best-doc, fastest |
| deep 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| flat 10000 | initial push store traffic up+down MiB | 4.23 | 4.83 | 4.58 | 6.90 | default, best-doc, fastest |
| flat 10000 | 1-file push store traffic up+down MiB | 0.01 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| flat 10000 | full clone, warm store traffic up+down MiB | 0.00 | 4.84 | 4.59 | 6.91 | default, best-doc, fastest |
| flat 10000 | full clone, cold store traffic up+down MiB | 4.22 | 4.84 | 4.59 | 6.91 | default, best-doc, fastest |
| flat 10000 | partial/zone clone, warm store traffic up+down MiB | 0.00 | 1.36 | 3.42 | 1.70 | default, best-doc, fastest |
| flat 10000 | fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| flat 10000 | no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |
| flat 10000 | after maint: full clone, warm store traffic up+down MiB | 0.00 | 4.85 | 4.59 | 6.92 | default, best-doc, fastest |
| flat 10000 | after maint: full clone, cold store traffic up+down MiB | 4.27 | 4.85 | 4.59 | 6.92 | default, best-doc, fastest |
| flat 10000 | after maint: partial/zone clone, warm store traffic up+down MiB | 0.00 | 1.36 | 3.42 | 1.70 | default, best-doc, fastest |
| flat 10000 | after maint: fetch 1 commit, warm store traffic up+down MiB | 0.00 | 0.01 | 0.01 | 0.01 | default, best-doc, fastest |
| flat 10000 | after maint: no-op fetch, warm store traffic up+down MiB | 0.00 | 0.00 | 0.00 | 0.00 | default, best-doc, fastest |

## Scoreboard 3: space (secondary)

Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.

| dataset | metric | walgit | vcs default | ratio | result | vcs best-doc | ratio | result | vcs fastest | ratio | result |
|---|---|---|---|---|---|---|---|---|---|---|---|
| binary 10000 | remote store after initial push MiB (du) | 39.79 | 40.34 | 1.01x | tie | 40.34 | 1.01x | tie | 40.48 | 1.02x | tie |
| binary 10000 | remote store after maintenance MiB (du) | 120 | 40.59 | 0.34x | win | 40.59 | 0.34x | win | 40.75 | 0.34x | win |
| binary 10000 | full-clone client store MiB (du) | 40.82 | 41.50 | 1.02x | tie | 41.50 | 1.02x | tie | 41.49 | 1.02x | tie |
| binary 10000 | partial-clone client store MiB (du) | 5.34 | 4.87 | 0.91x | win | 4.87 | 0.91x | win | 4.87 | 0.91x | win |
| deep 10000 | remote store after initial push MiB (du) | 6.03 | 6.99 | 1.16x | LOSS | 7.36 | 1.22x | LOSS | 8.89 | 1.47x | LOSS |
| deep 10000 | remote store after maintenance MiB (du) | 12.68 | 7.14 | 0.56x | win | 7.52 | 0.59x | win | 9.04 | 0.71x | win |
| deep 10000 | full-clone client store MiB (du) | 7.95 | 11.33 | 1.43x | LOSS | 11.33 | 1.43x | LOSS | 11.33 | 1.43x | LOSS |
| deep 10000 | partial-clone client store MiB (du) | 4.43 | 2.57 | 0.58x | win | 2.57 | 0.58x | win | 2.57 | 0.58x | win |
| flat 10000 | remote store after initial push MiB (du) | 4.27 | 4.89 | 1.14x | LOSS | 4.64 | 1.09x | LOSS | 6.96 | 1.63x | LOSS |
| flat 10000 | remote store after maintenance MiB (du) | 9.11 | 5.08 | 0.56x | win | 4.83 | 0.53x | win | 7.17 | 0.79x | win |
| flat 10000 | full-clone client store MiB (du) | 5.29 | 8.14 | 1.54x | LOSS | 8.14 | 1.54x | LOSS | 8.14 | 1.54x | LOSS |
| flat 10000 | partial-clone client store MiB (du) | 1.80 | 1.55 | 0.86x | win | 1.55 | 0.86x | win | 1.55 | 0.86x | win |

Totals over 12 cells (win / tie / LOSS): vcs default [6, 2, 4]; vcs best-doc [6, 2, 4]; vcs fastest [6, 2, 4].

### Cells where walgit still beats vcs (scoreboard 3: space (secondary))

| dataset | metric | walgit | vcs default | vcs best-doc | vcs fastest | vcs variants that lose |
|---|---|---|---|---|---|---|
| deep 10000 | remote store after initial push MiB (du) | 6.03 | 6.99 | 7.36 | 8.89 | default, best-doc, fastest |
| deep 10000 | full-clone client store MiB (du) | 7.95 | 11.33 | 11.33 | 11.33 | default, best-doc, fastest |
| flat 10000 | remote store after initial push MiB (du) | 4.27 | 4.89 | 4.64 | 6.96 | default, best-doc, fastest |
| flat 10000 | full-clone client store MiB (du) | 5.29 | 8.14 | 8.14 | 8.14 | default, best-doc, fastest |


---

# Earlier run (superseded; older vcs build, kept for reference)

# walgit vs vcs vs plain git on remote workflows (local RustFS)

Status: measured on one shared box, 2026-11-01. Research branch work for epic #190 (storage engine), companion to
[storage/walgit-analysis.md](storage/walgit-analysis.md). Raw JSON: `docs/research/data/walgit-bench/`. Driver:
`scripts/walgit_bench/walgit_bench.py`, table generator `report.py`, counting proxy `s3count_proxy.py`.
**Everything here is loopback on one machine against a local RustFS: no network, no cloud. Nothing is a statement about AWS S3.**
Only numbers in the tables below were measured; anything not run is listed under "Not run".

## What is and is not walgit "timing"

walgit is a **server**. It has no client: `add`, `commit`, `status`, `log` and `checkout` on a walgit repository are stock
`git` running on the client, so their cost is exactly the git column of the local-workflow table in
[benchmarks.md](benchmarks.md) (100k-file scale run, git vs vcs). The only walgit-specific costs are the remote ones:
push, clone, fetch (this document). Do not read the local git numbers as walgit numbers of its own.

## Setup (all CONFIRMED, from the JSON `meta` blocks)

| Item | Value |
|---|---|
| Machine | `lambda-final`, AMD EPYC 7J13, 30 cores, kernel 6.8.0-1046-nvidia, ext4 on local disk. **Shared with other agents' jobs**: load1 at start of each dataset is in the table headings (17 to 57); expect noise, read the min-max |
| git | 2.56.0 (git-core PPA). The box had 2.34.1, which broke 9 of 57 walgit server tests; see "walgit test suite" |
| walgit | `80e9a20b29e2` (tobi/walgit), release build, one process, roles all (serve+maintain+events), auth `none`, TLS off, `cache.dir` on local disk |
| vcs | this repo at `28e2980` plus this PR, release build; `vcs-server --backend s3://vcs/<prefix>?endpoint=...`, bearer token; the `vcs` CLI talks HTTP to it |
| Object store | RustFS 1.0.4, single node, one data dir, own ports 19100 (RustFS), 19101 (walgit), 19102 (vcs-server), 19110 (counting proxy), started fresh per tool and pass, stopped at the end |
| Plain git reference | bare repo on local disk, `file://` transport (upload-pack/receive-pack as local child processes; no HTTP, no object store) |
| Datasets | `datagen flat|deep|binary N`, seed 42, then regrouped into 10 contiguous groups `g0..g9` (one extra leading path level, same bytes). History: initial commit + 50 commits each editing 1 file (deterministic, seed 42 index formula in the script). `g0` is ~10% of files |
| Client commands | git/walgit: stock git over HTTP (v2). vcs: `vcs publish`, `vcs clone`, `vcs fetch`. `gc.auto=0` and `maintenance.auto=false` for all git invocations so no background repack runs during a measurement |

Reps: median of 5 (10k) with min-max in parentheses; the 100k datasets use 3 reps and 2 fresh remotes for the initial push (time budget on a loaded box), stated in each heading.

### How the operations map

| Operation | git (plain) | walgit | vcs |
|---|---|---|---|
| initial push | `git push` of 51 commits to an empty bare repo | `git push` to a new repo (auto-create) | `vcs publish` to a fresh prefix (51 commits across 11 zones: root + g0..g9) |
| 1-file push | edit, commit (untimed), push | same | edit, commit, `vcs publish` |
| full clone | `git clone file://` | `git clone http://` | `vcs clone` (all zones) |
| partial clone, 10% | `--filter=blob:none --sparse` then `sparse-checkout set g0` | same (walgit supports filters; verified) | `vcs clone --zone g0` |
| fetch 1 commit | `git fetch` after another clone pushed 1 commit | same | `vcs fetch` |

**The partial clones are not the same thing.** git/walgit: full commit and tree history, blobs fetched lazily for the cone (a second round trip for the blobs). vcs: the zone's history only, no other zone's commits, trees or blobs. The client-store rows show the consequence.

Cold variants: `cold_os` = `sync; echo 3 > /proc/sys/vm/drop_caches` before each run (sudo is permitted on the box; page cache of client, server and RustFS dropped together). `cold_server` (walgit only) = walgit stopped, its cache dir deleted, restarted with `roles=["serve"]` (the maintainer role otherwise re-materialises every repo within a second of start-up, which hides the cold cost), then drop_caches. vcs-server keeps no state, so it has no separate cold-server case; its `warm` and `cold_os` rows differ only by the OS cache.

Server CPU = utime+stime (+ reaped children) delta of the walgit-server / vcs-server process over the command; "rf" = same for RustFS. Request and byte counts come from a counting HTTP proxy placed between the server and RustFS in a separate pass (1 run per operation; the timed pass talks to RustFS directly). The vcs CLI's own `--stats` counts client-to-vcs-server requests, not store requests, so it was not used for the store columns.

## Results (clean pass, gc.auto=0)

### binary 10000 files (39.1 MiB, g0 = 1000 files), reps 5, init reps 5, load1 at start 13.97, end 10.52

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 3419 (3305-3519) | 3896 (3813-4975) srv 0.71s rf 0.31s | 585 (560-671) srv 0.12s rf 0.26s |
| 1-file push |  | 49 (35-63) | 78 (74-103) srv 0.02s rf 0.02s | 35 (34-39) srv 0.00s rf 0.02s |
| full clone | warm | 1063 (896-1205) | 832 (825-876) srv 0.31s rf 0.00s | 2699 (2676-2791) srv 0.14s rf 0.22s |
| full clone | cold_os | 907 (893-912) | 922 (907-942) srv 0.35s rf 0.00s | 2789 (2690-2897) srv 0.13s rf 0.24s |
| full clone | cold_server | n/a | 994 (988-1033) srv 0.44s rf 0.10s | n/a |
| partial clone (10%) | warm | 184 (181-226) | 207 (203-216) srv 0.06s rf 0.00s | 336 (312-349) srv 0.02s rf 0.04s |
| partial clone (10%) | cold_os | 244 (243-259) | 262 (260-291) srv 0.08s rf 0.00s | 352 (329-364) srv 0.03s rf 0.04s |
| partial clone (10%) | cold_server | n/a | 380 (371-490) srv 0.19s rf 0.09s | n/a |
| fetch 1 commit | warm | 27 (25-51) | 37 (35-58) srv 0.01s rf 0.00s | 212 (187-238) srv 0.07s rf 0.10s |
| fetch 1 commit | cold_os | 70 (64-74) | 95 (93-103) srv 0.01s rf 0.00s | 287 (277-322) srv 0.09s rf 0.14s |
| fetch 1 commit | cold_server | n/a | 235 (225-275) srv 0.18s rf 0.12s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 5 identical repos) | 39.9 MiB | 39.8 MiB | 40.5 MiB |
| full clone: files checked out / client store | 10000 / 40.8 MiB | 10000 / 40.8 MiB | 10000 / 81.8 MiB |
| partial clone: files checked out / client store | 1000 / 5.3 MiB | 1000 / 5.3 MiB | 1000 / 9.2 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 23 (13/5/5) 39.8 MiB / 1 KiB | 8 (4/4/0) 40.4 MiB / 0 KiB |
| idle_65s_after_push | 120 (69/31/20) 6 KiB / 3 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 10 KiB / 0 KiB | 8 (3/4/1) 10 KiB / 1 KiB |
| clone_full_warm | 4 (4/0/0) 0 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 49.8 MiB |
| clone_full_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 49.8 MiB |
| clone_full_cold_server | 13 (13/0/0) 0 KiB / 39.7 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 6.6 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 6.6 MiB |
| clone_partial_cold_server | 16 (16/0/0) 0 KiB / 39.7 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 49 (49/0/0) 0 KiB / 4.8 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 60 (60/0/0) 0 KiB / 4.8 MiB |
| fetch_1_cold_server | 22 (22/0/0) 0 KiB / 39.8 MiB | n/a |

### deep 10000 files (5.7 MiB, g0 = 1000 files), reps 5, init reps 5, load1 at start 21.78, end 13.80

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 3113 (3022-4111) | 3355 (3324-3372) srv 0.51s rf 0.11s | 631 (571-1237) srv 0.02s rf 0.09s |
| 1-file push |  | 145 (142-145) | 91 (89-112) srv 0.05s rf 0.02s | 32 (30-34) srv 0.00s rf 0.02s |
| full clone | warm | 1984 (1891-2168) | 1821 (1787-2186) srv 0.04s rf 0.07s | 7901 (6875-9198) srv 0.13s rf 0.22s |
| full clone | cold_os | 2001 (1964-2635) | 2521 (1936-4750) srv 0.04s rf 0.00s | 9352 (7722-9742) srv 0.14s rf 0.22s |
| full clone | cold_server | n/a | 3124 (2461-3743) srv 0.24s rf 0.02s | n/a |
| partial clone (10%) | warm | 551 (515-614) | 506 (503-577) srv 0.04s rf 0.00s | 821 (765-841) srv 0.03s rf 0.04s |
| partial clone (10%) | cold_os | 577 (559-598) | 565 (555-587) srv 0.05s rf 0.00s | 810 (782-863) srv 0.03s rf 0.05s |
| partial clone (10%) | cold_server | n/a | 795 (773-886) srv 0.25s rf 0.03s | n/a |
| fetch 1 commit | warm | 59 (56-65) | 72 (69-84) srv 0.01s rf 0.00s | 331 (315-370) srv 0.07s rf 0.10s |
| fetch 1 commit | cold_os | 110 (107-112) | 131 (126-138) srv 0.01s rf 0.00s | 412 (398-449) srv 0.10s rf 0.14s |
| fetch 1 commit | cold_server | n/a | 373 (357-408) srv 0.26s rf 0.05s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 5 identical repos) | 6.2 MiB | 6.0 MiB | 8.9 MiB |
| full clone: files checked out / client store | 10000 / 7.9 MiB | 10000 / 7.9 MiB | 10000 / 150.1 MiB |
| partial clone: files checked out / client store | 1000 / 4.4 MiB | 1000 / 4.4 MiB | 1000 / 15.6 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 11 (6/5/0) 6.0 MiB / 0 KiB | 8 (4/4/0) 8.8 MiB / 0 KiB |
| idle_65s_after_push | 132 (76/31/25) 6 KiB / 4 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 4 KiB / 0 KiB | 8 (3/4/1) 2 KiB / 1 KiB |
| clone_full_warm | 30 (18/10/2) 2 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 43.0 MiB |
| clone_full_cold_os | 5 (5/0/0) 0 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 43.0 MiB |
| clone_full_cold_server | 12 (12/0/0) 0 KiB / 6.0 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 10.4 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 10.4 MiB |
| clone_partial_cold_server | 15 (15/0/0) 0 KiB / 6.0 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 48 (48/0/0) 0 KiB / 16.7 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 59 (59/0/0) 0 KiB / 16.7 MiB |
| fetch_1_cold_server | 21 (21/0/0) 0 KiB / 6.0 MiB | n/a |

### flat 10000 files (5.7 MiB, g0 = 1000 files), reps 5, init reps 5, load1 at start 17.36, end 20.02

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 2038 (1959-2167) | 2858 (2247-3091) srv 0.26s rf 0.11s | 333 (243-433) srv 0.02s rf 0.06s |
| 1-file push |  | 30 (29-32) | 102 (92-188) srv 0.03s rf 0.02s | 37 (35-40) srv 0.00s rf 0.02s |
| full clone | warm | 640 (621-700) | 641 (622-1016) srv 0.12s rf 0.00s | 2741 (2276-8542) srv 0.12s rf 0.23s |
| full clone | cold_os | 638 (628-661) | 732 (659-749) srv 0.10s rf 0.00s | 2539 (2191-2639) srv 0.11s rf 0.19s |
| full clone | cold_server | n/a | 1593 (938-2399) srv 0.15s rf 0.03s | n/a |
| partial clone (10%) | warm | 169 (158-191) | 556 (295-697) srv 0.05s rf 0.00s | 274 (267-359) srv 0.02s rf 0.03s |
| partial clone (10%) | cold_os | 196 (192-208) | 1050 (635-1579) srv 0.06s rf 0.00s | 280 (266-298) srv 0.02s rf 0.04s |
| partial clone (10%) | cold_server | n/a | 335 (279-1183) srv 0.10s rf 0.02s | n/a |
| fetch 1 commit | warm | 27 (24-38) | 52 (37-65) srv 0.01s rf 0.00s | 199 (193-294) srv 0.05s rf 0.11s |
| fetch 1 commit | cold_os | 62 (59-66) | 158 (96-1110) srv 0.01s rf 0.00s | 295 (277-343) srv 0.09s rf 0.14s |
| fetch 1 commit | cold_server | n/a | 152 (151-179) srv 0.10s rf 0.04s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 5 identical repos) | 4.4 MiB | 4.3 MiB | 7.0 MiB |
| full clone: files checked out / client store | 10000 / 5.3 MiB | 10000 / 5.3 MiB | 10000 / 42.8 MiB |
| partial clone: files checked out / client store | 1000 / 1.8 MiB | 1000 / 1.8 MiB | 1000 / 5.3 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 11 (6/5/0) 4.2 MiB / 0 KiB | 8 (4/4/0) 6.9 MiB / 0 KiB |
| idle_65s_after_push | 132 (76/31/25) 6 KiB / 4 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 7 KiB / 0 KiB | 8 (3/4/1) 6 KiB / 1 KiB |
| clone_full_warm | 4 (4/0/0) 0 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 16.3 MiB |
| clone_full_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 95 (95/0/0) 0 KiB / 16.3 MiB |
| clone_full_cold_server | 12 (12/0/0) 0 KiB / 4.2 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 3.2 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 3.2 MiB |
| clone_partial_cold_server | 15 (15/0/0) 0 KiB / 4.2 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 49 (49/0/0) 0 KiB / 4.8 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 60 (60/0/0) 0 KiB / 4.8 MiB |
| fetch_1_cold_server | 21 (21/0/0) 0 KiB / 4.2 MiB | n/a |

### binary 100000 files (390.6 MiB, g0 = 10000 files), reps 3, init reps 2, load1 at start 8.34, end 36.50

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 26824 (25674-27974) | 31074 (30580-31568) srv 9.12s rf 2.51s | 13272 (8768-17775) srv 1.25s rf 2.37s |
| 1-file push |  | 92 (88-103) | 143 (124-144) srv 0.08s rf 0.02s | 47 (46-48) srv 0.00s rf 0.01s |
| full clone | warm | 10281 (9741-11670) | 8322 (8220-8419) srv 2.23s rf 0.04s | 29892 (25729-32863) srv 0.84s rf 0.88s |
| full clone | cold_os | 10385 (9192-16123) | 8350 (8277-8597) srv 2.29s rf 0.00s | 32165 (23833-34186) srv 0.87s rf 1.00s |
| full clone | cold_server | n/a | 11149 (10760-11430) srv 3.42s rf 2.02s | n/a |
| partial clone (10%) | warm | 1713 (1455-2013) | 2004 (1947-2243) srv 0.36s rf 0.01s | 2464 (2422-2504) srv 0.10s rf 0.11s |
| partial clone (10%) | cold_os | 2014 (1788-2831) | 2270 (2087-2752) srv 0.49s rf 0.00s | 2496 (2414-2556) srv 0.10s rf 0.12s |
| partial clone (10%) | cold_server | n/a | 2914 (2867-3182) srv 1.66s rf 1.93s | n/a |
| fetch 1 commit | warm | 170 (156-582) | 121 (110-364) srv 0.01s rf 0.00s | 686 (683-703) srv 0.09s rf 0.12s |
| fetch 1 commit | cold_os | 315 (306-338) | 420 (382-472) srv 0.01s rf 0.00s | 798 (762-806) srv 0.11s rf 0.18s |
| fetch 1 commit | cold_server | n/a | 1727 (1544-1850) srv 1.37s rf 2.08s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 2 identical repos) | 397.7 MiB | 397.3 MiB | 400.5 MiB |
| full clone: files checked out / client store | 100000 / 406.2 MiB | 100000 / 406.2 MiB | 100000 / 810.5 MiB |
| partial clone: files checked out / client store | 10000 / 51.3 MiB | 10000 / 51.3 MiB | 10000 / 82.0 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 28 (13/11/4) 397.7 MiB / 1 KiB | 8 (4/4/0) 400.4 MiB / 0 KiB |
| idle_65s_after_push | 21 (16/1/4) 0 KiB / 2 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 12 KiB / 0 KiB | 8 (3/4/1) 13 KiB / 1 KiB |
| clone_full_warm | 39 (18/17/4) 398.0 MiB / 1 KiB | 96 (96/0/0) 0 KiB / 486.0 MiB |
| clone_full_cold_os | 11 (9/0/2) 0 KiB / 1 KiB | 96 (96/0/0) 0 KiB / 486.0 MiB |
| clone_full_cold_server | 35 (32/0/3) 0 KiB / 397.5 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 57.2 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 57.2 MiB |
| clone_partial_cold_server | 38 (35/0/3) 0 KiB / 397.5 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 49 (49/0/0) 0 KiB / 46.8 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 60 (60/0/0) 0 KiB / 46.8 MiB |
| fetch_1_cold_server | 44 (41/0/3) 0 KiB / 397.5 MiB | n/a |

### deep 100000 files (57.2 MiB, g0 = 10005 files), reps 3, init reps 2, load1 at start 15.20, end 9.45

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 31462 (29992-32932) | 21521 (21134-21909) srv 3.33s rf 0.36s | 18727 (17081-20373) srv 0.26s rf 0.51s |
| 1-file push |  | 467 (464-507) | 244 (238-249) srv 0.18s rf 0.02s | 50 (49-54) srv 0.00s rf 0.02s |
| full clone | warm | 16554 (11401-17887) | 11425 (11234-12567) srv 0.92s rf 0.01s | 40712 (37934-55116) srv 0.31s rf 0.48s |
| full clone | cold_os | 12500 (12204-13502) | 11805 (11730-12043) srv 0.31s rf 0.00s | 58165 (43075-82993) srv 0.33s rf 0.53s |
| full clone | cold_server | n/a | 16056 (15043-21247) srv 1.18s rf 0.12s | n/a |
| partial clone (10%) | warm | 2613 (2598-2713) | 3071 (2932-3797) srv 0.19s rf 0.01s | 3860 (3666-4234) srv 0.05s rf 0.07s |
| partial clone (10%) | cold_os | 2685 (2681-2770) | 3086 (2719-4059) srv 0.24s rf 0.01s | 3648 (3622-5758) srv 0.05s rf 0.09s |
| partial clone (10%) | cold_server | n/a | 3538 (3447-3794) srv 1.07s rf 0.11s | n/a |
| fetch 1 commit | warm | 203 (194-243) | 231 (220-429) srv 0.02s rf 0.01s | 1196 (1151-1372) srv 0.14s rf 0.19s |
| fetch 1 commit | cold_os | 291 (282-298) | 329 (313-329) srv 0.02s rf 0.00s | 1556 (1212-2035) srv 0.16s rf 0.26s |
| fetch 1 commit | cold_server | n/a | 1217 (1194-1231) srv 0.95s rf 0.13s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 2 identical repos) | 49.3 MiB | 48.6 MiB | 73.7 MiB |
| full clone: files checked out / client store | 100000 / 61.6 MiB | 100000 / 61.6 MiB | 100000 / 774.7 MiB |
| partial clone: files checked out / client store | 10005 / 26.4 MiB | 10005 / 26.4 MiB | 10005 / 78.7 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 26 (18/6/2) 48.6 MiB / 1 KiB | 8 (4/4/0) 73.6 MiB / 0 KiB |
| idle_65s_after_push | 19 (13/2/4) 0 KiB / 2 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 5 KiB / 0 KiB | 8 (3/4/1) 3 KiB / 1 KiB |
| clone_full_warm | 10 (8/0/2) 0 KiB / 1 KiB | 87 (87/0/0) 0 KiB / 232.8 MiB |
| clone_full_cold_os | 5 (5/0/0) 0 KiB / 0 KiB | 87 (87/0/0) 0 KiB / 232.8 MiB |
| clone_full_cold_server | 13 (13/0/0) 0 KiB / 48.5 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 38.8 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 20 (20/0/0) 0 KiB / 38.8 MiB |
| clone_partial_cold_server | 16 (16/0/0) 0 KiB / 48.5 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 48 (48/0/0) 0 KiB / 86.8 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 59 (59/0/0) 0 KiB / 86.8 MiB |
| fetch_1_cold_server | 22 (22/0/0) 0 KiB / 48.5 MiB | n/a |

### flat 100000 files (57.2 MiB, g0 = 10000 files), reps 3, init reps 2, load1 at start 12.22, end 13.17

Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.

| op | variant | git (local bare) | walgit | vcs-server |
|---|---|---|---|---|
| initial push | 51 commits | 14597 (14455-14739) | 13845 (13562-14127) srv 2.10s rf 0.31s | 7387 (2057-12717) srv 0.18s rf 0.42s |
| 1-file push |  | 155 (103-161) | 134 (119-450) srv 0.08s rf 0.01s | 35 (33-140) srv 0.00s rf 0.02s |
| full clone | warm | 6434 (6009-6817) | 6063 (5637-6548) srv 0.22s rf 0.01s | 17868 (16684-22272) srv 0.23s rf 0.35s |
| full clone | cold_os | 6042 (6029-6119) | 10096 (9297-10705) srv 0.28s rf 0.01s | 18547 (17336-18815) srv 0.23s rf 0.39s |
| full clone | cold_server | n/a | 7544 (7047-9514) srv 0.72s rf 0.09s | n/a |
| partial clone (10%) | warm | 1456 (1201-1810) | 1425 (1357-1431) srv 0.16s rf 0.00s | 1849 (1797-1881) srv 0.03s rf 0.05s |
| partial clone (10%) | cold_os | 1308 (1298-1317) | 1586 (1453-1852) srv 0.18s rf 0.00s | 1905 (1840-2396) srv 0.03s rf 0.08s |
| partial clone (10%) | cold_server | n/a | 1802 (1796-1991) srv 0.62s rf 0.10s | n/a |
| fetch 1 commit | warm | 84 (77-163) | 102 (99-184) srv 0.01s rf 0.01s | 1463 (1301-1944) srv 0.12s rf 0.19s |
| fetch 1 commit | cold_os | 175 (175-176) | 207 (205-213) srv 0.01s rf 0.00s | 1694 (860-2164) srv 0.12s rf 0.18s |
| fetch 1 commit | cold_server | n/a | 667 (657-675) srv 0.51s rf 0.12s | n/a |

| fact | git | walgit | vcs |
|---|---|---|---|
| remote store per repo after initial push (du; walgit/vcs bucket du / 2 identical repos) | 42.5 MiB | 42.1 MiB | 66.8 MiB |
| full clone: files checked out / client store | 100000 / 51.0 MiB | 100000 / 51.0 MiB | 100000 / 419.8 MiB |
| partial clone: files checked out / client store | 10000 / 15.8 MiB | 10000 / 15.8 MiB | 10000 / 43.0 MiB |

Object-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):

| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |
|---|---|---|
| init_push | 40 (23/13/4) 42.1 MiB / 1 KiB | 8 (4/4/0) 66.8 MiB / 0 KiB |
| idle_65s_after_push | 36 (22/8/6) 1 KiB / 2 KiB | 0 (0/0/0) 0 KiB / 0 KiB |
| inc_push | 7 (3/4/0) 8 KiB / 0 KiB | 8 (3/4/1) 9 KiB / 1 KiB |
| clone_full_warm | 4 (4/0/0) 0 KiB / 0 KiB | 87 (87/0/0) 0 KiB / 152.3 MiB |
| clone_full_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 87 (87/0/0) 0 KiB / 152.3 MiB |
| clone_full_cold_server | 13 (13/0/0) 0 KiB / 42.0 MiB | n/a |
| clone_partial_warm | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 23.8 MiB |
| clone_partial_cold_os | 7 (7/0/0) 0 KiB / 0 KiB | 19 (19/0/0) 0 KiB / 23.8 MiB |
| clone_partial_cold_server | 16 (16/0/0) 0 KiB / 42.0 MiB | n/a |
| fetch_1_warm | 4 (4/0/0) 0 KiB / 0 KiB | 49 (49/0/0) 0 KiB / 46.8 MiB |
| fetch_1_cold_os | 4 (4/0/0) 0 KiB / 0 KiB | 60 (60/0/0) 0 KiB / 46.8 MiB |
| fetch_1_cold_server | 22 (22/0/0) 0 KiB / 42.0 MiB | n/a |

## What each system does that the other does not (CONFIRMED from the runs above and from walgit-analysis.md; mechanism statements about walgit source are from that analysis)

walgit does, vcs does not:

* **A local materialised copy on the server.** Warm reads are served from `cache.dir`: the counting pass shows 4 conditional GETs (all 304, 0 payload bytes) for a warm full clone or a fetch. A cold server pulls the packs once (see the `cold_server` rows: the whole pack set, e.g. 4.2 MiB on flat 10k, even for a partial clone). vcs-server is a stateless pass-through: every clone reads from the store (95 GETs, 16.3 MiB on flat 10k).
* **A WAL plus manifest CAS** (immutable log segment, then manifest). A push is 5 PUT-class requests and 7 to 11 requests in total in our counts, independent of file count.
* **Git packs with `.idx`, `.rev`, bitmap, commit-graph side files; multi-pack bitmaps and pack reuse; history packs.** These make the git protocol cheap to serve (thin packs, negotiation) and are why the bytes on the wire for clone/fetch match plain git. Not measured separately here: we did not compare with and without them.
* **A maintainer loop** (checkpoints, compaction, fsck). Visible cost: `idle_65s_after_push` in the counting table, 60 to 130 requests per minute in the idle server with one repo present, vcs-server 0.
* Stock git clients, partial clone filters and push policies with no client changes.

vcs does, walgit does not:

* **Zones**: the 10% subscription reads only that zone's objects (19 GETs, 3.2 MiB on flat 10k) and writes a client store of 5.3 MiB against 42.8 MiB for the full clone. walgit's partial clone gets the same 1000 files but transfers the full tree/commit history first.
* **One atomic multi-zone transaction** and per-zone write policy at the server (see docs/server.md); walgit's partitions are pack groups, not paths.
* A server with **no local state** (nothing to warm, evict or place).

Where vcs is worse in these measurements: full clone and fetch are several times slower than git/walgit and read far more from the store (fetch of 1 commit downloads MiB where git/walgit move KiB); the client store is 2x (binary) to roughly 10-20x (flat/deep) larger than git's (see the client-store rows; vcs keeps loose objects). The cause of the fetch volume was not diagnosed here (HYPOTHESIS: it re-reads per-zone history/manifest data; unverified). Also the vcs initial push uploads loose objects while git uploads one pack; at flat 10k the vcs initial push is faster than git's because git has to pack 10k loose objects first.

## walgit test suite with git 2.56.0 (re-run)

`cargo test -p walgit-server --release` per target on the box, git 2.56.0, SPA not built (logs `~/ref/walgit-tests4.log` on the box):

| Target | Passed | Failed | Note |
|---|---|---|---|
| `e2e` | 38 | 0 | 1 ignored by design; was 34/4 with git 2.34.1 |
| `packfile_uri` | 5 | 0 | was 3/2 |
| `maintain` | 9 | 0 | was 7/2 |
| `static_http` | 4 | 1 | `ui_assets_etag_304_and_precompressed`: needs the web SPA (node/pnpm), not a git issue |

Total 56 passed, 1 failed (the SPA test). The earlier 9 failures were the old git, as suspected. Other suites from walgit-analysis.md were not re-run.

How git was obtained: `sudo add-apt-repository ppa:git-core/ppa` and `sudo apt-get install git` on the box. **This upgraded the system git (2.34.1 to 2.56.0) rather than installing into a private prefix**, because the apt route replaces the package; building from source was not attempted (an earlier agent reported it blocked). Other jobs on the box now see git 2.56.0. To revert: `sudo apt-get install git=1:2.34.1-1ubuntu1.16 git-man=...` after removing the PPA (not done).

## Findings from the 100k runs worth stating

* **vcs-server refused the 390 MiB binary-100k publish with its default settings.** The single pack upload exceeds the default `max_body` of 256 MiB; the client sees `Connection reset by peer` and reports `outcome unknown after 8 retries` (a 413-style refusal surfaced as a reset). The vcs rows for binary 100k come from a rerun with `vcs-server --max-body 2147483648` (separate JSON run, recorded in `meta.note`). walgit accepted the same data (git protocol, multipart store uploads) with default settings.
* **walgit's server CPU on large pushes**: 9.1 s of walgit-server CPU for the binary 100k initial push against 1.3 s for vcs-server (it indexes and verifies the pack: `index-pack`, connectivity), plus background maintenance. In the counting pass of that dataset the warm full clone window also contained 17 PUTs and about 398 MiB of reads from the store; we attribute this to the maintainer compacting after the push (not verified by log inspection).
* **vcs fetch of one commit** reads tens of MiB from the store at 100k files (47 MiB on flat/binary 100k), cause not diagnosed.

## Caveats

* Shared, loaded machine (load1 17 to 65 across runs). A first pass over the 10k datasets ran with git's default `gc.auto`; it let git auto-pack the source repository after the initial commit, which made git/walgit initial pushes look faster (a packed source) than a freshly committed tree. That pass is kept as `data/walgit-bench/gc-default-10k/` and is NOT used in the tables; the tables use `gc.auto=0` (loose-object source, as after a normal commit of a big tree).
* "remote store" is `du` of the RustFS data dir (allocated blocks, includes RustFS metadata per object), divided by the number of identical repos; compare with the payload bytes the proxy counted.
* The proxy pass is a single run per operation; counts are deterministic here but walgit's background maintainer adds a few requests of its own.
* One RustFS node on local disk: request latency is microseconds-to-milliseconds, so the request counts are the portable result and the wall times are not predictive of S3.
* The plain git reference uses `file://` and so has no network or HTTP cost at all; it is a lower bound, not a like-for-like server.
* For the clone rows of every tool, client-side checkout (writing the files) is part of the wall time and dominates on the `deep` layout.

## Not run

* Cloud S3 / GCS (no cloud resources allowed).
* TLS, authenticated modes, concurrent clients, multiple walgit instances, large-blob/LFS, packfile-URI delivery for walgit.
* walgit's remote-reader mode (repo larger than cache budget).
* Local-workflow timings of walgit: there are none (stock git); see benchmarks.md.
* Reproduce: `python3 scripts/walgit_bench/walgit_bench.py flat 10000 5 out.json --init-reps 5` on a Linux box with RustFS, a built walgit-server and vcs release binaries (paths at the top of the script), then `python3 scripts/walgit_bench/report.py out.json`.
