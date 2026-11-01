# Chunking experiments (issue #183, experiment D)

Status: v1, measured 2026-10-31. Epic #190. Companion: [compression-experiments.md](compression-experiments.md) (codecs,
dictionaries, independently compressed blocks). Harness: `crates/vcs-compress-lab` (standalone workspace; no product
code touched). Raw data: `data/compress-lab/*_chunking.json`. Every number below is read from those files.

## 1. Summary

CONFIRMED (measured, re-runnable, see section 8):

1. For source text and small binaries, whole-file objects are within 1.3 percent of the best strategy on tokio, django
   and the linux `fs/` slice. They are 5 to 33 percent smaller than fixed 4 to 16 KiB chunks on those corpora and 2 to
   15 percent smaller than FastCDC at 4 to 16 KiB on django and linux (tied on tokio), once the manifest and index
   overhead model is applied. The exception is ripgrep, where FastCDC and Rabin at 4 KiB average win by 10 to 11
   percent. Chunk-level dedup is therefore not a free win for text.
2. For large binaries that are edited in place or by insertion (synthetic asset container, 3 revisions), content-defined
   chunking at 4 to 16 KiB average removes 65 percent of the physical bytes versus whole-file storage; fixed-size
   chunks remove only 48 to 49 percent because an insertion shifts every later boundary. On real asset repos where
   files are replaced wholesale (godot-demo-projects, 10 revisions) no chunking strategy beats whole-file storage
   (chunking costs +1 to +6 percent).
3. For synthetic model checkpoints, dedup is bounded by how many tensors are bit-identical between revisions: 27.3 to
   27.8 percent of bytes dedup across a base, a partial fine-tune (4 of 24 tensors replaced) and a full fine-tune
   (every weight perturbed by noise, zero dedup). The result is the theoretical bound, so every strategy from fixed
   64 KiB to FastCDC 256 KiB reaches it; the choice between them is decided by overhead, not dedup.
4. Rabin fingerprinting is 4 to 8 times slower than FastCDC (about 270 to 300 MB/s versus 1000 to 2100 MB/s,
   single thread, chunking plus blake3). Its physical result is within 1.7 percent of FastCDC at the same average size on
   7 of 8 corpora; on the sparse table it is erratic (26 percent worse at 4 KiB, 7 percent better at 16 and 64 KiB) and it
   degenerates on long zero runs (section 8).
5. Per-zone chunk namespaces (a separate dedup domain per zone) cost 0.04 to 0.7 percent extra payload with whole-file
   storage on the four code corpora and 8.2 percent on the asset-heavy corpus (identical files duplicated across demo projects). That cost is
   whole-file duplication; CDC does not reduce it.
6. Manifest plus index overhead (modelled at 36 bytes per manifest entry and 44 bytes per index entry) is dominated by
   the index, i.e. by the number of distinct objects: 0.0 to 0.3 percent of payload on the large-file corpora at
   64 KiB average and above, 1.5 to 1.6 percent on django (many small files), and 2.1 to 6.5 percent at 4 KiB average.

HYPOTHESIS (not established by these runs):

- Real model checkpoints behave like the synthetic ones for dedup (the synthetic revisions are aligned by construction,
  which favours fixed-size chunks). Shifted tensors (changed header length, re-sharding) would favour CDC.
- Chunk sizes found here transfer to other repositories; each corpus has one data point per strategy.

## 2. Method

### 2.1 Machine and tools

| item | value |
|---|---|
| host | Lambda box `lambda-final`, AMD EPYC 7J13, 30 vCPUs visible, 216 GB RAM, Linux 6.8.0-1046-nvidia |
| load | shared box; load average at start of each run 2 to 29 (recorded per run in `machine.loadavg_at_start`). Throughput numbers are therefore noisy; medians of 3 repetitions are reported and spreads are in the JSON. |
| Rust | rustc 1.99.1 (2026-10-13), `cargo build --release` |
| crates | fastcdc 5.0.0 (`v2020::FastCDC`), blake3 1.x, zstd 0.14.3 (libzstd 1.5.7). Rabin: own implementation in `src/rabin.rs` (restic-style polynomial `0x3DA3358B4DC173`, 64-byte window, table driven). |
| harness | `crates/vcs-compress-lab` at the commit of this PR |

