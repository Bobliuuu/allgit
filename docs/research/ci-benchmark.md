# CI-style benchmark: cold runner, warm runner, repeated cycles, parallel runners

Driver: `scripts/bench_ci.py` (plus `scripts/ci_tcpcount.py`, and `scripts/walgit_bench/s3count_proxy.py` as an HTTP counting forwarder).
Raw JSON: `docs/research/data/ci-bench/`. Regenerate the tables with `scripts/bench_ci.py report <file.json>`.

## Read this first: what is and is not measured

- **Real S3 was used only for the vcs-server and walgit object stores, and only in the three 10k runs.** The bucket is a private
  bucket in us-east-1 (all keys under one run prefix, deleted at the end of each run). The credentials expired before the 100k
  real-S3 runs, so **there are no real-S3 numbers at 100k**. No R2 numbers exist. Rows labelled `[vcs@s3]` and walgit are real S3;
  rows labelled `[vcs@disk]` use `vcs-server --root <dir>` (local disk, no object store) and are the "no object-store latency" case.
- **Git never uses S3.** Every `git` row is stock git talking to `git daemon` (git://) in front of a bare repo repacked once with bitmaps,
  on local disk. Comparing `[vcs@s3]` with git therefore compares "object store over a WAN" with "local disk"; the fair like-for-like
  object-store comparison is `[vcs@s3]` against walgit (stock git over HTTP, object store in the same S3 bucket).
- **Machine: the Lambda box (`lambda-final`, 30 cores, AMD EPYC 7J13, shared with other agents), NOT a quiet box.** Load1 during the
  runs was 15 to 49 (per-run start/end values are in each section). Clients, servers and the "runner" all live on this one host (loopback;
  no network between runner and server), so server CPU competes with client CPU. The dedicated AWS boxes a and b were stopped while this was
  running; box c (c6i.4xlarge, us-east-1) was offered later but its benchmark lock was busy with other jobs and the S3 credentials had
  expired, so **no c6i.4xlarge numbers exist in this document**. Treat every wall time as noisy; min-max is shown for that reason.
- **S3 latency observed from this box**: the Lambda box is not in us-east-1. An unauthenticated keep-alive HEAD against the bucket host
  (reply is a 403/404, no object touched) measured p50 66 ms, p95 73 ms, min 65, max 99 ms (n=39); a fresh connection costs about 70 ms TCP
  plus about 110 ms TLS. That is internet distance, not same-region latency (same-region is typically single-digit ms, which was not measured here).
  An earlier measured data point from another client outside the region was about 150 ms/request.
- No emulated-latency columns were run (the earlier RustFS plan was dropped when real S3 became available).
- Page caches were warm; caches were not dropped (no sudo). Runs are interleaved with a per-repetition rotation of the cell order.
- Git 2.56.0 (Lambda). vcs 0.1.0 built from this branch (`cargo build --release`, vcs-server with the `tls` feature so it can talk to https S3).
  The vcs-server `s3://` backend, 50-commit history (initial commit + 50 one-file edits touching one file in the job slice and one outside).
- walgit: the existing build under `~/ref/walgit` on the Lambda box (revision not re-checked here; `walgit-benchmark.md` records 80e9a20b29e2), auth none, roles `[]`, object store in the same S3 bucket under its own prefix; present in the 10k runs only.

## Method

Fixtures: `datagen flat|deep` regrouped into 50 groups `g00..g49` (the job slice is `g07`, 2.0 % of files), and a 70-zone
layout (50 Cargo packages, 20 npm packages, docs; the generator from `scripts/bench_zones_autodetect.sh`; vcs zones come from
`vcs zone suggest` + `apply`, job = `--for crates/p10/src/m0/f1.rs` plus its dependency chain, 4.1 % of files; git sparse cone = `crates/p08..p10`).
The datagen files are small (10k files are 6 to 28 MiB), so byte counts are small and request counts matter more.

Cells: `vcs --for` (zone clone), `vcs --task` (zones70 only), `vcs full`, `git partial` (`--filter=blob:none --sparse` + `sparse-checkout set`),
`git shallow` (`--depth 1 --sparse`, no filter), `git shallow+blobless` (`--depth 1 --filter=blob:none --sparse`), `git full`, and walgit partial/full.
Metrics per cell: wall ms and client CPU (rusage of the child tree via `wait4`), server CPU (utime+stime of the server process, including reaped children),
runner bytes on disk (`du -sk` of the whole clone including `.git`/`.vcs`), files checked out, load1 before each command.
Requests and bytes: vcs from `--stats` (client to vcs-server); git via a TCP counting forwarder (counts connections, not git protocol round trips) and walgit via an HTTP counting forwarder, each in a single separate pass.
**Store-side S3 request counts were not measured** (the forwarders cannot sit in front of https S3 with signed headers).
Scenario 2 = 5 warm fetch cycles; scenario 3 = the next 20 cycles on the same runners (so it starts from a repo with 5 more commits). A cycle is: dev commits two files (one in the slice, one outside) and pushes to every server (untimed);
each warm runner then runs `fetch + checkout` (git: `git fetch origin main && git checkout FETCH_HEAD`, with `--depth 1` for shallow; vcs: `vcs sync`), timed; a no-op "build step" (`status`) is timed separately.
Scenario 4 = 8 clones started at the same instant against one server; wall of the slowest, summed client CPU, server CPU. Runs per cell: 5 at 10k (S4: 5), 3 at 100k.


## Winners (median wall, by cell; read together with the noise caveat)

| Scenario | zones70 10k | flat 10k | deep 10k | zones70 100k (no S3) |
|---|---|---|---|---|
| 1 cold clone, fastest overall | git shallow+blobless 302 ms | git shallow+blobless 262 ms | vcs --for [disk] 312 ms | vcs --task [disk] 1021 ms (git shallow+blobless 1074, ranges overlap) |
| 1 cold clone, fastest with an S3 object store | walgit partial 1094 ms | walgit partial 951 ms | walgit partial 1026 ms | not run |
| 2/3 warm fetch+checkout, fastest | vcs [disk] 74 / 68 ms | vcs [disk] 149 / 87 ms | vcs [disk] 167 / 87 ms | vcs [disk] 232 / 186 ms |
| 2/3 warm, fastest with S3 object store | walgit partial 633 / 633 ms | walgit partial 652 / 639 ms | walgit partial 668 / 656 ms | not run |
| 4 eight parallel cold clones (slowest) | git shallow+blobless 558 ms | git partial 356 ms (blobless 354) | vcs --for [disk] 687 ms | git shallow+blobless 1358 ms (vcs --for [disk] 1456) |

Scenario 3 per-cycle median and p95 (20 cycles) are in the tables below. Neither vcs nor git showed a growth trend in the per-cycle numbers over 20 commits (min-max stays narrow; no fit was done).

## Where vcs wins

- Warm fetch+checkout against a local-disk vcs-server, every layout and both scales: 68 to 232 ms versus 130 to 1010 ms for the git variants, and `vcs status` 11 to 59 ms versus 7 to 95 ms (status is mixed at 10k, favouring git on flat/zones70 by a few ms, and vcs at 100k).
- Cold zone clone on the deep layout at 10k, and at 100k zones70 (`--task` 1021 ms, `--for` 1273 ms, versus git partial 1215 ms and git shallow+blobless 1074 ms: a tie within noise, not a win for `--for`).
- Cold clone versus the naive CI clones: `vcs --for` is 3.5 to 7.5x faster than `git clone` full and than `git clone --depth 1 --sparse` without a filter at 100k (1273 ms versus 9512 and 4438 ms), and its runner disk is 34 MiB versus 533 and 154 MiB.
- Eight parallel clones at 100k: `vcs --for` 1456 ms slowest versus 12762 ms for git full (the fair comparator is git partial, 2233 ms in a noisy run).

## Cells where vcs loses, with the likely reason (reasons are hypotheses; none was isolated by an experiment)

1. **Everything with `[vcs@s3]` from this box.** Cold `--for`: 4.8 s (zones70), 2.0 s (flat), 2.1 s (deep) versus walgit partial 1.1 / 1.0 / 1.0 s, i.e. vcs-server over S3 is 2 to 5x slower than walgit over the same bucket. Warm fetch: 1.1 to 1.2 s versus walgit partial 0.63 to 0.67 s. Eight parallel `--for` clones: 16.5 to 21.6 s versus 4.0 to 6.2 s for walgit partial. Likely reason: the vcs client makes 16 to 41 requests per zone clone (and 5 per warm fetch) and the vcs-server turns them into sequential S3 reads at about 66 ms each from this box, while git needs 1 to 7 connections and walgit serves from a local cache. The `vcs --task` plan needed fewer sequential round trips on zones70 (3.3 s versus 4.8 s) even though it issued more requests (89 versus 41). With same-region S3 (not measured) this gap would shrink; by how much is unknown.
2. **Bytes and requests on a cold zone clone.** `vcs --for` downloads 7.3 MiB in 41 requests (zones70 10k), 2.8 MiB in 16 (flat), 8.0 MiB in 16 (deep), 54.4 MiB in 56 (zones70 100k) versus 0.8 / 0.3 / 1.4 / 7.4 MiB for git blob:none and 3 connections. About 7 to 9x more bytes. Likely reason: the zone's history (all 51 commits of the slice, full blobs) and per-zone metadata are fetched, while git blobless fetches only the tip's blobs. Not verified.
3. **Client CPU on a cold clone.** 0.33 to 0.44 s (10k) and 1.42 s (100k) for `vcs --for` versus 0.09 to 0.14 s and 0.86 s for git blobless on flat/zones70 (on deep at 10k vcs is lower, 0.33 versus 0.45 s). Likely reason: decoding and writing more objects (see 2).
4. **Cold clone wall on flat and zones70 at 10k, local disk.** vcs 349 / 564 ms versus git blobless 262 / 302 ms and git partial 307 / 366 ms. Same likely reason as 2 and 3.
5. **Eight parallel clones on flat/zones70 at 10k, local disk.** vcs --for 1108 / 1048 ms versus git partial 356 / 971 ms (summed client CPU 6.0 / 3.6 s versus 0.8 / 1.2 s). Also the vcs-server used 0.2 to 0.4 s CPU versus about 0 for git daemon (which spawns upload-pack children that may not be fully attributed to the daemon's CPU in this measurement).
6. **Warm fetch protocol size.** 5 to 6 requests and 7 to 13 KB per one-commit fetch versus 1 connection and about 1 to 2 KB for git. This is not a latency loss on local disk but it is the reason for loss 1 on S3.
7. **vcs full clone** is not better than `git clone` except at 100k wall (3.2 s versus 9.5 s) and is equal or worse on CPU and bytes elsewhere; it is not the CI shape anyway.

## Scope that was not run, so no claim is made

- Real S3 at 100k (credentials expired); R2; same-region S3; the c6i.4xlarge box; walgit at 100k; cold OS caches; flat and deep at 100k; a separate network between runner and server; store-side S3 request counts; git over HTTP (git was served by `git daemon`); the `vcs --task` variant on flat/deep (no zone names to match).
- Server-store sizes are in each section's last line (git bare repo versus vcs root versus S3 prefix), for reporting only.

## Results

Each section is the output of `scripts/bench_ci.py report`. "files" is files checked out outside `.git`/`.vcs`; git cells show 2 to 3 more files than vcs (top-level files in the sparse checkout).

## Real S3 for vcs-server and walgit (Lambda box, 10k files, 5 runs, 20 cycles, 8 parallel)

### zones70 9953 files (28 MiB), 70 vcs zones, 51 commits, job slice 405 files (4.1%)

box cores 30, load1 start/5/15 20.47/21.01/17.74, end 35.42/27.32/21.96; git version 2.56.0; vcs 0.1.0; runs 5; date 2026-11-01T15:39:36Z

**Scenario 1: cold runner clone** (wall ms median (min-max); client CPU s; server CPU s; runner disk MiB; files checked out; requests; payload MiB)

| tool [server] | wall ms | client CPU s | server CPU s | runner MiB | files | requests | MiB down |
|---|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 4800 (4582-6285) | 0.43 (0.38-0.51) | 0.05 (0.04-0.06) | 4.8 | 405 | 41 (vcs-server) | 7.3 |
| vcs --task [vcs@s3] | 3275 (3186-3504) | 0.42 (0.38-0.45) | 0.04 (0.04-0.05) | 4.8 | 405 | 89 (vcs-server) | 3.9 |
| vcs full [vcs@s3] | 1622 (1393-2013) | 1.10 (1.00-1.15) | 0.05 (0.05-0.06) | 70.4 | 9953 | 23 (vcs-server) | 13.0 |
| vcs --for [vcs@disk] | 564 (385-741) | 0.44 (0.38-0.49) | 0.01 (0.01-0.02) | 4.8 | 405 | 41 (vcs-server) | 7.3 |
| vcs --task [vcs@disk] | 385 (345-422) | 0.46 (0.42-0.53) | 0.02 (0.01-0.02) | 4.8 | 405 | 89 (vcs-server) | 3.9 |
| vcs full [vcs@disk] | 658 (429-1043) | 1.09 (1.03-1.20) | 0.03 (0.03-0.05) | 70.4 | 9953 | 23 (vcs-server) | 13.0 |
| git partial (blob:none+sparse) [git] | 366 (291-544) | 0.14 (0.13-0.14) | 0.00 (0.00-0.00) | 3.6 | 408 | 3 (tcp connections) | 0.8 |
| git full [git] | 1478 (1106-2859) | 1.03 (1.00-1.19) | 0.00 (0.00-0.00) | 53.9 | 9953 | 1 (tcp connections) | 12.6 |
| git partial (blob:none+sparse) [walgit] | 1094 (1020-1813) | 0.17 (0.16-0.19) | 0.04 (0.03-0.08) | 3.6 | 408 | 7 (http requests) | 0.8 |
| git full [walgit] | 1838 (1394-4242) | 1.07 (1.05-1.11) | 0.09 (0.07-0.11) | 53.9 | 9953 | 3 (http requests) | 12.6 |
| git shallow (depth1+sparse) [git] | 614 (553-1565) | 0.46 (0.43-0.50) | 0.00 (0.00-0.00) | 15.5 | 408 | 1 (tcp connections) | 12.5 |
| git shallow+blobless [git] | 302 (279-597) | 0.13 (0.12-0.13) | 0.00 (0.00-0.00) | 3.5 | 408 | 3 (tcp connections) | 0.7 |

**Scenario 2: warm runner, fetch 1 new commit + checkout** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1165 | 1249 | 982-1249 | 0.03 | 0.02 | 26 |
| vcs --for [vcs@disk] | 74 | 149 | 54-149 | 0.02 | 0.00 | 14 |
| git partial (blob:none+sparse) [git] | 210 | 257 | 200-257 | 0.05 | 0.00 | 13 |
| git full [git] | 155 | 488 | 136-488 | 0.07 | 0.00 | 26 |
| git partial (blob:none+sparse) [walgit] | 633 | 841 | 598-841 | 0.06 | 0.03 | 14 |
| git full [walgit] | 350 | 400 | 339-400 | 0.08 | 0.01 | 28 |
| git shallow (depth1+sparse) [git] | 145 | 171 | 127-171 | 0.04 | 0.00 | 13 |
| git shallow+blobless [git] | 236 | 290 | 219-290 | 0.04 | 0.00 | 14 |

**Scenario 3: 20 sequential CI cycles (fetch+checkout), per-cycle** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1202 | 1353 | 988-1433 | 0.03 | 0.02 | 11 |
| vcs --for [vcs@disk] | 68 | 182 | 41-344 | 0.03 | 0.00 | 11 |
| git partial (blob:none+sparse) [git] | 228 | 324 | 179-485 | 0.05 | 0.00 | 15 |
| git full [git] | 155 | 214 | 124-254 | 0.07 | 0.00 | 29 |
| git partial (blob:none+sparse) [walgit] | 633 | 884 | 561-951 | 0.07 | 0.02 | 18 |
| git full [walgit] | 367 | 434 | 320-676 | 0.09 | 0.01 | 28 |
| git shallow (depth1+sparse) [git] | 152 | 207 | 128-217 | 0.05 | 0.00 | 18 |
| git shallow+blobless [git] | 230 | 292 | 198-323 | 0.05 | 0.00 | 17 |

**Warm fetch, one commit: requests and payload**

| tool | requests | bytes down |
|---|---|---|
| vcs --for [vcs@s3] | 5 (vcs-server requests) | 11726 |
| vcs --for [vcs@disk] | 5 (vcs-server requests) | 11726 |
| git partial (blob:none+sparse) [git] | 1 (tcp connections) | 1160 |
| git full [git] | 1 (tcp connections) | 1160 |
| git partial (blob:none+sparse) [walgit] | 3 (http requests) | 1238 |
| git full [walgit] | 3 (http requests) | 1238 |
| git shallow (depth1+sparse) [git] | 1 (tcp connections) | 1282 |
| git shallow+blobless [git] | 1 (tcp connections) | 1282 |

**Scenario 4: 8 simultaneous cold clones from one server** (wall of slowest ms; summed client CPU s; server CPU s)

| tool [server] | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for [vcs@s3] | 21609 (21185-24147) | 3.5 (3.4-3.9) | 1.4 (1.3-1.6) |
| vcs --for [vcs@disk] | 1048 (966-1541) | 3.6 (3.3-3.9) | 0.4 (0.4-0.4) |
| vcs full [vcs@disk] | 1451 (1368-1990) | 9.5 (9.4-10.4) | 0.3 (0.2-0.3) |
| git partial (blob:none+sparse) [git] | 971 (498-3434) | 1.2 (1.1-1.2) | 0.0 (0.0-0.0) |
| git full [git] | 2135 (1877-3225) | 8.5 (8.4-9.1) | 0.0 (0.0-0.0) |
| git partial (blob:none+sparse) [walgit] | 6190 (5912-6849) | 1.5 (1.5-1.5) | 0.3 (0.3-0.7) |
| git full [walgit] | 4593 (4360-5128) | 9.4 (8.9-9.8) | 0.7 (0.7-1.0) |
| git shallow+blobless [git] | 558 (504-633) | 1.1 (1.0-1.1) | 0.0 (0.0-0.0) |

server stores: {"git_bare_kib": 13416, "vcs_disk_root_kib": 13292, "vcs_s3_objects": 4, "vcs_s3_kib": 13273, "walgit_s3_objects": 5, "walgit_s3_kib": 13181}

### flat 10000 files (6 MiB), 50 vcs zones, 51 commits, job slice 200 files (2.0%)

box cores 30, load1 start/5/15 38.33/28.36/22.39, end 48.97/38.24/28.89; git version 2.56.0; vcs 0.1.0; runs 5; date 2026-11-01T15:50:36Z

**Scenario 1: cold runner clone** (wall ms median (min-max); client CPU s; server CPU s; runner disk MiB; files checked out; requests; payload MiB)

| tool [server] | wall ms | client CPU s | server CPU s | runner MiB | files | requests | MiB down |
|---|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1977 (1703-2534) | 0.37 (0.34-0.43) | 0.01 (0.01-0.02) | 3.7 | 200 | 16 (vcs-server) | 2.8 |
| vcs full [vcs@s3] | 1509 (1427-1946) | 0.78 (0.76-0.80) | 0.03 (0.03-0.04) | 48.0 | 10000 | 22 (vcs-server) | 5.0 |
| vcs --for [vcs@disk] | 349 (247-967) | 0.42 (0.35-0.49) | 0.01 (0.00-0.01) | 3.7 | 200 | 16 (vcs-server) | 2.8 |
| vcs full [vcs@disk] | 422 (383-502) | 0.77 (0.75-0.80) | 0.02 (0.01-0.02) | 48.0 | 10000 | 22 (vcs-server) | 5.0 |
| git partial (blob:none+sparse) [git] | 307 (274-415) | 0.10 (0.10-0.11) | 0.00 (0.00-0.00) | 2.2 | 200 | 2 (tcp connections) | 0.3 |
| git full [git] | 728 (690-815) | 0.63 (0.61-0.66) | 0.00 (0.00-0.00) | 44.9 | 10000 | 1 (tcp connections) | 4.0 |
| git partial (blob:none+sparse) [walgit] | 951 (889-1114) | 0.13 (0.12-0.13) | 0.04 (0.03-0.08) | 2.2 | 200 | 5 (http requests) | 0.3 |
| git full [walgit] | 1089 (979-1180) | 0.67 (0.63-0.70) | 0.07 (0.03-0.41) | 44.9 | 10000 | 3 (http requests) | 4.0 |
| git shallow (depth1+sparse) [git] | 340 (307-351) | 0.20 (0.19-0.21) | 0.00 (0.00-0.00) | 6.1 | 200 | 1 (tcp connections) | 3.9 |
| git shallow+blobless [git] | 262 (223-313) | 0.09 (0.08-0.10) | 0.00 (0.00-0.00) | 2.2 | 200 | 2 (tcp connections) | 0.3 |

**Scenario 2: warm runner, fetch 1 new commit + checkout** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1099 | 1148 | 1049-1148 | 0.03 | 0.00 | 15 |
| vcs --for [vcs@disk] | 149 | 200 | 49-200 | 0.04 | 0.00 | 9 |
| git partial (blob:none+sparse) [git] | 219 | 241 | 194-241 | 0.05 | 0.00 | 15 |
| git full [git] | 154 | 1093 | 135-1093 | 0.17 | 0.00 | 23 |
| git partial (blob:none+sparse) [walgit] | 652 | 751 | 596-751 | 0.06 | 0.03 | 13 |
| git full [walgit] | 332 | 495 | 328-495 | 0.08 | 0.01 | 23 |
| git shallow (depth1+sparse) [git] | 132 | 181 | 124-181 | 0.04 | 0.00 | 16 |
| git shallow+blobless [git] | 247 | 267 | 189-267 | 0.04 | 0.00 | 14 |

**Scenario 3: 20 sequential CI cycles (fetch+checkout), per-cycle** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1117 | 1411 | 933-1751 | 0.03 | 0.02 | 11 |
| vcs --for [vcs@disk] | 87 | 403 | 35-442 | 0.03 | 0.00 | 11 |
| git partial (blob:none+sparse) [git] | 224 | 353 | 188-515 | 0.05 | 0.00 | 15 |
| git full [git] | 138 | 184 | 124-217 | 0.06 | 0.00 | 25 |
| git partial (blob:none+sparse) [walgit] | 639 | 927 | 555-1160 | 0.07 | 0.02 | 16 |
| git full [walgit] | 348 | 431 | 318-442 | 0.07 | 0.01 | 25 |
| git shallow (depth1+sparse) [git] | 139 | 165 | 126-243 | 0.04 | 0.00 | 15 |
| git shallow+blobless [git] | 221 | 422 | 185-427 | 0.05 | 0.00 | 17 |

**Warm fetch, one commit: requests and payload**

| tool | requests | bytes down |
|---|---|---|
| vcs --for [vcs@s3] | 5 (vcs-server requests) | 12164 |
| vcs --for [vcs@disk] | 5 (vcs-server requests) | 12164 |
| git partial (blob:none+sparse) [git] | 1 (tcp connections) | 938 |
| git full [git] | 1 (tcp connections) | 938 |
| git partial (blob:none+sparse) [walgit] | 3 (http requests) | 1016 |
| git full [walgit] | 3 (http requests) | 1016 |
| git shallow (depth1+sparse) [git] | 1 (tcp connections) | 1060 |
| git shallow+blobless [git] | 1 (tcp connections) | 1060 |

**Scenario 4: 8 simultaneous cold clones from one server** (wall of slowest ms; summed client CPU s; server CPU s)

| tool [server] | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for [vcs@s3] | 16829 (16447-18383) | 4.7 (4.3-5.3) | 0.9 (0.9-1.0) |
| vcs --for [vcs@disk] | 1108 (760-1447) | 6.0 (4.8-7.7) | 0.2 (0.2-0.2) |
| vcs full [vcs@disk] | 1001 (933-1460) | 6.7 (6.5-7.7) | 0.2 (0.1-0.2) |
| git partial (blob:none+sparse) [git] | 356 (308-398) | 0.8 (0.8-0.8) | 0.0 (0.0-0.0) |
| git full [git] | 1045 (913-2237) | 5.5 (5.3-5.7) | 0.0 (0.0-0.0) |
| git partial (blob:none+sparse) [walgit] | 4054 (4030-4379) | 1.0 (1.0-1.0) | 0.4 (0.2-0.6) |
| git full [walgit] | 3285 (2988-3554) | 5.6 (5.5-6.1) | 0.4 (0.3-0.5) |
| git shallow+blobless [git] | 354 (341-542) | 0.7 (0.7-0.8) | 0.0 (0.0-0.0) |

server stores: {"git_bare_kib": 4568, "vcs_disk_root_kib": 5180, "vcs_s3_objects": 4, "vcs_s3_kib": 5153, "walgit_s3_objects": 5, "walgit_s3_kib": 4338}

### deep 10000 files (6 MiB), 50 vcs zones, 51 commits, job slice 200 files (2.0%)

box cores 30, load1 start/5/15 44.14/37.56/28.77, end 22.97/22.05/24.45; git version 2.56.0; vcs 0.1.0; runs 5; date 2026-11-01T15:58:30Z

**Scenario 1: cold runner clone** (wall ms median (min-max); client CPU s; server CPU s; runner disk MiB; files checked out; requests; payload MiB)

| tool [server] | wall ms | client CPU s | server CPU s | runner MiB | files | requests | MiB down |
|---|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 2135 (2089-2193) | 0.34 (0.31-0.41) | 0.03 (0.02-0.04) | 5.9 | 200 | 16 (vcs-server) | 8.0 |
| vcs full [vcs@s3] | 2237 (2090-2887) | 2.72 (2.38-2.88) | 0.04 (0.03-0.04) | 151.2 | 10000 | 22 (vcs-server) | 7.0 |
| vcs --for [vcs@disk] | 312 (281-331) | 0.33 (0.29-0.40) | 0.02 (0.00-0.02) | 5.9 | 200 | 16 (vcs-server) | 8.0 |
| vcs full [vcs@disk] | 1212 (1155-1274) | 2.64 (2.38-2.80) | 0.02 (0.01-0.02) | 151.2 | 10000 | 22 (vcs-server) | 7.0 |
| git partial (blob:none+sparse) [git] | 476 (432-495) | 0.45 (0.43-0.48) | 0.00 (0.00-0.00) | 6.9 | 200 | 2 (tcp connections) | 1.4 |
| git full [git] | 2224 (2141-2304) | 2.12 (2.06-2.17) | 0.00 (0.00-0.00) | 147.8 | 10000 | 1 (tcp connections) | 5.0 |
| git partial (blob:none+sparse) [walgit] | 1026 (902-1095) | 0.47 (0.46-0.51) | 0.04 (0.02-0.06) | 6.9 | 200 | 5 (http requests) | 1.4 |
| git full [walgit] | 2572 (2319-2680) | 2.20 (2.03-2.27) | 0.05 (0.03-0.07) | 147.8 | 10000 | 3 (http requests) | 5.0 |
| git shallow (depth1+sparse) [git] | 601 (583-651) | 0.44 (0.44-0.47) | 0.00 (0.00-0.00) | 10.7 | 200 | 1 (tcp connections) | 5.0 |
| git shallow+blobless [git] | 517 (461-547) | 0.31 (0.30-0.35) | 0.00 (0.00-0.00) | 6.8 | 200 | 2 (tcp connections) | 1.4 |

**Scenario 2: warm runner, fetch 1 new commit + checkout** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1209 | 1502 | 993-1502 | 0.11 | 0.01 | 15 |
| vcs --for [vcs@disk] | 167 | 201 | 134-201 | 0.09 | 0.00 | 14 |
| git partial (blob:none+sparse) [git] | 258 | 294 | 238-294 | 0.10 | 0.00 | 37 |
| git full [git] | 542 | 1701 | 202-1701 | 1.17 | 0.00 | 377 |
| git partial (blob:none+sparse) [walgit] | 668 | 854 | 632-854 | 0.14 | 0.02 | 40 |
| git full [walgit] | 774 | 868 | 760-868 | 1.15 | 0.01 | 388 |
| git shallow (depth1+sparse) [git] | 272 | 283 | 238-283 | 0.14 | 0.00 | 36 |
| git shallow+blobless [git] | 276 | 291 | 253-291 | 0.10 | 0.00 | 37 |

**Scenario 3: 20 sequential CI cycles (fetch+checkout), per-cycle** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@s3] | 1142 | 1265 | 1015-1695 | 0.07 | 0.02 | 15 |
| vcs --for [vcs@disk] | 87 | 164 | 71-178 | 0.07 | 0.00 | 16 |
| git partial (blob:none+sparse) [git] | 223 | 271 | 209-314 | 0.07 | 0.00 | 34 |
| git full [git] | 199 | 539 | 189-611 | 0.19 | 0.00 | 355 |
| git partial (blob:none+sparse) [walgit] | 656 | 827 | 587-880 | 0.10 | 0.02 | 34 |
| git full [walgit] | 406 | 815 | 380-932 | 0.20 | 0.01 | 355 |
| git shallow (depth1+sparse) [git] | 211 | 278 | 182-289 | 0.11 | 0.00 | 34 |
| git shallow+blobless [git] | 232 | 262 | 210-293 | 0.07 | 0.00 | 34 |

**Warm fetch, one commit: requests and payload**

| tool | requests | bytes down |
|---|---|---|
| vcs --for [vcs@s3] | 5 (vcs-server requests) | 7272 |
| vcs --for [vcs@disk] | 5 (vcs-server requests) | 7272 |
| git partial (blob:none+sparse) [git] | 1 (tcp connections) | 1915 |
| git full [git] | 1 (tcp connections) | 1915 |
| git partial (blob:none+sparse) [walgit] | 3 (http requests) | 1993 |
| git full [walgit] | 3 (http requests) | 1993 |
| git shallow (depth1+sparse) [git] | 1 (tcp connections) | 2037 |
| git shallow+blobless [git] | 1 (tcp connections) | 2037 |

**Scenario 4: 8 simultaneous cold clones from one server** (wall of slowest ms; summed client CPU s; server CPU s)

| tool [server] | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for [vcs@s3] | 16519 (15818-17227) | 3.5 (3.1-3.8) | 0.9 (0.9-1.1) |
| vcs --for [vcs@disk] | 687 (660-757) | 4.1 (2.9-4.2) | 0.3 (0.2-0.3) |
| vcs full [vcs@disk] | 4794 (4148-5653) | 26.8 (23.6-28.3) | 0.2 (0.2-0.2) |
| git partial (blob:none+sparse) [git] | 742 (622-925) | 3.2 (3.1-3.5) | 0.0 (0.0-0.0) |
| git full [git] | 4130 (3785-6631) | 19.3 (18.6-22.4) | 0.0 (0.0-0.0) |
| git partial (blob:none+sparse) [walgit] | 4218 (3944-4558) | 3.7 (3.4-3.9) | 0.3 (0.3-0.6) |
| git full [walgit] | 6091 (5607-9056) | 20.3 (18.9-22.1) | 0.4 (0.4-0.4) |
| git shallow+blobless [git] | 739 (677-819) | 2.6 (2.6-2.8) | 0.0 (0.0-0.0) |

server stores: {"git_bare_kib": 6624, "vcs_disk_root_kib": 7236, "vcs_s3_objects": 4, "vcs_s3_kib": 7215, "walgit_s3_objects": 6, "walgit_s3_kib": 6180}

## 100k files, no object store (vcs-server on local disk, git daemon; 3 runs, 20 cycles, 8 parallel)

### zones70 99993 files (281 MiB), 70 vcs zones, 51 commits, job slice 4071 files (4.1%)

box cores 30, load1 start/5/15 19.32/23.17/24.66, end 22.78/17.98/19.50; git version 2.56.0; vcs 0.1.0; runs 3; date 2026-11-01T16:19:59Z

**Scenario 1: cold runner clone** (wall ms median (min-max); client CPU s; server CPU s; runner disk MiB; files checked out; requests; payload MiB)

| tool [server] | wall ms | client CPU s | server CPU s | runner MiB | files | requests | MiB down |
|---|---|---|---|---|---|---|---|
| vcs --for [vcs@disk] | 1273 (1082-1507) | 1.42 (1.39-1.48) | 0.07 (0.07-0.08) | 34.3 | 4071 | 56 (vcs-server) | 54.4 |
| vcs --task [vcs@disk] | 1021 (872-1074) | 1.41 (1.32-1.46) | 0.05 (0.05-0.05) | 34.3 | 4071 | 105 (vcs-server) | 29.6 |
| vcs full [vcs@disk] | 3201 (3136-3815) | 9.48 (9.42-10.01) | 0.23 (0.22-0.25) | 697.3 | 99993 | 23 (vcs-server) | 125.7 |
| git partial (blob:none+sparse) [git] | 1215 (1014-1240) | 0.86 (0.85-0.88) | 0.00 (0.00-0.00) | 33.2 | 4074 | 3 (tcp connections) | 7.4 |
| git full [git] | 9512 (9358-13078) | 9.94 (9.71-10.09) | 0.00 (0.00-0.00) | 533.2 | 99993 | 1 (tcp connections) | 126.2 |
| git shallow (depth1+sparse) [git] | 4438 (4226-5212) | 4.15 (4.03-4.36) | 0.00 (0.00-0.00) | 154.4 | 4074 | 1 (tcp connections) | 126.1 |
| git shallow+blobless [git] | 1074 (1021-1324) | 0.86 (0.82-0.87) | 0.00 (0.00-0.00) | 33.2 | 4074 | 3 (tcp connections) | 7.4 |

**Scenario 2: warm runner, fetch 1 new commit + checkout** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@disk] | 232 | 583 | 225-583 | 0.17 | 0.01 | 59 |
| git partial (blob:none+sparse) [git] | 360 | 754 | 357-754 | 0.19 | 0.00 | 89 |
| git full [git] | 820 | 849 | 685-849 | 1.90 | 0.00 | 186 |
| git shallow (depth1+sparse) [git] | 1010 | 1145 | 673-1145 | 0.40 | 0.00 | 92 |
| git shallow+blobless [git] | 600 | 910 | 401-910 | 0.24 | 0.00 | 93 |

