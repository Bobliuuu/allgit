# High-load push matrix: vcs vs git and walgit

Measured on box D (c6i.4xlarge, 16 vCPU, ext4 on gp3, Ubuntu 24.04, git 2.56), 2026-11-02, with
`scripts/highload_bench/highload.py` (PRs #442, #445). Each pusher has its own clone and does
commit + push, 10 commits per pusher (5 for the same-branch runs). Three repetitions per cell, rotated
arm order. "pushes/s" is commit plus push per second over the push phase only (clones excluded).
Raw JSON is in `docs/research/highload-data/`.

**Audit caveats.** The group-commit raw file (`highload-data/branches-groupcommit-pr452.json`) records load average 11.6 to 14.6 at the start on this 16-vCPU box
(not a quiet box) and `vcs_rev: "?"` (the build is not recorded). git here is 2.56.0, while the scoreboards on box c used 2.43.0. The git arm is `git daemon` (no auth, no TLS, no HTTP)
against `vcs-server` over HTTP, and pushes/s counts commit plus push. Treat the vcs-vs-git rows as indicative; see [benchmark-audit](benchmark-audit.md).

Arms: **git** = `git daemon` on local disk; **vcs disk** = `vcs-server --root`; **walgit** and **vcs RustFS** = the
same RustFS on local disk. Pairs compared: vcs disk vs git, vcs RustFS vs walgit.

Tie rule: TIE when the medians are within 10 percent or the min..max ranges overlap; otherwise WIN or LOSS for vcs.
Every push in every cell was verified by a fresh clone unless noted.

## Summary

- vcs RustFS beats walgit under load once the server fix (#441) and group commit (#452) are in: 8 wins, 1 tie, 0 losses
  on separate branches at 8/32/128 pushers (pushes/s 42 vs 15 at 128, p99 3.5 s vs 14 s).
- vcs on disk beats git on single-client push latency (25 ms vs 57 ms) and on push p50 at 8 pushers, but git
  delivers more pushes/s at 32 and 128 pushers (git 290 and 222; vcs 100 and 85).
- That gap is durability cost, not CPU. With git forced to the same fsync discipline (`core.fsync=all`) git
  drops to 120 and 113 pushes/s and the gap shrinks to 15 to 25 percent; on tmpfs (no fsync) vcs does
  780 pushes/s against git 380 to 445. `vcs add` plus `vcs commit` costs 28 ms (about 13 fsyncs) against 6 ms for
  git, and the server does 4 fsynced object writes per push.
- Same branch, 32 pushers: only vcs lands every commit (with `publish --auto-merge`, #444). git landed 60 to 75 of 160
  and walgit 35 of 160 before pushers gave up after 30 retries; their pushes/s in those cells count only what landed.

## Separate branches and clones under push load, before group commit (main with #440 to #442)

| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |
|---|---|---|---|---|---|---|---|
| branches | 8 | push p50 ms | vcs_disk | 40.5 (39.1..44.7) | git | 62.8 (57.4..64.5) | win |
| branches | 8 | push p50 ms | vcs_s3 | 128.5 (126.1..131.3) | walgit | 182.7 (178.8..210.9) | win |
| branches | 8 | push p99 ms | vcs_disk | 718.8 (675.6..890.4) | git | 68.1 (66.9..71.4) | loss |
| branches | 8 | push p99 ms | vcs_s3 | 230.8 (228.8..323.4) | walgit | 269.1 (268.8..284.0) | tie |
| branches | 8 | pushes/s | vcs_disk | 52.1 (49.6..55.8) | git | 112.8 (109.1..120.0) | loss |
| branches | 8 | pushes/s | vcs_s3 | 38.6 (37.7..40.7) | walgit | 42.0 (36.1..42.0) | tie |
| branches | 32 | push p50 ms | vcs_disk | 65.4 (62.6..67.5) | git | 83.0 (80.7..91.3) | win |
| branches | 32 | push p50 ms | vcs_s3 | 324.4 (273.3..338.9) | walgit | 808.6 (790.7..817.3) | win |
| branches | 32 | push p99 ms | vcs_disk | 1801.5 (1758.6..1847.1) | git | 122.3 (113.6..128.6) | loss |
| branches | 32 | push p99 ms | vcs_s3 | 2978.3 (2644.6..3289.7) | walgit | 1041.0 (1008.5..1063.6) | loss |
| branches | 32 | pushes/s | vcs_disk | 77.1 (75.4..77.4) | git | 289.5 (260.8..301.9) | loss |
| branches | 32 | pushes/s | vcs_s3 | 39.7 (38.8..40.6) | walgit | 37.5 (37.4..37.6) | tie |
| branches | 128 | push p50 ms | vcs_disk | 491.1 (415.1..578.5) | git | 342.4 (293.1..423.4) | tie |
| branches | 128 | push p50 ms | vcs_s3 | 1805.9 (1723.8..1882.0) | walgit | 6749.5 (6393.4..10091.0) | win |
| branches | 128 | push p99 ms | vcs_disk | 6604.1 (6440.7..6648.2) | git | 1442.7 (1408.5..1759.5) | loss |
| branches | 128 | push p99 ms | vcs_s3 | 14970.6 (13852.7..15423.8) | walgit | 14133.2 (13076.4..18288.4) | tie |
| branches | 128 | pushes/s | vcs_disk | 82.3 (77.2..83.3) | git | 222.0 (218.5..269.8) | loss |
| branches | 128 | pushes/s | vcs_s3 | 35.2 (33.8..35.2) | walgit | 15.5 (12.6..16.6) | win |
| clones | 8 | clone p50 ms | vcs_disk | 161.7 (133.6..184.2) | git | 117.6 (110.1..128.8) | loss |
| clones | 8 | clone p50 ms | vcs_s3 | 173.5 (173.3..176.5) | walgit | 76.2 (67.0..79.4) | loss |
| clones | 8 | push p50 ms | vcs_disk | 40.1 (34.9..44.1) | git | 61.0 (57.8..64.8) | win |
| clones | 8 | push p50 ms | vcs_s3 | 121.0 (116.2..123.2) | walgit | 163.9 (138.2..165.6) | win |
| clones | 8 | push p99 ms | vcs_disk | 89.8 (61.5..181.7) | git | 69.9 (63.9..73.4) | tie |
| clones | 8 | push p99 ms | vcs_s3 | 179.5 (177.8..204.3) | walgit | 312.5 (241.3..316.8) | win |
| clones | 8 | pushes/s | vcs_disk | 34.6 (32.4..36.5) | git | 57.9 (50.5..61.7) | loss |
| clones | 8 | pushes/s | vcs_s3 | 19.9 (19.1..20.8) | walgit | 19.6 (18.9..24.2) | tie |
| clones | 32 | clone p50 ms | vcs_disk | 155.4 (147.8..157.0) | git | 115.8 (112.3..126.1) | loss |
| clones | 32 | clone p50 ms | vcs_s3 | 166.2 (159.5..194.5) | walgit | 89.2 (86.2..96.6) | loss |
| clones | 32 | push p50 ms | vcs_disk | 61.3 (57.8..62.1) | git | 70.1 (68.9..75.2) | win |
| clones | 32 | push p50 ms | vcs_s3 | 237.2 (231.4..249.6) | walgit | 468.0 (457.4..538.2) | win |
| clones | 32 | push p99 ms | vcs_disk | 1312.6 (951.1..1551.7) | git | 88.3 (86.3..1112.9) | tie |
| clones | 32 | push p99 ms | vcs_s3 | 1948.3 (1684.9..2049.0) | walgit | 580.5 (553.5..676.8) | loss |
| clones | 32 | pushes/s | vcs_disk | 52.8 (48.3..63.1) | git | 165.0 (93.0..178.9) | loss |
| clones | 32 | pushes/s | vcs_s3 | 30.8 (28.6..30.8) | walgit | 32.7 (27.7..34.0) | tie |
| clones | 128 | clone p50 ms | vcs_disk | 144.7 (142.5..159.0) | git | 112.8 (112.6..125.4) | loss |
| clones | 128 | clone p50 ms | vcs_s3 | 150.7 (146.7..185.4) | walgit | 107.7 (106.8..115.1) | loss |
| clones | 128 | push p50 ms | vcs_disk | 160.7 (156.4..225.4) | git | 162.1 (160.1..181.2) | tie |
| clones | 128 | push p50 ms | vcs_s3 | 1173.2 (994.7..1269.5) | walgit | 2641.6 (2560.6..2808.9) | win |
| clones | 128 | push p99 ms | vcs_disk | 4176.2 (4110.6..4938.2) | git | 330.2 (319.2..1105.1) | loss |
| clones | 128 | push p99 ms | vcs_s3 | 9322.4 (8374.2..15874.5) | walgit | 4007.2 (3863.3..4159.6) | loss |
| clones | 128 | pushes/s | vcs_disk | 67.5 (55.1..68.5) | git | 218.3 (210.3..270.6) | loss |
| clones | 128 | pushes/s | vcs_s3 | 28.0 (19.2..28.2) | walgit | 22.6 (20.6..23.0) | tie |

vcs_disk vs git: 4 wins, 4 ties, 13 losses
vcs_s3 vs walgit: 8 wins, 7 ties, 6 losses

Cells with verification failures (a push that did not land) or fatal errors: none

## Separate branches, with vcs-server group commit (#452) on top

git and walgit rows are the same runs as above; vcs rows are re-measured.

| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |
|---|---|---|---|---|---|---|---|
| branches | 8 | push p50 ms | vcs_disk | 36.8 (36.5..37.4) | git | 62.8 (57.4..64.5) | win |
| branches | 8 | push p50 ms | vcs_s3 | 111.9 (87.8..123.6) | walgit | 182.7 (178.8..210.9) | win |
| branches | 8 | push p99 ms | vcs_disk | 271.3 (67.3..375.8) | git | 68.1 (66.9..71.4) | tie |
| branches | 8 | push p99 ms | vcs_s3 | 190.7 (176.5..248.6) | walgit | 269.1 (268.8..284.0) | win |
| branches | 8 | pushes/s | vcs_disk | 66.2 (57.2..80.8) | git | 112.8 (109.1..120.0) | loss |
| branches | 8 | pushes/s | vcs_s3 | 44.8 (42.6..52.6) | walgit | 42.0 (36.1..42.0) | tie |
| branches | 32 | push p50 ms | vcs_disk | 240.5 (236.0..243.7) | git | 83.0 (80.7..91.3) | loss |
| branches | 32 | push p50 ms | vcs_s3 | 643.4 (631.4..649.8) | walgit | 808.6 (790.7..817.3) | win |
| branches | 32 | push p99 ms | vcs_disk | 277.2 (274.2..286.7) | git | 122.3 (113.6..128.6) | loss |
| branches | 32 | push p99 ms | vcs_s3 | 803.2 (770.9..826.7) | walgit | 1041.0 (1008.5..1063.6) | win |
| branches | 32 | pushes/s | vcs_disk | 99.7 (98.6..100.8) | git | 289.5 (260.8..301.9) | loss |
| branches | 32 | pushes/s | vcs_s3 | 43.9 (43.5..45.4) | walgit | 37.5 (37.4..37.6) | win |
| branches | 128 | push p50 ms | vcs_disk | 1443.0 (1278.9..1464.0) | git | 342.4 (293.1..423.4) | loss |
| branches | 128 | push p50 ms | vcs_s3 | 2885.2 (2856.1..2956.4) | walgit | 6749.5 (6393.4..10091.0) | win |
| branches | 128 | push p99 ms | vcs_disk | 1722.8 (1714.8..2412.6) | git | 1442.7 (1408.5..1759.5) | tie |
| branches | 128 | push p99 ms | vcs_s3 | 3487.8 (3419.9..3680.1) | walgit | 14133.2 (13076.4..18288.4) | win |
| branches | 128 | pushes/s | vcs_disk | 85.3 (83.7..88.7) | git | 222.0 (218.5..269.8) | loss |
| branches | 128 | pushes/s | vcs_s3 | 42.2 (40.6..42.5) | walgit | 15.5 (12.6..16.6) | win |

vcs_disk vs git: 1 wins, 2 ties, 6 losses
vcs_s3 vs walgit: 8 wins, 1 ties, 0 losses

Cells with verification failures (a push that did not land) or fatal errors: none

## Separate branches, git with `core.fsync=all` (comparable durability) vs vcs disk with group commit

| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |
|---|---|---|---|---|---|---|---|
| branches | 8 | push p50 ms | vcs_disk | 36.8 (36.5..37.4) | git | 71.9 (70.4..81.8) | win |
| branches | 8 | push p99 ms | vcs_disk | 271.3 (67.3..375.8) | git | 164.4 (137.8..608.9) | tie |
| branches | 8 | pushes/s | vcs_disk | 66.2 (57.2..80.8) | git | 63.9 (47.2..69.1) | tie |
| branches | 32 | push p50 ms | vcs_disk | 240.5 (236.0..243.7) | git | 125.1 (124.7..125.6) | loss |
| branches | 32 | push p99 ms | vcs_disk | 277.2 (274.2..286.7) | git | 182.8 (171.6..191.8) | loss |
| branches | 32 | pushes/s | vcs_disk | 99.7 (98.6..100.8) | git | 119.9 (118.6..121.0) | loss |
| branches | 128 | push p50 ms | vcs_disk | 1443.0 (1278.9..1464.0) | git | 703.1 (685.1..706.3) | loss |
| branches | 128 | push p99 ms | vcs_disk | 1722.8 (1714.8..2412.6) | git | 1735.2 (1656.9..1775.0) | tie |
| branches | 128 | pushes/s | vcs_disk | 85.3 (83.7..88.7) | git | 113.4 (111.7..115.4) | loss |

vcs_disk vs git: 1 wins, 3 ties, 5 losses

Cells with verification failures (a push that did not land) or fatal errors: none

## Sequential, 1000 small commits each pushed

| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |
|---|---|---|---|---|---|---|---|
| seq | 1 | push p50 ms | vcs_disk | 25.3 (22.2..25.9) | git | 56.6 (55.9..57.1) | win |
| seq | 1 | push p50 ms | vcs_s3 | 44.6 (44.2..58.3) | walgit | 113.0 (110.9..113.5) | win |
| seq | 1 | push p99 ms | vcs_disk | 45.9 (34.7..48.8) | git | 59.9 (59.3..60.6) | win |
| seq | 1 | push p99 ms | vcs_s3 | 810.8 (792.3..826.1) | walgit | 233.2 (227.9..430.9) | loss |

vcs_disk vs git: 2 wins, 0 ties, 0 losses
vcs_s3 vs walgit: 1 wins, 0 ties, 1 losses

Cells with verification failures (a push that did not land) or fatal errors: none

Push latency over the 1000 pushes (median of first and last 10 percent, vcs disk): 18 to 19 ms then 23 to 29 ms; git 54 to 56
then 58; walgit 60 to 83 then 193 to 358 (grows with history); vcs RustFS 41 to 71 then 45 to 69 with a 800 ms p99 (periodic
spikes, not investigated). Fresh clone after 1000 commits: git 465 to 515 ms, vcs disk 405 to 659, vcs RustFS 857 to 1059,
walgit 233 to 406.

## One shared branch, with `publish --auto-merge` (vcs) and manual pull/rebase (git, walgit)

Recovery is capped at 30 retries per commit. Verified column below: git at 32 pushers landed 60, 65 and 75 of 160, and walgit
35 of 160 in every rep, so their pushes/s are over fewer pushes and are not comparable; vcs landed 160 of 160 on both backends.

| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |
|---|---|---|---|---|---|---|---|
| same | 8 | push p50 ms | vcs_disk | 32.8 (30.0..33.5) | git | 55.4 (55.0..58.6) | win |
| same | 8 | push p50 ms | vcs_s3 | 79.5 (76.4..99.3) | walgit | 103.7 (102.7..156.2) | win |
| same | 8 | push p99 ms | vcs_disk | 3311.3 (3053.1..3501.5) | git | 2661.9 (2654.6..2919.2) | loss |
| same | 8 | push p99 ms | vcs_s3 | 4623.7 (4572.3..6126.5) | walgit | 3472.9 (3459.1..5353.7) | tie |
| same | 8 | pushes/s | vcs_disk | 11.2 (10.4..12.0) | git | 13.8 (12.5..13.8) | loss |
| same | 8 | pushes/s | vcs_s3 | 8.0 (5.8..8.0) | walgit | 9.3 (6.2..9.3) | tie |
| same | 32 | push p50 ms | vcs_disk | 62.6 (52.7..66.1) | git | 82.3 (60.2..85.3) | tie |
| same | 32 | push p50 ms | vcs_s3 | 102.5 (93.0..123.2) | walgit | 1041.7 (1035.3..1183.7) | win |
| same | 32 | push p99 ms | vcs_disk | 33329.9 (31016.2..35440.1) | git | 8453.6 (4919.9..8481.0) | loss |
| same | 32 | push p99 ms | vcs_s3 | 43850.0 (41448.1..49330.2) | walgit | 33875.1 (31404.6..40340.7) | loss |
| same | 32 | pushes/s | vcs_disk | 4.7 (4.4..5.0) | git | 8.4 (6.8..12.5) | loss |
| same | 32 | pushes/s | vcs_s3 | 3.5 (3.1..3.7) | walgit | 1.0 (0.8..1.0) | win |

vcs_disk vs git: 1 wins, 1 ties, 4 losses
vcs_s3 vs walgit: 3 wins, 2 ties, 1 losses

Cells with verification failures (a push that did not land) or fatal errors: same/8/walgit (0/3 verified), same/32/git (0/3 verified), same/32/walgit (0/3 verified)

## Findings behind the numbers

1. RustFS serializes same-key conditional PUTs on a polled lock; 32 pushers made manifest GETs take 350 ms. Fixed by
   serializing manifest writes in the server (#441), then group commit (#452): vcs RustFS 2.3 to 42 pushes/s at 32 to 128 pushers.
2. A merge that needed unfetched blobs left files behind and then refused on retry (#440).
3. Pushers on one branch gave up (about a quarter of them) until `publish --auto-merge` (#444).
4. The remaining disk gap is fsync count. Client commit: about 13 fsyncs in about 7 serial waves (objects, refs, op log,
   op head). Server push: pack, idx, tx and manifest each fsynced. Candidates, each a durability decision for the owner:
   one barrier for the operation-log object and head; skip the fsync on `tx/` records; a `core.fsync`-style setting.
5. Not done: perf profiles of the server beyond request logs and CPU totals (strace needs sudo, which was refused in this
   session); 1M-file and 1M-commit cases; large bursts; 128-pusher same-branch runs.

## Caveats

- One box, one run of 3 reps per cell; git's server CPU is not recorded (the daemon forks).
- Throughput includes client-side `commit`, so it is a whole-workflow number, not server capacity.
- The vcs-server group-commit rows used `ulimit -n 65536`; the default 1024 is too low for 512 sockets.
- Binary for the "before group commit" rows had #440, #441 and #444 built in except the first cells, which ran before the
  `--auto-merge` flag existed (no effect on branch scenarios).
