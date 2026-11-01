# Implementation roadmap (issue #189, decided in #188)

Status: plan, written 2026-10-31. It implements the decision in [architecture-decision.md](architecture-decision.md): keep the baseline and make targeted, versioned changes. Evidence for every threshold is in [results.md](results.md). Nothing here has been implemented by this change.

Rules inherited from the epic and #189: small PRs; every optimization measured; a performance regression test per change; **no existing user data rewritten without a tested migration and approval**; no change weakens atomic multi-zone commits, and any protocol change extends the model checker with a mutation test that fails when the new safety rule is removed; any unsafe Rust needs a measured benefit, a safety argument and focused tests. Vocabulary: zone.

## 1. Issues

Each issue has Summary, Context, Scope, Acceptance criteria, Handoff notes and Blocked by, labels `m5-perf` plus `zone:*`, `type:*`, `priority:*`, `size:*`. Existing issues that already cover a recommended item are linked rather than duplicated.

| Stage | Issue | Title | Priority / size | Blocked by | Measurable success criterion (source of the number) |
|---|---|---|---|---|---|
| 0 | #227 (existing) | Tighten pack metadata read-ahead | p2 / m | none | sparse clone of a 7-byte file next to a 50 KB asset reads far less than the 21.8 KB seen in #226; 1,000-commit lazy clone stays near 9 requests (#212) |
| 0 | #240 | One manifest read per publish and a conditional-GET freshness probe | p1 / m | none | 1-file publish: manifest GETs 2 to 1, requests 6 to 5; unchanged-manifest refresh 1 request, 0 body bytes (audit F14; walgit W4, W6) |
| 0 | #241 | Per-zone commit-graph cache files | p2 / s | none | `extend` of 10 commits at 8 zones x 50,000 commits <= 50 ms (437 ms monolithic, 31.5 ms measured per zone); bytes written <= 1/6 (indexing 2.2) |
| 0 | #242 | Provider contract, storebench and latency tools against local MinIO and RustFS | p1 / m | none | first real per-request latency and contract results committed as JSON; 1-file publish and 1,000-commit lazy clone timed with counts 6 and 9 |
| 0 | #243 | S3 backend: connection reuse for safe requests, signing cost | p1 / m | #242 | connection setups per 1,000 sequential GETs 1,000 to <= 1 per worker; CAS winners test still passes |
| 0 | #244 | Replay compression and packing experiments on allgit-encoded objects | p1 / m | none | table with the columns of packing 3.1 and 4.4 for allgit objects (trees separate); verdict on the 1.15x delta gate |
| 1 | #245 | Pack format v2: per-entry codec byte (zstd 3, raw fallback), versioned, legacy read | p1 / l | soft #244; shares the bump with #246 | source-text packs <= 1/3.4 of raw, incompressible <= 1.001x; clone still 9 / 4 requests; rollback test v2 to v1 |
| 1 | #246 | Pack index v2 (fanout) and cheaper read/open path | p2 / m | format bump with #245 | miss at 1M objects / 256 packs <= 5 us (29.0 today, 4.56 measured with fanout); open of a 1M-object pack <= 45 ms (90.1 today); packed warm get <= 8 us (12.0 today) |
| 1 | #247 | Automatic local repack policy | p2 / m | #246 | allocated/apparent <= 1.25x after `add`+`commit` of flat 5,000 (6.4x today; 1.13x in one pack) |
| 2 | #248 | Tiered compaction of remote packs, pack discovery decided, model-checked | p1 / l | #245 (soft); S3/HTTP part blocked by #232 | after 1,000 single-file publishes: packs <= 12; compaction bytes <= 6.5x published (simulated worst 6.50); all new mutations fail |
| 2 | #232 (existing) | S3 and HTTP remote GC, remote pack rewriting | p1 / l | #231 landed | required before compaction can run on S3 or the HTTP server |
| 3 | #249 | Path-ordered independently compressed blocks in compacted packs | p2 / l | #248, #245, #244 | <= 1/3 of per-object zstd-3 bytes on text histories (measured 3.8 to 4.5x); warm 4 KiB read <= 2 ms at 4 MiB blocks (1.5 ms measured) |
| 3 | #251 | Byte-plane shuffle codec for float tensors | p2 / s | #245 | >= 7% fewer bytes than zstd 3 at the real chunk size (measured 7.8% fp32, 8.9% bf16 on 32 MiB windows); compress >= 150 MB/s |
| 4 | #250 | Gate: in-block deltas, only if blocks lose on allgit data | p2 / m | #244, #249 | verdict table; open production work only if blocks > 1.15x the delta pack (a proposed threshold) |
| 4 | #252 | Manifest scaling: measure contention and size, record trigger decision | p2 / m | #242, #240; #187 for real providers | table of manifest bytes, publish p50/p95 and retry rate for writers 1/4/16 x zones 100/1k/10k; measured trigger values |
| ext | #187 (existing) | Provider benchmarks on real clouds | p1 / m | operator credentials and approval (the lab IAM user cannot create buckets) | `s3bench` contract and latency JSON from AWS, R2, GCS, B2 as available |

