# Monorepo stress run: 1M files, 200k commits, 65 GB of big files, up to 256 writers

One documented, reproducible end-to-end run of the whole system on a large synthetic monorepo, on one EC2 box, with
chaos. Everything below was measured on that box in one session (2026-11-05); nothing is extrapolated. Where a number
could not be reached it says so. Harness: `scripts/stress/` (new) plus the extended `scripts/soak/soak.py`.

## Scale actually reached

| dimension | reached | note |
|---|---|---|
| files in the tree | 1,000,659 | 100 x 100 directories of 100 files, `histgen.py` nested layout, plus 35 zones |
| commits | 200,000 on `main` (+ side branches, 147 tags) | merge commits and renames included; not 1M (see "Limits tried") |
| big files | 20 binaries, 1 to 5 GiB, 64.7 GB; 3 committed versions each | 3 x 1 MiB overwritten per version, 1 MiB inserted in every 5th file, 64 MiB appended to every 4th |
| zones | 35 (32 writer zones `z0..z31`, `art` lockable zone, `big`, root) | the 1M files and the history live in the root zone |
| remote objects | 2,553,155 in 25 packs after the base publish; 70 GB store | |
| concurrent writers | 64, 128 and 256 CLI clients (one process each, `vcs` CLI against `vcs-server`) | single manifest (64, 256) and sharded manifest (128, 256) |
| concurrent readers | 3 reader clients per run (`clone --auto`, root-zone clone, slice clone) | the first two take 4 to 5 minutes to clone, so they did little in runs A to D; run F started them six minutes before the writers |
| chaos | `kill -9` of `vcs-server` every 90 to 300 s (8 kills in each of C and D), periodic audits; `gc --remote` kills only ran in run G (after #547) | the first `gc` rounds were all blocked by the leaked `gc/lock`, see "What broke" item 4 |
| spend | US$10.3 | see the end |

Not reached: 10M-file monorepo with the full history (tried separately, see "Limits tried"), 1M commits with 1M files,
512+ writers, real S3 (not part of this run). Closed-loop CLI clients with 4 to 8 s think time are not a server capacity
test: the server averaged 2 to 6 of the 32 cores (peaks 5 to 12) and peaked at 4.7 to 13.1 GiB RSS, while the box was CPU-bound on the *clients* (load1 peaks 40 to 87).
The protocol-level hot-branch test below is the server capacity number.

## Setup

- Box: EC2 `c6id.8xlarge` (32 vCPU Xeon 8375C, 61 GiB RAM, 1.7 TB local NVMe formatted ext4 without journal as `/data`,
  100 GB gp3 root), us-east-1, Ubuntu 24.04, git 2.43.0, rust 1.99.1. Everything under `/data`; the client cache
  (`~/.cache/vcs`) is moved to `/data` with `XDG_CACHE_HOME` (see "What broke", item 6).
- Software: `origin/main` at `525aa7f` plus four fixes found by this run, each in its own PR: #542 (import), #543 (publish pack
  split), #546 (gc block cache), #547 (gc lock). The harness PR is this one. Runs A to F used binaries with #542, #543 and #546; run G used the
  binaries with #547 as well.
- Base repository (`scripts/stress/build_base.sh`, timings in `data/monorepo-stress/build.log`):

| step | wall | peak RSS |
|---|---|---|
| `histgen.py --commits 200000 --dirs 100 --subdirs 100 --files 100 --lines 6` (644 MB fast-import stream) | 11.8 s | 0.7 GB |
| `vcs import-git` of that stream (200,000 commits, 1,586,265 objects) | 76.1 s | 1.6 GB |
| `big_gen`: 20 files, 64.7 GB of random bytes, 16 processes | 47.5 s | |
| `vcs add .` (1M files + 64.7 GB, chunked) | 59.6 s | 1.9 GB |
| `vcs commit` | 7.5 s | 0.5 GB |
| `add` + `commit` of big-file version 2 / 3 (3 MiB changed, insert, append) | 10.0 + 7.4 s / 9.9 + 7.4 s | 4.4 / 3.9 GB |
| `vcs publish` of everything to `vcs-server` (the first attempt failed with 413, see "What broke" item 2) | 212.8 s | 14.9 GB |
| the same history in git: `git fast-import` of the same stream (no big files) | 95.1 s | 0.5 GB |

  The base store is 65 GB for the three versions of 65 GB of big files plus the history: chunk deduplication stored the two
  later versions as small deltas. The git repository of the same history without the big files is 360 MB.
- Every run starts from a copy of the base store (`cp -a`, about 91 s), starts `vcs-server` (single manifest, or
  `vcs-server migrate-sharded` first for the sharded runs), and creates tokens `u0..uN` (write) plus `admin`.

## Commands

