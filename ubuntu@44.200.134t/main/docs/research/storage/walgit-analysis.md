# walgit as a storage reference for allgit (research #179, part 2)

Status: research, branch `research/storage-engine`, epic #190. Written 2026-10-31.
Companion: [git-analysis.md](git-analysis.md). Vocabulary: allgit's unit of independent history and sparse sync is the **zone**.

## 0. Source, revision, method

| Item | Value |
|---|---|
| Repository | https://github.com/tobi/walgit (MIT) |
| Revision read, built and tested | `80e9a20b29e29aefd16a4dae6f8e274cce85cca5`, "Merge pull request #58 from tobi/packfiles/07-seal-evidence", 2026-09-12 19:35 -0400 |
| Accessed | 2026-10-31 (cloned to `lambda-final:~/ref/walgit`) |
| Size | 73,465 lines of Rust, Markdown and TS across `crates/walgit-{proto,store,git,wal,server,config,cli}` plus `web/` |
| Read | `GOAL.md`, `README.md`, `AGENTS.md` (the decision log D1 to D49), `docs/ROUNDTRIPS.md`, `docs/PACKFILE_URI_DESIGN.md`, `docs/PACKFILE_MIGRATION.md` (skimmed), `docs/INTEGRITY.md`, `docs/LFS.md`, `docs/spec/README.md`, `docs/CONTRACT.md`; source: `walgit-store/src/{lib,s3,fault}.rs`, `walgit-wal/src/{publish,sync,remote,index_cache,checkpoint}.rs`, `walgit-git/src/{lib,pack_segments,midx}.rs`, `walgit-server/src/pack_lifecycle.rs`, `walgit-proto/proto/walgit/v1/wal.proto`, `walgit.example.toml`. Not read in depth: `walgit-server` auth/web/UI, `walgit-config`, `classification.rs`, `closure.rs`, `upload_gix.rs` beyond headers, the TLA+ models. |

Evidence labels: **CONFIRMED** = read in the cited file at the revision above, or measured by us (data under `data/`); **WALGIT-REPORTED** = a number or claim written in walgit's docs or comments, which we did not reproduce; **HYPOTHESIS** = our inference.

**Statements about absence** are worded as "not found by search at this revision" and name the search. We did not read all 73k lines, so a search miss is weaker than a proof of absence. In particular walgit uses upstream `git` for packing, so every Git feature in [git-analysis.md](git-analysis.md) that `git` itself provides is available to walgit by configuration, whether or not walgit's source mentions it.

## 1. What walgit is, in one paragraph

walgit is a Git server whose only durable state is an object-store bucket. It does not invent an object format ("weird stuff happens around git, never inside it", `GOAL.md`): the stored objects are ordinary Git packs plus their sidecar files. What it adds is a **commit protocol** (a write-ahead log and one CAS-rewritten manifest), a **serving model** for repositories larger than the host (indexes local, pack bytes by range read), and a **pack lifecycle** (classification, geometric folds, history/blob segmentation, group coverage proofs) driven by a lease-holding maintainer. It follows the design Cursor described in "Git at any scale" (kept verbatim in `docs/reference/cursor-git-at-any-scale.md`). Its target is a monorepo of 57 GiB, 73 M objects, 1.4 M commits and 466 k refs served from hosts with about 20 GiB of tmpfs (`AGENTS.md` section 0 and 1.1).

The consequence for allgit: walgit is a strong reference for the **commit protocol, round-trip discipline, and failure testing**, and a reference for **how far you can get by keeping Git's physical format**. It is not a reference for a new physical format, and it has no zones: its closest analogue is the pack "group" (code or meta) which partitions by ref reachability, not by path.

## 2. Build and test record (CONFIRMED, our runs)

Machine: `lambda-final`, AMD EPYC 7J13, 30 cores, kernel `6.8.0-1046-nvidia`. Toolchain: the repo pins Rust `1.97.1` (`rust-toolchain.toml`); rustup fetched it; `rustc 1.97.1 (8bab26f4f 2026-07-14)`. System git on the box is **2.34.1**; walgit's docs recommend git 2.47 or later (`pack.writeReverseIndex`) and its installer requires 2.46 or later. The box load average was 23 to 31 while some of this ran (other jobs of this study).

| Step | Result |
|---|---|
| `cargo build --release --workspace` (first attempt) | **Failed**: `prost-build` could not find `protoc` ("Could not find `protoc`"). Not a walgit defect; a missing system dependency. |
| Same, with `PROTOC` pointing to protoc 28.3 downloaded from the protobuf GitHub release (no root needed) | **Succeeded**: `Finished release profile [optimized + debuginfo] in 2m 59s`; target dir 8.9 GB. The web SPA was not built (needs node and pnpm); the build script writes a placeholder `index.html`. |
| `cargo test -p walgit-store --test contract -- memory_contract` | 1 passed |
| `cargo test -p walgit-wal` | all passed (8, 5, 1, 1, 28 tests across targets) |
| `cargo test -p walgit-git --release` | all passed; 3 tests ignored by design (benches) |
| `cargo test -p walgit-server --test sim --release` (fault-injection simulation, includes the pinned request budgets) | 19 passed, 0 failed |
| Server integration tests (`e2e`, `packfile_uri`, `maintain`, `static_http`) | 48 passed, 9 failed on git 2.34.1 without the SPA; see section 2.1 for why this is inconclusive |
| S3/rustfs and GCS contract suites | **Not run**. They need a rustfs container (`just dev-store`) or a real bucket; no cloud resources were used. Documented operator command: `just dev-store && just test-s3`. |
| Web UI build and TLA+ model checking (`just spec`) | **Not run** (node/pnpm, Java 11 and the TLC jar not installed on the box). |

