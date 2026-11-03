# CI-workload benchmark: vcs vs git at 100k files

Driver: `scripts/ci_bench.py` (counting TCP forwarder: `scripts/ci_tcpcount.py`). Raw JSON: `docs/research/data/ci-suite/`.
Regenerate the tables with `scripts/ci_bench.py report <file.json>`.

## Read this first

- **Machine**: AWS c6i.4xlarge (16 vCPU Xeon Platinum 8375C, us-east-1, Ubuntu, git 2.43.0, vcs 0.1.0 built from origin/main plus PR 458),
  runner, server and clients all on this one host over loopback (no network latency or bandwidth limit between them), local EBS disk,
  warm page cache (caches were not dropped). The box is shared with other agents' benchmarks under `flock ~/bench.lock`, but their
  `cargo` builds and tests are not under the lock, so load1 was not zero; per-run load1 values are in the JSON and the start/end load is
  printed under every table. Every wall time is therefore noisy; min-max is shown, and the verdict rule uses it.
- **Servers**: `vcs-server --root` (local disk, no object store) for vcs; `git daemon` (git://) and `file://` (git upload-pack as a child
  process) for git. **Git over HTTP was not run** (no HTTP server for git was set up; git:// is the closest like-for-like network
  protocol, file:// is the no-server case). **walgit was not run** (it needs S3 and the S3 credentials expired; see held PR 354 for the
  10k-file S3 + walgit runs). **No S3, no R2, no real network.**
- **Data**: 100,000 files, 57 MiB total (`datagen flat|deep`, regrouped into 50 directories `g00..g49`; the job slice is `g07`, 2000 files, 2 %).
  History: initial commit plus 5 commits touching 3 slice files each; the per-round benchmark commits add more. vcs: one zone per `gNN`.
- **Rounds**: 5 interleaved rounds for scenarios 1 to 5 (cell order rotated every round); fan-out (scenario 6) has 3 rounds, not 5, because
  each round clones up to 64 trees and the 100k-file deletes alone take minutes. First run of the fan-out died on a git failure (below);
  the harness now records failed rounds instead of aborting.
- **Verdict rule (owner)**: TIE if the medians are within 10 %, or the min-max ranges overlap, or the gap is under 2 ms. LOSS only if vcs is
  slower by more than 10 % *and* the ranges do not overlap; WIN is the mirror image. The verdict tables in the raw output compare each vcs
  cell with each git cell on wall time (and client CPU for scenario 1).
- **Not comparable cells**: `vcs full` checks out all 100k files; the git cells with `sparse` check out 2000. A "LOSS" of `vcs full` against a
  sparse git clone is an artefact of comparing different amounts of work. Compare `vcs full` with `git full` / `git depth1 (whole tree)`,
  and `vcs --for (slice)` with the sparse/blobless git cells.
- `vcs` has **no shallow clone and no blob filter**; the vcs equivalents of "depth 1" and "blobless sparse" are its zone clone (`--for PATH`
  or `--zone Z`) which downloads only the zone's data plus metadata.

## Scenarios

1. Cold clone of the job slice. 2. Warm runner: fetch one specific new commit and check it out (git: `fetch origin <sha>` + `checkout FETCH_HEAD`;
vcs: `fetch` + `switch --detach <id>`, which needed PR 458: before it, a fetched commit could not be named). 3. PR checks: list the changed files
of a 20-file PR against its base (`git diff --name-only A B` / `vcs diff-tree A B --zone g07`) and `status` with 20 modified files.
4. Between jobs: `git clean -fdxq && git checkout -f <other sha>` vs `vcs clean -f -d && vcs restore . && vcs switch --detach <id>` with 3000 untracked
build files and 20 modified tracked files. 5. Cache-warm re-clone from a local cache (git `--reference`/local mirror; vcs from a local directory
remote, and from a bundle file). 6. Fan-out: K simultaneous cold clones from one server (K = 8, 32, 64; heavy cells only at the K they can finish).


## Win / tie / loss summary (100k files; owner rule; vcs `--for` slice vs equivalent git work unless stated)