### 2.2 Datasets

All clones made on the box on 2026-10-31 (access date); revisions are first-parent commits spaced evenly through the
history, listed in full in `data/compress-lab/datasets.json`. Blobs are deduplicated by content before analysis, so each
unique content is chunked once; revision membership is kept to compute cross-revision numbers.

| name | source URL | HEAD revision | license | revisions | unique blobs / bytes | logical bytes (all revisions) |
|---|---|---|---|---|---|---|
| ripgrep | https://github.com/BurntSushi/ripgrep | 91f5d22e | Unlicense OR MIT | 20 | 1286 / 21 MiB | 42 MiB |
| tokio | https://github.com/tokio-rs/tokio | b499257e | MIT | 20 | 6186 / 48 MiB | 61 MiB |
| django (Python project) | https://github.com/django/django | a05e16a7 | BSD-3-Clause | 20 | 46601 / 418 MiB | 522 MiB |
| linux `fs/` slice | https://github.com/torvalds/linux | cbbddb6b | GPL-2.0 WITH Linux-syscall-note | 12 (2005 to 2026) | 16267 / 315 MiB | 354 MiB |
| godot-demo-projects (asset heavy) | https://github.com/godotengine/godot-demo-projects | b761b4cd | MIT | 10 | 11499 / 393 MiB | 2402 MiB |
| synth_ml | generated, seed 7, `compress-lab synth ml <dir> 256` | n/a | n/a | 3 | 9 / 1344 MiB | 1344 MiB |
| synth_assets | generated, `synth assets <dir> 128` | n/a | n/a | 3 | 3 / 385 MiB | 385 MiB |
| synth_sparse | generated, `synth sparse <dir> 128` | n/a | n/a | 2 | 2 / 256 MiB | 256 MiB |

Notes on the datasets:

- The linux slice spans 2005 to 2026 with 12 samples, so consecutive revisions differ by a large fraction of the files.
  It measures dedup between distant revisions, not between adjacent commits. The other repositories are sampled the same
  way (20 or 10 evenly spaced points), not as consecutive commits.
- godot-demo-projects was cloned with `--depth 300`; the 10 revisions are sampled from that window.
- synth_ml: 24 tensors; three formats per revision (fp32 256 MiB, bf16 128 MiB, int8 64 MiB). r1 = Gaussian weights
  (sigma 0.02), r2 = tensors 4, 9, 14, 19 replaced by fresh Gaussians (4 of 24), r3 = every weight of r1 plus
  Gaussian noise of sigma 1e-4. Tensor sizes are equal, so file offsets are aligned across revisions.
- synth_assets: a 128 MiB container of random-size random-byte assets (incompressible) with a compressible header per
  entry; r2 inserts a 100 000-byte asset in the middle, r3 rewrites one asset.
- synth_sparse: embedding table (90 percent zero rows) with 2 percent of rows rewritten in r2.
- Classification (`src/corpus.rs`): `source_text` = no NUL in the first 8 KiB and valid UTF-8 or a known text extension;
  `model_ckpt` by extension (safetensors, gguf, pt, ...); otherwise `large_binary` at 1 MiB or more, else
  `small_binary`.

### 2.3 Strategies and metrics

Strategies: `whole`; `fixedN` (4, 16, 64 KiB); `fastcdcN` (4, 16, 64, 256 KiB average; min = avg/4, max = 4 avg);
`rabinN` (4, 16, 64 KiB, same min/max rule); `hybrid_size<=64K->fastcdc16K`, `hybrid_size<=256K->fastcdc64K` (whole
file at or below the threshold, CDC above); `hybrid_type` (source text and small binary whole, large binary FastCDC
64 KiB, model checkpoint FastCDC 256 KiB).

Per unique chunk: blake3 id (first 128 bits are used as the dedup key in memory), zstd level 3 compression of the
chunk on its own. Reported:

- *unique zstd3 MiB* = physical payload of all distinct chunks.
- *manifest* (MODEL, not measured on disk): a file with more than one chunk needs a manifest of 16 bytes plus 36 bytes
  per chunk (32-byte id plus 4-byte length), one per distinct file content. Single-chunk files need none.
- *index* (MODEL): 44 bytes per distinct chunk (32-byte id, 8-byte offset, 4-byte stored length).
- *total* = payload + manifest + index; *total/logical* matches `space_amp` in
  [benchmark-methodology.md](benchmark-methodology.md).