Logs on the box: `~/ref/walgit-build.log`, `~/ref/walgit-build2.log`, `~/ref/walgit-tests.log`, `~/ref/walgit-tests2.log`, `~/ref/walgit-bench.log`.

### 2.1 Server integration tests

Run with the box's git 2.34.1 and no built web SPA. Result per target (`~/ref/walgit-tests2.log`, `~/ref/walgit-tests3.log`):

| Target | Passed | Failed | Failing tests |
|---|---|---|---|
| `e2e` | 34 | 4 (1 ignored) | `fetch_is_narrated_over_sideband`, `fetch_from_front_that_serves_the_base_remotely`, `stale_cached_credential_is_erased_by_the_401_and_replaced_on_the_next_command`, `max_wants_refuses_the_blobless_checkout_storm_with_the_fix` |
| `packfile_uri` | 3 | 2 | `blobless_uri_clone_and_dynamic_checkout_work_in_both_object_formats`, `stock_git_downloads_static_baseline_and_native_residual_while_gix_stays_dynamic` |
| `maintain` | 7 | 2 | `manual_base_rebuild_on_an_ssd_maintainer`, `classification_preserves_push_bytes_and_shared_group_cuts_settle` |
| `static_http` | 4 | 1 | `ui_assets_etag_304_and_precompressed` (panics with "import map": the SPA was not built, expected) |

**These failures are not evidence about walgit's design.** The server logs for the pack-related failures show `git upload-pack` exiting 128 with `unable to access '(null)': Bad address ... git-pack-objects died` (`maintain`, `packfile_uri`, `e2e`), and one test needs git credential `authtype` (git 2.46 or later). The box has git 2.34.1 and walgit documents 2.46 and 2.47 as minimums. We did not diagnose the exact cause, and we did not confirm that a newer git fixes them: installing one was out of reach (building Git from the cloned source was declined by the sandbox permission check, and no newer packaged git exists on the box). `AGENTS.md` section 5 also lists `fetch_from_front_that_serves_the_base_remotely` as known flaky (about 1 in 3 under the full e2e suite), which may apply to that one. Treat these 9 failures as "environment not meeting documented prerequisites, cause undiagnosed", not as a walgit regression. Tests that exercise the storage and publish path without needing a modern git (store contract, WAL crate, simulation suite, git crate) all passed.

### 2.2 walgit's own benchmarks that exist in the tree, and what we ran

walgit ships five `#[ignore]` benches or stress tests (`justfile`, `test-slow`). The ones we ran use the local filesystem only; none of them measures latency to an object store.

| Bench | Ran? | Result (CONFIRMED, 3 trials where noted, raw in `data/walgit-bench-lambda.json`) |
|---|---|---|
| `refs500k` (per-push ref bookkeeping at 500k refs) | yes, 3 runs | cold parse of 500,000 refs 416 / 555 / 429 ms; warm shared view 7 to 10 microseconds; `apply_ref_txn` for a new ref 6 to 8 ms for the first push; deleting a packed ref 356 to 751 ms. Spread is large on a loaded box; one of three runs showed a 386 ms apply that the others did not. |
| `upload_pack::bench_fetch_engines` (needs `WALGIT_BENCH_REPO`) | yes, 3 runs, on a bare clone of git.git (422,883 objects, one pack from `repack -adf`, bitmap and commit-graph present) | **diff fetch** (want HEAD, have HEAD~50): stock `git` median 199 ms, 845 objects, about 263 KB; `gix` engine median 966 ms, 2,059 objects, **11.3 MB** (43x the bytes). **Full clone**: `git` median 5.4 s, 130.0 MB; `gix` median 2.9 s (range 2.0 to 3.2 s), 136.4 MB. |
| `upload_gix_scale` 300k-object variant | yes, 1 run | passed in 132.7 s; peak RSS of the process tree 672 MB (the bound the test asserts is a small multiple of pack bytes). |
| 20k-ref mirror push (`e2e.rs`, about 70 s per its annotation) and `ls_refs` large synthetic | not run | time; neither measures storage layout. |

Reading: the first row is about refs, not objects. The second row is the useful one for storage: walgit chose the gix engine only for "remote-served" bases because, per its own comment (`AGENTS.md` D2), gix never emits thin packs, which costs bytes on incremental fetches; our measurement reproduces that cost (43x bytes on a 50-commit catch-up) and shows gix is about 2x faster on a full clone of an already-packed repo with a bitmap present. These are CPU-bound local numbers; they say nothing about latency to a bucket.