| Scenario | flat | deep | Notes |
|---|---|---|---|
| 1 cold clone, slice, wall | WIN vs depth-1 whole tree / full; TIE vs blobless (ranges overlap: 170 ms vs 447-490 ms medians) | WIN vs every git cell (432 ms vs 1154-1673 ms for blobless, 4.2 s sparse depth-1, 9-12 s whole tree) | vcs has no shallow clone; its slice clone replaces it |
| 1 cold clone, whole tree (`vcs full` vs `git full` / `depth1`) | WIN (1.1 s vs 3.2-3.4 s) | TIE (10.7 s vs 9.5-12.0 s) | |
| 1 client CPU, slice | WIN vs whole-tree git, TIE vs sparse/blobless | same | |
| 1 bytes down and request count | **LOSS** vs blobless git (20 MiB / 16 requests vs 3.1 MiB / 2 connections flat; 40 MiB / 18 vs 7.3 MiB / 2 deep); TIE vs unfiltered git depth-1 (39-43 MiB) | | Not a wall loss on loopback; matters on a real link. See "Where vcs loses" |
| 2 warm fetch of a specific commit + checkout | WIN (65 ms vs 158-338 ms) | WIN (79 ms vs 411-646 ms) | requires PR 458 |
| 3 changed files of a PR (`diff --name-only` / `diff-tree`) | TIE (3-4 ms each) | TIE | |
| 3 `status` with 20 modified files | WIN (14 ms vs 61-119 ms) | WIN (18 ms vs 148-564 ms) | |
| 4 `clean -fdx` + checkout of another commit | WIN vs blobless/file (85 vs 182-220), TIE vs depth-1 sparse (119 ms) | WIN (68 ms vs 278-629 ms) | |
| 5 cache-warm re-clone, vcs from a local directory remote | WIN (1195 ms vs 2058-2605) | TIE (9.4 s vs 8.9-9.7 s) | slice from cache: 184 / 470 ms |
| 5 cache-warm re-clone, vcs from a bundle file | measured under load: LOSS (5976 ms vs 2058); re-measured on a quiet box (see below): 2158 ms base, 1775 ms with PR 461, git local clone 2058-2605 ms: TIE | TIE vs `--reference`, LOSS vs hardlink clone (11.1 s vs 9.2 s, ranges overlap so read as TIE by the rule) | the bundle path cannot clone one zone (`--for` fails, see below) |
| 6 fan-out K=8, slowest clone | WIN vs every unfiltered git cell (1.4 s vs 4.0-15.2 s); TIE vs blobless (0.7 s; ranges 0.55-2.7 s vs 0.65-0.84 s) | see deep fan-out section | server CPU 0.0 s for both servers |
| 6 fan-out K=32 | WIN vs depth-1 sparse (1.9 s vs 15.7 s); TIE vs blobless (3.3-3.4 s) | | one git:// blobless round failed ("Connection reset by peer") |
| 6 fan-out K=64 | TIE (7.1 s vs 7.3-7.5 s) | | two git:// blobless rounds failed; vcs had no failed round |

Wall-time LOSS count against the owner rule: none in cells that compare equal work. The one **bytes/requests loss** and the bundle-clone result are discussed below.

## Where vcs loses, and what was done

1. **Bytes and request count of a zone clone (not fixed).** `vcs clone --for g07` downloads 20.3 MiB (flat) / 40.4 MiB (deep) in 16 to 18 requests for a 1.1 MiB
   slice; `git clone --depth 1 --filter=blob:none --sparse` downloads 3.1 / 7.3 MiB in 2 connections. A side experiment (flat 100k, 3 runs not
   repeated, no timing claim): `--for` spends 12.4 MB in the planning phase (it reads every tree of the repository to resolve the path and dependencies) and 8.8 MB
   on the clone itself; `vcs clone --zone g07` (no planning) downloaded 8.8 MB in 8 requests (about 4.4 MB of that is likely the pack index of the 100k objects,
   not verified). A fix needs a per-zone summary in the manifest so `--for` can plan without the trees, or a zone-scoped pack index; both are format-level changes and were not attempted.