- *cross-rev gain* = 1 - (global unique payload) / (sum over revisions of each revision deduplicated on its own).
- *per-zone extra* = payload of distinct (zone, chunk) pairs minus payload of distinct chunks. A zone is the first
  N directory components of the path (N = 1 for ripgrep and tokio, N = 2 for django, linux `fs/<subsystem>`, and
  godot (one zone per demo project)); `zone_depth` and the zone count are in the JSON for the runs that record them.
- *chunk+hash MB/s*: chunking plus blake3 over all unique blobs, one thread, median of 3 repetitions, no compression.
- The dedup and compression pass runs one strategy per thread (up to 8 in parallel), so only the chunk+hash column is a
  throughput measurement.

## 3. Physical bytes per strategy and dataset

Total MiB including modelled manifest and index overhead; percent is relative to `whole` on the same dataset (negative
= smaller). Source: `data/compress-lab/<dataset>_chunking.json`.

| strategy | ripgrep | tokio | django | linux_fs | godot_demo | synth_ml | synth_assets | synth_sparse |
|---|---|---|---|---|---|---|---|---|
| logical MiB (all revisions) | 42 | 61 | 522 | 354 | 2402 | 1344 | 385 | 256 |
| whole | 5.5 (+0%) | 13.0 (+0%) | 132.3 (+0%) | 89.2 (+0%) | 291.8 (+0%) | 1172.7 (+0%) | 385.2 (+0%) | 25.8 (+0%) |
| fixed4K | 6.8 (+25%) | 16.1 (+23%) | 157.5 (+19%) | 118.5 (+33%) | 309.2 (+6%) | 881.6 (-25%) | 200.7 (-48%) | 18.5 (-28%) |
| fixed16K | 5.8 (+7%) | 13.7 (+5%) | 139.2 (+5%) | 97.5 (+9%) | 302.0 (+3%) | 854.1 (-27%) | 196.2 (-49%) | 18.5 (-28%) |
| fixed64K | 5.5 (+1%) | 13.0 (+0%) | 133.2 (+1%) | 89.9 (+1%) | 301.5 (+3%) | 848.5 (-28%) | 195.2 (-49%) | 22.8 (-12%) |
| fastcdc4K | 4.9 (-11%) | 13.0 (-0%) | 141.2 (+7%) | 102.1 (+15%) | 300.2 (+3%) | 875.1 (-25%) | 133.8 (-65%) | 20.2 (-22%) |
| fastcdc16K | 5.2 (-5%) | 13.0 (-0%) | 135.5 (+2%) | 93.4 (+5%) | 297.7 (+2%) | 852.8 (-27%) | 133.1 (-65%) | 22.9 (-11%) |
| fastcdc64K | 5.5 (+1%) | 12.9 (-0%) | 132.8 (+0%) | 89.2 (+0%) | 296.3 (+2%) | 849.0 (-28%) | 140.3 (-64%) | 25.6 (-1%) |
| fastcdc256K | 5.5 (+0%) | 13.0 (+0%) | 132.3 (+0%) | 88.3 (-1%) | 295.1 (+1%) | 852.5 (-27%) | 169.4 (-56%) | 25.8 (+0%) |
| rabin4K | 4.9 (-10%) | 13.1 (+1%) | 139.9 (+6%) | 101.1 (+13%) | 300.5 (+3%) | 875.0 (-25%) | 134.0 (-65%) | 25.4 (-2%) |
| rabin16K | 5.3 (-4%) | 12.9 (-1%) | 135.2 (+2%) | 93.5 (+5%) | 297.8 (+2%) | 852.9 (-27%) | 133.9 (-65%) | 21.3 (-17%) |
| rabin64K | 5.5 (+0%) | 13.0 (+0%) | 133.3 (+1%) | 89.5 (+0%) | 294.6 (+1%) | 848.6 (-28%) | 142.7 (-63%) | 23.8 (-8%) |
| hybrid_size<=64K->fastcdc16K | 5.4 (-1%) | 12.9 (-1%) | 133.4 (+1%) | 91.1 (+2%) | 297.7 (+2%) | 852.8 (-27%) | 133.1 (-65%) | 22.9 (-11%) |
| hybrid_size<=256K->fastcdc64K | 5.5 (+0%) | 13.0 (+0%) | 132.3 (-0%) | 88.0 (-1%) | 296.2 (+2%) | 849.0 (-28%) | 140.3 (-64%) | 25.6 (-1%) |
| hybrid_type | 5.5 (+0%) | 13.0 (+0%) | 132.3 (+0%) | 89.2 (+0%) | 297.7 (+2%) | 852.5 (-27%) | 140.3 (-64%) | 25.8 (+0%) |