## 3. Architecture in the bucket (CONFIRMED from `AGENTS.md` 2.1 and `wal.proto`)

All keys live under `repos/<owner>/<repo>/`. Repositories do not share a keyspace, so **there is no cross-repository deduplication** (a consequence of the key layout; LFS objects are also under the repo prefix).

| Object | Mutability | Role |
|---|---|---|
| `manifest.pb` | CAS-rewritten (`If-Match` on S3, generation match on GCS) | head sequence, live `PackRef[]` (checksum, sizes, tier, kind, side-file flags, group memberships), log segment list, checkpoint pointer, inline repo settings, `revision`, and `retired_packs[]` |
| `log/<first_seq>.pb` | immutable (create-only) | framed `LogEntry` records: PUSH, REF_UPDATE, COMPACT (new pack plus `supersedes[]`), CHECKPOINT, SETTINGS |
| `wal/<pack-checksum>.{pack,idx,rev,bitmap,commit-graph}` | immutable, content-addressed by the pack's trailing checksum | the data |
| `checkpoints/<seq>/...`, `checkpoints/refs/<sha256>.pb` | immutable, content-addressed ref snapshots | cold start is snapshot plus tail |
| `leases/<name>.pb` | CAS with TTL | the only cross-instance mutex (compaction) |
| `lfs/objects/<aa>/<bb>/<sha256>` | immutable | LFS bytes |
| `fsck.pb`, `policy.json`, `events/cursor.json`, `cache/api/v1/*.json` | overwritten | audit result, push policy, event cursor, shared render cache |

Data format is stock Git: `git index-pack --stdin --fix-thin --keep --rev-index --threads=0` on ingest (optionally `--fsck-objects`), `git repack`, `git pack-objects`, `git multi-pack-index`, `git commit-graph`.

## 4. Techniques

Each entry: what it does, cost, applicability to allgit, evidence.

### W1. `ObjectStore` trait with CAS version tokens

- What: `get` with `if_none_match`, `if_match` and range; `head`; `put` with `PutMode::{Overwrite, Create, Update(Version)}` and an `immutable` hint; conditional `delete`; `list` and delimited `list_prefixes`; optional `signed_get_url`, `accel_target`, and `compose` (native on GCS, multipart `UploadPartCopy` on S3). `Version` is opaque (GCS generation, S3 ETag, counter in memory). Backends: S3, GCS, memory, plus a `FaultStore` wrapper that injects failures and counts requests (`walgit-store/src/lib.rs:199`, `fault.rs`).
- Cost and caveats (CONFIRMED in code comments, `s3.rs`): S3 has no conditional delete, so it is emulated as HEAD, compare, DELETE with a documented race (acceptable to walgit because deletes are lease-guarded). Multipart upload cannot carry `If-None-Match`, so large immutable packs use a single-shot PUT with `Create` and "accept the (tiny) risk of concurrent create races" (`s3.rs` header). Thresholds: multipart above 64 MiB with 32 MiB parts (`walgit.example.toml`); pack PUTs are striped above 256 MiB with 8 stripes (`publish.rs:131`).
- Applicability to allgit: allgit's `Backend` has `get, get_range, exists, put_if_absent, put_if_match, list`; it is nearly the same surface. Gaps relative to walgit: conditional GET (`NotModified`, the cheap freshness probe), delimited prefix listing, a request-counting fault wrapper, and the large-object caveats above (the create-only race for large objects is a real property of S3 that allgit's `put_if_absent` for packs will meet at multipart sizes).
- Evidence: CONFIRMED (source read).

### W2. Manifest CAS as the single commit point

- What: the manifest is a small protobuf; nothing is visible before its CAS and everything after is idempotent and replayable. It holds the full live pack set inline plus settings, so one conditional GET tells a reader what to fetch.
- Cost: one serialized overwrite per second per object is walgit's stated cap on a CAS-ed object (`docs/ROUNDTRIPS.md` table; WALGIT-REPORTED, GCS measurements). Group commit and a single-writer broker lift the practical rate. The manifest grows with the number of live packs and with `retired_packs[]`, which "do not expire and are not count-capped" (`PACKFILE_URI_DESIGN.md` section 3; `wal.proto` `RetiredPack`). HYPOTHESIS: manifest size, hence the cost of every freshness refresh and every CAS, grows without bound with compaction churn unless a collector is added; walgit states it has no bucket collector in this port (`docs/spec/README.md`).
- Applicability to allgit: this is the closest match to allgit's design (single manifest CAS, model-checked). walgit adds: retaining the retired set, an inline settings document, and a content-addressed ref snapshot referenced from the manifest. Zones add an axis walgit lacks: a multi-zone atomic update must not force every zone writer through one object. That is #186; walgit offers no sharded alternative (the "disjoint writer sets" in D28 shard by repository, not within a repository).
- Evidence: CONFIRMED (docs, `wal.proto`); rate cap WALGIT-REPORTED.

### W3. Write-ahead log with group commit and orphan handling

