# Consolidated results (issue #188, epic #190)

Every row is one datum, with the document and section that holds it, the data file where one exists, its status, and where it was measured. The decision that uses these rows is [architecture-decision.md](architecture-decision.md); the plan is [implementation-roadmap.md](implementation-roadmap.md).

How to read this file:

- **No composite score.** Rows are grouped by axis. A variant that is best on one axis is usually worst on another; the "Pareto reading" line under each table says which variants are on the frontier of that table. Frontier statements were read by hand from the small tables below (they are not output of `scripts/storage_score.py`, which needs the `allgit-bench/v1` JSON schema these experiments did not emit).
- **Status**: CONFIRMED = measured by a committed command; MODEL = measured on synthetic or simulated input; HYPOTHESIS = reasoned; WALGIT-REPORTED = from walgit's docs, not reproduced; ARITHMETIC = computed from other rows and list prices for this file.
- **Machine and date.** Unless a row says otherwise: Lambda box `lambda-final`, AMD EPYC 7J13 (30 vCPU visible, 216 GiB RAM), Linux 6.8.0-1046-nvidia, rustc 1.99.1, shared and busy (load average about 2 to 38, recorded in each data file), measured 2026-10-31. Timings are warm page cache unless stated; request, syscall, byte and hash counts are deterministic and are the primary evidence.
- **Section references** like "packing 4.1" mean `packing-experiments.md` section 4.1; "compression 8.5" means section 8 item 5 of `compression-experiments.md`. Documents are abbreviated: audit, git, walgit, landscape, packing, compaction, compression, chunking, indexing, caching, concurrency, cost, provider, method.
- **Not here**: any real cloud result (none exists; issue #187), any local S3-compatible server result, any 100k-run rerun (see the last section).

## 1. Remote request counts (the audit's biggest finding and its fix)

Counted with typed or `CountingBackend` wrappers over `MemBackend`; no latency, no S3.

| Datum | Value | Where | Status |
|---|---|---|---|
| First publish, flat 1,000 files (loose layout, cd0fd09) | 2,028 requests (1,012 HEAD, 1,014 PUT), 642,863 B up | audit F10; `data/audit_publish-req.jsonl` | CONFIRMED |
| Incremental 1-file publish, flat 1,000 / flat 5,000 / bigdir 5,000 / deep 5,000 (loose) | 121 / 161 / 5,012 / 45 requests | audit F10; same file | CONFIRMED |
| Incremental 1-file publish into a 5,000-entry directory, loose vs packed (main, #212) | 5,009 to 6 requests; bytes up 200,304 to 200,576 | main `docs/benchmarks.md` "Remote packs"; PR #212 | CONFIRMED (in-memory, no latency) |
| Lazy clone 1,000 commits x 100 files, loose vs packed | 2,002 to 9 requests; bytes down 3,899,939 to 4,063,554 | same | CONFIRMED |
| Full fetch, same history, loose vs packed | 3,101 to 4 requests; bytes down 3,909,413 to 4,058,293 | same | CONFIRMED |
| Clone of 1 zone, 4 / 8 / 16 zones on the remote (loose) | 221 / 221 / 221 requests, 137,619 / 137,987 / 138,759 B | audit 3.5; `data/audit_sparse_history_manifest.jsonl` | CONFIRMED |
| Clone of all zones, 4 / 8 / 16 zones | 875 / 1,747 / 3,491 requests | same | CONFIRMED |
| History clone, 50 / 200 / 1,000 commits x 100 files (loose) | 253 / 703 / 3,103 requests; 262 KB / 933 KB / 4.66 MB | audit F15 | CONFIRMED |
| Manifest GETs per publish | 2 (loose layout); packed publish breakdown not printed | audit F14 | CONFIRMED / [unverified] on main |
| Manifest encoded size: empty / 1 branch + 4,096 applied ids / 100 zones x 2 / 1,000 x 5 / 10,000 x 5 | 90 B / 131,163 B / 144,284 B / 419,085 B / 3.0 MB | audit F14; `data/audit_misc.jsonl` | CONFIRMED |
| walgit push request budget (happy path): freshness GET, then pack PUT, idx PUT, log PUT, manifest CAS | 5 requests, 4 sequential rounds; warm refs 1; cold refs with one tail segment 2; checkpoint 4 | walgit W4 (pinned by test, our run 19/19 passed) | CONFIRMED (count), WALGIT-REPORTED (latencies 15 to 80 ms, GCS) |
| Remote GC / pack counts after many publishes (packs accumulate, one idx GET each per fetch) | not measured | main `docs/remote-packs.md`; PR #212 caveats | HYPOTHESIS for cost |

Pareto reading: packed publish dominates loose publish on requests for every row measured and costs about 4% more bytes down on clone; the loose layout is on no frontier except "legacy compatibility". The unmeasured axis is wall time on a real network (not available).

## 2. Local syscalls, hashing and wall time (audit F1 to F12 and the fast paths on main)

| Datum | Value | Where | Status |
|---|---|---|---|
| `add .` flat 1,000 / 5,000 / 20,000: total syscalls (fsync) | 16,928 (1,256) / 77,530 (5,259) / 304,061 (20,259) | audit table; `data/audit_cli.jsonl` | CONFIRMED |
| Unchanged `add .` flat 5,000: `read` / `openat` before to after (#229) | 10,017 to 17 / 5,120 to 119 | main benchmarks.md "After: local fast paths"; PR #229 | CONFIRMED (median of 7, load 13 to 38) |
| 1-file `commit` deep 5,000: `statx` before to after; wall | 15,508 to 65; 130.6 to 30.3 ms | same | CONFIRMED |
| 1-file `commit` flat 1,000: wall before vs after | 12.9 vs 13.3 ms | same | CONFIRMED (no change; negative) |
| Second `status` after racy `add`+`commit`, flat 5,000: `read` | 10,025 to 25 | same | CONFIRMED |
| Hash finalizations: `add` of 21 files twice / `commit` / `status` | 243 / 243 / 7 / 4 | audit; `data/audit_hash_finalize_counts.txt` | CONFIRMED |
| Large-file chunk hashing: `content_id` 4 MiB vs one BLAKE3 pass | 5.26 ms vs 1.00 ms | audit F3 | CONFIRMED |
| `add`+`commit` 20 x 2 MiB x 4 versions; loose `fsck`, before to after (#221) | 2.44 to 0.85 s; 385 to 54 ms | PR #221 | CONFIRMED (interleaved, medians, load 10 to 22) |
| Loose vs packed warm read, 20,000 x 300 B objects | 7.8 us (5 syscalls) vs 12.0 us (9 syscalls) per `get` | audit 3.2, F7; `data/audit_strace_store.txt` | CONFIRMED (negative: packed is slower warm) |
| Loose objects: allocated vs apparent bytes, 20,000 x 300 B | 83.2 MB vs 7.34 MB (11.3x); same objects in one pack 8.31 MB | audit F6 | CONFIRMED |
| Loose overhead measured after #229: flat 5,000 / deep 5,000 | 6.4x / 21.8x allocated vs apparent | issue #230 | CONFIRMED |
| `LocalStore::open`: 100k objects (4.4 MB idx) / 1M objects (44 MB idx) | 5.6 ms / 90.1 ms (89.4 to 91.3) | audit F8; `data/audit_openpacks.jsonl` | CONFIRMED |
| Pack lookup miss, 100k objects in 1 / 10 / 100 / 1000 packs | 2.7 / 3.8 / 20.7 / 96.3 us | audit F9 | CONFIRMED |
| GC mark reads the store | 2.01x store bytes (8.0 MB store: 16.07 MB read; 585.8 ms for 5,055 objects) | audit F19 | CONFIRMED (before #235; not re-measured) |
| Operation objects, 20 commits on 10 / 100 / 1,000 branches: total op bytes | 14.2 KB / 304 KB / 21.9 MB | audit F13 | CONFIRMED |
| Index file, 5,000 files: before / after #229 | 309,873 B / 356,839 B (+15%); load 61.0 ms at 100,000 entries | main benchmarks.md | CONFIRMED |
| 100k-file `vcs` vs git (older run, `c47fcda`): `add .` flat; clean `status` flat; `log -n 50` flat | git 22,170 / vcs 47,741 ms; 121 / 558 ms; 20.1 / 3.8 ms | main `docs/benchmarks.md` "100k-file scale run"; Linux box, git 2.34.1, 5 runs per cell, warm, load1 1.5 to 34 | CONFIRMED (older code; see section 12 for the rerun) |

Pareto reading: after #229 the unchanged-file paths dominate the old ones on syscalls and wall time on deep trees; flat 1-file commit is a tie. Packed reads are dominated by loose reads on warm latency and dominate on file count and allocated bytes.

## 3. Compression: ratio versus speed (per object)

Source: compression 3 (`data/compress-lab/*_codecs.json`). Single thread, medians of 3 (1 for the slowest codecs), loaded box. Ratio = uncompressed / compressed.

Source text, django sample (59.5 MiB):

| Codec | Ratio | Compress MB/s | Decompress MB/s | Range over 5 corpora (ratio) |
|---|---|---|---|---|
| lz4 (`lz4_flex`) | 2.49 | 296 | 1,399 | 2.33 to 3.56 |
| zstd 1 | 3.51 | 182 | 707 | 3.36 to 5.59 |
| zstd 3 | 3.64 | 136 | 701 | 3.55 to 5.73 |
| zstd 9 | 3.94 | 34 | 736 | 3.91 to 6.26 |
| zstd 19 | 4.07 | 2 | 697 | 4.07 to 6.69 |
| brotli 5 | 4.12 | 24 | 262 | 4.01 to 6.53 |
| brotli 9 | 4.18 | 2 | 260 | 4.09 to 6.74 |
| brotli 11 | 4.65 | 1 | 229 | 4.52 to 7.56 |

Pareto reading (ratio, compress speed, decompress speed, all maximised): lz4, zstd 1, zstd 3, zstd 9 and brotli 11 are each best on at least one axis or not dominated; zstd 3 is the knee for write speed (91 to 97% of zstd 9's ratio at 3 to 4x its speed, compression 1.1). zstd 19 and the brotli levels buy 12 to 28% more ratio at 1 to 2 MB/s (offline compaction only). No codec is dominated on all three axes on this table; the choice is a knee, not a winner.

Other classes (zstd 3 ratio unless noted):

| Datum | Value | Where | Status |
|---|---|---|---|
| Small binaries (django `.mo` etc.) / godot small binaries | 2.05 / 1.07 | compression 3.2 | CONFIRMED |
| Large binaries, godot assets / random asset container | 1.28 (zstd 19: 1.42) / 1.000 (lz4 0.996) | compression 3.3 | CONFIRMED |
| Real GPT-2 fp32 / real SmolLM2 bf16, plain zstd 3 | 1.227 / 1.277 | compression 3.4 | CONFIRMED (one 32 MiB window each) |
| Same with byte-plane shuffle (shuf4 / shuf2) + zstd 3 | 1.331 / 1.402 (7.8% / 8.9% fewer bytes); compress 324 to 183 MB/s (fp32) | compression 3.4 | CONFIRMED on 2 real files |
| zstd level 1 to 19 on fp32 weights | 1.228 to 1.227 (no effect) | compression 8.5 | CONFIRMED |
| 112 KiB trained dictionary, files < 1 KiB / 1 to 4 KiB / 4 to 32 KiB / > 32 KiB (source) | 33 to 57% / 24 to 30% / 12 to 17% / 3 to 11% smaller | compression 3.5 | CONFIRMED (same-repo, disjoint paths) |
| Dictionary on godot assets, > 4 KiB | 0.1 percent (up to -0.2) | compression 3.5, 8.2 | CONFIRMED (negative) |
| Adaptive LZ4-probe policy vs fixed zstd 3: large game assets / fp32 / bf16 / q8 weights | +6.5% / +7.9% / +28.2% / +18.7% bytes (worse) | compression 3.6 | CONFIRMED (negative) |
| Best fixed choice among {none, lz4, zstd1, zstd3} per file vs zstd 3 | within 0.8% on every class | compression 3.6 | CONFIRMED |
| Peak RSS growth compressing a 32 MiB window: zstd 3 / zstd 19 / brotli 11 | 25 to 31 MiB (output buffer) / 107 to 112 / 65 to 94 | compression 3.4 | CONFIRMED (coarse) |

## 4. Stored bytes: deltas, blocks, chunking

Delta packs (packing 3.1; `data/packing/*.jsonl`; git objects; zstd 3 unless noted; "vs git" = stored / real `git repack --window=10 --depth=50`, zlib):

| Variant | git.git MiB | cargo MiB | synth MiB | ripgrep MiB |
|---|---|---|---|---|
| Real git, window 0 depth 0 | 3,151.6 | 379.9 | 642.8 | 31.2 |
| Real git, window 10 depth 50 | 140.0 | 39.2 | 253.2 | 3.6 |
| Per-object zstd 3, no deltas | 2,635.3 | 372.6 | 639.2 | 32.6 |
| Lab delta, git order, window 10, depth <= 50 | 136.1 | 36.6 | 249.4 | 3.4 |
| Lab delta, path then commit descending | 158.3 | 37.4 | 248.2 | 3.5 |
| Lab delta, arrival (commit) order | 991.8 | 248.8 | 624.6 | 18.8 |
| Lab delta, id order (current pack order, control) | 2,397.8 | 363.7 | 638.3 | 28.3 |
| Whole-pack zstd 3, 128 MiB window, git order (size bound, not a design) | 61.6 | 21.1 | 235.7 | 2.2 |

Independent compressed blocks, no deltas, git order (packing 4.4; MiB; decompressed per random read in brackets):

| Block raw size | cargo | git.git | synth |
|---|---|---|---|
| 256 KiB (0.26 MiB) | 89.6 | 620.0 | n/a in doc |
| 1 MiB (1.01) | 43.7 (41.4 with +ldm) | 260.0 (256.2) | n/a |
| 4 MiB (3.97) | 29.3 (27.0) | 128.8 (124.0) | 323 (vs deltas 249) |
| 16 MiB (15.0) | 25.2 (22.8) | 84.2 (78.3) | n/a |

Block ratio by order, django source text, zstd 3 (compression 5): path order 64K / 256K / 1M / 4M / 16M / solid = 6.02 / 10.00 / 13.82 / 15.75 / 16.33 / 16.48; first-seen order: 4.16 / 4.61 / 4.92 / 5.04 / 5.08 / 5.09. Per-object zstd 3 on the same text: 3.64. Binaries and checkpoints: ratio flat from 64 KiB to solid (godot large binary 1.20 to 1.26; synthetic checkpoint 1.25).

Block vs Git delta pack, same blobs (compression 7; `data/compress-lab/gitpack_noreuse.log`; MB): ripgrep: 1 MiB blocks 1.69, 4 MiB blocks 1.52, git window 50: 1.70, window 10: 2.55, no deltas 5.33. tokio: 1 MiB blocks 3.18, 4 MiB blocks 2.87, git window 50: 6.08, window 10: 8.54, no deltas 12.41.

Delta depth bound, cargo (packing 4.1): bound 0 / 1 / 4 / 8 / 16 / 32 / 50 stores 372.6 / 97.2 / 68.5 / 58.2 / 47.6 / 40.4 / 37.2 MiB at 1.0 / 1.9 / 3.9 / 5.9 / 8.8 / 12.7 / 15.8 reads per random object (HEAD objects 1.0 / 1.6 / 2.6 / 3.2 / 4.0 / 4.9 / 5.5). git.git: 2,635 / 408 / 271 / 228 / 185 / 155 / 136 MiB at 1.0 / 1.9 / 4.4 / 7.0 / 11.3 / 17.3 / 24.0 reads. Git's own default pack on git.git: mean chain 14.18, 24.2% undeltified (git G5).

Delta inside blocks vs across (packing 4.3), git.git stored / reads per random object: 256 KiB logical blocks 356.9 vs 136.0 MiB (reads 7.0 vs 24.0); 1 MiB 179.5 vs 136.0 (12.7 vs 24.0); 4 MiB 135.7 vs 136.0 (18.9 vs 24.0, one block per read); 16 MiB 133.7 vs 136.0.

Chunking, total MiB incl. modelled manifest and index (chunking 3; percent vs whole):

| Strategy | ripgrep | tokio | django | linux fs | godot | synth_ml | synth_assets | synth_sparse |
|---|---|---|---|---|---|---|---|---|
| whole | 5.5 | 13.0 | 132.3 | 89.2 | 291.8 | 1172.7 | 385.2 | 25.8 |
| fastcdc4K | -11% | -0% | +7% | +15% | +3% | -25% | -65% | -22% |
| fastcdc16K | -5% | -0% | +2% | +5% | +2% | -27% | -65% | -11% |
| fastcdc64K | +1% | -0% | +0% | +0% | +2% | -28% | -64% | -1% |
| fixed4K | +25% | +23% | +19% | +33% | +6% | -25% | -48% | -28% |
| hybrid_type | +0% | +0% | +0% | +0% | +2% | -27% | -64% | +0% |

Other chunking data: FastCDC chunk+hash 980 (4K) / 1,303 (16K) / 1,691 (64K) MB/s vs whole-file BLAKE3 2,288 and Rabin 267 to 290 MB/s (chunking 7); overhead 2.1 to 6.5% of payload at 4 KiB average, < 2% at 64 KiB (chunking 4, MODEL); per-zone namespaces cost 0.04 to 0.7% on code corpora, 8.2% on godot-demo-projects (chunking 6); last revision of the asset container costs 128.4 MiB whole vs 0.45 to 0.54 MiB chunked (chunking 5).

Pareto reading: for text history the frontier on (stored bytes, bytes read per random access) is {per-object zstd (largest, 1 read), 1 MiB blocks, 4 MiB blocks, bounded-depth delta, unbounded delta}; id-ordered delta search is dominated by git-order delta (larger and no fewer reads); arrival-order delta is larger but needs fewer reads (9.6 vs 20.8 per random object on git.git, packing 3.1), so it is a different frontier point, not a dominated one. Whole-pack long-window compression has the best size and the worst random access (280 ms for a 4 KiB read of 256 MiB text, compression 6). On random or dense data every variant ties (packing 4.5). On mutated random binaries deltas (249 MiB) beat 4 MiB blocks (323 MiB), and CDC beats both for in-place edits (chunking 3).

## 5. Read cost: blocks and delta chains

| Datum | Value | Where | Status |
|---|---|---|---|
| 4 KiB random read of text, zstd-3 blocks 64K / 256K / 1M / 4M / 16M: warm median; compressed KiB fetched | 49 / 124 / 405 / 1,547 / 5,998 us; 11.4 / 26.2 / 74.2 / 263.8 / 993.2 KiB | compression 6 | CONFIRMED (local memory; no network) |
| Same on large binaries, blocks 64K / 1M / 4M | 69 / 1,217 / 4,580 us; 56.8 / 832 / 3,255 KiB fetched | compression 6 | CONFIRMED |
| Solid 256 MiB text stream, 4 KiB read | 280 ms | compression 6 | CONFIRMED |
| Reconstructing a random git.git object, whole vs depth 50 (24.0 reads) | 32 us vs 129 us (warm, memory) | packing 4.1 | CONFIRMED (CPU only; a read is a ranged GET remotely, not measured) |
| One path-limited tree diff, loose / pack | 30.5 / 46.3 us (4.1 tree reads); commit read from pack 10.7 us | indexing 2.3; `data/treecost.jsonl` | CONFIRMED |
| Git delta chain: mean on git.git at depth 50 / 10 | 14.18 / 4.84; pack 147.5 vs 172.0 MB | git 4 | CONFIRMED (git 2.34.1, one trial) |
| Git idx size: git.git 422,883 objects | 11,841,796 B (28 B/object): 8.0% / 11.0% / 6.9% of the 147.5 / 107.5 / 172.0 MB packs | git 4; `data/git-structure-gitgit.json` | CONFIRMED |
| walgit remote reader: 1 MiB blocks through an LRU, idx local (about 6% of pack, comment) | not benchmarked by walgit | walgit W12 | WALGIT-REPORTED (6%), no latency data |
| walgit fetch engines on git.git: 50-commit catch-up, stock git vs gix | 199 ms, about 263 KB vs 966 ms, 11.3 MB (43x bytes); full clone 5.4 s / 130.0 MB vs 2.9 s / 136.4 MB | walgit 2.2; `data/walgit-bench-lambda.json` | CONFIRMED (3 trials, local FS, no object store) |

Pareto reading: bigger blocks improve ratio but cost linearly in read latency and fetched bytes; on binaries the ratio is flat so the frontier collapses to the smallest block; on text the knee is 1 to 4 MiB.

## 6. Placement: GETs and read amplification (packing 5)

Merged ranged GETs (gap <= 256 KiB) and read amplification (bytes fetched / needed), mean of 40 trials; model GET = 5 MB-equivalent.

| Layout | cargo 1 zone sparse checkout of HEAD | git.git 50% zones | cargo file history | cargo 20-commit pull |
|---|---|---|---|---|
| id order (current pack) | 70.6 / 20.0x | 1,680.6 / 10.9x | 16.8 / 2.0x | 125.9 / 7.6x |
| arrival (commit) order | 19.3 / 24.8x | 714.4 / 8.0x | 12.0 / 20.6x | 1.0 / 1.0x |
| zone, commit | 9.5 / 9.3x | 476.7 / 10.0x | 9.6 / 39.1x | 4.6 / 1.0x |
| zone, directory, commit | 7.3 / 15.3x | 204.4 / 7.6x | 1.0 / 1.0x | 35.5 / 4.1x |
| zone, head-first, path, commit | 1.0 / 1.0x | 11.7 / 1.1x | 1.6 / 1.3x | 34.8 / 4.1x |

Adaptive placement (packing 5.2, cost-equivalent MB per request, phase A to D): cargo hot-set-only rewrite 38.0 to 19.0 after an 11.1 MiB rewrite; git.git 145.7 to 24.5 after 33.9 MiB; **synth: 55.4 to 50.4 (static zone,commit: 43.6)**, i.e. worse than static after a 240.7 MiB rewrite. Full co-access rewrite: git.git 2,610.9 MiB rewritten, phase D 32.0 vs static head-first 31.7.

Pareto reading: no layout dominates. Arrival order is best for pulls (1.0 GET), zone head-first is best for sparse checkout (1.0 to 11.7 GETs), zone/directory is best for file history; id order (current local pack) is dominated by at least one layout on every row.

## 7. Compaction and tiering (compaction 1 and 3; simulations on real key streams; MODEL)

cargo, 1 MiB buffer, no duplicates (compaction 1.2):

| Policy | Write amp | Sorted runs | Hit probes (no bloom / bloom 1%) | Objects PUT | Compaction read MiB |
|---|---|---|---|---|---|
| flush only | 1.00 | 371 | 205.09 / 3.09 | 371 | 0 |
| tiered T=4 | 4.54 | 8 | 7.57 / 1.07 | 492 | 1,322.9 |
| tiered T=10 | 2.81 | 11 | 9.35 / 1.08 | 411 | 675.6 |
| leveled T=4 | 10.44 | 8 | 6.86 / 1.06 | 4,260 | 3,524.3 |
| leveled T=10 | 11.33 | 6 | 5.70 / 1.05 | 4,453 | 3,854.9 |

Across datasets and buffers: tiered T=4 write amp 2.69 to 6.50, runs 3 to 15; leveled 4.27 to 19.47 with 4x to 15x the PUTs of tiered; flush only 24 to 11,895 runs. Space amplification is 1.00 for every policy without duplicates; with 25% duplicate puts flush-only holds 1.18 to 1.33 and compaction brings it to 1.00 to 1.23 (compaction 1.3). Peak space amplification: tiered 1.07 to 2.19, leveled 1.00 to 1.39.

Hot tier (compaction 3.2, phase 2 after drift, zone/path/commit layout, MODEL; cold GET 50 ms, 100 MB/s, hot 0.3 ms):

| Hot capacity | cargo hit rate: age / LRU / epoch / oracle | cargo model ms per request: all cold vs epoch | git.git hit rate: age / LRU / epoch / oracle |
|---|---|---|---|
| 5% | 8 / 17 / 22 / 23% | 482 vs 450 | 34 / 34 / 43 / 41% |
| 25% | 70 / 66 / 78 / 79% | 482 vs 158 | 82 / 87 / 88 / 92% |
| 50% | 86 / 88 / 92 / 93% | 482 vs 75 | 88 / 95 / 95 / 98% |

Migration traffic at 25% hot on cargo: epoch policy 27 MiB, LRU promote-on-miss 4,632 MiB (compaction 3.3).

Pareto reading: write amplification vs run count vs PUT count is a three-way trade: tiered T=10 is on the write-amp frontier, leveled with a large buffer on the run-count frontier, flush only on the PUT frontier; none dominates. Tiered T=4 is the middle point and the one roadmap item uses.

## 8. Indexes (indexing; MODEL where synthetic; single-thread, warm, 5 trials)

N = 1,000,000 objects (`data/idx_1000000.jsonl`):

| Layout | Packs | Bytes/object | Hit ns | Miss ns |
|---|---|---|---|---|
| per-pack flat sorted (product) | 1 / 16 / 256 | 48.0 | 713 / 1,058 / 2,974 | 721 / 4,260 / 29,021 |
| per-pack + 256 fanout | 1 / 16 / 256 | 48.0 / 48.0 / 48.3 | 621 / 643 / 913 | 515 / 1,744 / 4,564 |
| global sorted + 64K fanout | any | 48.3 | 221 to 355 | 126 to 179 |
| global open-addressing hash | any | 33.6 | 31 to 40 | 34 to 41 |
| minimal perfect hash (BBHash, gamma 1.7) | any | 16.4 to 16.7 | 84 to 198 | 77 to 171 |
| in-memory B-tree | any | about 71.6 (estimate) | 317 to 406 | 319 to 423 |

Product index open: 2 ms at 100k, 47 ms at 1M (12 ms BLAKE3) (indexing 1.2). On disk at 1M objects (indexing 1.4): redb 1,078 B/object random insert (135 sorted), SQLite WAL 61 B, RocksDB 59 B; hit 1.5 to 4.2 us; RocksDB miss 0.39 to 0.45 us. Filters at 1M keys (indexing 1.3): Bloom 10 bits 0.8% false positives, binary fuse 8 at 9.0 bits 0.4%, cuckoo 33.6 bits.

History and path (indexing 2, MODEL, 8 zones x 50,000 commits):

| Datum | Value |
|---|---|
| `is_ancestor` visited commits, near query: no index / generation v1 / commit-time cutoff | 9,350 / 9.9 / 9 (892 wrong of 8,000 under clock skew) |
| Commit graph, monolithic vs per-zone: file; `extend` with 10 new commits; bytes written per publish | 19.7 MB, 437 ms, 19.7 MB vs 2.46 MB, 31.5 ms, 2.46 MB |
| Product graph file | 49.2 B per commit; decode 80 ms for 400k commits; extend from scratch 4.7 s |
| Changed-path filter, 10 bits/entry: bytes per commit; tree diffs per uniform-file query (of 50,000) | 33.0; 1,008 (inverted index: 48.6 B, 17.9) |
| Tree index at 200k files: nested dirs / flat array / FST: heap B per file; subtree list | 127 / 62 / 21.6 (8.5 serialised); 20 us / 20 us / 1,365 us |

Pareto reading: for absent-object lookup the global index dominates every per-pack variant once packs > 1; the MPH is smallest but static; embedded stores are dominated for location lookup by the in-memory array on latency and by the flat file on bytes.

## 9. Caches (caching; generated traces, MODEL)

| Datum | Value | Where |
|---|---|---|
| Local backends, 100k objects: put obj/s; warm get p50; absent-key p50; disk/payload | files 13,900; 11.5 us; 9.6 us; 1.00. SQLite 22,100; 33 us; 31.7 us; 1.41. redb 31,900; 3.9 us; 1.9 us; 5.24. RocksDB 46,800; 3.1 us; 0.43 us; 1.01. Memory 275,000; 0.88 us | caching 1; `data/backend.jsonl` |
| `checkout_sync` hit rate at 1 / 10 / 30% cache: LRU; W-TinyLFU; Belady bound | 0.008 / 0.372 / 0.696; 0.087 / 0.297 / 0.660; 0.16 / 0.57 / 0.83 | caching 2; `data/cache.jsonl` |
| `ci_fresh` at 1 / 3% cache: LRU; W-TinyLFU | 0.000 / 0.000; 0.169 / 0.336 | caching 2 |
| Modelled trace time, no cache vs 30% LRU (batched) | 28,068 s vs 9,211 s; one-RTT-per-object no cache 421,281 s | caching 2 (assumed 40 ms RTT, 50 MB/s) |
| Next-commit prefetch | +0.007 hit rate, 1.0% extra bytes with LRU; useless with W-TinyLFU | caching 3 |
| Restart warming, first 1,500 accesses: cold / most-frequent / most-recent | 0.000 / 0.027 / 0.151 | caching 3 |
| Positive existence cache with TTL only: interleaving model check | violated in 8 of 8 configurations; guarded version holds; negative control violated | caching 4.2; `data/modelcheck.jsonl` |
| Unguarded positive cache dangling-reference rate, 14-day grace, TTL 1 d / 3.5 d / 14 d | 4.5% / 14% / 50% of considered hits | caching 4.3; `data/negcache.jsonl` (MODEL) |
| Negative cache stale answers, publish delay 60 s, TTL 10 s / 60 s / 600 s | 0.6% / 9.9% / 25% of re-probes | caching 4.3 |

Pareto reading: the policy choice depends on cache size relative to the working set (W-TinyLFU at 1 to 3%, LRU at 10 to 30%); batching dominates all policies; existence caches are dominated by "no cache" on safety.

## 10. Correctness evidence (gates, not objectives)

| Datum | Value | Where | Status |
|---|---|---|---|
| Sharded manifests, two-phase cross-zone commit: scenarios; distinct states; mutations | 4 safety scenarios pass (42,803 / 15,922 / 9,926 / 4,327 states); 5 mutations fail as required; 12 of 88 fault-free terminal states commit nothing without wound-wait | concurrency 1.1; `tests/model_sharded.rs` | CONFIRMED (model) |
| GC protocol G1 model: states; mutations | 4,532 + 3,572 states; 7 mutations each fail | concurrency 2.3; `tests/model_gc.rs` | CONFIRMED (model) |
| G1 against the real code (main) | exhaustive interleavings of publisher and sweeper pass; 7 mutations ported and each caught | PR #235; `tests/gc_real.rs` | CONFIRMED (Mem/Fs backends only) |
| Fault injection on real fetch/publish code | 10 tests pass, two documented gaps (rows 17, 18), both turned into proof tests by #239 | concurrency 3; PR #239 | CONFIRMED |
| Baseline manifest CAS model | about 1.7M schedules | issue #190 text | [unverified here] |
| Sharded publish cost | `3k+1` metadata writes for k zones vs 2; reader retries | concurrency 1 | CONFIRMED by protocol (counts), not timed |
| Compaction crash points; two sweepers; tomb expiry; clock-rate skew | not covered | concurrency 3 rows 22, 23, 2.3 open list; docs/gc.md | gap |
| walgit memory contract and simulation suite | 1 and 19 tests passed; server e2e 9 of 57 failed on git 2.34.1 (cause undiagnosed, environment) | walgit 2, 2.1 | CONFIRMED / inconclusive |

## 11. Cost and provider facts

Prices are list, retrieved 2026-10-31 (`data/pricing-2026-10-31.json`; cost 1). The arithmetic rows are computed for the decision document.

| Datum | Value | Where | Status |
|---|---|---|---|
| S3 Standard us-east-1: storage; PUT-class; GET/HEAD; egress first 10 TB | $0.023 /GB-mo; $0.005 /1k; $0.0004 /1k; $0.09 /GB | cost 1 | [V] AWS Price List 20261015173540 |
| R2 Standard: storage; Class A; Class B; egress | $0.015 /GB-mo; $4.50 /M; $0.36 /M; $0 | cost 1 | [V] |
| Worked example: 1 TB, 10 M PUT, 100 M GET, 500 GB egress, one month | S3 $158.00; R2 $96.00; GCS $164.51; B2 $6.95 | cost 4; `scripts/test_storage_cost_model.py` | CONFIRMED (test asserts) |
| 100 M 1 KiB loose objects vs 1 MiB packs, S3 PUT cost | about $500 vs about $0.50 | cost 5 | HYPOTHESIS (arithmetic) |
| 1-file publish into a 5,000-entry directory on S3: loose vs packed | about $0.0020 vs at most $0.00003 | arithmetic on #212 counts | ARITHMETIC |
| 1 TB source text at S3 Standard: raw / ratio 3.64 / 13.82 / 15.75 | $23.00 / $6.32 / $1.66 / $1.46 per month | arithmetic on compression 3.1, 5 | ARITHMETIC (corpus-specific) |
| Same on R2 Standard | $15.00 / $4.12 / $1.09 / $0.95 | same | ARITHMETIC |
| R2 and GCS write limit | about 1 write per second per object key | provider 2 finding 1 | [V] provider docs, via summary |
| GCS CAS | generation, not ETag `If-Match` on PUT | provider 2 finding 2 | [V] |
| B2 conditional writes | unverified, probably absent | provider 2 finding 3 | [U] / [3P] |
| ETag ABA hazard if the version tag is the content MD5 | possible; manifest must embed an epoch; `s3bench` has the check, never run on a provider | provider 2 finding 4; PR #236 | [H] |
| MinIO upstream repository | archived 2026-04-25 | provider 2 finding 5 | [V] |
| `s3bench` real run | one attempt against AWS, AccessDenied on `s3:CreateBucket` for the lab IAM user; no result | PR #236, issue #187 | gap |

## 12. 100k-file rerun on the Lambda box (flat complete, deep in progress)

Source: `~/rt/bench100k/out-flat.log` on `lambda-final` (not committed: the log lives on the box and was not touched; the table below is a transcription). Run started 2026-11-01T01:30:04Z; AMD EPYC 7J13, 30 cores, 216 GiB, ext4, git 2.34.1, a `vcs` release build of the tree synced under `~/rt/bench100k` (the log does not print the commit, so which main commit it contains is unverified), `layout=flat files=100000 seed=42`, 5 runs per cell, **warm cache**, free disk 381 GiB. Status: CONFIRMED for this run; one run session on a shared box. **The box was not quiet and the load differs by tool**: git cells ran at load1 3.3 to 4.4, `vcs` cells at load1 11 to 21 (listed per row in the log), so the `vcs` numbers are if anything pessimistic; differences under about 2x should be read as noise.

Median ms (raw runs in the log):

| workflow | git | vcs | vcs / git | older run on main docs (`c47fcda`, git / vcs) |
|---|---|---|---|---|
| add (fresh index) | 9,561.7 | 12,510.3 | 1.31 | 22,170 / 47,741 |
| commit (all staged) | 646.5 | 291.0 | 0.45 | 729 / 684 |
| status (clean) | 105.5 | 188.4 | 1.79 | 121 / 558 |
| status (1% modified) | 116.5 | 198.8 | 1.71 | 122 / 570 |
| log (50 commits) | 19.9 | 4.3 | 0.22 | 20.1 / 3.8 |
| checkout (1% differ) | 584.0 | 284.8 | 0.49 | 605 / 1,067 |

Store size after first commit: git 421,656 KiB, vcs 439,972 KiB (both loose layouts; vcs stores no compression, git zlib-compresses loose objects; git has not repacked).

These are local worktree and loose-store workflows; they do not exercise the remote pack path. Reading (HYPOTHESIS for the cause, since the build commit is unverified): the changes are in the direction of the fast paths of #221 and #229 (commit and checkout now faster than git on flat; `status` and `add` still slower). The `deep` layout was still running when this file was written (`out-deep.log` held only git `add`, `commit`, `status` rows) and `binary` had not started; **those are not reported**. When they finish, add them here with their own machine and date. Not measured by any run in this folder: a 100k-commit history, 1,000 branches, 50 zones, or any remote at 100k files.

## 13. Index from question to datum

| Question in epic #190 | Where answered | Main datum |
|---|---|---|
| Compression layer worth its CPU? Which codec and block size? | sections 3, 4, 5 | zstd 3; 1 to 4 MiB path-ordered blocks for text; flat ratio for binaries |
| Delta compression vs CDC for source, binaries, checkpoints? | section 4 | text: deltas or blocks; in-place binaries: CDC; checkpoints: dedup bound 25 to 28% (synthetic), codec shuffle 8% |
| Which physical layout minimizes GETs and bytes? | sections 1, 6 | packed publish; zone head-first for sparse checkout; no layout wins all |
| Which indexes pay for themselves? | section 8 | fanout or global location index; per-zone commit-graph; Bloom changed-path |
| Can metadata be sharded without weakening atomic multi-zone commits? | section 10 | yes in a model (two-phase, mutation-checked); costs `3k+1` writes; not needed until measured |
| Cheapest safe GC and compaction? | sections 7, 10 | G1 tombstone handshake (done for directory remotes); tiered merging; compaction safety unmodelled |
| Does an adaptive representation beat any universal one? | sections 3, 6 | not by runtime adaptation: probe policy negative, hot-set rewrite wins 2 of 3 traces |