Reading the table:

- Source text (ripgrep, tokio, django, linux): `whole`, `fastcdc64K`, `fastcdc256K`, and both hybrids are within 1 to
  2 percent of each other. Going below 64 KiB average loses (django +7 percent at FastCDC 4 KiB, +19 percent at fixed
  4 KiB; linux +15 and +33 percent). Only ripgrep gains at 4 KiB (-10 to -11 percent).
- Why ripgrep differs: its sampled revisions contain many edited mid-size files, so partial-file dedup outweighs the
  per-chunk compression loss and index cost. This is one corpus; I did not find a cheap predictor in these runs.
- Asset container: FastCDC and Rabin at 4 to 16 KiB give -65 percent; FastCDC 64 KiB -64 percent; 256 KiB only -56
  percent (the inserted 100 KB is smaller than one chunk, so the chunk straddling it is re-stored); fixed chunks
  -48 to -49 percent.
- Model checkpoints: every strategy from 4 KiB to 256 KiB lands at -25 to -28 percent, the theoretical bound for this
  construction, and `whole` loses all of it. The 64 KiB strategies are best because 4 KiB pays 2.6 percent overhead.
- Sparse embedding table: 2 percent of rows (1 KiB each) are rewritten at random positions, which touches most large
  chunks. Fixed 4 and 16 KiB reach -28 percent, fixed 64 KiB -12 percent, FastCDC 64 KiB only -1 percent (its realised
  average chunk is 208 KB, because zero rows suppress cuts), FastCDC 256 KiB 0 percent.

## 4. Metadata overhead

| dataset | strategy | unique chunks | payload zstd3 MiB | manifest MiB* | index MiB* | overhead % of payload |
|---|---|---|---|---|---|---|
| django | whole | 46600 | 130.4 | 0.00 | 1.96 | 1.5 |
| django | fixed4K | 130155 | 147.9 | 4.08 | 5.46 | 6.5 |
| django | fastcdc4K | 101841 | 133.5 | 3.44 | 4.27 | 5.8 |
| django | fastcdc16K | 57299 | 132.3 | 0.77 | 2.40 | 2.4 |
| django | fastcdc64K | 47717 | 130.7 | 0.09 | 2.00 | 1.6 |
| django | fastcdc256K | 46664 | 130.4 | 0.01 | 1.96 | 1.5 |
| godot_demo | whole | 11498 | 291.3 | 0.00 | 0.48 | 0.2 |
| godot_demo | fixed4K | 106468 | 301.3 | 3.45 | 4.47 | 2.6 |
| godot_demo | fastcdc4K | 83343 | 294.0 | 2.74 | 3.50 | 2.1 |
| godot_demo | fastcdc16K | 29190 | 295.8 | 0.68 | 1.22 | 0.6 |
| godot_demo | fastcdc64K | 15808 | 295.5 | 0.17 | 0.66 | 0.3 |
| godot_demo | fastcdc256K | 12472 | 294.5 | 0.04 | 0.52 | 0.2 |
| synth_ml | whole | 9 | 1172.7 | 0.00 | 0.00 | 0.0 |
| synth_ml | fixed4K | 248508 | 859.4 | 11.81 | 10.43 | 2.6 |
| synth_ml | fastcdc4K | 204071 | 856.9 | 9.70 | 8.56 | 2.1 |
| synth_ml | fastcdc16K | 51076 | 848.2 | 2.43 | 2.14 | 0.5 |
| synth_ml | fastcdc64K | 12828 | 847.9 | 0.61 | 0.54 | 0.1 |
| synth_ml | fastcdc256K | 3234 | 852.2 | 0.15 | 0.14 | 0.0 |
| synth_assets | whole | 3 | 385.2 | 0.00 | 0.00 | 0.0 |
| synth_assets | fixed4K | 49849 | 195.2 | 3.39 | 2.09 | 2.8 |
| synth_assets | fastcdc4K | 26971 | 129.9 | 2.76 | 1.13 | 3.0 |
| synth_assets | fastcdc16K | 6800 | 132.2 | 0.69 | 0.29 | 0.7 |
| synth_assets | fastcdc64K | 1811 | 140.1 | 0.17 | 0.08 | 0.2 |
| synth_assets | fastcdc256K | 514 | 169.3 | 0.04 | 0.02 | 0.0 |