```bash
# on the box (VCS_BIN = target/release of this tree)
scripts/stress/build_base.sh /data/run/base                      # stream, import, zones, big files, publish, git copy
scripts/stress/single_ops.sh /data/run/base /data/run/single     # one client: clone, status, log, blame, fsck, gc against git
scripts/stress/run_stress.sh a64   64  12 --readers 3 --ramp 60  --sample 5
scripts/stress/run_stress.sh b128  128 12 --readers 3 --ramp 120 --sample 5 --sharded
scripts/stress/run_stress.sh c256  256 20 --readers 3 --ramp 300 --think 8 --sample 5 --chaos mixed \
    --kill-min 90 --kill-max 300 --gc-first 180 --gc-every 240 --gc-grace 600 --audit-every 420
scripts/stress/run_stress.sh d256  256 20 --readers 3 --ramp 300 --think 8 --sample 5 --sharded --chaos mixed \
    --kill-min 90 --kill-max 300 --gc-first 180 --gc-every 240 --gc-grace 600 --audit-every 420 --full-verify
scripts/stress/run_hot.sh /data/run/hot                          # protocol-level hot branch: writerbench, tickets on/off
scripts/stress/limits.sh /data/run/limits                        # 1M commits, 10M files
scripts/stress/bigfile_cmp.sh /data/run/base /data/run/bigcmp /data/run/base/seed/big/big03.bin
scripts/stress/summarize.py /data/run/runs a64 b128-sharded c256 d256-sharded
```

`run_stress.sh` wraps `scripts/soak/soak.py run` (extended here, see below) and records `sar` and loopback byte counters.
Environment: `SOAK_NZ=32` zones, `SOAK_HOT=1` (a quarter of the clients all write zone `z0`: the hot shared branch, the
rest are disjoint zones).

### What each client does (soak.py, extended)

Weights: commit 40, publish 28, sync 12, branch create/switch/delete 5, undo 4, fresh clone 3, lock cycle 8 (lock a
shared `art/f*.bin`, edit, commit, `publish --auto-merge`, unlock), and for every 8th client a large-file push of 6 (a
128 MiB `blob.bin` in its zone: 2 MiB rewritten and 4 MiB appended per version; the sha256 of the whole file is acked).
Writers clone only their zone and `art` (`vcs clone --zone zN --zone art`); the root zone with the 1M files is never in a
writer's work tree. Readers (`--readers 3`, admin token): `clone --auto`, `clone --zone root` (the 1M-file tree), a one-zone
slice; then `sync`, `status`, `log -- path`, `blame`, `fsck`, `gc --dry-run`, `gc --remote --dry-run` and fresh clones.
Changes to `soak.py`: `--base-store`, `--slice`, `--bigmb`, `--readers`, `--full-verify`, `--big-manifest`, `SOAK_NZ`,
`SOAK_HOT`, sliced verification clones, big-file acks (newest acked version must be in the final tree, a newer
committed one is allowed, an older or missing one is a loss), `vcs locks` after the run, a per-call timeout for readers,
`--zone` on the branch commands, no `log.txt` edit before an undo (it left an unstaged change that made the next
`vcs merge` refuse; 1,400 of the errors in run B and 1,426 in run C are that harness artefact), and the stale-`gc/lock`
cleanup now only removes a lock whose holder pid is gone (the first version removed locks of live admin clients).

## Results

### One client on the base repository (`single_ops.sh`, cold client cache, quiet box, one run per cell)

vcs goes through `vcs-server` over loopback HTTP; git clones the bare repository from a local path (no network in its path, so
the clone rows flatter git). `blame` and the real remote sweep were killed by hand (marked). Raw: `data/monorepo-stress/single-ops.tsv`.

| operation (1,000,659 files, 200,000 commits) | vcs wall | vcs user+sys CPU | vcs peak RSS | git wall | git CPU | git peak RSS |
|---|---|---|---|---|---|---|
| clone, whole root zone (7.7 GB tree) | 177.5 s | 190.6 s | 1.6 GiB | 24.3 s | 24.3 s | 0.6 GiB |
| `clone --auto` | 288.3 s | 291.5 s | 1.9 GiB | n/a | | |
| `clone --zone z0` (a zone with 3 files) | 7.7 s | 8.0 s | 0.9 GiB | n/a | | |
| `status`, first | 1.7 s | 12.5 s | 0.4 GiB | 0.65 s | 1.7 s | 0.2 GiB |
| `status`, second | 0.65 s | 1.7 s | 0.3 GiB | 0.64 s | 1.7 s | 0.2 GiB |
| `log -n 50 -- hot/app.txt`, first | 17.6 s | 17.6 s | 0.1 GiB | 0.02 s | 0.01 s | 0.07 GiB |
| `blame hot/app.txt` (10k revisions of a 400 to 2,500 line file) | **1,131 s, killed** | 1,068 s | 0.1 GiB | 3.7 s | 3.7 s | 0.5 GiB |
| `fsck` of the full clone | 181 s | 201 s | **12.6 GiB** | 60.4 s | 60.4 s | 0.8 GiB |
| local `gc` (vcs `--dry-run`; git a real `gc -q` of the clone) | 28.6 s | 139 s | 4.2 GiB | 35.6 s | 54.6 s | 1.5 GiB |
| remote `gc --dry-run` (vcs over HTTP) / `git gc` of the bare repo | 279 s | 109 s | 0.8 GiB | 76.6 s | 95.6 s | 1.4 GiB |
| `fsck` of the bare repository | | | | 59.2 s | 59.2 s | 0.7 GiB |

