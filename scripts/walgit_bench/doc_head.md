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