(* modelled with 36 bytes per manifest entry and 44 bytes per index entry; not measured on disk.)

At 4 KiB average the overhead is 2.1 to 6.5 percent of payload and the number of objects or index entries grows 2.2 to
2.8 times on django (46 600 chunks whole, 101 841 FastCDC 4 KiB, 130 155 fixed 4 KiB). At 64 KiB average it is under 2 percent everywhere.
In-memory dedup index (MODEL, 36 bytes per chunk plus 24 per blob): 2.7 MiB for django whole, 4.6 MiB for FastCDC 4 KiB;
this does not matter at these sizes. HYPOTHESIS: it starts to matter at hundreds of millions of chunks, which none of
these corpora reach.

## 5. Dedup across revisions

Bytes added by the last sampled revision when all earlier revisions are already stored (zstd3, before overhead):

| dataset | strategy | last-revision logical MiB | new unique raw MiB | new unique zstd3 MiB |
|---|---|---|---|---|
| ripgrep | whole | 3.1 | 1.24 | 0.33 |
| ripgrep | fixed16K | 3.1 | 1.04 | 0.32 |
| ripgrep | fastcdc16K | 3.1 | 0.87 | 0.26 |
| ripgrep | fastcdc64K | 3.1 | 1.12 | 0.31 |
| ripgrep | hybrid_type | 3.1 | 1.24 | 0.33 |
| django | whole | 44.1 | 29.50 | 8.57 |
| django | fixed16K | 44.1 | 29.22 | 9.06 |
| django | fastcdc16K | 44.1 | 28.59 | 8.80 |
| django | fastcdc64K | 44.1 | 29.38 | 8.61 |
| django | hybrid_type | 44.1 | 29.50 | 8.57 |
| godot_demo | whole | 298.4 | 33.56 | 21.52 |
| godot_demo | fixed16K | 298.4 | 30.93 | 21.68 |
| godot_demo | fastcdc16K | 298.4 | 28.91 | 21.24 |
| godot_demo | fastcdc64K | 298.4 | 30.61 | 21.55 |
| godot_demo | hybrid_type | 298.4 | 31.69 | 21.87 |
| synth_ml | whole | 448.0 | 448.01 | 390.93 |
| synth_ml | fixed16K | 448.0 | 448.01 | 391.54 |
| synth_ml | fastcdc16K | 448.0 | 448.01 | 391.35 |
| synth_ml | fastcdc64K | 448.0 | 448.01 | 390.70 |
| synth_ml | hybrid_type | 448.0 | 448.01 | 390.71 |
| synth_assets | whole | 128.4 | 128.42 | 128.42 |
| synth_assets | fixed16K | 128.4 | 0.45 | 0.45 |
| synth_assets | fastcdc16K | 128.4 | 0.49 | 0.49 |
| synth_assets | fastcdc64K | 128.4 | 0.54 | 0.54 |
| synth_assets | hybrid_type | 128.4 | 0.54 | 0.54 |

- ripgrep: FastCDC 16 KiB adds 0.87 MiB raw versus 1.24 MiB for whole-file (-30 percent raw, -21 percent compressed).
- django and godot: whole-file, FastCDC 16 KiB and 64 KiB add the same compressed bytes within 6 percent; fixed 16 KiB
  is 5.7 percent larger on django (9.06 versus 8.57 MiB) and FastCDC 16 KiB 2.7 percent larger (8.80 MiB), because small
  chunks compress worse.
- synth_assets: the last revision (one rewritten asset after an inserted one) costs 128 MiB under whole-file and
  0.45 to 0.54 MiB under any chunking, a 99.6 percent reduction.
- synth_ml: the last revision is a full fine-tune; nothing dedups under any strategy (448 MiB new raw, 391 MiB after
  zstd3).
  Compression, not chunking, is the only lever there (see the compression document).