Remote operations on the 2.55M-object store, as an operator runs them on the server box (`gc_ops.sh`, directory remote):

| step | before the #546 fix | with #546 |
|---|---|---|
| sweep dry run (25 packs, v2) | 157.2 s, 0.8 GiB | 158.5 s, 0.8 GiB |
| `gc --remote --compact` (25 packs into 23, 1.43M objects, 689 MB read, 419 MB written) | 6.5 s, 2.5 GiB | 6.6 s, 2.5 GiB |
| sweep dry run after compaction (v3 blocks) | not finished | **155.9 s**, 1.8 GiB |
| real sweep after compaction (deletes 0, everything is reachable) | **killed at 811 s** | **315.0 s**, 3.4 GiB |

### Concurrent writers (`run_stress.sh`)

Closed-loop CLI clients, 4 s think time (8 s at 256), ramp 60 to 300 s so that the initial clones do not all start together.
Every client is one OS process per command; a quarter of the clients write the same branch (`z0/main`), the rest are
spread over 31 zones. Latencies are per CLI invocation, wall clock. "fail" is the sum of expected rejections (the remote
branch advanced, the client then syncs, merges and retries) and errors; in C and D it also holds the calls that hit a killed server.

| run | writers | manifest | chaos | client activity | server peak RSS | server CPU mean / max | box load1 peak | store growth |
|---|---|---|---|---|---|---|---|---|
| A | 64 | single | none | 739 s | 6.7 GiB | 363% / 567% | 41 | +85.8 GiB (loopback 1.4 TB) |
| B | 128 | sharded | none | 838 s | 13.1 GiB | 465% / 880% | 87 | +75.7 GiB (1.9 TB) |
| C | 256 | single | 8 x `kill -9` server, gc rounds | 1,554 s | 9.6 GiB | 531% / 1,084% | 56 | +83.7 GiB (4.1 TB) |
| D | 256 | sharded | 8 x `kill -9` server, gc rounds | 1,560 s | 10.0 GiB | 561% / 1,211% | 60 | +80.0 GiB (4.5 TB) |

The loopback traffic is dominated by slice clones and lazy metadata fetches, not by pushes (see "What broke", item 7).

p50 / p95 / p99 in ms (ok count, fail count):

| op | A (64, single) | B (128, sharded) | C (256, single, chaos) | D (256, sharded, chaos) |
|---|---|---|---|---|
| add | 10 / 29 / 49 (3,813) | 20 / 51 / 131 (3,486) | 10 / 32 / 65 (5,270) | 10 / 35 / 79 (5,033) |
| commit | 9 / 24 / 37 (3,813) | 16 / 39 / 58 (3,403, 83) | 9 / 26 / 40 (5,270) | 9 / 27 / 43 (5,033) |
| sync | 428 / 737 / 905 (3,823) | 397 / 1,175 / 1,444 (5,013) | 251 / 748 / 1,302 (6,434, 549) | 275 / 773 / 1,527 (6,108, 530) |
| publish | 15 / 433 / 978 (860, 1,212) | 111 / 6,294 / 10,094 (739, 2,669) | 45 / 2,529 / 7,611 (705, 4,213) | 53 / 4,828 / 13,771 (738, 3,886) |
| publish --auto-merge | 178 / 22,418 / 57,103 (366, 168) | 3,326 / 127,820 / 198,800 (326, 357) | 2,202 / 87,089 / 246,866 (324, 1,030) | 2,237 / 79,153 / 148,971 (314, 980) |
| lock cycle: publish | 1,425 / 22,014 / 67,591 (444, 156) | 6,753 / 116,934 / 168,481 (145, 210) | 3,659 / 111,997 / 157,645 (212, 447) | 3,915 / 76,124 / 140,075 (190, 450) |
| merge | 1,736 / 8,762 / 14,371 (879, 903) | 5,528 / 39,286 / 64,561 (2,066, 1,419) | 3,416 / 70,730 / 110,918 (3,138, 1,798) | 6,147 / 66,219 / 107,047 (2,873, 1,744) |
| lock | 8 / 249 / 788 (470, 155) | 25 / 3,730 / 19,357 (182, 423) | 9 / 2,532 / 4,086 (285, 590) | 12 / 2,786 / 4,735 (264, 601) |
| unlock | 7 / 93 / 471 (469, 1) | 18 / 4,363 / 22,590 (156, 26) | 8 / 342 / 3,471 (243, 42) | 10 / 911 / 10,156 (223, 41) |
| clone of a zone slice (first) | 13,557 / 26,220 / 26,794 (64) | 22,209 / 69,675 / 74,786 (128) | 15,764 / 39,875 / 53,572 (150, 530) | 15,395 / 40,110 / 50,112 (148, 540) |
| fresh slice clone | 14,270 / 24,007 / 25,298 (226) | 19,132 / 47,326 / 57,366 (236) | 15,485 / 25,756 / 32,646 (216, 105) | 15,837 / 25,556 / 30,963 (201, 109) |
| big file `add` (128 MiB, 2 MiB changed) | 147 / 212 / 274 (63) | 182 / 451 / 683 (79) | 130 / 234 / 394 (99) | 125 / 211 / 264 (100) |
| big file `commit` | 68 / 160 / 210 (63) | 134 / 251 / 404 (69, 10) | 55 / 138 / 238 (99) | 54 / 129 / 174 (100) |

