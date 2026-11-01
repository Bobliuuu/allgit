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