## 6. Dedup across zones

| dataset | zone definition | zones | whole: per-zone extra MiB (% of payload) | fastcdc16K extra MiB (%) | fastcdc4K extra MiB (%) |
|---|---|---|---|---|---|
| ripgrep | depth 1 | n/a (depth 1 run before zone count was recorded) | 0.03 (0.64%) | 0.04 (0.85%) | 0.09 (1.92%) |
| tokio | depth 1 | n/a (depth 1 run before zone count was recorded) | 0.08 (0.64%) | 0.09 (0.68%) | 0.11 (0.92%) |
| django | depth 2 | 303 | 0.44 (0.33%) | 0.46 (0.35%) | 0.62 (0.46%) |
| linux_fs | depth 2 (fs/<subsystem>) | 100 | 0.04 (0.04%) | 0.09 (0.10%) | 0.16 (0.17%) |
| godot_demo | depth 2 (one zone per demo project) | 155 | 23.93 (8.21%) | 25.39 (8.58%) | 25.56 (8.69%) |

- Separate stores per zone cost 0.04 to 0.7 percent payload with whole-file storage on the four code corpora (up to
  1.9 percent for ripgrep with FastCDC 4 KiB), and add few index entries (26 KB under the model for django whole,
  281 KB for godot FastCDC 4 KiB).
- godot-demo-projects: 8.2 to 8.7 percent more payload when each demo project is its own dedup domain. The same is true
  for whole-file storage (8.21 percent) and for every CDC variant, so the cross-zone saving is from byte-identical files
  (shared icons and assets), not from partial overlap.
- Consequence: a global content-addressed namespace shared by zones is worth about 8 percent on asset-heavy trees and
  almost nothing on code, and it does not need chunking to get that.

## 7. Throughput and memory

| strategy | chunk+hash MB/s (1 thread, median of 3) | avg chunk bytes (django) | peak RSS delta KiB |
|---|---|---|---|
| whole | 2288 | 9404 | 8 |
| fixed4K | 1845 | 3252 | 0 |
| fixed16K | 2268 | 7185 | 0 |
| fixed64K | 2249 | 9200 | 0 |
| fastcdc4K | 980 | 3819 | 64 |
| fastcdc16K | 1303 | 7464 | 0 |
| fastcdc64K | 1691 | 9155 | 0 |
| fastcdc256K | 2132 | 9389 | 0 |
| rabin4K | 267 | 3689 | 0 |
| rabin16K | 282 | 6987 | 0 |
| rabin64K | 290 | 9026 | 0 |
| hybrid_size<=64K->fastcdc16K | 1926 | 8640 | 0 |
| hybrid_size<=256K->fastcdc64K | 2269 | 9382 | 0 |
| hybrid_type | 2310 | 9404 | 0 |

- Single-thread chunk+hash on this box (loaded): whole-file blake3 2288 MB/s, fixed 1845 to 2268 MB/s, FastCDC 4 KiB
  980 MB/s, FastCDC 16 KiB 1303 MB/s, FastCDC 64 KiB 1691 MB/s, Rabin 267 to 290 MB/s. A 100 GB checkpoint therefore
  costs about 43 s (whole), 77 s (FastCDC 16 KiB), 102 s (FastCDC 4 KiB) or 6 minutes (Rabin) of one core, before
  compression.  Only the median of the 3 repetitions is stored in the JSON (no min/max for this pass).
- Memory: the chunking pass itself allocates only the chunk list; peak RSS delta was below 100 KiB (the measurement is
  coarse because of allocator reuse). The dominating memory is the dedup map, section 4.

## 8. Negative results

1. CDC is not a general win for source text. At 4 KiB average it loses 7 to 15 percent on django and linux, and fixed
   4 KiB loses 19 to 33 percent. Only ripgrep gains.
2. Hybrid-by-size and hybrid-by-type did not beat the simple choice: `hybrid_type` equals `whole` on source text and
   FastCDC 256 KiB on models. `hybrid_size<=256K->fastcdc64K` is within 1.5 percent of the best strategy on 5 of 8
   corpora (tokio, django, linux, godot, synth_ml) but 5 percent off on assets, 12 percent off on ripgrep and 39
   percent off on synth_sparse, where only small chunks work.
