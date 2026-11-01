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

Where vcs is worse in these measurements: full clone and fetch are several times slower than git/walgit and read far more from the store (fetch of 1 commit downloads MiB where git/walgit move KiB); the client store is up to 8x larger (loose objects). The cause of the fetch volume was not diagnosed here (HYPOTHESIS: it re-reads per-zone history/manifest data; unverified). Also the vcs initial push uploads loose objects while git uploads one pack; at flat 10k the vcs initial push is faster than git's because git has to pack 10k loose objects first.

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

## Caveats

* Shared, loaded machine (load1 17 to 65 across runs). A first pass over the 10k datasets ran with git's default `gc.auto`; it let git auto-pack the source repository after the initial commit, which made git/walgit initial pushes look faster (a packed source) than a freshly committed tree. That pass is kept as `data/walgit-bench/gc-default-10k/` and is NOT used in the tables; the tables use `gc.auto=0` (loose-object source, as after a normal commit of a big tree).
* "remote store" is `du` of the RustFS data dir (allocated blocks, includes RustFS metadata per object), divided by the number of identical repos; compare with the payload bytes the proxy counted.
* The proxy pass is a single run per operation; counts are deterministic here but walgit's background maintainer adds a few requests of its own.
* One RustFS node on local disk: request latency is microseconds-to-milliseconds, so the request counts are the portable result and the wall times are not predictive of S3.
* The plain git reference uses `file://` and so has no network or HTTP cost at all; it is a lower bound, not a like-for-like server.
* Only 100k-file datasets with fewer reps (3) and the `deep` layout's client-side checkout cost dominate clone time for every tool.

## Not run

* Cloud S3 / GCS (no cloud resources allowed).
* TLS, authenticated modes, concurrent clients, multiple walgit instances, large-blob/LFS, packfile-URI delivery for walgit.
* walgit's remote-reader mode (repo larger than cache budget).
* Local-workflow timings of walgit: there are none (stock git); see benchmarks.md.
* Reproduce: `python3 scripts/walgit_bench/walgit_bench.py flat 10000 5 out.json --init-reps 5` on a Linux box with RustFS, a built walgit-server and vcs release binaries (paths at the top of the script), then `python3 scripts/walgit_bench/report.py out.json`.