The contended rows (`publish --auto-merge`, lock-cycle publish, `merge`) are the hot shared branch plus the 16 shared lockable
files: tens of seconds at p95 and minutes at p99 and max. That is the "one hot branch is a retry storm" limit already in the
README, now seen with real CLI clients; disjoint-zone publishes (`publish`) stay at p50 15 to 111 ms and p95 0.4 to 6.3 s. The
sharded manifest did not make the contended rows better (B and D have higher p50 than A and C, with twice the writers in B, the
same in D); the single-manifest and sharded runs are not the same number of writers except C against D, where they are within
noise of each other (publish p50 45 against 53 ms, p95 2.5 against 4.8 s, 705 against 738 acked).

Readers in A to D did almost nothing. In A and B the initial clones of the 1M-file root zone and of `--auto` hit the 300 s per-call
timeout (the clone takes 177 to 288 s on a quiet box and the box was at load 40 to 90); in C and D they were cut by the first
`kill -9` of the server (`Connection refused` after 92 to 116 s; the harness does not retry a failed initial reader clone). Only the
slice reader ran. So run F was added for the reads.

### Readers during writes (run F: 64 writers, readers started 360 s earlier)

`run_stress.sh f64-readers 64 12 --readers 3 --ramp 60 --writer-delay 360`: the three readers clone first (root zone, `--auto`, one
slice), the 64 writers start six minutes later and run for 12; the readers keep working throughout. Writers: 5,338 acked files, none
missing, none wrong, 5 big files ok, `fsck` of the fresh clone ok (data in `data/monorepo-stress/f64-readers/`).

| reader and operation | while writers run | |
|---|---|---|
| `--auto` clone (rd0) | 271 s (quiet box: 288 s) | before the writers started |
| root-zone clone, 1M files (rd1) | 243 s | before the writers started |
| rd1 `status` | first 2.0 s, then 0.0 s (4 more) | |
| rd1 `log -n 50 --oneline -- hot/app.txt` | first 17.8 s, then 0.1 s | the first one builds the commit graph |
| rd0 and rd1 `blame hot/app.txt` | **killed by hand after 1,171 s and 1,090 s** | the same 10k-revision file; git does it in 3.7 s |
| rd1 `sync` | 0.2 s (3 times) | |
| slice reader (rd2): `status`, `log -- path`, `blame`, `sync`, `fsck` | 0.0 to 0.8 s, 35 operations | |
| rd2 fresh slice clone | 5.2 s before, 22.6 s under writer load | |
| rd2 `gc --remote --dry-run` over HTTP | **609.9 s** with the writers active (279 s on the idle box) | a remote gc dry run needs 2,224 packs of metadata here |

The blame of the 1M-file repository's hot file is the one read that cannot be used during a working day. Everything else stayed in
milliseconds to seconds while 64 writers hammered the server, apart from the first clones and the remote gc dry run.

### One hot branch at the protocol level (`run_hot.sh`, `writerbench --mode same`)