Dependency order (an arrow means "must land before"):

```text
#227, #240, #241, #242 ---> #243, #252          (stage 0, independent of each other except #242 feeds #243 and #252)
#244 ----------------------> #249, #250          (replay is the gate for blocks and deltas)
#245 + #246 (one format bump) --> #247, #248 --> #249 --> #250
                                  #232 --------> #248 on S3/HTTP
#245 ----------------------> #251
```

## 2. Stages, exit criteria and rollback

### Stage 0: no format change (weeks 1 to 2 of effort, parallelizable)

Items: #227, #240, #241, #242, #243, #244.

- Exit: each issue's criterion met and the numbers added to `docs/benchmarks.md` (command, machine, load, trials).
- First measurable gains: manifest GET count and bytes; commit-graph publish cost; the first latency data of any kind from an object-store server (#242), which re-orders the rest of the plan if per-request cost turns out to dominate (it is the largest unmeasured axis, decision section 8).
- Rollback: revert the commit. Caches (commit-graph) are rebuilt on use; the conditional GET falls back to a plain GET on a backend that does not support it.

### Stage 1: pack format v2, dual-read first

Items: #245, #246, then #247.

- Release N: readers understand v2 packs and indexes (codec byte, fanout); writers still emit v1. Release N+1: `storage.pack_format = 2` becomes available (default off); it flips to default only after a release without regressions in the criteria.
- A repository/manifest minimum-reader marker makes an old client fail with "unsupported pack format" instead of a hash or parse error. Existing packs are never rewritten without an explicit command and approval.
- Exit: criteria of #245 and #246 pass in CI or the documented box command; mixed v1/v2 remote tests; `faults.rs` extended with codec-byte corruption and truncated entries; `model.rs` untouched and green.
- Rollback: set `storage.pack_format = 1` (new packs are v1 again); `vcs repack --format 1` rewrites local v2 packs; the remote side re-publishes as v1 with the repair path; v2 readers stay in the code forever so nothing already written becomes unreadable. Tested in #245 (v2 repo, rollback, clone by a v1-only reader).

### Stage 2: remote compaction (opt-in)

Items: #248, with #232 for S3 and HTTP.

- Directory remotes first. Compaction is opt-in (`--compact`) until it has soaked across releases.
- Exit: packs <= 12 after 1,000 publishes; compaction write cost <= 6.5x published bytes; new model-checker actor and every mutation failing when its rule is removed; crash-after-each-step fault tests; `gc_real.rs` unchanged and green. A decision on pack discovery (LIST vs a catalog versioned with the manifest CAS) is recorded in the PR and model-checked.
- Rollback: stop compacting. Output is additive (new pack, then new index); inputs are removed only through the G1 handshake after an integrity check, so a compacted pack is an ordinary pack. If a bug is found after deletion, `vcs publish --repair` (#239) re-uploads from a client that still has the objects; keep a retention window before enabling deletion by default.

### Stage 3: compressed blocks and the tensor codec

Items: #249, #251.

- Only on compacted output, only for classes where the replay (#244) shows the gain. Text 1 to 4 MiB blocks; binaries and checkpoints 64 KiB to 1 MiB (ratio is flat across block size there, compression 5).
- Exit: criteria of #249 and #251; no regression of the sparse-clone request counts (the 1-zone checkout model result plus one request per pack).
- Rollback: per-class off switch; the compactor can emit per-object v2 entries instead of blocks; blocks remain readable.

### Stage 4: gated decisions

Items: #250, #252, plus the deferred ones below. Each opens production work only when its trigger fires and brings its own model-checker extension.

## 3. Regression tests every PR must carry

| What | How | Threshold source |
|---|---|---|
| Request counts for publish and clone | extend `crates/vcs-remote/tests/packs.rs` with `CountingBackend`; assert exact counts (1-file publish 6, now 5 after #240; lazy 1,000-commit clone 9; full fetch 4) | `docs/benchmarks.md` "Remote packs" |
| Local syscalls for `add`/`commit`/`status` | the existing `strace -c -f` harness and `tests/fastpath.rs` | PR #229 |
| Stored bytes at rest | a test that packs a fixed corpus (committed seed or small fixture) and asserts size ratio ranges | `compression-experiments.md` 3 and 5 |
| Index lookup and open cost | `idxbench`/`auditprobe` thresholds recorded in the PR (box command, not CI: wall time is noisy at load 10 to 38) | `indexing-experiments.md` 1; audit F8, F9 |
| Safety | `model.rs`, `gc_real.rs`, `faults.rs`, and a compaction actor with mutation tests | `concurrency-and-correctness.md` |

Wall-clock thresholds are enforced only as same-box before/after pairs with medians and spread (benchmark-methodology.md 3.3); counts (requests, syscalls, bytes, hashes) are enforced exactly.

## 4. Deferred (with the trigger that would reopen them)

| Item | Trigger | Why deferred |
|---|---|---|
| Global or MIDX-like object-location index | measured pack count after compaction still > about 16 on real workloads, or remote index GETs dominate fetch | per-pack fanout reaches 1.74 us at 16 packs (indexing 1.1); global index gain is for hundreds of packs |
| Metadata/bulk split into separate packs; trailing header | an experiment comparing split packs with one pack ordered metadata-first shows fewer requests or bytes; or pack building memory (O(pack), audit 3.2) becomes a problem | no allgit measurement; the in-pack metadata prefix already gives 9 requests for a 1,000-commit lazy clone |
| Sharded manifests | measured trigger from #252 (proposed: > 1 MiB manifest or CAS conflicts > 50% of p95 publish latency) | `3k+1` writes vs 2, reader retries, needs the model extended (single-zone fast path, registry, fencing counter) |
| Snapshot-metadata design (pointer CAS to immutable manifest files) | same trigger; preferred over sharding | smaller step that keeps one linearization point (concurrency 1); needs metadata-file GC |
| Metadata database behind `vcs-server` | measured single-key write cap on a real provider makes CAS contention the bottleneck of a hosted deployment | gives up bucket-authoritative storage; not measured at all |
| Runtime adaptive re-placement | a real access trace shows a small stable hot set (hot-set rewrite won 2 of 3 traces and lost on synth) | maintenance cost and the head-first region upkeep are unmeasured |
| Dictionary compression | per-class gain measured on allgit objects in packs; distribution and versioning design | 3 to 11% on files above 32 KiB, 0 on assets (compression 3.5) |
| Positive remote-existence cache | never | violates safety in 8 of 8 model configurations; saves at most one HEAD per dedup hit (caching 4.2) |
| Leveled compaction, LSM as object store | never for packs | 4x to 15x more PUTs than tiered on hash keys (compaction 1.3) |
| Real-cloud decisions (provider choice, R2/GCS 1 write/s cap, B2 CAS) | #187 results | no real cloud run exists |

## 5. Next experiments (ordered by what could change the plan)

1. #242: local S3-compatible latency and contract data (largest unmeasured axis).
2. #244: allgit-encoded replay (tests the central HYPOTHESIS behind #245, #249 and #250).
3. #252: manifest contention and size curve.
4. #187: real clouds, when an operator provides a bucket policy for `allgit-bench-*` buckets (the `s3bench` operator commands are in `docs/research/storage/s3-benchmark.md` on main).
5. The 100k rerun on the Lambda box (`~/rt/bench100k/out-*.log`): the flat table is in [results.md](results.md) section 12; add the deep and binary tables there, with their machine and date, when they finish.
6. Cold-cache runs (drop caches with sudo) for pack reads and checkout; none exist for the local store.
7. Real access traces for sparse checkout and pulls (all current workloads are generated).