3. Rabin: 4 to 8 times slower, no consistently better dedup, and degenerate on zero runs: on synth_sparse Rabin 4 KiB produced 17 983
   chunks averaging 1.8 KB (its manifest model is 8.2 MiB versus 0.7 MiB for FastCDC 4 KiB) because a zero window has
   fingerprint 0, which satisfies the cut condition at every minimum-size boundary.
4. Replaced (not edited) binaries: godot-demo-projects has 83.8 to 84.2 percent cross-revision gain under every
   strategy including `whole`; chunking only adds overhead there.
5. 256 KiB FastCDC on a 100 KB insertion (assets): -56 percent versus -65 percent at 4 to 16 KiB. Too coarse for small
   edits.
6. Per-zone namespaces: cost is real only for duplicated whole files.
7. Fixed-size chunks do fine on the aligned synthetic checkpoints (best at -28 percent) and badly on the insertion
   workload; the checkpoint result is flattered by the synthetic alignment (HYPOTHESIS above).

## 9. Recommendation per object class

| class | recommendation | evidence | status |
|---|---|---|---|
| source text (< 1 MiB) | store whole-file objects; no chunking | sections 3, 8.1 | CONFIRMED on 4 of 5 corpora |
| small binary (< 1 MiB) | whole-file | same data; no chunking win | CONFIRMED |
| large binary (>= 1 MiB, edited in place / inserted into) | FastCDC, 16 KiB average (min 4 KiB, max 64 KiB); fall back to 64 KiB average if the index budget matters (-64 percent versus -65 percent, 4 times fewer chunks) | synth_assets | CONFIRMED on synthetic, HYPOTHESIS on real files |
| large binary (typically replaced whole, e.g. compressed images) | whole-file; the chunking cost is +1 to +6 percent | godot_demo | CONFIRMED |
| model checkpoint | chunk with FastCDC or fixed at 64 KiB to 256 KiB average; compress the chunks (see compression doc) | synth_ml | HYPOTHESIS for real checkpoints |
| any class, size-driven rule | whole file at or below 256 KiB, FastCDC 64 KiB average above (`hybrid_size<=256K->fastcdc64K`) is a reasonable single default: within 1.5 percent of the best on 5 of 8 corpora, never more than 12 percent off except on the sparse table (39 percent) | section 3 | CONFIRMED as measured |

## 10. Limitations

- Eight corpora, one run each for dedup (dedup is deterministic; throughput has 3 repetitions). Boundaries between
  strategies below 2 percent should not be trusted.
- Revisions are sampled, not consecutive commits; consecutive commits would show more dedup for every strategy and favour
  small chunks more.
- Manifest and index overhead is a model. The on-disk format (pack index, chunk tables, per-object headers) of allgit
  is not implemented here, and no GET/PUT request costs are modelled; those belong to the other storage experiments.
- Compression after chunking uses zstd3 per chunk with no dictionary. Dictionary training over chunks would shift
  the 4 KiB results.
- Windowed (sub-file) delta compression between versions is not a chunking strategy here, but it is much stronger for
  text than any chunking measured: see the block experiment in the compression document.

## 11. Reproduce

```sh
# on the box, from the workspace root (repo clones under $R, synthetic data under $D)
cd crates/vcs-compress-lab && cargo build --release && cd ../..
crates/vcs-compress-lab/target/release/compress-lab synth ml $D/synth_ml 256
crates/vcs-compress-lab/target/release/compress-lab synth assets $D/synth_assets 128
crates/vcs-compress-lab/target/release/compress-lab synth sparse $D/synth_sparse 128
sh crates/vcs-compress-lab/run_all.sh ripgrep tokio django godot_demo linux_fs synth_assets synth_sparse synth_ml
# zone depth 2 reruns for linux_fs, godot_demo, django:
compress-lab chunking --name linux_fs --source git:$R/linux:12:fs --zone-depth 2 --out linux_fs_chunking.json
python3 -I crates/vcs-compress-lab/render.py chunking data/compress-lab/django_chunking.json   # full per-dataset table
```

Per-dataset full tables (all 14 strategies, with per-revision detail) are in the JSON and are rendered by `render.py`.
Commands for this document were run with `--reps 3 --threads 8` (defaults) on the machine in section 2.1.