2. **Fetching a specific commit was impossible** on a warm runner: after `vcs fetch` the commit could not be named (`unknown revision`), and the checkout then
   fetched one object per retry. Fixed in PR 458 (`switch --detach <id|origin/main>`, batched blob download). Not yet fixed: a failing lazy retry can leave the
   zone with unstaged changes.
3. **Clone from a bundle (PR 461).** Three pack scans reduced to two and no re-hash: on a quiet box 100k files, 5 interleaved runs, user CPU 1.82-1.91 s to 1.45-1.53 s, wall
   medians 2158 ms to 1775 ms with overlapping ranges (a TIE on wall). The S5 table's 5976 ms bundle cell for the flat layout was taken while the box had load1 up to 187 and is not a reliable
   figure (the A/B run is in `data/ci-suite/bundle-clone-ab.txt`, box load1 0.12).
4. **`vcs clone <bundle> --for PATH` fails** ("cannot open remote ...: Not a directory"): bundles clone whole or with `--zone` only. Not fixed.
5. **Server-side git failures at high fan-out.** `git daemon` (git://) dropped connections ("fatal: read error: Connection reset by peer") in 1 of 3 rounds at K=32 and 2 of 3 rounds at K=64 for blobless clones, and once at K=32 in an earlier unrecorded run (a depth-1 unfiltered round) that aborted the
   first version of the harness. vcs-server had no failed round. Cause not investigated (daemon limits, box memory pressure from other agents, or load); recorded as observed.

## Scope that was not run (no claim made)

Git over HTTP; walgit; S3 or any network latency; cold OS caches; `--task` and zones70 layouts (see held PR 354 for 10k zones70 + S3 data); more than 3 fan-out rounds; deep layout fan-out unless a section above says otherwise; `vcs` shallow-clone behaviour (it has none); `git clone --filter=tree:0`; bytes for scenarios 2 to 4.
Git cells use git 2.43.0 on the box (not a newer git).

## Results: scenarios 1 to 5

## flat 100000 files (57 MiB), slice 2000 files

box cores 16 (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), load1 start/5/15 187.54/154.18/81.47, end 6.28/48.38/56.46; git version 2.43.0; vcs 0.1.0; rounds 5, fan-out rounds 3; 2026-11-03T02:53:24Z

**S1 cold clone of the slice** (wall ms; client CPU s; server CPU s; runner MiB; requests; MiB down)

| cell | wall ms | client CPU s | server CPU s | runner MiB | requests | MiB down |
|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | 170 (162-487) | 0.15 (0.14-0.16) | 0.00 (0.00-0.01) | 14.1 | 16 | 20.3 |
| vcs full [vcs-server] | 1120 (1049-1975) | 2.93 (2.75-3.36) | 0.00 (0.00-0.04) | 472.7 | 15 | 46.2 |
| git depth1 (whole tree) [git://] | 3384 (3290-4972) | 3.07 (3.04-3.17) | 0.00 (0.00-0.00) | 445.7 | 1 conn | 39.3 |
| git depth1 sparse [git://] | 1655 (1649-2539) | 1.40 (1.40-1.46) | 0.00 (0.00-0.00) | 58.9 | 1 conn | 39.3 |
| git depth1 blobless sparse [git://] | 490 (473-574) | 0.38 (0.37-0.38) | 0.00 (0.00-0.00) | 19.9 | 2 conn | 3.1 |
| git blobless sparse [git://] | 447 (443-567) | 0.38 (0.38-0.39) | 0.00 (0.00-0.00) | 19.9 | 2 conn | 3.1 |
| git full [git://] | 3186 (3160-6536) | 3.29 (3.26-3.46) | 0.00 (0.00-0.00) | 445.7 | 1 conn | 39.3 |
| git depth1 blobless sparse [file://] | 451 (447-601) | 0.44 (0.43-0.44) | 0.00 (0.00-0.00) | 19.9 | n/a | n/a |
| git depth1 (whole tree) [file://] | 3256 (3239-6484) | 3.71 (3.70-3.86) | 0.00 (0.00-0.00) | 445.7 | n/a | n/a |
| git full [file://] | 3200 (3145-6157) | 3.42 (3.42-3.59) | 0.00 (0.00-0.00) | 445.7 | n/a | n/a |

**Verdicts, S1 wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 (whole tree) [git://] | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] | git depth1 (whole tree) [file://] | git full [file://] |
|---|---|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | TIE | TIE | WIN | TIE | WIN | WIN |
| vcs full [vcs-server] | WIN | TIE | LOSS | LOSS | WIN | LOSS | WIN | WIN |

**Verdicts, S1 client CPU** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 (whole tree) [git://] | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] | git depth1 (whole tree) [file://] | git full [file://] |
|---|---|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | TIE | TIE | TIE | WIN | TIE | WIN | WIN |
| vcs full [vcs-server] | TIE | TIE | LOSS | LOSS | TIE | LOSS | TIE | TIE |

**S2 warm: fetch specific commit + checkout**

| cell | wall ms | client CPU s |
|---|---|---|
| vcs --for (slice) [vcs-server] | 65 (51-91) | 0.04 (0.04-0.05) |
| git depth1 sparse [git://] | 229 (227-357) | 0.14 (0.14-0.24) |
| git depth1 blobless sparse [git://] | 247 (236-363) | 0.11 (0.10-0.17) |
| git blobless sparse [git://] | 243 (240-352) | 0.11 (0.10-0.17) |
| git full [git://] | 338 (219-412) | 0.33 (0.25-0.39) |
| git depth1 blobless sparse [file://] | 158 (147-263) | 0.15 (0.14-0.22) |

**Verdicts, S2 warm: fetch specific commit + checkout wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN |

**S4 clean -fdx + checkout**

| cell | wall ms | client CPU s |
|---|---|---|
| vcs --for (slice) [vcs-server] | 85 (45-147) | 0.03 (0.03-0.05) |
| git depth1 sparse [git://] | 119 (117-204) | 0.12 (0.11-0.19) |
| git depth1 blobless sparse [git://] | 220 (177-273) | 0.13 (0.12-0.20) |
| git blobless sparse [git://] | 182 (176-295) | 0.13 (0.12-0.20) |
| git full [git://] | 216 (156-273) | 0.28 (0.27-0.38) |
| git depth1 blobless sparse [file://] | 219 (142-246) | 0.13 (0.13-0.22) |

**Verdicts, S4 clean -fdx + checkout wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | TIE | WIN | WIN | WIN | TIE |

**S5 cache-warm re-clone**

| cell | wall ms | client CPU s |
|---|---|---|
| git clone --reference cache --dissociate (full) | 2605 (2552-3330) | 2.71 (2.67-3.45) |
| git clone cache (local path, hardlinks) | 2058 (2013-2073) | 1.99 (1.97-2.02) |
| git clone --reference cache (depth-free, borrowed objects) | 2326 (2041-2808) | 2.19 (1.96-2.68) |
| vcs clone cache dir (full) | 1195 (1148-1981) | 2.96 (2.85-4.25) |
| vcs clone cache dir --for (slice) | 184 (173-365) | 0.15 (0.15-0.23) |
| vcs clone cache bundle (full) | 5976 (4515-6207) | 7.28 (5.68-7.59) |
| vcs clone cache bundle --for (slice) | failed: vcs clone '/home/ubuntu/rt/cibench2-work/flat-100000 | failed: vcs clone '/home/ubuntu/rt/cibench2-work/flat-100000 |

**Verdicts, S5 cache-warm re-clone wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git clone --reference cache --dissociate (full) | git clone cache (local path, hardlinks) | git clone --reference cache (depth-free, borrowed objects) |
|---|---|---|---|
| vcs clone cache dir (full) | WIN | WIN | WIN |
| vcs clone cache dir --for (slice) | WIN | WIN | WIN |
| vcs clone cache bundle (full) | LOSS | LOSS | LOSS |
| vcs clone cache bundle --for (slice) | n/a | n/a | n/a |

**S3 PR: changed files vs base, and status with 20 modified files** (ms)

| cell | diff --name-only | status |
|---|---|---|
| vcs --for (slice) [vcs-server] | 4 (3-7) | 14 (10-21) |
| git depth1 sparse [git://] | 3 (3-4) | 63 (26-108) |
| git depth1 blobless sparse [git://] | 3 (3-4) | 61 (60-106) |
| git blobless sparse [git://] | 3 (2-7) | 62 (60-93) |
| git full [git://] | 3 (2-4) | 119 (105-202) |
| git depth1 blobless sparse [file://] | 3 (3-6) | 63 (60-104) |

**Verdicts, S3 changed-files wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | TIE | TIE | TIE | TIE | TIE |

**Verdicts, S3 status wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN |

server stores KiB: {"git_bare": 43964, "vcs_root": 47356}


## deep 100000 files (57 MiB), slice 2000 files

box cores 16 (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), load1 start/5/15 12.64/6.58/12.13, end 1.35/1.59/4.88; git version 2.43.0; vcs 0.1.0; rounds 5, fan-out rounds 3; 2026-11-03T03:33:48Z

**S1 cold clone of the slice** (wall ms; client CPU s; server CPU s; runner MiB; requests; MiB down)

| cell | wall ms | client CPU s | server CPU s | runner MiB | requests | MiB down |
|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | 432 (379-740) | 0.30 (0.29-0.31) | 0.00 (0.00-0.01) | 24.3 | 18 | 40.4 |
| vcs full [vcs-server] | 10677 (9297-15590) | 4.38 (4.33-4.63) | 0.02 (0.01-0.06) | 824.0 | 16 | 54.5 |
| git depth1 (whole tree) [git://] | 12031 (11225-12488) | 4.81 (4.78-4.81) | 0.00 (0.00-0.00) | 795.0 | 1 conn | 43.5 |
| git depth1 sparse [git://] | 4203 (4195-4254) | 2.24 (2.22-2.25) | 0.00 (0.00-0.00) | 76.2 | 1 conn | 43.5 |
| git depth1 blobless sparse [git://] | 1673 (1669-1779) | 1.02 (1.02-1.03) | 0.00 (0.00-0.00) | 37.2 | 2 conn | 7.3 |
| git blobless sparse [git://] | 1154 (1137-1160) | 1.23 (1.22-1.24) | 0.00 (0.00-0.00) | 37.2 | 2 conn | 7.3 |
| git full [git://] | 9468 (9187-9617) | 5.13 (5.09-5.18) | 0.00 (0.00-0.00) | 795.0 | 1 conn | 43.5 |
| git depth1 blobless sparse [file://] | 1700 (1687-1703) | 1.85 (1.83-1.87) | 0.00 (0.00-0.00) | 37.2 | n/a | n/a |
| git depth1 (whole tree) [file://] | 11592 (11139-11778) | 7.41 (7.38-7.57) | 0.00 (0.00-0.00) | 795.0 | n/a | n/a |
| git full [file://] | 10396 (10130-11930) | 5.30 (5.27-5.32) | 0.00 (0.00-0.00) | 795.0 | n/a | n/a |

**Verdicts, S1 wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 (whole tree) [git://] | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] | git depth1 (whole tree) [file://] | git full [file://] |
|---|---|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN | WIN | WIN | WIN |
| vcs full [vcs-server] | TIE | LOSS | LOSS | LOSS | TIE | LOSS | TIE | TIE |

**Verdicts, S1 client CPU** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 (whole tree) [git://] | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] | git depth1 (whole tree) [file://] | git full [file://] |
|---|---|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | TIE | TIE | TIE | WIN | TIE | WIN | WIN |
| vcs full [vcs-server] | TIE | LOSS | LOSS | LOSS | TIE | LOSS | WIN | TIE |

**S2 warm: fetch specific commit + checkout**

| cell | wall ms | client CPU s |
|---|---|---|
| vcs --for (slice) [vcs-server] | 79 (75-88) | 0.07 (0.06-0.07) |
| git depth1 sparse [git://] | 646 (613-657) | 0.47 (0.46-0.47) |
| git depth1 blobless sparse [git://] | 531 (526-547) | 0.32 (0.31-0.33) |
| git blobless sparse [git://] | 463 (448-677) | 0.32 (0.31-0.33) |
| git full [git://] | 411 (390-475) | 0.57 (0.55-0.65) |
| git depth1 blobless sparse [file://] | 450 (439-457) | 0.44 (0.43-0.45) |

**Verdicts, S2 warm: fetch specific commit + checkout wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN |

**S4 clean -fdx + checkout**

| cell | wall ms | client CPU s |
|---|---|---|
| vcs --for (slice) [vcs-server] | 68 (64-73) | 0.06 (0.06-0.06) |
| git depth1 sparse [git://] | 278 (272-281) | 0.27 (0.27-0.27) |
| git depth1 blobless sparse [git://] | 335 (331-335) | 0.28 (0.28-0.28) |
| git blobless sparse [git://] | 333 (330-342) | 0.28 (0.28-0.28) |
| git full [git://] | 629 (626-635) | 0.88 (0.87-0.89) |
| git depth1 blobless sparse [file://] | 292 (290-304) | 0.28 (0.28-0.28) |

**Verdicts, S4 clean -fdx + checkout wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN |

**S5 cache-warm re-clone**

| cell | wall ms | client CPU s |
|---|---|---|
| git clone --reference cache --dissociate (full) | 9737 (8438-10745) | 4.89 (4.81-5.04) |
| git clone cache (local path, hardlinks) | 9175 (9041-9943) | 3.45 (3.43-3.49) |
| git clone --reference cache (depth-free, borrowed objects) | 8940 (7902-9741) | 3.47 (3.44-3.48) |
| vcs clone cache dir (full) | 9353 (9066-9757) | 4.39 (4.36-4.42) |
| vcs clone cache dir --for (slice) | 470 (382-495) | 0.32 (0.31-0.33) |
| vcs clone cache bundle (full) | 11087 (10323-11543) | 8.08 (8.02-8.14) |
| vcs clone cache bundle --for (slice) | failed: vcs clone '/home/ubuntu/rt/cibench2-work/deep-100000 | failed: vcs clone '/home/ubuntu/rt/cibench2-work/deep-100000 |

**Verdicts, S5 cache-warm re-clone wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git clone --reference cache --dissociate (full) | git clone cache (local path, hardlinks) | git clone --reference cache (depth-free, borrowed objects) |
|---|---|---|---|
| vcs clone cache dir (full) | TIE | TIE | TIE |
| vcs clone cache dir --for (slice) | WIN | WIN | WIN |
| vcs clone cache bundle (full) | TIE | LOSS | LOSS |
| vcs clone cache bundle --for (slice) | n/a | n/a | n/a |

**S3 PR: changed files vs base, and status with 20 modified files** (ms)

| cell | diff --name-only | status |
|---|---|---|
| vcs --for (slice) [vcs-server] | 3 (3-4) | 18 (18-27) |
| git depth1 sparse [git://] | 3 (2-3) | 149 (147-151) |
| git depth1 blobless sparse [git://] | 3 (3-3) | 149 (146-151) |
| git blobless sparse [git://] | 3 (2-3) | 148 (83-153) |
| git full [git://] | 3 (2-3) | 564 (557-1253) |
| git depth1 blobless sparse [file://] | 3 (2-3) | 149 (148-151) |

**Verdicts, S3 changed-files wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | TIE | TIE | TIE | TIE | TIE |

**Verdicts, S3 status wall** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | WIN | WIN | WIN |

server stores KiB: {"git_bare": 51296, "vcs_root": 55784}

## Results: scenario 6 fan-out (separate run, same box and script)

## flat 100000 files (57 MiB), slice 2000 files

box cores 16 (Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz), load1 start/5/15 0.84/1.18/3.28, end 44.21/24.56/14.10; git version 2.43.0; vcs 0.1.0; rounds 5, fan-out rounds 3; 2026-11-03T04:06:14Z

**S6 fan-out: 8 simultaneous cold clones, one server** (slowest ms; summed client CPU s; server CPU s)

| cell | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for (slice) [vcs-server] | 1415 (550-2663) | 1.5 (1.5-1.6) | 0.0 (0.0-0.0) |
| vcs full [vcs-server] | 12425 (10603-14151) | 32.9 (32.6-33.5) | 0.0 (0.0-0.1) |
| git depth1 (whole tree) [git://] | 15174 (13084-15457) | 28.9 (28.8-29.0) | 0.0 (0.0-0.0) |
| git depth1 sparse [git://] | 3992 (3939-4024) | 13.4 (13.4-13.5) | 0.0 (0.0-0.0) |
| git depth1 blobless sparse [git://] | 702 (645-773) | 3.5 (3.5-3.6) | 0.0 (0.0-0.0) |
| git blobless sparse [git://] | 727 (668-769) | 3.5 (3.4-3.6) | 0.0 (0.0-0.0) |
| git full [git://] | 13925 (13147-14082) | 30.0 (29.9-30.5) | 0.0 (0.0-0.0) |
| git depth1 blobless sparse [file://] | 681 (633-839) | 4.0 (3.9-4.1) | 0.0 (0.0-0.0) |
| git full [file://] | 14491 (14253-14817) | 31.9 (31.6-31.9) | 0.0 (0.0-0.0) |

**Verdicts, S6 K=8 slowest** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 (whole tree) [git://] | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git full [git://] | git depth1 blobless sparse [file://] | git full [file://] |
|---|---|---|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | WIN | TIE | TIE | WIN | TIE | WIN |
| vcs full [vcs-server] | TIE | LOSS | LOSS | LOSS | TIE | LOSS | WIN |

**S6 fan-out: 32 simultaneous cold clones, one server** (slowest ms; summed client CPU s; server CPU s)

| cell | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for (slice) [vcs-server] | 1857 (1644-4755) | 6.8 (6.8-6.9) | 0.0 (0.0-0.0) |
| git depth1 sparse [git://] | 15737 (15236-15744) | 70.4 (67.5-70.8) | 0.0 (0.0-0.0) |
| git depth1 blobless sparse [git://] | 3420 (2976-3476) | 18.3 (18.1-18.5) | 0.0 (0.0-0.0) |
| git blobless sparse [git://] | 3337 (3313-3361) (1 of 3 rounds FAILED: parallel failed: fatal: read error: Connection reset by peer
) | 18.4 (18.1-18.7) | 0.0 (0.0-0.0) |
| git depth1 blobless sparse [file://] | 3418 (3372-3479) | 21.8 (21.7-21.9) | 0.0 (0.0-0.0) |

**Verdicts, S6 K=32 slowest** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 sparse [git://] | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|---|
| vcs --for (slice) [vcs-server] | WIN | TIE | TIE | TIE |

**S6 fan-out: 64 simultaneous cold clones, one server** (slowest ms; summed client CPU s; server CPU s)

| cell | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for (slice) [vcs-server] | 7147 (4223-7200) | 13.9 (13.7-14.2) | 0.1 (0.0-0.1) |
| git depth1 blobless sparse [git://] | 7289 (7194-7383) (1 of 3 rounds FAILED: parallel failed: fatal: read error: Connection reset by peer
) | 40.8 (40.7-40.9) | 0.0 (0.0-0.0) |
| git blobless sparse [git://] | 7531 (7290-7772) (1 of 3 rounds FAILED: parallel failed: fatal: read error: Connection reset by peer
) | 41.2 (40.9-41.5) | 0.0 (0.0-0.0) |
| git depth1 blobless sparse [file://] | 7304 (7204-7676) | 47.1 (46.5-47.3) | 0.0 (0.0-0.0) |

**Verdicts, S6 K=64 slowest** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)

| vcs cell | git depth1 blobless sparse [git://] | git blobless sparse [git://] | git depth1 blobless sparse [file://] |
|---|---|---|---|
| vcs --for (slice) [vcs-server] | TIE | TIE | TIE |

server stores KiB: {"git_bare": 43964, "vcs_root": 47356}