64, 128 and 256 protocol-level clients (threads, no CLI process per push) push 30 commits each to the same zone and branch through
`vcs-server` on a copy of the base store, with and without the per-branch ticket queue (`--ticket`, #533), single and sharded
manifest. Each client retries a rejected push with decorrelated jitter; "latency" is per acked push including retries and ticket wait. Every run is
audited by `writerbench`: the commit chain behind the branch grew by exactly the number of acked pushes (12 of 12 runs: ok).
Raw: `data/monorepo-stress/hot/`.

| manifest | tickets | clients | acked | rejected pushes (conflicts) | pushes/s | p50 / p95 / p99 ms | server RSS at end |
|---|---|---|---|---|---|---|---|
| single | no | 64 | 1,920 | 1,188 | 81.8 | 1.4 / 18.9 / 10,089 | 0.2 GiB |
| single | no | 128 | 3,840 | 3,296 | 115.8 | 2.0 / 2.5 / 15,035 | 0.5 GiB |
| single | no | 256 | 7,680 | 9,469 | 157.1 | 2.2 / 2.6 / 22,762 | 0.6 GiB |
| single | **yes** | 64 | 1,920 | **0** | **337.6** | 188 / 194 / **227** | 0.5 GiB |
| single | **yes** | 128 | 3,840 | **0** | **302.6** | 420 / 442 / **493** | 0.6 GiB |
| single | **yes** | 256 | 7,680 | **0** | **250.3** | 1,020 / 1,102 / **1,110** | 0.7 GiB |
| sharded | no | 64 | 1,920 | 1,642 | 64.4 | 2.5 / 63 / 15,691 | 0.3 GiB |
| sharded | no | 128 | 3,840 | 3,527 | 84.9 | 3.2 / 7.2 / 21,959 | 1.1 GiB |
| sharded | no | 256 | 7,680 | 15,191 | 53.9 | 4.9 / 30 / **78,149** | 5.1 GiB |
| sharded | yes | 64 | 1,920 | 0 | 288.9 | 197 / 399 / 439 | 1.3 GiB |
| sharded | yes | 128 | 3,840 | 0 | 212.2 | 518 / 1,041 / 1,192 | 1.9 GiB |
| sharded | yes | 256 | 7,680 | 0 | 218.8 | 1,099 / 1,822 / 2,119 | 2.2 GiB |

- On one hot branch the ticket queue turns a retry storm into a queue: no rejected push, p99 from 10 to 23 s down to 0.2 to 1.1 s,
  and 1.6 to 4.1 times the pushes per second (the no-ticket p50 is small because the winners are fast and the losers are in the tail).
  Throughput with tickets falls from 338 to 250 pushes/s from 64 to 256 clients (the queue is serial; its wait is the latency).
- The sharded manifest does **not** help a single hot branch: without tickets it is slower than the single manifest (54 against
  157 pushes/s and p99 78 s against 23 s at 256 clients, 5.1 GiB server RSS at the end of the 256-client run), with tickets it is 13 to 30% slower. Sharding
  is for disjoint zones; the hot branch needs the ticket queue. (Disjoint zones in the same box: runs A to D above.)
- These are the server capacity numbers. The CLI-client runs above are bounded by the clients' own CPU.

### Integrity (every run)

| run | acked files in a fresh clone of the writers' zones | missing | wrong content | last-wins (`art/uN.txt`) | big files (128 MiB, newest acked version present) | `vcs locks` at the end | fsck of the fresh clone |
|---|---|---|---|---|---|---|---|
| A | 5,791 | 0 | 0 | 61 files, 0 mismatch | 6 files, 0 bad | none | ok |
| B | 5,038 | 0 | 0 | 76 files, 0 mismatch | 9 files, 0 bad | none | ok |
| C | 7,168 | 0 | 0 | 89 files, 0 mismatch | 8 files, 0 bad | none | ok; 3 mid-run audits, 0 bad |
| D | 7,223 | 0 | 0 | 88 files, 0 mismatch | 7 files, 0 bad | none | ok; 3 mid-run audits, 0 bad |
| E (64, gc chaos, old binaries) | 5,738 | 0 | 0 | 62 files, 0 mismatch | 6 files, 0 bad | none | ok; 2 audits, 0 bad |
| F (64, readers first) | 5,338 | 0 | 0 | 60 files, 0 mismatch | 5 files, 0 bad | none | ok |
| G (64, gc chaos, fixed binaries) | 7,091 | 0 | 0 | 61 files, 0 mismatch | 5 files, 0 bad | none | ok; 4 audits, 0 bad |

"Acked" means the publish returned success to the client that then recorded the file and its sha256. The A to C fscks are on the
sliced verification clone (20 to 23 thousand objects); the **full** verification was run once, after D:

- full clone of the whole repository (1M files + big zone): 316.3 s, exit 0; every acked file present and byte-equal;
- `vcs fsck` of that clone: **ok, 2,190,663 objects checked (389,242 not cached locally, refetched on demand), 351 s**;
- `vcs clone` of the server's store directory itself and `vcs fsck` of that: ok, the same 2,190,663 objects;
- `vcs gc --remote --dry-run` of the store: 2,588,786 objects in 1,517 packs, 2,585,729 reachable, 214 young unreachable units, 0 deletable.
- the 20 prebuilt big files, hashed after a clone of zone `big`: **20 of 20 equal in sha256 and size** (65,044,217,856 bytes, version 3), cloned from the store directory of run D after its chaos in 111.7 s (1.6 GiB RSS) and hashed in 23.7 s (`verify_big.sh`; the in-harness version of this check failed on a missing author setting in the first D run and was rerun this way)

### Chaos

C and D: `vcs-server` killed with `kill -9` eight times each, 90 to 300 s apart, with 3 to 58 client processes in flight; restarted
within 0.3 to 1.0 s. The clients saw `Connection refused` on publish, sync, clone and lock, retried, and every acked write was
present afterwards (table above).

**`gc --remote` killed mid-run.** In C, D and in E (64 writers, only gc chaos, no server kill, same binaries) every one of the 20 gc
rounds was refused with `another gc holds gc/lock` (dead holder, "What broke" item 4). Run G repeats E with the binaries that include #547
(`run_stress.sh g64-gcfixed 64 14 --readers 0 --chaos gcmix --gc-first 60 --gc-every 60 --gc-grace 600`; only gc is killed):

| round | what | result |
|---|---|---|
| 1 | `gc --remote --compact` | SIGKILLed after 0.81 s; audit right after: 0 bad |
| 2 | sweep | refused: the lease of the gc killed in round 1 is still on `gc/lock` (holder dead, removed by the harness) |
| 3 | compact | **ran to the end**, 13.1 s, with 64 writers active (1.43M objects read) |
| 4 | sweep | SIGKILLed after 3.95 s (it had announced the destructive sweep); audit right after: 0 bad |
| 5 | compact | refused, the lease of the gc killed in round 4 |
| 6 | sweep | **ran to the end**, 518.6 s (315 s on the idle box), 2,567,645 objects reachable, 0 deleted |

Four audits (two right after the kills; each is a fresh clone of the writers' zones plus fsck): 0 bad. At the end: 7,091 acked files, 0 missing,
0 wrong, 61 last-wins files 0 mismatch, 5 big files ok, `fsck` ok, no locks. So a killed gc did not damage the store, writers continued, and
the next gc ran. The leftover lease of a killed gc (rounds 2 and 5) is the part of item 4 that is not fixed. Run E for comparison (old
binaries): 8 of 8 rounds refused, 5,738 acked files, 0 missing, packs grew to 1,731 without compaction.



## What broke

Found by this run, in the order they were hit. Items 1 to 4 have a fix with a regression test, in PRs #542, #543, #546 and #547.

1. **`import-git` was commits x files** (fixed, #542). It cloned and rebuilt the whole flat file map for every commit. 20,000
   commits on a 100,000-file tree did not finish in 150 s (a 200k-commit, 1M-file import was out of reach). A commit with at most
   2,048 operations now edits its first parent's tree in place (`treeops::set_entry` reads and writes only the directories on the
   changed paths): the same 20k-commit import takes 4.5 s, and 200,000 commits on 1,000,659 files take 76 s, 1.6 GiB RSS. Test:
   in-place edits (modify, delete a file or directory, rename and copy a directory, file to directory and back) give the same root tree id
   as one flat root commit.
2. **A push of more than 1 GiB of new content through `vcs-server` could never succeed** (fixed, #543). A publish built one pack per
   transaction and the server refuses bodies over `max_body` (1 GiB) with 413; the client saw `outcome unknown after 8 retries
   ... Connection reset by peer` (the server log shows `status=413` on `PUT /v1/o/packs/...`). A 4 GiB file is 4 GiB of chunks, so
   the "huge files through the server" path was broken above 1 GiB per transaction; the first publish of the 64.7 GB of binaries hit
   it (`data/monorepo-stress/build.log`, the first `publish_all` row). The publisher now cuts the transaction into packs of at most
   256 MiB and puts the transaction record with the last index. After the fix the same publish (1M files, 200k commits, 65 GB) took 212.8 s.
   Still true: the client holds all new objects of the transaction in memory (14.9 GB RSS for this publish).
3. **Remote GC after compaction was five times slower than before it** (fixed, #546). The reachability walk decoded one v3 block per
   object read (8-block cache, block fetched before the cache lookup). `perf` showed 61% of the time in `ZSTD_decompressSequences`.
   Table above: the real sweep was killed at 811 s, with the fix it takes 315 s and the dry run is the same 156 s as on the
   uncompacted store.
4. **Every client-side auto-compaction left `gc/lock` behind for its 6 h lease** (fixed, #547). `compact_with` took `gc/lock` and then ran
   its `DELETE compact/probe` check. A write-role token on `vcs-server` gets 403 for every `DELETE`, so it could not release the lock either:
   each automatic compaction a client attempted (the server log has one refused `DELETE /v1/o/compact/probe` per attempt, every few minutes)
   left a lock whose holder pid was dead, and every later `gc --remote` / `--compact`, by an operator too, failed with `another gc holds
   gc/lock until ...`. In C and D **all 12 gc rounds** found such a lock, and so did both rounds of the run E below, which has no server
   kill at all (I first blamed the server kills; E showed it was not them). So the gc-kill chaos never actually ran in C, D and E, and in
   A and B the automatic compaction never ran either (packs 25 to 1,969 and 1,656 without a compaction). The probe now comes before the
   lock, and the release also retries for about 8 s on `Unavailable` (a restarted server). Not fixed: a holder that is itself killed, or a server
   down for longer, still leaves the lease; an operator removes `gc/lock`. Run G repeats the chaos with the fixed binaries.
   (The first version of the harness also removed locks held by live admin clients; it now checks the pid.)
5. **Server memory grows with time under load, no plateau seen** (not fixed, not root-caused). `vcs-server` RSS at the first and last
   quartile of the run: A 1.7 to 6.1 GiB (64 writers, 12 min), B 3.3 to 12.8 GiB (128, 12 min, peak 13.1). In C and D the eight restarts
   reset it (peaks 9.6 and 10.0 GiB). It is not packs or the manifest alone (1,500 to 2,000 packs). Could be a cache or allocator
   arena retention with 240 threads; `MALLOC_ARENA_MAX` and a heap profile are the next step.
6. **The client cache lives in `~/.cache/vcs` and is written on every read** (operational, not fixed). On a 3,000-IOPS root volume a
   1M-file `clone` was IO-bound at 1,600 4 KiB reads per second (`strace`: `open(O_WRONLY)` on each cached object after reading it)
   and ran for minutes more; the whole harness sets `XDG_CACHE_HOME` to the fast disk. A CI runner or a laptop with a slow home volume
   will see the same.
7. **A slice clone of a tiny zone pulls the metadata of the whole repository.** `vcs clone --zone z3` (3 files): 143 requests, 531 MB
   downloaded, 0.9 GiB RSS, 7.7 s CPU, 107 MB of `.vcs`. Its commit sits in the one transaction pack that also holds the 200,000-commit
   root history (my base publishes everything in one transaction, which a real repository would not), and metadata read-ahead reads that
   pack's whole commit-and-tree prefix. With 148 to 226 slice clones in a run this made 1.4 to 4.5 TB of loopback traffic. Likely
   half harness artefact, half "read-ahead does not know which zone it serves"; not investigated further.
8. **`blame` and `log -- path` on a long, hot file are far slower than git's** (not fixed). `blame hot/app.txt` (about 10,000
   revisions of a file that grows from 400 to 2,500 lines): 1,131 s killed, against git 3.7 s. `perf`: 28% in `diff::script`, 21% in
   `split_lines`, 6% in `intern`; the line diff is an O(n x m) table on whatever is left after trimming the common prefix and suffix, which
   for edits at both ends of a 2,500-line file is millions of cells per revision. A Myers or patience diff, or splitting at unique
   lines, would remove it (not tried). `log -n 50 -- hot/app.txt` takes 17.6 s the first time (it builds the commit graph for 200,000 commits, #436
   item 3) against git 0.02 s.
9. **`fsck` memory at 200k commits and 1M files is 12.6 GiB** (git: 0.8 GiB), 181 s (git 60 s). Local `gc --dry-run` 4.2 GiB. This is the
   "gc/fsck memory" known limit, now measured at 200k commits; the 1M-commit point is below.
10. **Sporadic `untracked path ... would be overwritten` in `merge` / `publish --auto-merge` of slice clones** (observed, not root-caused):
    1 in A, 19 in B, 73 in C, the path being a file another writer of the same zone published. The first failing call in the traces
    is a merge that fetched missing objects and then failed; later calls refuse because the file is now in the work tree but not in the
    index. Integrity was not affected (no acked write was lost), but a failed `merge` that leaves
    materialised files behind looks like a real bug. Needs a reproducer with `RUST_LOG`-level detail.
11. **Harness problems found and fixed** (they would otherwise have produced false alarms): the stale-lock cleanup removed locks of
    live processes; fresh-clone and verification steps cloned the whole repository (65 GB, 1M files) per call; branch commands ran
    against the root zone, which a slice clone does not have; an undone commit left an unstaged `log.txt` edit that made the next
    `merge` refuse (about 1,400 errors per run in B, C, D); reader clones timed out at 300 s; a first attempt at run D reused a server
    orphaned by an earlier aborted attempt (that run was discarded and redone). The `git` rows of the first `single_ops` pass were
    discarded because the bare repository's HEAD pointed at `master`.


## Limits tried

`scripts/stress/limits.sh`, run on the same box after the soak runs (one run per cell, `data/monorepo-stress/limits.tsv`).

| limit (docs/known-limits.md) | what was run | result |
|---|---|---|
| gc / fsck memory at 1M commits | 1,000,000 commits on a 20,000-file tree (`histgen.py --commits 1000000 --dirs 20 --subdirs 20 --files 50`) | `import-git` **479 s, 3.3 GiB**; `fsck` **208 s, 35.1 GiB peak RSS** (the box has 61 GiB); local `gc --dry-run` 54.5 s, 8.3 GiB; `log` of all commits 3.8 s, 0.8 GiB; first `log -n 5 -- hot/app.txt` 0.25 s (the import writes the commit graph). Not moved: fsck memory grows with the number of commits (12.6 GiB at 200k commits on the 1M-file tree, 35.1 GiB at 1M commits on a small tree); a 64 GiB box is the limit of what was tried. |
| first `status` on 10M files | 10,000,000 files (nested layout, 16 generator processes, 70 s) | `add` **109 s, 6.6 GiB**; `commit` 6.4 s, 2.7 GiB; `status` right after the commit **6.1 s** (6.4 s user + 32.1 s sys CPU, 2.8 GiB), the next `status` 0.03 s, 27 MiB; store 8.8 GiB. This is **not** the cold first `status` of the known limit (which is the first one after an import or a fresh clone, 22 s for 300,000 files in 300 directories): the index was warm from the `add`. A cold first status on 10M files was not measured. |
| delta-layout soak `server_alive_at_end` false (#293, #436) | not run | needs the delta layout, which this run did not enable. All four runs of this document ended with `server_alive_at_end: true` (default layout) |
| remote GC concurrent with pushes at scale | `gc --remote` every 4 minutes during C and D | blocked by the leaked `gc/lock` in all 12 rounds (see "What broke" item 4); the dedicated run E below repeats it with only gc killed |

Reached after the fixes: 200,000 commits x 1,000,659 files imports in 76 s (it did not finish before the fix), 1,000,000 commits imports
in 8 minutes. Not reached: 1M commits x 1M files, 10M files with history.


## Comparison with git

Same box, same bytes, one run per cell (`single_ops.sh`, `git_ops.sh`, `bigfile_cmp.sh`; in the big-file rows the second vcs pass is in parentheses). git runs against local paths
(`clone` of a bare repository on the same disk, `push` to a bare repository), vcs through `vcs-server` over loopback HTTP; where that
favours git it is said. **No git-lfs row: it needs an LFS server and was not set up** (the earlier single-file git-lfs numbers
are in [large-files-bench](large-files-bench.md)). git was not run on the 64.7 GB, 20-file set (at 126 s per 4 GiB file it would take
about 33 minutes for `add` alone) nor with concurrent writers (git has no equivalent of the server here; the earlier
[highload-matrix](highload-matrix.md) has the many-writer comparison).

| operation | vcs | git | |
|---|---|---|---|
| import the 200,000-commit, 1M-file stream | 76.1 s, 1.6 GiB (`import-git`, after #542) | 95.1 s, 0.45 GiB (`git fast-import`) | vcs wins on time, git on memory |
| `add` of one 4 GiB binary | **3.0 s** (second pass 4.0 s), 0.2 GiB | 126.3 s, 4.7 MiB | vcs 32 to 42x faster (chunking, parallel hash) |
| publish / `push` of that file (v1) | **4.7 s** (6.3 s), 0.5 GiB | 126.3 s, 4.0 GiB | vcs 20 to 27x faster, git needs the file in memory |
| `add` after 3 MiB overwritten + 1 MiB inserted | **0.67 s** (0.70 s) | 125.4 s | git re-reads the whole file |
| push of that change | **0.03 s** (0.04 s) | 126.3 s | vcs sends the chunks of the 4 MiB that changed, git repacks the whole file |
| `clone` of the 2-version repository | **5.5 s** (5.4 s) | 227.3 s (`file://`) | |
| remote size, two versions | **4.0 GiB** | 8.0 GiB | git stores both versions whole |
| clone result equals the source (sha256) | yes (second pass) | not checked | the first pass compared two different random mutations, so its `False` is void; only the vcs rows were rerun with the fixed check |
| first `status`, 1M files | 1.7 s (12.5 s CPU) | 0.65 s | git wins |
| second `status` | 0.65 s | 0.64 s | tie |
| `log -n 50 -- path`, first | 17.6 s (builds the commit graph) | 0.02 s | git wins by 800x |
| `blame` of a 10k-revision file | 1,131 s, killed | 3.7 s | git wins by 300x or more |
| `fsck` of the full clone | 181 s, 12.6 GiB | 60 s, 0.8 GiB | git wins |
| local `gc` | 28.6 s (dry run), 4.2 GiB | 35.6 s (real), 1.5 GiB | not like for like |
| `clone` of the whole tree | 177 s over HTTP, 1.6 GiB | 24 s from a local path, 0.6 GiB | git wins; not like for like (no network in git's path); an earlier local-path run of vcs on the same 1M files took 40.5 s ([huge-scale](huge-scale.md)) |


## Reproduce and cost

Everything ran on one `c6id.8xlarge` (us-east-1, on demand US$1.6128/h, checked with the pricing API) tagged `allgit-bench`:
launched 02:20 UTC, terminated 08:42 UTC, 6 h 22 min = **US$10.3** plus about US$0.07 of gp3 root volume; no data transfer, no other
resource. The instance, its security group and its volume are deleted. The hard cap was US$30.

To reproduce (about 7 hours and the same US$10 with the PRs merged or with the four branches applied):

1. Launch the instance, `mkfs.ext4 -O ^has_journal` the local NVMe, mount it at `/data`, install `build-essential git git-lfs sysstat rsync bc` and
   rust, build `-p vcs-cli -p vcs-server -p vcs-bench` in release mode.
2. `scripts/stress/build_base.sh /data/run/base` (15 minutes, 130 GB on disk including the seed working tree), then
   `ONLY_PUBLISH=1` if a publish has to be repeated.
3. The commands under "Commands"; each soak run needs a 65 GB copy of the base store (about 90 s) and leaves about 72 GB behind.
   `XDG_CACHE_HOME` must point at the fast disk (`run_stress.sh` and `single_ops.sh` do it).

Files: `scripts/stress/{build_base.sh,bigfiles.py,single_ops.sh,git_ops.sh,gc_ops.sh,run_stress.sh,run_hot.sh,limits.sh,
bigfile_cmp.sh,verify_big.sh,summarize.py}`, `scripts/soak/soak.py` (extended), `scripts/histgen.py --lines`,
`scripts/huge_gen_tree.py` (slice arguments), `crates/vcs-bench/src/bin/writerbench.rs` (chain audit for `--mode same`). Data:
`docs/research/data/monorepo-stress/` (per run: `verify.json`, `report.json`, `samples.csv`, `chaos.log`, loopback counters).

