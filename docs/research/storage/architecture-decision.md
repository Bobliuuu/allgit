# Storage architecture decision (issue #188, epic #190)

Status: decision record, written 2026-10-31. Reproducible from committed data only (see [results.md](results.md) for every datum with its source section and data file; the staged plan with measurable success criteria is [implementation-roadmap.md](implementation-roadmap.md)).

Section references such as "packing 4.1" or "compression 8.5" mean the document of that name, section 4.1 or, for the second form, section 8 item 5 (negative-result and summary lists are numbered). Evidence labels as in the rest of this folder: **CONFIRMED** (measured by a committed, re-runnable command, with the machine recorded), **MODEL** (measured on a synthetic or simulated model), **HYPOTHESIS** (argued, not measured), **WALGIT-REPORTED** (a number in walgit's own docs that we did not reproduce), **[unverified]**. Vocabulary: the unit of independent history and sparse sync is the zone.

Sources of truth used: the 15 documents in this folder at `origin/research/storage-engine` (`8d6cc78`), and the code and docs on `origin/main` at `b70670e` (read 2026-10-31; several audit findings were fixed there after the audit's base commit `cd0fd09`). No other web source was consulted for this document; every external fact is cited through the report that read it.

## 1. Decision

**Keep the baseline architecture and make targeted, versioned, incremental changes to its physical layer.** The baseline is: BLAKE3 content ids over logical objects, immutable objects stored as loose files locally and as packs remotely, a single manifest object whose compare-and-swap is the one linearization point of every multi-zone transaction, lazy and sparse fetch, and local-first operation with an immutable remote. Concretely:

1. Do **not** replace the engine, adopt walgit's format, move to chunk-addressing for all data, adopt an LSM store, add a metadata database, or shard the manifest. None of these is supported by evidence strong enough to justify its cost or risk to atomic multi-zone commits (section 4).
2. Do the cheap, well-evidenced things first, none of which needs a format change: bound the metadata read-ahead (#227), cut the manifest to one read per publish and add a conditional-GET freshness probe, per-zone commit-graph cache files, S3 connection reuse (after measuring on a local S3-compatible server).
3. Then, behind a **pack format version bump with legacy read support and a tested rollback**: a per-entry codec byte (zstd level 3 with raw fallback), a fanout in the pack index, and **geometric (tiered) compaction of the one-pack-per-publish remote layout**. Compaction is the central structural gap left on main (`docs/remote-packs.md`: "Many small packs ... there is no compaction yet").
4. Only after compaction exists: path-ordered, independently compressed 1 to 4 MiB blocks inside compacted packs. Delta compression is **deferred behind a decision gate**: independent blocks matched or beat the lab's delta pack on text-like histories and need no delta code; deltas are revisited only if a replay on allgit-encoded objects says otherwise.
5. Do **not** build a positive remote-existence cache. Do not build leveled compaction. Do not build adaptive runtime re-placement of objects. Sharded manifests stay a documented, model-checked option with explicit trigger conditions (section 5.9).

This is a recommendation to keep the baseline plus targeted changes. It is not a claim that the baseline is optimal: the largest unmeasured items (real object-store latency, real clouds, the 100k-file and 100k-commit regimes at the storage layer) could change the ordering of the roadmap, and section 8 lists them.

## 2. What the audit found that main already fixed

The audit ([architecture-audit.md](architecture-audit.md), base `cd0fd09`) listed 23 findings. Commits between `cd0fd09` and main `b70670e` resolved or reduced several. Numbers "after" are from the PR bodies and `docs/benchmarks.md` on main; they were produced by those PRs' authors with the same harness as "before" where stated, on the shared Lambda box (AMD EPYC 7J13, load 10 to 38), and were not re-run for this document.

| Audit finding | Status on main | PR / commit | Datum (before to after) |
|---|---|---|---|
| F10: publish does a HEAD per tree entry of each changed directory | **Resolved for the default (packed) publisher**; the legacy loose path still has it behind `Publisher::with_loose_objects()` | #212 `3880b1f` | 1-file publish into a 5,000-entry directory: 5,009 to 6 requests (`CountingBackend` over `MemBackend`; no latency); bytes up 200,304 to 200,576 |
| F15: no production remote pack writer; clone costs one GET per commit and tree | **Resolved** (one pack plus index per transaction, index-based ranged fetch) | #212 `3880b1f` | lazy clone of 1,000 commits x 100 files: 2,002 to 9 requests; full fetch 3,101 to 4; bytes down +4% (3,899,939 to 4,063,554 lazy) |
| F16: `fetch_many` LISTs `packs/` per call | **Addressed by design**: `RemoteSource` lists `packs/` once and downloads each index once (`1 + packs` requests) (`docs/remote-packs.md`); not re-measured separately | #212 | n/a |
| F2: `add .` re-reads and re-hashes every unchanged file | **Resolved** | #229 `a2be38e` | unchanged `add .`, flat 5,000, `read` calls 10,017 to 17; `openat` 5,120 to 119; wall 55.2 to 33.6 ms (median of 7, noisy) |
| F4: `commit` rebuilds and re-puts every tree | **Resolved** (cache-tree in the index, never re-put a tree equal to the parent's) | #229 | 1-file `commit`, deep 5,000: `statx` 15,508 to 65; wall 130.6 to 30.3 ms. Flat 1,000: 12.9 vs 13.3 ms, no change |
| F12: `status` re-hashes racy files on every run | **Resolved** (racy boundary moved forward after verification) | #229 | second `status` after racy `add`, flat 5,000: `read` 10,025 to 25 |
| F20: whole index rewritten on every `add` | **Partly**: an unchanged `add` no longer rewrites it; a one-file `add` still rewrites it whole (about 357 KB at 5,000 entries; index grew about 15% for ctime and cache-tree) | #229 | decision on lazy index format (#112): not built, load 61 ms at 100,000 entries |
| F3: every chunk hashed twice, chunking dominates | **Resolved** | #221 `9a1ee20` | 20 x 2 MiB x 4 versions: `add`+`commit` 2.44 to 0.85 s; loose `fsck` 385 to 54 ms |
| F19 / section 3.7: GC dry run only, no remote delete, leftover `tmp/` and index-less packs never cleaned | **Partly resolved**: real local sweep, orphan cleanup, `Backend::delete` (Mem, Fs, S3, HTTP), remote GC (protocol G1) for **directory remotes only**; S3 and HTTP remote GC refused; no remote pack rewriting | #235 `1c29d60`; open follow-up #232 | model-checked interleavings (`gc_real.rs`), seven mutations each caught; the mark-phase read volume of F19 (2.01x store bytes) was not re-measured |
| Concurrency row 13: remote pack trailer never read | **Resolved** | #222 `2a00522` | one 33-byte ranged GET per pack touched |
| Concurrency rows 17 and 18: lazy fetch misses a missing chunk; publish trusts a present object whose descendant is gone | **Resolved, bounded**: loose-path publish verifies up to 64 present objects; lazy fetch probes up to 128 files per head; `publish --repair` is the unbounded fix. Packed publishes skip the verification by design | #239 `b70670e` | the two "known gap" tests became proof tests |
| F18 (hypothesis): fs backend holds the CAS lock across an fsync | Hardened, cause not confirmed | #225 `ee4c51c` | 200 + 208 + 60 runs, 0 failures before and after; the PR states "a hypothesis, not a confirmed fix" |
| Flaky sparse-clone byte bound | Test relaxed 8,000 to 30,000 bytes; behaviour **not** changed | #226 `b8e1e29`; open #227 | clone read about 21.8 KB for a 7-byte file next to a 50 KB asset |

### 2.1 Audit findings with no merged fix on main (as far as the commit list shows)

I checked the 30 commits in `cd0fd09..b70670e` and the current source of the named files. "Open" below means no commit in that range targets it; it does not mean it was re-measured.

| Finding | Why it still matters | Evidence it is still there |
|---|---|---|
| F1 one fsync, rename, tmp file per new object on `add` | 1,256 `fsync` for 1,000 files (audit) | not touched by #229 (it removed reads, not writes) |
| F5, F23 dedup `put` still encodes, hashes, stats; payload copies | CPU | no change in the commit list |
| F6 loose objects occupy a 4 KiB block each | audit: 11.3x allocated vs apparent for 300 B objects; #230: 6.4x flat 5,000, 21.8x deep 5,000 | #229 measured and documented it, "format unchanged"; automatic repack policy is an unchecked box in #230 |
| F7 packed read slower than loose read (12.0 vs 7.8 us), no fd cache | the benefit of packs is space and file count, not warm latency | `pack.rs` diff in the range only adds `build_pack`, `entries`, `checksum` |
| F8, F9 open cost 90 ms at 1M objects per process; miss lookup linear in pack count (96.3 us at 1,000 packs) | paid by every CLI call | index format unchanged |
| F13 each `Operation` embeds the branch view (21.9 MB for 1,022 ops at 1,000 branches) | metadata growth | `ops.rs` still builds the full `OpView` |
| F14 manifest read twice per publish, never cached, no conditional GET, 131 KB at 4,096 applied ids and 3.0 MB at 10,000 zones x 5 branches | every publish and clone moves the whole manifest | `MAX_APPLIED` = 4096 unchanged; the 6-request publish on main plausibly still contains two manifest GETs [unverified: the request breakdown was not printed] |
| F17 S3 backend opens a new connection per request and SHA-256s every body | now the likely next cost once requests are few | `vcs-store-s3/src/lib.rs` line 123 on main still `max_idle_connections(0)`; never measured (no S3 or MinIO run) |
| F21 fetched objects decoded, re-encoded, re-hashed, one fsynced loose file each | clone cost | no change in the range |
| F22 state-file rename not followed by directory fsync | durability after power loss (HYPOTHESIS, untested) | `write_atomic_inner` on main: `sync_all` on the file then `rename`, no directory fsync |
| Remote pack accumulation: one pack and one index GET per publish | the direct consequence of fixing F10 and F15 | stated in PR #212 caveats and `docs/remote-packs.md` |

The consequence for the decision: the single largest *request-count* problem the audit identified (F10, F15) is gone on main; the next structural problem is the pack accumulation that fix creates.

## 3. How the ten architectures were compared

The brief of epic #190 lists ten complete architectures. For each: what it would mean for allgit, the evidence in this folder for and against, and where it is **worse than Git or worse than walgit**. Evidence comes only from the reports in this folder (section cited); where an architecture was not measured, the row says so. Axes follow [benchmark-methodology.md](benchmark-methodology.md): no weighted score is computed (section 5 there forbids it), so the verdict column is a reading of the Pareto tables in [results.md](results.md), not a ranking.

| # | Architecture | Closest existing thing | Verdict |
|---|---|---|---|
| A | Git-style packs on S3 | allgit packs (main) plus Git delta/idx/MIDX features | **Already the base**; adopt selected Git features (fanout idx, geometric repack, cruft-style grace) |
| B | walgit-style WAL plus packs | walgit @ `80e9a20` | Borrow the protocol discipline (request budgets, conditional GET, fault simulation); do not adopt the log or the format |
| C | Custom cloud-native packs | `crates/vcs-storage-lab` prototypes | Adopt in pieces (codec byte, blocks, ordering); not as a rewrite |
| D | Chunk-addressed (everything chunked) | chunk manifests above 1 MiB | **Reject** as a universal design; keep size-split |
| E | Hybrid chunk-and-pack | allgit today | **Already the base**; no threshold change supported by data |
| F | LSM-inspired | compaction experiments F | Adopt *tiered* merging of packs; reject leveled and reject an LSM as object or location store |
| G | Metadata database plus object storage | Mononoke, Perforce, `vcs-server` | Not measured here; keep as the hosted-service option, not the default |
| H | Database-free CAS plus objects | allgit manifest CAS | **Already the base**; keep |
| I | Local-first with immutable remote backing | allgit | **Already the base**; keep; two cache rules added |
| J | Adaptive hybrid | experiments E2, B (probe policy) | Static per-class policy yes; runtime adaptation no, with one narrow exception under study |

### A. Git-style packs on S3

- For: Git's delta packs are the strongest measured size result: on the lab's git.git object set, per-object zstd-3 stores 2,635 MiB while a Git-order delta pack stores 136 MiB (19.4x), and on cargo 372.6 to 36.6 MiB (10.2x) ([packing-experiments.md](packing-experiments.md) section 3.1). Git's idx costs 28 bytes per object, 6.9 to 11.0% of the pack ([git-analysis.md](git-analysis.md) G6), against allgit's 44 bytes per entry. Geometric repack and cruft-style grace periods are established maintenance policy (G11, G12).
- Against: object ids in allgit pack order defeat delta search: id order 364 MiB vs 37 MiB on cargo (packing section 3.1). A random object at depth 50 needs 24.0 reads on git.git and 15.8 on cargo; each read is a ranged GET on object storage (packing section 4.1; remote latency not measured). The idx must be fetched before the first byte of data (G6); closure bitmaps assume one repository-wide closure and map poorly to zones (G9).
- **Where this is worse than Git**: it is Git, so nothing, except that allgit's own pack code is not Git's mature delta search; walgit's gix-engine thin-pack defects are the cautionary case (walgit-analysis W21: 178 GB RSS; a 50-commit catch-up moving 43x the bytes of stock git, CONFIRMED by our run, section 2.2 there).
- Where worse than walgit: n/a (walgit is this plus a log).

### B. walgit-style WAL plus packs

- For: a request budget table pinned by a passing test (push 5 requests, warm refs 1, cold refs with one tail segment 2, checkpoint 4; our run of the simulation suite, 19 of 19 passed, walgit-analysis section 2); conditional-GET freshness (W6); a fault-injection wrapper and a contract suite shared by backends (W26); a retiring-CAS compaction commit with a conservation proof (W16).
- Against: the manifest is rewritten per publish and walgit reports about one serialized overwrite per second per object (W2, WALGIT-REPORTED, GCS); `retired_packs[]` grows without bound and there is no bucket collector (walgit-analysis section 5 item 3); no cross-repository dedup (per-repo key layout); no chunking, no compression research, LFS stored whole (W24, section 5 item 2); no zones (nearest concept is ref-reachability groups, W20); its scale figures (57 GiB, 73 M objects, 2075 s to 8 s CI clone) are WALGIT-REPORTED and could not be reproduced.
- **Where this is worse than allgit's baseline**: no dedup for large binaries; no zone isolation; unbounded retained metadata. **Where allgit's baseline is worse than walgit** (to fix): conditional-GET freshness probe (absent in `Backend`), a pack list named in the commit object instead of a LIST of `packs/` (allgit discovers remote packs by listing; walgit names live packs in its manifest, W2), a request-budget test table for every path (allgit has counts for publish and clone in `tests/packs.rs` only), 5 versus 6 requests per small publish (the 6 is from #212's test on an in-memory backend; walgit's 5 is a pinned count against a fault store; different harnesses, so only suggestive).

### C. Custom cloud-native packs

A zone-major pack with a head-first region, independently compressed blocks and a per-entry codec.

- For: placement is a large effect for sparse reads: on cargo, a 1-zone HEAD checkout needs 70.6 GETs at 20.0x read amplification in the current id-sorted pack, 7.2 GETs in zone/path/commit order, and 1.0 GET at 1.0x with a zone head-first layout; git.git at 50% of zones: 1,680.6 GETs (id order), 204.4 (zone, directory), 11.7 (head-first) (packing section 5.1, CONFIRMED on real object sets, one object set per row, noted caveats there).
- Against: every static ordering hurts another access pattern: zone head-first is perfect for sparse checkout but needs 34.8 GETs for a 20-commit pull on cargo (arrival order: 1.0) (packing section 5.1). The maintenance cost of keeping a head-first region as HEAD advances was **not measured** (packing section 5.2 last bullet). Write-path cost of building packs online, crash safety and fsck cost were not measured (packing section 7).
- **Where worse than Git**: Git's arrival-order packs serve incremental pulls in 1 GET (E1, commit order); a zone-major layout multiplies that to 4 to 68 depending on dataset. Git also has a decade of delta-search tuning; the lab encoder is a greedy 16-byte matcher (packing section 7).
- **Where worse than walgit**: walgit reuses `git pack-objects`; a custom writer takes on the correctness and memory risk walgit explicitly avoided (W21).

### D. Chunk-addressed (every object chunked)

- For: on in-place-edited large binaries, content-defined chunking removes 65% of physical bytes (synthetic asset container: FastCDC 4 to 16 KiB average, -65% vs whole file; fixed chunks only -48 to -49%) and on the last revision of that set costs 0.45 to 0.54 MiB instead of 128 MiB ([chunking-experiments.md](chunking-experiments.md) sections 3 and 5). Model checkpoints dedup up to the theoretical bound (-25 to -28%, synthetic, aligned by construction).
- Against: for source text and small binaries whole-file objects are within 1.3% of the best strategy; FastCDC at 4 KiB loses 7% (django) and 15% (linux fs) and fixed 4 KiB loses 19 to 33% (chunking section 3, 8.1). Object count grows 2.2 to 2.8x at 4 KiB average and manifest plus index overhead is 2.1 to 6.5% of payload (chunking section 4, MODEL for the overhead). On real replaced assets (godot-demo-projects, 10 revisions) chunking costs +1 to +6% (chunking 8.4). Dedup across zones is worth about 8% on asset-heavy trees and almost nothing on code, and needs no chunking (chunking section 6).
- **Where worse than Git**: Git packs a text history at 0.022 of raw (git.git, packing 3.1); chunk-addressing of text cannot get there (no delta, no cross-version compression below the chunk).
- **Where worse than walgit**: walgit does not chunk at all, so on text it is no worse than Git; D is strictly worse on text.

### E. Hybrid chunk-and-pack (whole objects below a threshold, CDC chunks above, all packed)

This is allgit today (chunk manifests at 1 MiB, FastCDC-like gear chunker, min/avg/max 4/16/64 KiB).

- For: `hybrid_type` (text and small binary whole, large binary FastCDC 64 KiB, checkpoints FastCDC 256 KiB) is within 2% of the best strategy on 5 of 8 corpora (tokio, django, linux fs, godot, synth_ml) and 5% off on the asset container, 12% off on ripgrep and 39% off on the sparse table; the size-driven `hybrid_size<=256K->fastcdc64K` is within 1.5% of best on 5 of 8 (chunking section 3 table, 8.2 and 9; percentages computed from that table). The size-split matches Git's own large-object-promisor direction (git-analysis G15).
- Against: no data in this folder says 1 MiB or 16 KiB average is optimal; the chunking study recommends FastCDC 16 KiB average for in-place-edited binaries and 64 KiB if the index budget matters (chunking 9). Real checkpoints are untested (synthetic revisions are aligned by construction, chunking section 1 HYPOTHESIS).
- **No change recommended** to the threshold or the chunker: changing the chunker changes ids (format bump, landscape "Physical-only?" column) and the PR #221 note says even a faster gear table was rejected for that reason.
- Worse than Git: Git has no chunking; the +1 to +6% chunking cost on replaced assets is a cost Git does not pay. Worse than walgit: same.

### F. LSM-inspired (write buffer, sorted runs, compaction; or an LSM as store or index)

- For (packs as sorted runs): simulated on real key streams, tiered merging has write amplification 1.9 to 5.5 with 3 to 13 sorted runs, against 371 runs (cargo, 1 MiB buffer) and 11,895 runs (git.git, 256 KiB) for flush-only ([compaction-experiments.md](compaction-experiments.md) section 1.2; MODEL, no files or network). Without duplicates compaction reclaims no space (space amplification 1.00) because content addressing has nothing to overwrite; it exists to cut request count and read cost (section 1.3).
- Against: **leveled compaction is unusually expensive on hash keys**: write amplification 4.3 to 19.5 and 4x to 15x more object PUTs than tiered T=4, because every flushed run spans the whole key range (compaction 1.3). Embedded KV stores as the object-location index are 9x slower per hit than an in-memory global array and 1.3x to 22x larger on disk (indexing 1.4), though RocksDB gives the best absent-key latency (0.4 us). As a cache backend an LSM beats loose files (3.4x put rate, 3.7x lower warm get, 22x faster absent-key check) but takes an exclusive process lock (caching section 1; so it needs a daemon or per-process shards).
- **Where worse than Git**: Git's geometric repack (`--geometric`) is the same tiered idea with a production implementation; GitHub reports 5.67 CPU-days saved per hour from it (landscape section 17, reported through the fetched blog; not reproduced). Leveled is worse than that. **Where worse than walgit**: walgit's compaction publishes additive outputs and one retiring CAS with a conservation proof (W16); an LSM-style compactor would need an equal proof.
- Adopt: tiered merge policy at pack granularity. Reject: leveled, LSM as the primary store.

### G. Metadata database plus object storage

- For: multi-zone atomicity is free inside one database transaction; no one-write-per-second-per-key cap (R2 and GCS document 1 write per second per object, provider-comparison finding 1); history is a table. Mononoke and Perforce use this split (landscape sections 2 and 3). allgit already has `vcs-server` with an `HttpBackend` and a lease validator (commits `c76478a`, `d2c1d49`), so a server-side database behind the `Backend` trait is possible without changing clients ([concurrency-and-correctness.md](concurrency-and-correctness.md) section 1 row "Database-backed").
- Against: it gives up "any S3-compatible bucket is authoritative" (same row). **No experiment in this folder measured it**; the only evidence is design analysis and the landscape survey. Perforce's metadata database is described as a single scaling and failover point (landscape section 3, via search-result summary).
- **Where worse than Git**: Git needs no server. **Where worse than walgit**: walgit's single-key CAS also needs no database. Verdict: not chosen, not rejected; it stays the answer to a measured CAS-contention problem on a hosted deployment (section 5.9).

### H. Database-free CAS plus objects

This is allgit's manifest design, model-checked (about 1.7M schedules per the issue text, not re-run here; the new models count distinct states, not schedules, concurrency section header).

- For: one linearization point; a reader gets a consistent multi-zone snapshot with one GET; recovery is "re-run the same transaction". The sharded alternative is safe but costs `3k+1` metadata writes for a k-zone transaction instead of 2, adds reader retries, and can livelock without a priority rule (concurrency 1 and 1.1, `model_sharded.rs`: 4 safety scenarios pass, 5 mutations fail as required; negative result: 12 of 88 fault-free terminal states commit nothing without wound-wait).
- Against: the manifest is rewritten whole by every publish and read by every clone: 131,163 B with 4,096 applied ids, 144,284 B (100 zones x 2 branches), 419,085 B (1,000 x 5), 3.0 MB (10,000 x 5) (audit F14, CONFIRMED). R2 and GCS cap writes at about 1 per second per key (provider-comparison, [V] provider documentation). ETag-as-version has an ABA hazard if identical bytes can recur (provider-comparison finding 4; `s3bench` has the check but was never run against a provider).
- **Where worse than Git**: Git's refs are per-ref files or reftable, so a push is `O(size_of_update)` (git-analysis G20); a single manifest is `O(manifest)`. **Where worse than walgit**: walgit names the live pack set and settings inline and uses a conditional GET, so a read within `freshness_ttl` costs 0 requests and a changed manifest costs one (W2, W6); allgit's manifest is never cached (audit 3.6).

### I. Local-first with immutable remote backing

This is allgit's design: the working repository is authoritative locally, remote objects are immutable and content-addressed, and the manifest is the only mutable remote object.

- For: caching *bytes* by id is safe under any publish or GC interleaving (the exhaustive model check passes for "no cache" and for byte caches; [caching-experiments.md](caching-experiments.md) section 4.1). A hot tier of about 25% of the data gives block hit rates of 66 to 92% across the tested policies; 5% barely helps (compaction 3.3, MODEL). Lazy fetch with batching is the structural equivalent of Git's partial clone (git-analysis G14); batching matters more than any eviction policy: modelled 15x between one-RTT-per-object and batched (caching section 2, MODEL with assumed 40 ms RTT).
- Against: eviction is specified but not implemented (`docs/cache.md` on main); the hot-tier and cache numbers use generated traces, not real access logs (compaction section 5, caching section 0).
- **Where worse than Git**: Git promisor remotes are a negotiated filter on a mature protocol; allgit's lazy walk reads metadata with a 4 KiB x4 read-ahead that can over-read into bulk blobs (#227). **Where worse than walgit**: walgit has sync levels (Refs, Serve, Full, Objects) with an explicit refusal when a set does not fit its cache (W11); allgit has no cache size cap.

### J. Adaptive hybrid

- For: reorganising only the observed hot set paid for itself on cargo (38.0 to 18.3 MB-equivalent per request after an 11 MiB rewrite of a 373 MiB store) and git.git (145.7 to 29.9 after 34 MiB) and tracked a popularity drift without a second pass (packing 5.2).
- Against: **the same policy lost on `synth`** (hot-set rewrite of 241 MiB ended 1.3k MB-eq behind static zone/commit order); full co-access re-sorting is a negative result (rewrites 368 MiB on cargo and 2.6 GB on git.git per pass and ends behind a static head-first layout); the model-parameter for a GET is 5 MB-equivalent, not a measurement (packing 5.2). For compression, LZ4-probe adaptive codec choice is worse than fixed zstd 3 by 6.5% on large game assets and 8 to 28% on float weights (compression 3.6); zstd 3 is within 0.8% of the per-file best among {none, lz4, zstd1, zstd3} on every class.
- **Where worse than Git and walgit**: both use static policies and have none of this maintenance machinery to get wrong. Verdict: static per-class policy (codec by class, chunk only large binaries, block size by class) is what the evidence supports. Runtime re-placement stays a research item.

## 4. Pareto reading, in one table

Details and data pointers are in [results.md](results.md). "Best" and "worst" are among the compared variants of that axis, not absolute.

| Axis | Best variant measured | Worst variant measured | Section |
|---|---|---|---|
| Stored bytes, text history | whole-pack long-window zstd (unusable for random access), then 4 MiB path-ordered blocks, then Git-order delta pack | id-ordered per-object compression (current pack order) | packing 3.1, 4.4; compression 5 |
| Stored bytes, random or already-dense data | all tie | all tie | packing 4.5 |
| Stored bytes, in-place-edited binaries | FastCDC 4 to 16 KiB | whole file | chunking 3 |
| Random read cost (GETs) | whole objects (1.0) | unbounded delta chains (24 to 52 reads) | packing 4.1, 4.2 |
| Sparse checkout GETs | zone head-first | id order | packing 5.1 |
| Incremental pull GETs | arrival order (1.0) | zone/path order (35 to 68) | packing 5.1 |
| Write amplification | flush-only, then tiered T=10 | leveled | compaction 1.3 |
| Cold-start requests (run count) | leveled, tiered with a large buffer | flush-only | compaction 1.3 |
| Absent-object lookup | global index (0.13 us) | per-pack scan at 256 packs (29 us) | indexing 1.1 |
| Request count per publish | packed publisher (6) | loose publisher (5,009) | benchmarks.md on main |
| Atomic multi-zone commit cost | single manifest CAS (2 metadata writes) | sharded two-phase (`3k+1`) | concurrency 1 |

No row has one variant best across all rows; that is why no composite score exists.

## 5. Candidate changes checked against the data

Each candidate named in the brief was verified against the reports before being recommended. "Priority" is the roadmap priority label.

### 5.1 History-ordered packs so deltas work

- Data: ordering decides delta savings. cargo, window 10: 36.6 MiB in Git order, 37.4 path then commit descending, 40.3 path then commit ascending, 172 size order, 249 arrival order, 364 id order (packing 3.1). The current pack is sorted by id (audit 3.2). Newest-first chains give HEAD blobs 1.0 reads (1.1 to 1.2 on git.git) at any checkpoint spacing (packing 4.2).
- Caveat that changes the design: **a remote pack written by one publish contains one transaction's new objects**, so versions of the same path are almost never adjacent inside it. History order only pays when packs from many transactions meet, i.e. in compaction. That is why ordering is bound to the compaction work, not to the publish path.
- Caveat: the lab ran on git objects (sha1, git encodings, trees in git format) with a lab rule for zones; allgit trees are canonical varint-encoded and may delta differently (packing 6 HYPOTHESIS; section 7).
- Verdict: **adopt the ordering in the compactor, defer deltas** (5.2).

### 5.2 Independently compressed blocks vs delta packs

- Data: with path-ordered adjacency, zstd-3 +ldm blocks of 4 MiB store 27.0 MiB on cargo against 36.6 MiB for the delta pack (26% smaller); git.git 124 MiB vs 136 (9% smaller); at 1 MiB blocks cargo is 13% larger and git.git 1.9x larger (packing 4.4). Compression lab: path-ordered 4 MiB zstd-3 blocks reach ratio 15.75 on django text vs 5.04 in first-seen order (compression 5), and on ripgrep 1.52 MB vs a Git window-50 pack of 1.70 MB, on tokio 2.87 vs 6.08 MB (compression 7).
- Against: on mutated random binaries blocks lose: synth 323 vs 249 MiB (packing 4.4); a 4 KiB random read decompresses a whole block: 1.5 ms warm at 4 MiB text blocks, 264 KiB of compressed bytes fetched per 4 KiB requested (compression 6); on binaries and checkpoints the ratio is flat across block sizes (compression 5) so large blocks only add cost there. Deltas inside blocks at 4 MiB cost no size and give one block per read (packing 4.3), so the options are not exclusive.
- Verdict: **blocks first** (no delta engine to write and verify), class-dependent block size (text 1 to 4 MiB, binaries 64 KiB to 1 MiB), deltas behind a gate: only if a replay on allgit-encoded objects shows blocks more than 1.15x the delta pack on a text history or if mutated-binary workloads (synth-like) matter. The 1.15 threshold is a proposal, not a measured value.

### 5.3 zstd level 3 default with raw fallback

- Data: level 3 gives 91 to 97% of zstd 9's ratio at 3 to 4x its speed and 86 to 91% of zstd 19's at 27 to 70x (compression 1.1); source text ratio 3.55 to 5.73; zstd stores incompressible blocks raw, so random data compresses at 2.1 GB/s and decompresses at 14.8 GB/s with ratio 1.000 (compression 3.3); the LZ4-probe adaptive policy is never better and up to 28% worse (compression 3.6). Float weights gain nothing from levels (1.228 to 1.227, compression 8.5).
- Local interaction: a loose object under 4 KiB occupies a 4 KiB block regardless of its size (audit F6; #230: 6.4x allocated/apparent on flat 5,000), so per-object compression only saves disk when objects live in packs. It saves request bytes and storage bytes remotely.
- Verdict: **adopt** as the per-entry codec of pack format v2. Ids are unchanged because they hash the logical object (landscape, "physical-only" rule).

### 5.4 Per-zone commit-graph files

- Data (MODEL, 8 zones x 50,000 commits): monolithic file 19.7 MB, `extend` with 10 new commits 437 ms; per-zone file 2.46 MB, 31.5 ms: 14x CPU and 8x I/O per publish (indexing 2.2). The partition is exact because a parent always lives in its child's zone (`cgraph.rs` enforces it on main).
- Verdict: **adopt**, low risk because the file is a rebuildable cache. Generation v1 (already in the product) is enough; corrected-date generation and GRAIL gave no extra pruning, and commit-time cutoffs gave 892 wrong answers of 8,000 under clock skew (indexing 2.1).

### 5.5 Geometric pack merging and compaction (Backend delete now exists)

- Data: see F. Tiered merging bounds runs at 3 to 13 with write amplification 1.9 to 5.5 (MODEL). On main each publish adds one pack and each fetch costs `1 + packs` requests plus one 33-byte trailer GET per pack touched (#222). `Backend::delete` exists for four backends (#235) but remote GC and any pack rewriting are directory-remote only (#232 open).
- Gaps: compaction crash points are listed as not covered in the failure matrix (concurrency rows 22 and 23); the compaction-aware deletion order (index first, then pack) is a HYPOTHESIS, not tested (concurrency 2.4); readers slower than the grace period can lose objects of a replaced head (docs/gc.md on main).
- Open design question the evidence raises: remote packs are discovered by LISTing `packs/`; they are not named by the manifest (docs/remote-packs.md). walgit names them in its manifest (W2) and the GC handshake protects deletions of listed units. Whether compaction needs a pack catalog that is versioned with the manifest CAS, or can rely on LIST plus tombstones, must be decided in the compaction issue with a model-checked answer.
- Verdict: **adopt, priority p1**, directory remotes first, S3 after #232.

### 5.6 Metadata/bulk split with a trailing header

- Data: the landscape ranks it first by hypothesis (restic, Kopia). The measured evidence is different: main already orders a publish pack commits, then trees, then blobs and reads the metadata prefix with a 4 KiB x4 read-ahead: a 1,000-commit lazy clone is 9 requests (#212). The remaining defect is over-read into bulk (21.8 KB for a 7-byte file, #226), and the pack index already contains every entry offset, so the metadata end is knowable without a trailing header (#227 says this).
- Not supported by any allgit experiment: separate metadata packs versus a metadata prefix inside one pack.
- Verdict: **do #227; defer the split and the trailing header**. A trailing header would help streaming writes (`build_pack` assembles the pack in memory; audit 3.2 HYPOTHESIS that memory is O(pack)), which is a different motivation and untested.

### 5.7 Global or fanout object-location index

- Data (indexing 1.1, N = 1,000,000): per-pack flat layout (today) misses cost 0.72 us at 1 pack, 4.26 us at 16, **29.0 us at 256**; per-pack with a 256-entry fanout 0.52, 1.74, 4.56 us; a global sorted array with a 64K fanout 0.13 to 0.18 us for any pack count; a global hash 0.03 to 0.04 us at 33.6 B/object; a minimal perfect hash 16.4 B/object but static. Open cost of the product index: 47 ms per 1M objects, of which 12 ms is BLAKE3 (indexing 1.2); the audit measured 90 ms for a real 1M-object pack file.
- Interaction: tiered compaction bounds the number of packs, so the miss cost at 16 packs (4.26 us flat, 1.74 us with fanout) is the relevant operating point once compaction lands; the 29 us regime exists because packs are never merged locally either (`repack` only packs loose objects).
- Verdict: **fanout in index v2 and lazy hash verification at open: adopt, p2. A global index: defer** until measured pack counts after compaction justify it. Remote-resident global indexes were not run (indexing section 5).

### 5.8 Tightening the metadata read-ahead (#227)

- Data: #226 reproduced a 1-in-3 flake caused by the second request (16 KiB) running into the bulk region: about 21.8 KB read for a 7-byte file next to a 50 KB asset. #212 keeps a 1,000-commit lazy clone at 9 requests; the fix must preserve that.
- Verdict: **adopt, existing issue #227, p2** (stays on the roadmap as the cheap first step, no format change).

### 5.9 Manifest growth and when to shard

- Data: sizes listed under H. The single-key write cap on R2 and GCS (about 1 write per second per object, [V] in provider-comparison) makes a measured CAS-contention number the deciding datum, and it does not exist: `docs/locks.md` calls the contention "unmeasured" (concurrency section 1). `s3bench` has an 8 KiB manifest CAS loop and was never run against a provider (#187, #236).
- Interpolation (not a measurement): between the measured 419 KB at 1,000 zones x 5 branches and 3.0 MB at 10,000, the manifest passes 1 MiB near 2,500 to 3,000 such zones.
- Verdict, as a decision rule: (1) first remove avoidable cost: one manifest read per publish, a conditional-GET freshness probe, and a smaller `applied` window encoding (the 4,096 ids are about 131 KB of the 131,163-byte manifest if each is 32 bytes); (2) measure CAS retry rate and publish latency versus writers and zones on a local S3-compatible server and then on a real provider; (3) shard only when a measured trigger fires. Proposed triggers (proposals, not data): manifest larger than 1 MiB on the target deployment, or CAS conflicts accounting for more than half of p95 publish latency at the expected writer count. If it fires, the snapshot-metadata design (pointer CAS to immutable manifest files) is the smaller step than sharded manifests (concurrency 1, "Recommendation"); the sharded protocol in `tests/model_sharded.rs` is the fallback and needs its single-zone fast path, the registry and the fencing counter modelled first (listed as "not modelled" there).

### 5.10 Byte-plane shuffle for ML weights

- Data: shuffle+zstd-3 saves 7.8% of bytes on real GPT-2 fp32 (ratio 1.227 to 1.331) and 8.9% on real SmolLM2 bf16 (1.277 to 1.402); compress speed 324 to 183 MB/s (fp32); int8 gains nothing (compression 3.4). Two real files, one 32 MiB window each (compression 10).
- Interaction not measured: allgit chunks large files with CDC at 4 to 64 KiB, and a shuffle needs a width-aligned span; the measurements are on 32 MiB windows, not on chunks.
- Verdict: **adopt as an optional codec id, p2**, after the codec byte exists and only for classes the writer can identify as float tensors (safetensors, gguf by extension as in the lab's classifier). The expected gain is about 8% on tensors that do not dedup anyway; it is real but small.

### 5.11 No positive existence cache

- Data: exhaustive search over the publisher/GC/cache/clock interleavings: no cache, holds; positive cache with TTL only, violated in 8 of 8 configurations; positive cache guarded by GC epoch and age <= touch-after, holds; negative control trusting entries one tick longer, violated for initial ages 0 and 2 (caching 4.2). A TTL-only negative cache is also unsafe (5-step counterexample); a generation-guarded one is safe but worthless when the remote is busy (reusable about 9% of the time at 10 publishes per minute, caching 4.3).
- A guarded positive cache saves at most one HEAD per dedup hit and carries the largest risk in the design (caching 4.2, author's recommendation).
- Verdict: **do not build**. Byte caches keyed by id are fine. Roadmap carries no issue for this; the rule is recorded here and in section 7.

## 6. Cost comparison

Pricing: [cost-model.md](cost-model.md), list prices retrieved 2026-10-31 (S3 Standard us-east-1: $0.023 per GB-month, $5 per million PUT-class, $0.40 per million GET/HEAD; R2 Standard $0.015, $4.50 per million Class A, $0.36 per million Class B, free egress). The arithmetic below is computed for this document from those prices; it is **not a measurement** and ignores latency, egress, retries and free tiers.

| Quantity | Value | Basis |
|---|---|---|
| One 1-file publish into a 5,000-entry directory, loose layout, S3 | at most about $0.0020 (5,004 HEAD-class at $0.40 per million; 5,009 requests total) | audit F10, `docs/benchmarks.md` |
| Same, packed layout | at most $0.00003 even if all 6 were PUT-class | #212 |
| One million such publishes | about $2,000 vs at most $30 | arithmetic |
| 1 TB of source text, S3 Standard, stored raw | $23.00 per month | cost-model |
| Same at per-object zstd-3 (ratio 3.64, django) | $6.32 | compression 3.1 ratio, arithmetic |
| Same at path-ordered 1 MiB blocks (13.82) / 4 MiB (15.75) | $1.66 / $1.46 | compression 5 ratio, arithmetic |
| Same on R2 Standard, raw / 3.64 / 13.82 / 15.75 | $15.00 / $4.12 / $1.09 / $0.95 | arithmetic |
| Extra PUTs from tiered T=4 merging (cargo) | 1.29x to 1.33x the flush-only PUT count (1,934 vs 1,453 at 256 KiB buffers; 31 vs 24 at 16 MiB) | compaction 1.2 |

The ratios are for the corpus's unique-blob streams, not a whole repository: binaries, model weights and already-dense assets stay near 1.0 to 1.4 (compression 3.2 to 3.4, 5). On B2 request cost is zero and the cost axis collapses to storage; B2's conditional-write support is unverified, so it can hold immutable objects only (provider-comparison finding 3).

## 7. Correctness constraints the roadmap must keep

1. **Atomic multi-zone commits are not weakened.** No roadmap item changes the manifest CAS protocol except by removing a redundant read and adding a conditional GET. Anything that touches `Publisher::step`, the manifest, GC or compaction must extend the existing model checkers (`model.rs`, `gc_real.rs`, and a new compaction actor) and ship a mutation test that fails when the new safety rule is removed. This is the epic's success criterion.
2. **Ids hash the logical object**; codecs, blocks and ordering are physical-only. No chunker change (it changes ids).
3. **No existing user data is rewritten without a tested migration and approval** (#189 acceptance). New formats apply to new packs; compaction rewrites packs only under an explicit `--compact` opt-in until it has soaked; old formats stay readable.
4. **No positive existence cache** (5.11); negative or manifest caches only with a generation or ETag guard (caching 4.4 rules 2 to 4).
5. **Delete order for packs: index first, then pack**, behind the G1 tombstone handshake (docs/gc.md on main). Remote GC and compaction on S3 and HTTP remain refused until #232 provides touch, age and clock there.

## 8. What remains unmeasured (and could change this decision)

1. **No real cloud run.** The `s3bench` contract and latency tool (#236) was run once against AWS; the lab IAM user is not allowed `s3:CreateBucket` (AccessDenied), so there is no provider result: no GET/PUT/HEAD latency, no conditional-write behaviour, no throttling, no multipart numbers (issue #187). Every latency in this folder is modelled (assumed 40 ms RTT in caching; cold-GET latencies 20/50/100 ms in compaction G; 5 MB-equivalent per GET in packing E). The request counts are real; the times are not.
2. **No local S3-compatible server run either** (MinIO or RustFS; `s3bench` "has not been run against MinIO or any other S3-compatible server yet", #236). This needs no approval and is a roadmap item.
3. **The 100k-file regime at the storage layer.** `docs/benchmarks.md` on main has an older 100k-file table (git vs `vcs`, `c47fcda`). A rerun on a recent build synced to the Lambda box (`~/rt/bench100k/out-flat.log`, started 2026-11-01T01:30Z, flat layout, 100,000 files, 5 runs per cell, warm, shared box with load1 3.3 to 4.4 for git cells and 11 to 21 for `vcs` cells) finished for the flat layout: median ms git / `vcs`: `add .` 9,562 / 12,510; `commit` 647 / 291; clean `status` 106 / 188; `log -n 50` 19.9 / 4.3; `checkout` (1% differ) 584 / 285 ([results.md](results.md) section 12). The deep layout was still running and is not used. These are local-store workflows; nothing in this folder measures a 100k-commit history, 1,000 branches, 50 zones, or any remote at 100k files at the storage layer (benchmarks.md "Not run").
4. **All pack, compression and placement experiments used git objects** (sha1 ids, git tree encodings, a lab zone rule) or file contents, not allgit-encoded objects (packing section 2 and 7). Trees are the biggest unknown for delta and block ratios.
5. **Cold cache.** The audit could not drop the page cache; the caching study did (with sudo) only for backend cold-get rows; block "cold-file" reads are a local-disk lower bound (compression 2.2, 8.9).
6. **Real access traces.** All sparse-checkout, pull, tiering and cache workloads are generated (Zipf zones; compaction 5; caching 0).
7. **Write-path cost of online delta/block packing, crash safety of compaction, fsck cost, index size with codec fields** (packing 7); compaction crash points (concurrency rows 22 and 23); two sweepers and tomb expiry (concurrency 2.3 open list).
8. **Real model checkpoints beyond two files; shifted tensors; dictionary generalization across repositories** (chunking 1 and compression 10).
9. **Conditional GET support per provider** (needed for the freshness probe): not in any fetched provider page summary; S3 range-read support is itself marked [U] in provider-comparison (universal in practice, but not verified there).
10. **walgit's scale claims**, which we could not reproduce (walgit-analysis section 5), and walgit's server integration tests that failed on the box's git 2.34.1 (9 of 57; cause undiagnosed, explicitly not evidence about walgit, section 2.1 there).
11. **Concurrency on one manifest in practice** (`docs/locks.md`: unmeasured), which is the deciding datum for sharding.

## 9. Migration and rollback

The staged plan, with exit criteria and issue links, is in [implementation-roadmap.md](implementation-roadmap.md). Principles:

- **Stage 0 (no format change)**: read-ahead bound, manifest read count and conditional GET, per-zone commit-graph cache, connection-reuse measurement, local S3-compatible run. Rollback: revert the commit; caches are rebuilt on demand.
- **Stage 1 (format v2, dual-read first)**: release N reads v2 packs and indexes (codec byte, fanout) and still writes v1; release N+1 may write v2 behind `storage.pack_format = 2` (default off). A repository records its minimum reader version so an old client gets a clean error, not a hash failure. Rollback: set the option back (new packs v1 again); a `vcs repack --format 1` rewrites v2 packs to v1 locally and a `vcs publish --repair`-style path covers the remote; v2 readers are never removed.
- **Stage 2 (compaction, opt-in)**: additive output first (new pack, then its index), readers search all indexes so duplicates are harmless, old packs removed only through the G1 handshake and only after an `fsck` pass over the output. Rollback: stop compacting; compacted packs are ordinary packs and stay valid; garbage is bounded by the existing GC.
- **Stage 3 (blocks, shuffle)**: apply only to compacted output, only for classes where the data showed a gain; same rollback as Stage 1 plus a per-class off switch.
- **Stage 4 (conditional)**: delta gate, global index, manifest sharding or snapshot-metadata: only when a measurement-based trigger fires, each with its own model-checker extension.

At every stage the performance regression tests named in the roadmap (counted requests, syscalls, stored bytes on the committed datasets) run in CI or in the documented box command, and a change is rolled back if its criterion regresses.

## 10. Reproducibility

Every number in this document is in one of the 15 reports with its data file under `data/` (map in [results.md](results.md)). Numbers taken from main (`docs/benchmarks.md`, PR bodies) are cited by PR and section. Commands: each report's "Reproduce" section; the cost arithmetic uses `scripts/storage_cost_model.py` prices (`data/pricing-2026-10-31.json`). Nothing in this decision depends on a result that is not committed or cited.