- What (`publish.rs` header, `process_batch`): one single-flight publisher per repository handle collects requests for `wal.batch_window` (default 5 ms, up to `max_batch` 64), syncs, verifies each ref transaction's old values, writes one immutable log segment with `PutMode::Create`, then CASes the manifest. On a 412 at the log slot it distinguishes a committed claim from an orphan (a writer that crashed between log PUT and manifest CAS); after a grace of `ORPHAN_GRACE_PROBES = 3` steps of 100 ms the sequence is "burned" and the segment goes to the next free sequence (the log is strictly increasing, not dense).
- Cost: see W4 for depth. Orphan handling puts extra probes only on the failure path.
- Applicability: allgit's transaction record is a content-addressed object, and publication is a manifest CAS. walgit's log is a second, ordered record. For allgit the Transaction object already plays the log role; the useful import is the failure taxonomy (orphan, burned sequence, lost response).
- Evidence: CONFIRMED (source header and constants).

### W4. Round trips as the cost model

- What: `docs/ROUNDTRIPS.md` is a rule set plus a budget table: conditional GET returning 304 about 15 to 18 ms, small GET or PUT 60 to 80 ms, HEAD about GET, 404 free (probe, do not list), LIST slow and never on a hot path, CAS overwrite about 1 per second, range read about 100 MB/s per connection, compose one request. Happy-path budgets: any read 1 conditional GET (0 within `freshness_ttl`); cold refs 1 manifest GET then 1 parallel round; **push: 1 freshness GET, then `pack PUT || idx PUT || log PUT` in one round, then the manifest CAS: 5 requests, 4 sequential rounds** (already-synced publish 4); checkpoint 4 requests; lease acquire 2. Rules: depth before count, let the conditional write be the read, verification goes on the failure path, carry state in the object you already fetch, jitter retries on shared CAS objects, never LIST on a hot path.
- Cost/evidence: the latencies are WALGIT-REPORTED GCS numbers (region and date not stated in the doc). The request counts are **CONFIRMED by test**: `healthy_request_round_trip_budgets` in `sim.rs` pins push 5, warm refs 1, cold refs with one tail segment 2, checkpoint 4 against a counting `FaultStore`, and that test passed in our run of the simulation suite (19/19).
- Applicability to allgit: directly adoptable as a method: write the budget table for allgit's publish, sparse sync and lazy fetch paths and assert it in a test with a counting backend. The numbers are GCS, not S3; allgit's cost model (#181) must use its own measured or documented S3 and MinIO figures.
- Evidence: CONFIRMED (doc and passing test); latencies WALGIT-REPORTED.

### W5. Lost-response resolution by nonce

- What: a CAS may land but its response be lost. Each log claim carries a fresh reserved `walgit.publication_nonce`; resolution re-reads the manifest and, only if the claimed segment is listed, compares the log bytes (`cas_landed`, `publish.rs:336`; D47). Unresolvable cases return a typed "commit unknown" rather than retrying a push.
- Applicability: allgit's Transaction id is content-addressed, so "did my transaction land" is answerable by id presence in the manifest history, with no nonce, provided two identical transactions cannot be confused. This is worth a test in #186, not a copy.
- Evidence: CONFIRMED (docs, function located).

### W6. Freshness by conditional GET

- What: every read does a conditional GET of the manifest (`If-None-Match`); 304 serves local state; 200 applies new entries; `wal.freshness_ttl` defaults to 0 (always revalidate). "Push acknowledged implies the next request anywhere sees it."
- Cost: 1 request, 15 to 18 ms (WALGIT-REPORTED) per read on GCS.
- Applicability: allgit sync could use the same probe if its backends return validators; it needs the conditional GET in `Backend`, which is missing today (W1).
- Evidence: CONFIRMED (docs, config).

### W7. Checkpoints (folded state)

- What: a checkpoint is an immutable, content-addressed ref snapshot (`checkpoints/refs/<sha256>.pb`) plus pack inventory at a sequence number; the manifest points at it. Triggers: 256 entries, 1 hour, or 8 MiB of log tail (`snapshot_every_entries`, `checkpoint_interval`, `checkpoint_tail_bytes`; `checkpoint_due`). Writing one is refs-level work: freshness GET, then refs PUT in parallel with checkpoint PUT, then manifest CAS (4 requests).
- Applicability: allgit has a manifest with branches and could shard or snapshot likewise; content-addressing the snapshot makes proofs bind to an exact state (walgit uses that for coverage certificates, W19).
- Evidence: CONFIRMED (source, docs).

### W8. Leases for maintenance

- What: `leases/<name>.pb` with TTL, acquired by `Create` or `Update` over an expired lease (2 requests), heartbeat extends, drop releases best-effort. Only the compaction lease exists as a cross-instance mutex.
- Applicability: allgit's GC and compaction need an exclusion or an idempotent protocol; walgit chose a lease plus a final CAS that rechecks live membership (W16). The S3 conditional-delete emulation (W1) means a lease release can race; walgit accepts that because TTL expiry covers it.
- Evidence: CONFIRMED (`CONTRACT.md`, `coord.rs` located; semantic detail from docs).

### W9. Ingest-time verification before publish

- What: receive-pack indexes the pack into a per-ingest scratch git dir (so a rejected push leaves nothing), checks connectivity (`check_connectivity` default true) and optionally object-level fsck (`fsck_objects` true) and policy, then uploads. "Never ACK before the bucket ACKs."
- Applicability: allgit's equivalent is verifying hashes and referenced-object presence before the manifest CAS; the scratch-directory-then-publish pattern keeps a failed upload from leaving partial state, which is also what content addressing gives for free.
- Evidence: CONFIRMED (docs, config defaults).

### W10. Immutable, content-addressed side files published with each pack

- What: `.pack`, `.idx`, `.rev`, `.bitmap`, and for bases a split `.commit-graph` layer are separate immutable objects keyed by the pack checksum. Every pack writer produces `.rev`; a pack of 250k or more objects without one gets it from the maintainer. WALGIT-REPORTED rationale: without `.rev` Git rebuilds the reverse index in memory on each `pack-objects` (2.85 s per fetch on a 60 M-object repository).
- Cost: extra objects per pack. WALGIT-REPORTED: the idx for a large repository was 2.1 GB and took 217 s to download at 8 stripes (`sync.rs` comment), consistent with about 28 bytes per object if the index covers the 73 M objects of the reference repository [unverified link between the two figures]; our own measurement on git.git gives 28 bytes per object (git-analysis G6).
- Applicability: allgit's pack has one idx and no `.rev`/bitmap; its object count per zone is likely smaller than a monorepo. The lesson is structural: a reader's cold cost is dominated by side files, not pack bytes, once packs are large.
- Evidence: CONFIRMED (source), numbers WALGIT-REPORTED.

### W11. Sync levels

- What (`handle.rs`, `sync.rs`): Refs (snapshot plus log tail, no packs), Serve (refs plus the packs this host can hold: small tiers and the history pack local; a large base linked from a read-only bucket mount or remote-served), Full (every live pack local, striped parallel range downloads), Objects (Serve if the set fits `cache.max_bytes`, else Refs plus the remote reader). `check_fits` refuses to pull a set that does not fit. Cache has a budget mode (LRU, tmpfs) and a disk mode (watermark eviction).
- Cost: depends on mode; bulk downloads run on a separate runtime and, on GCS, separate gRPC channels (D19).
- Applicability: the four levels are an existing taxonomy for allgit's "sparse workspace": refs-only, serve, full, and on-demand objects. allgit's zones add a fifth dimension (which zones). The explicit refusal when a set does not fit is a feature allgit should have.
- Evidence: CONFIRMED (docs, config; sync code partly read).

### W12. Remote reader: indexes local, data by range

- What (`walgit-wal/src/remote.rs`): idx files downloaded locally (comment: "about 6% of the pack"); pack data read in **1 MiB blocks** through a process-wide, byte-weighted LRU (`moka`, single-flight per block, default `cache.remote_block_bytes` 1 GiB) with incremental inflate; delta chains resolved iteratively with a **decoded-object LRU** (default 256 MiB) keyed by pack offset so bases are shared across chains; chain depth guard of 4096; metrics for range reads, bytes and chain length. The web API "faults exactly the objects a git command will touch into the loose store". The contract test also runs with a 1-byte cache.
- Cost: each cold object costs at least one 1 MiB block GET, plus more blocks per delta base outside the block; the reported idx fraction is 6% (comment, WALGIT-REPORTED) against our measured 6.9 to 11.0% on git.git depending on pack tightness (git-analysis G6): the same order, the walgit figure being for its own packs.
- Applicability: this is the most directly relevant module for allgit's sparse reads. Findings to carry: 1 MiB is a block-size choice that allgit's #183 block experiments should sweep (it is not shown to be optimal); a decoded-object cache above the block cache matters only when deltas exist; the same code path serves both read-through and "fault into local store".
- Evidence: CONFIRMED (source read: constants, structure). Hit-rate or latency figures: none published by walgit that we found.

### W13. Validated remote index cache

- What (`index_cache.rs`, D48): downloaded idx files are verified (idx checksum, pack identity, known size and count) before admission; a cross-process kernel file lock protects names while opened or replaced; mmap handles outlive later unlinks; temp names are per attempt; at most four concurrent index downloads per repo; corruption triggers repair rather than admission.
- Applicability: allgit's idx is loaded fully into memory and hash-checked on open; the cross-process lock discipline matters if several allgit processes share a cache directory.
- Evidence: CONFIRMED (header and docs).

### W14. Striped transfers

- What: download uses 32 MiB chunks, 16 in flight (`sync.rs:376`), because "one gRPC stream tops out around 10 to 20 MB/s from a serverless host" (WALGIT-REPORTED, GCS); pack, idx and side files are fetched in one parallel round; uploads over 256 MiB are striped 8-wide.
- Applicability: bulk-transfer concurrency is a first-order lever on object stores. Stripe size and width for S3 and MinIO must be measured for allgit; the GCS figures do not transfer.
- Evidence: CONFIRMED constants; the per-stream throughput claim is WALGIT-REPORTED.

### W15. History pack and history/blob segments

- What: a tier-2 base is published with a derived `git pack-objects --filter=blob:none` pack (commits and trees, `PackKind::HISTORY`, `derived_from` base) kept as a real local pack so only blob bytes need range reads (D18). In the newer lifecycle, History segments (commits, trees, tags) and Blob segments are separate authoritative packs; "each segment is independently indexable, with all delta bases inside the pack" and graph dependencies may cross segments (`PACKFILE_URI_DESIGN.md` section 4).
- Cost: derived data is stored twice in the old form (history pack duplicates objects of the base); in the new form segments partition, with retained families holding unmatched objects. Whole cuts "recompute deltas with `--no-reuse-delta`" (`pack_segments.rs:305`).
- Applicability: separating metadata-like objects (commits, trees) from payload (blobs) into different physical units is exactly the access-pattern split a sparse allgit workspace wants; allgit already distinguishes object kinds. The partition key in walgit is object type; in allgit the natural keys are zone and object type.
- Evidence: CONFIRMED (proto, source, docs). The performance effect is not measured by us.

### W16. Geometric pack lifecycle with conserving seal

- What (`pack_lifecycle.rs` planner, `[packs]` defaults in `walgit.example.toml`): `Work::{Classify, Fold, Freeze, FirstFreeze, Resegment, RepairCoverage}`. Fold compatible fresh families geometrically (factor 2), trigger at 16 fresh packs or a 1-day age with at least 2 packs; freeze useful buffers into tier-2 segments by size or after 14 days of settlement; re-segment when frozen fold bytes exceed 0.5 of the previous global cut; `segment_max_bytes` 2 GiB, delta window 250, depth 50, window memory 2 GiB divided across resolved threads; lease TTL 10 minutes. Outputs publish additively; **only the final seal retires the exact captured inputs**, rechecking live descriptors, policy and index-level object conservation on each CAS attempt (D45, D49); a resumable operation verifies each chunk, proves conservation, then seals.
- Cost: whole cuts need real disk and CPU ("Running a 30 GB base repack on a tmpfs host" is a stated non-goal; it runs on an SSD host, `maintenance.disk = "ssd"`). The seal re-check is O(objects) streaming merge per attempt (`ROUNDTRIPS.md` rows "Final replacement evidence"; no pack download but index reads).
- Applicability: this is the best-documented design of a safe compaction commit in the corpus: additive outputs, one retiring CAS, conservation proved from indexes. allgit's #186 can reuse the structure. Note the cost: conservation proof by streaming union of indexes requires every object id of every input to be inspected.
- Evidence: CONFIRMED (docs, config defaults, planner source header and tests located; seal mechanics from docs). No walgit-reported timing for a seal except "about 10 min" for the fsck unit on the monorepo (`INTEGRITY.md`, WALGIT-REPORTED).

### W17. MIDX bitmap and native pack reuse

- What: after folding, walgit "maintains a real MIDX bitmap for segmented native Git reuse and verifies that a bitmap was produced", sets `pack.allowPackReuse=multi`, and `midx.rs` verifies the MIDX and its named bitmap including file digests and bitmap-to-MIDX linkage ("existence or a successful Git write is insufficient"). Not applied to remote-served packs; a gix/remote engine keeps ordinary traversal.
- Applicability: confirms that walgit does use multi-pack bitmaps and pack reuse (an earlier reading might assume it relies only on per-pack bitmaps). For allgit it is evidence that a MIDX-shaped catalog is workable at 73 M objects, but only on a host with the packs on disk.
- Evidence: CONFIRMED (docs, `midx.rs` header).

### W18. Commit-graph layers as side files

- What: for tier-2 bases a split commit-graph layer (`git commit-graph write --reachable --split=replace`) is stored as `wal/<checksum>.commit-graph` and installed as the base of the reader's commit-graph chain; incremental layers cover newer packs; changed-path Bloom filters are off by default for incremental layers (`commit_graph_changed_paths = false`) because computing them diffs against parent trees, which reads base-pack data.
- Applicability: matches allgit's commit-graph work and quantifies a tradeoff allgit will face for path-filtered history: Bloom data costs about 16% extra graph bytes in our git.git measurement (git-analysis G10) and needs tree reads to build.
- Evidence: CONFIRMED (source, config comment).

### W19. Native packfile-URI delivery with exact coverage proofs

- What (`PACKFILE_URI_DESIGN.md`, D42 to D46): the server negotiates protocol v2 `packfile-uris`; for anonymous-read repositories it appends proven roots as synthetic haves, emits URLs of ordinary stored packs for the covered part and generates only the uncovered remainder (non-thin). Coverage certificates bind a group, the packing-policy identity, a captured WAL sequence and the content-addressed ref snapshot key to the exact set of pack checksums; every CAS rechecks them. Static pack and idx routes require read authorization plus fresh live-or-retired manifest membership. Thresholds: a useful set has a pack of at least 32 MiB (`uri_min_bytes`, proposed), at most 64 URIs, sorted largest first; at most three snapshot reads per request.
- Cost: one extra snapshot GET normally; protected repositories fall back to dynamic transfer until an authenticated client exists; "No optimal-cost or zero-extra-read claim is made". Scale and protected-client qualification are open gates (`GOAL.md`).
- Applicability: for allgit the transport is simpler (its own client can fetch bucket keys directly), but the **proof structure** is transferable: a "covers" statement is only valid for an exact snapshot and exact member set, and retirement must not invalidate issued downloads.
- Evidence: CONFIRMED (docs; source files `packfile_uri.rs`, `smart.rs` located, not read in depth). Performance is explicitly not established by walgit for this layer; the 2075 s to 8 s CI-clone figure in `GOAL.md` is for a blobless depth-1 sparse clone on a 57 GiB repository and is WALGIT-REPORTED, not reproduced (we have no such dataset).

### W20. Groups (code and meta) as logical partitions of one object database

- What: named groups with ref-selector includes and subtractions (`[refs.packfiles.code]`, `[refs.packfiles.meta]`); the physical set of a group is its reachable closure minus the closure of subtraction dependencies; identical physical packs can belong to several groups; labels grant neither access nor coverage; limits proposed: 32 groups, 256 selectors per list, 16 KiB definition.
- Applicability: this is walgit's nearest concept to zones, and a point of contrast. walgit partitions by **ref reachability** inside a single Git object database; allgit partitions by **path ownership** with separate commit DAGs. A group's coverage requires a reachability closure proof; a zone's does not, because a zone commit's tree is self-contained. HYPOTHESIS: zone-scoped packs need a weaker, cheaper proof than walgit's group certificates, except for objects shared between zones.
- Evidence: CONFIRMED (docs, config).

### W21. Two upload-pack engines and the thin-pack finding

- What: `git.upload_pack_engine = "auto"` uses stock `git` wherever it can read the packs; the `gix` in-process engine serves only remote-served bases. D2 records two defects that led to this: gix-pack's thin-pack path built the base pack's whole offset-to-id table per chunk per thread (60 M entries x 44 threads, 178 GB RSS) and once paired offsets with another pack's table (a wrong object id); so the gix engine never emits thin packs and uses a frozen pack source. A reproducer test exists (`upload_gix_scale.rs`) and passed in our run (300k variant).
- Cost: our measurement (section 2.2): without thin packs, a 50-commit catch-up fetch moves 43x the bytes of stock git on git.git.
- Applicability: a cautionary data point for allgit's own packing code. Rolling your own pack/delta writer carries correctness and memory risks that a mature implementation has already paid for; walgit's resolution was to confine the custom code to the case where upstream git cannot run.
- Evidence: CONFIRMED (docs; test run; bench run).

### W22. Placement, brokers and disposable instances

- What: `[placement]` globs say which host serves and maintains which repository; the host that maintains a repo is its writer; non-maintaining hosts may forward pushes to a single-writer broker (`wal.push_broker_url`) so CAS conflicts stop being the common case; refs-level reads work anywhere; instances hold only cache.
- Applicability: relevant to allgit's server (#121 to #130) rather than the storage format. The single-writer-per-hot-object pattern is the practical answer to the CAS rate cap (W2).
- Evidence: CONFIRMED (docs, config).

### W23. Static serving contract and edge offload

- What: every immutable response carries `immutable` caching, a strong ETag and honors Range, If-Range, 304 and HEAD (D10); an optional nginx edge can stream bytes from the bucket via `X-Accel-Redirect`, announced per request in `X-Walgit-Capabilities`; `signed_get_url` for S3 presigned URLs.
- Applicability: with content-addressed objects the same property is free in allgit; the useful part is the discipline of announcing edge capability per request instead of inferring it.
- Evidence: CONFIRMED (docs, trait).

### W24. LFS in the same bucket, with read-through

- What: `lfs/objects/<aa>/<bb>/<sha256>`, Batch API and basic transfer, `PUT` verifies size and sha256 before the store write, `lfs.max_object_bytes` 16 GiB; for imported repositories a read-through to the upstream LFS server streams the object while tee-ing it into a spool file and persists it after a complete sha256 verification. A runbook backfill is reported at 118,391 objects, 482 GiB, about 140 minutes of `lfs fetch --all` plus 6 minutes of upload on a 44-core VM (WALGIT-REPORTED).
- Applicability: walgit stores LFS bytes whole, with no chunking, no deltas and no cross-repo dedup (not found by search for chunk or dedup code; key layout is per repo). This is the gap allgit's chunk manifests fill, and the comparison point for #183: the baseline for "large binaries" in a walgit-style design is whole-object storage.
- Evidence: CONFIRMED (docs, source located).

### W25. Integrity: fsck, repair, resumable import

- What: a lowest-priority maintainer unit runs `git fsck --connectivity-only` on a complete local copy and writes `fsck.pb`; a `repair` unit fetches missing objects from `upstream.git` by SHA and adds them as a tier-0 pack; `import --direct` verifies every published tip is in the pack set before any upload and is resumable via a marker keyed to the intent hash and the target's manifest version. Origin: a real monorepo import that left 1,952 blobs (70 MB) missing (WALGIT-REPORTED).
- Applicability: allgit's `fsck` exists locally; the lesson is the invariant ("every object reachable from an advertised ref is in a live pack") and that it must be audited periodically, because the commit protocol cannot express it.
- Evidence: CONFIRMED (docs, `INTEGRITY.md`).

### W26. Verification stack: simulation, contract suite, bounded models

- What: a `FaultStore` wrapper (crash, partition, stale, lost response, orphan) with per-link request counting; a simulation suite with randomized seeds (`WALGIT_SIM_SEEDS`); a store contract suite run against memory, S3 (rustfs) and GCS; TLA+ models with TLC runs documented as bounded ("they do not prove a refinement of Rust, validate Git pack bytes, or certify deployment performance", `docs/spec/README.md`). Known flaky tests are listed with causes (`AGENTS.md` section 5).
- Applicability: the strongest transferable asset. allgit's manifest CAS is model-checked (about 1.7M schedules); walgit shows how to couple that to executable simulation, request-budget assertions and a contract suite shared by all backends.
- Evidence: CONFIRMED for simulation and contract tests (we ran the memory contract and 19 simulation tests); TLA+ not run by us.

## 5. What we could not establish about walgit

1. **No object-store latency or throughput data from walgit.** The docs give GCS primitive costs (W4) but no bench in the tree runs against a bucket. The 2075 s to 8 s CI clone result and the 466k-ref and 73 M-object scale figures are WALGIT-REPORTED and cannot be reproduced without that dataset.
2. **No evaluation of compression or dedup.** walgit relies on Git's zlib and deltas. A search for `zstd`, `fastcdc`, `content-defined`, `cruft` and `island` over `crates/`, `docs/`, `AGENTS.md` and `walgit.example.toml` at this revision found no occurrences. That does not mean Git's own cruft or island features are unavailable to a repack run by walgit; it means walgit does not configure or discuss them.
3. **No bucket garbage collector.** Superseded packs and retired records are retained indefinitely by design (`PACKFILE_URI_DESIGN.md` section 3; `docs/spec/README.md`: "There is no bucket collector in this port"). Storage and manifest growth is an acknowledged tradeoff.
4. **Protected-client and large-repository acceptance gates are open** by walgit's own statement (`GOAL.md`, `PACKFILE_URI_DESIGN.md` section 7).
5. The S3 backend's conditional delete is a check-then-act sequence (W1); the contract suite and models do not remove that race, only argue it is lease-guarded.

## 6. Pareto view for allgit (HYPOTHESES)

| Dimension | walgit choice | Cost it accepts | allgit alternative to test |
|---|---|---|---|
| Physical format | stock Git packs | idx and side files dominate cold cost; deltas make remote reads chain | own pack with per-zone placement, bounded delta depth (#184) |
| Commit point | one manifest per repository | 1 write per second cap, growing retired list | sharded or two-level manifest (#186) |
| Cold read | idx local, 1 MiB range blocks | block-size choice unexamined | sweep block size and prefetch (#183, #185) |
| Compaction | additive outputs, one retiring CAS with conservation proof | needs a big-disk host, O(objects) proof | zone-local compaction, smaller proof (#186) |
| Large files | whole objects (LFS) | no dedup, no deltas | chunk manifests (exists), measure vs delta (#183) |
| GC | none | unbounded growth | grace-window protocol (#186) |

## 7. Reproduction

```
ssh lambda-final
git clone https://github.com/tobi/walgit ~/ref/walgit && cd ~/ref/walgit && git checkout 80e9a20b29e29aefd16a4dae6f8e274cce85cca5
# protoc is required; without root:
curl -sLO https://github.com/protocolbuffers/protobuf/releases/download/v28.3/protoc-28.3-linux-x86_64.zip && unzip protoc-28.3-linux-x86_64.zip -d ~/ref/protoc
export PROTOC=$HOME/ref/protoc/bin/protoc
cargo build --release --workspace          # rust-toolchain.toml selects 1.97.1
cargo test -p walgit-store --test contract -- memory_contract
cargo test -p walgit-wal
cargo test -p walgit-git --release
cargo test -p walgit-server --release --test sim
cargo test -p walgit-git --release --test refs500k -- --ignored --nocapture
WALGIT_BENCH_REPO=<bare git repo> cargo test -p walgit-git --release --test upload_pack bench_fetch_engines -- --ignored --nocapture
# operator, needs a container runtime: just dev-store && just test-s3
```

## 8. References

- walgit: https://github.com/tobi/walgit @ `80e9a20b29e29aefd16a4dae6f8e274cce85cca5`, files named inline, accessed 2026-10-31.
- Cursor, "Git at any scale" (the source design, as copied in `docs/reference/cursor-git-at-any-scale.md`), https://cursor.com/blog/git-at-any-scale (not re-fetched by us).
- protobuf releases: https://github.com/protocolbuffers/protobuf/releases/tag/v28.3