**Scenario 3: 20 sequential CI cycles (fetch+checkout), per-cycle** (ms; then no-op build step = status)

| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |
|---|---|---|---|---|---|---|
| vcs --for [vcs@disk] | 186 | 228 | 133-231 | 0.15 | 0.01 | 27 |
| git partial (blob:none+sparse) [git] | 354 | 377 | 293-412 | 0.20 | 0.00 | 95 |
| git full [git] | 570 | 629 | 281-632 | 1.79 | 0.00 | 170 |
| git shallow (depth1+sparse) [git] | 731 | 830 | 315-863 | 0.34 | 0.00 | 90 |
| git shallow+blobless [git] | 371 | 565 | 321-573 | 0.20 | 0.00 | 92 |

**Warm fetch, one commit: requests and payload**

| tool | requests | bytes down |
|---|---|---|
| vcs --for [vcs@disk] | 6 (vcs-server requests) | 13131 |
| git partial (blob:none+sparse) [git] | 1 (tcp connections) | 1151 |
| git full [git] | 1 (tcp connections) | 1151 |
| git shallow (depth1+sparse) [git] | 1 (tcp connections) | 1273 |
| git shallow+blobless [git] | 1 (tcp connections) | 1273 |

**Scenario 4: 8 simultaneous cold clones from one server** (wall of slowest ms; summed client CPU s; server CPU s)

| tool [server] | slowest ms | client CPU s | server CPU s |
|---|---|---|---|
| vcs --for [vcs@disk] | 1456 (1426-1572) | 11.2 (11.0-12.1) | 0.8 (0.7-0.9) |
| vcs full [vcs@disk] | 6605 (5859-8022) | 86.0 (82.4-88.1) | 1.6 (1.3-1.7) |
| git partial (blob:none+sparse) [git] | 2233 (1133-2287) | 7.2 (7.0-7.4) | 0.0 (0.0-0.0) |
| git full [git] | 12762 (10391-14374) | 80.1 (78.1-84.8) | 0.0 (0.0-0.0) |
| git shallow+blobless [git] | 1358 (1347-1696) | 7.2 (7.0-7.5) | 0.0 (0.0-0.0) |

server stores: {"git_bare_kib": 132972, "vcs_disk_root_kib": 128740}
