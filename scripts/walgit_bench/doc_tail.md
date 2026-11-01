
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
