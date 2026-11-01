# Compression and block-layout experiments (issue #183, experiments B and I)

Status: v1, measured 2026-10-31. Epic #190. Companion: [chunking-experiments.md](chunking-experiments.md). Harness:
`crates/vcs-compress-lab` (standalone workspace; no product code touched). Raw data: `data/compress-lab/*_codecs.json`
and `*_blocks_{path,firstseen}.json`. Every number below is read from those files unless marked MODEL or from the git
comparison in section 7.

## 1. Summary

CONFIRMED (measured, re-runnable, see section 11):

1. zstd level 3 is a good per-object default for every class: it delivers 91 to 97 percent of the ratio of zstd 9
   and 86 to 91 percent of zstd 19 at 3 to 4 times (vs zstd 9) and 27 to 70 times (vs zstd 19) the compression speed
   (source text: ratio 3.55 to 5.73 at 136 MB/s; zstd 9 is +8 to 10 percent at 34 MB/s; zstd 19 +12 to 17 percent at
   2 MB/s). Decompression speed is roughly level-independent (about 700 MB/s on text, 3.7 GB/s on incompressible data).
2. LZ4 (`lz4_flex`, pure Rust block format) is about 2x faster to compress and 2x faster to decompress than zstd 3 on
   text but reaches only 62 to 68 percent of its ratio (2.33 to 3.56 versus 3.55 to 5.73, i.e. 47 to 61 percent more
   bytes). On synthetic and bf16 weights LZ4 expands the data (ratio 0.996); on real fp32 GPT-2 weights it gets 1.137.
3. Brotli 11 beats zstd 19 on text by 10 to 14 percent (ratio 4.5 to 7.6 versus 4.1 to 6.7) but compresses at about
   1 MB/s and decompresses at 230 to 270 MB/s; it is an offline/archival option only. Brotli 5 is 3 to 5 percent better
   than zstd 9 in ratio on text, at 0.7 times its compress speed and 2.8 times slower decompression.
4. A trained zstd dictionary (112 KiB, trained on files with disjoint paths from the test files) cuts the size of source
   files under 1 KiB by 33 to 57 percent, 1 to 4 KiB by 24 to 30 percent, 4 to 32 KiB by 12 to 17 percent, and files over
   32 KiB by only 3 to 11 percent. On small binaries (django `.mo` files) it helps 11 to 64 percent; on game assets
   (godot) it helps nothing (-0.2 to 8 percent, 0.1 percent on 4 to 32 KiB files).
5. The biggest lever for text is not the codec but what is compressed together. Compressing versions of the same file in
   the same block (blocks filled in path order) gives ratio 11 to 18 at 1 to 16 MiB blocks, versus 3.5 to 5.7 for
   per-object zstd 3. With blocks filled in first-seen order (versions far apart) the ratio is 4.2 to 8.8. Block size
   then trades ratio against random-access cost (section 6).
6. For float model weights, byte-plane shuffling before zstd 3 saves 7.8 to 8.9 percent of the bytes on real models
   (GPT-2 fp32: ratio 1.227 to 1.331; SmolLM2 bf16: 1.277 to 1.402) and 7.6 to 8.7 percent on the synthetic equivalents,
   at 0.56 (fp32, 4-byte planes) to 0.33 (bf16) of the plain-zstd compress speed. int8 weights gain nothing.
7. Sampling-based "adaptive" codec choice (LZ4 probe on the first 16 KiB, store raw if it does not compress below 95
   percent) is a negative result: it is 6.5 percent worse than always using zstd 3 on large game assets and 8 to 28
   percent worse on float weights, because LZ4 does not see the structure zstd finds there.

HYPOTHESIS (not established by these runs):

- Real checkpoints other than the two tested; real corpora other than the five repositories; behaviour on SSD/NVMe
  versus the VM disk used here for the "cold" reads.
- Offline recompression (brotli 11 or zstd 19 during compaction) is a net win; I measured only the ratio and the speed.

## 2. Method

### 2.1 Machine, tools, commands

| item | value |
|---|---|
| host | Lambda box `lambda-final`, AMD EPYC 7J13, 30 vCPUs visible, 216 GB RAM, Linux 6.8.0-1046-nvidia, local disk |
| load | shared box; load average at run start 2 to 29, recorded in each JSON (`machine.loadavg_at_start`). All timings are single-threaded and noisy; medians of 3 repetitions (1 for the slow codecs zstd 19, brotli 9, brotli 11) with min and max in the JSON. |
| Rust | rustc 1.99.1 (2026-10-13), release build, standalone workspace `crates/vcs-compress-lab` |
| codec crates | zstd 0.14.3 (zstd-safe 8.2.0, libzstd 1.5.7), lz4_flex 0.14.0 (pure Rust, block format), brotli 9.1.1 (pure Rust, lgwin 22), fastcdc 5.0.0, blake3 1.x |
| git | version in `data/compress-lab/gitpack_noreuse.log` header (section 7) |

Crates.io availability (checked 2026-10-31 with `cargo search`, then built on the box): zstd 0.14.3, lz4_flex 0.14.0,
brotli 9.1.1 and fastcdc 5.0.0 all exist and all built without problems on Rust 1.99.1. Not tried: the C `lz4` crate
(only the pure Rust `lz4_flex` was used; its absolute speeds are not those of the reference C LZ4), `zstd` seekable
format, and `zstd --long`.

Datasets: the same five public repositories and synthetic sets as in [chunking-experiments.md](chunking-experiments.md)
section 2.2 (URLs, revisions, licenses, access date 2026-10-31; full revision lists in `data/compress-lab/datasets.json`),
plus two real model files downloaded on 2026-10-31:

| model | URL | license (from the model card front matter, read 2026-10-31) | file | sha256 |
|---|---|---|---|---|
| GPT-2 (fp32) | https://huggingface.co/openai-community/gpt2 (api sha 607a30d7) | MIT | model.safetensors, 548 105 171 bytes | 248dfc3911869ec4... |
| SmolLM2-135M (bf16) | https://huggingface.co/HuggingFaceTB/SmolLM2-135M (api sha 93efa2f0) | Apache-2.0 | model.safetensors, 269 060 552 bytes | 80521b40281d6ce7... |

### 2.2 What is measured

- *Codec experiment* (`compress-lab codecs`): per object class (classification in the chunking document), each file is
  compressed on its own with each codec; physical bytes = sum of compressed sizes. Sample: at most 64 MiB per class,
  chosen by a deterministic stride over files; a file larger than 16 MiB is represented by a contiguous window from its
  middle (and the window is shrunk, floor 1 MiB, when large files alone would exceed half of the cap). Model files
  were run per file with a 32 MiB middle window (GPT-2, SmolLM2, synthetic r1 files), so one data point per tensor mix.
  Speeds are MB/s of uncompressed bytes, single thread, median of 3 (min-max in JSON). "Peak RSS delta" is the growth of
  VmHWM during the phase after resetting it via `/proc/self/clear_refs`; values under about 1 MiB are not meaningful
  (allocator reuse) and the input data is included for in-memory phases.
- *Dictionaries*: trained with `zstd::dict::from_samples` on files whose blake3(path) low bit is 0 (up to 4000 samples,
  each truncated to 64 KiB) and tested on files with bit 1, so versions of the same file never appear on both sides.
  The dictionary itself (16 or 112 KiB) is not counted in the tested bytes; per-file savings below are before paying
  for the dictionary (one dictionary per class amortises to nothing at repository scale, but each reader must fetch it).
- *Adaptive policies* are computed from the per-file results: `oracle` = best codec per file (not implementable
  without compressing everything), restricted to none/lz4/zstd1/zstd3 or to all 9 codecs; `probe` = LZ4 on the first
  16 KiB, store raw if the ratio is not below 0.95, else zstd 3.
- *Block experiment* (`compress-lab blocks`): the unique blobs of a class are concatenated into a stream (cap 256 MiB;
  a stream starts with whole blobs in the chosen order) and cut into independently compressed blocks of 64 KiB, 256 KiB,
  1 MiB, 4 MiB, 16 MiB, plus one solid stream as the ceiling. `--order path` sorts blobs by (path, first-seen) so versions
  of one file are adjacent; `--order first-seen` keeps revision order. Block index is modelled at 12 bytes per block
  (offset plus length). Random-access cost: 2000 random 4 KiB reads (300 for blocks of 4 MiB and larger, 3 for solid)
  and one tenth as many 1 MiB range reads, each decompressing every block it touches ("warm": all blocks in memory);
  a "cold-file" 4 KiB read writes the compressed blocks to a file, calls `posix_fadvise(DONTNEED)` before each read and
  times `pread` plus decompress (the page cache is evicted per read; this is a lower bound for a cold device, not an S3
  GET). No cloud was used: network and request latency are not modelled.

## 3. Codec results

Compression ratio is uncompressed / compressed (bigger is better); percent differences below are physical-byte
differences. The [benchmark-methodology.md](benchmark-methodology.md) `compression_ratio` is the inverse.

### 3.1 Source text (per-file compression)

| codec | ripgrep ratio | tokio ratio | django ratio | linux_fs ratio | godot_demo ratio | compress MB/s (largest set) | decompress MB/s |
|---|---|---|---|---|---|---|---|
| lz4 | 2.58 | 2.51 | 2.49 | 2.33 | 3.56 | 296 | 1399 |
| zstd1 | 3.70 | 3.59 | 3.51 | 3.36 | 5.59 | 182 | 707 |
| zstd3 | 3.88 | 3.73 | 3.64 | 3.55 | 5.73 | 136 | 701 |
| zstd9 | 4.21 | 4.03 | 3.94 | 3.91 | 6.26 | 34 | 736 |
| zstd19 | 4.37 | 4.15 | 4.07 | 4.11 | 6.69 | 2 | 697 |
| brotli5 | 4.32 | 4.19 | 4.12 | 4.01 | 6.53 | 24 | 262 |
| brotli9 | 4.40 | 4.25 | 4.18 | 4.09 | 6.74 | 2 | 260 |
| brotli11 | 4.88 | 4.67 | 4.65 | 4.52 | 7.56 | 1 | 229 |

Samples are 21 to 61 MiB per corpus (the sampling cap is 64 MiB). Compared with zstd 3, zstd 9 needs 7 to 9 percent
fewer bytes, zstd 19 11 to 14 percent fewer, brotli 11 20 to 24 percent fewer (ratio +25 to +32 percent). Brotli's gain is
largest on small files; it ships a built-in static dictionary (RFC 7932), which I believe explains much of it
[unverified here: not isolated by an experiment]. The per-file oracle including brotli 11 reaches 4.30 MiB versus
5.41 MiB for zstd 3 on ripgrep. Speeds from django (59.5 MiB sample), one thread: lz4 296 MB/s, zstd1 182, zstd3 136, zstd9 34, zstd19 2,
brotli5 24, brotli9 2, brotli11 1; decompression 1399 / 707 / 701 / 736 / 697 / 262 / 260 / 229 MB/s.

### 3.2 Small binaries (< 1 MiB, non-text)

| codec | django ratio | godot_demo ratio | compress MB/s (largest set) | decompress MB/s |
|---|---|---|---|---|
| lz4 | 1.59 | 1.04 | 1668 | 5806 |
| zstd1 | 2.01 | 1.06 | 631 | 4200 |
| zstd3 | 2.05 | 1.07 | 418 | 3764 |
| zstd9 | 2.11 | 1.07 | 163 | 3966 |
| zstd19 | 2.25 | 1.08 | 11 | 2079 |
| brotli5 | 2.26 | 1.07 | 66 | 508 |
| brotli9 | 2.28 | 1.08 | 7 | 475 |
| brotli11 | 2.50 | 1.09 | 1 | 302 |

django is `.mo` translation catalogs and similar (compressible, ratio 2.0); godot small binaries (png, import files,
audio) are essentially incompressible (1.04 to 1.09). zstd stores incompressible blocks raw: on godot zstd 3 compresses
at 418 MB/s and decompresses at 3.8 GB/s, so there is no throughput reason to skip compression for them.

### 3.3 Large binaries (>= 1 MiB)

| codec | godot_demo ratio | compress MB/s (largest set) | decompress MB/s |
|---|---|---|---|
| lz4 | 1.11 | 590 | 3529 |
| zstd1 | 1.19 | 337 | 1397 |
| zstd3 | 1.28 | 137 | 1111 |
| zstd9 | 1.34 | 53 | 1145 |
| zstd19 | 1.42 | 5 | 690 |
| brotli5 | 1.37 | 21 | 171 |
| brotli9 | 1.40 | 5 | 177 |
| brotli11 | 1.48 | 0 | 133 |

godot large assets (textures, models, audio; 51.6 MiB sample): zstd 3 saves 22 percent, zstd 19 29 percent (4.7 MB/s).
Fully random data (the synthetic asset container, 32 MiB sample) does not compress under any codec: ratio 1.000 for
zstd and brotli to three decimals, 0.996 for lz4 (0.4 percent expansion); zstd stores incompressible blocks raw, so zstd
3 compresses it at 2.1 GB/s and decompresses at 14.8 GB/s (NEGATIVE: nothing to gain, but zstd's fallback makes trying
it nearly free; brotli 11 compresses it at 1 MB/s).

### 3.4 Model checkpoints

| codec | synthetic fp32 | synthetic bf16 | synthetic int8 | synthetic sparse fp32 | real GPT-2 fp32 | real SmolLM2 bf16 | compress MB/s (GPT-2) | decompress MB/s (GPT-2) |
|---|---|---|---|---|---|---|---|---|
| lz4 | 0.996 | 0.996 | 0.996 | 9.588 | 1.137 | 0.996 | 1472 | 933 |
| zstd1 | 1.080 | 1.283 | 1.187 | 10.724 | 1.228 | 1.277 | 463 | 654 |
| zstd3 | 1.079 | 1.282 | 1.187 | 10.726 | 1.227 | 1.277 | 324 | 590 |
| zstd9 | 1.080 | 1.297 | 1.187 | 10.757 | 1.228 | 1.288 | 274 | 667 |
| zstd19 | 1.080 | 1.298 | 1.186 | 10.711 | 1.227 | 1.297 | 5 | 623 |
| brotli5 | 1.080 | 1.281 | 1.192 | 10.759 | 1.229 | 1.277 | 53 | 138 |
| brotli9 | 1.080 | 1.286 | 1.192 | 10.777 | 1.229 | 1.280 | 22 | 136 |
| brotli11 | 1.091 | 1.399 | 1.192 | 10.854 | 1.242 | 1.390 | 1 | 94 |
| shuf2+zstd3 | 1.123 | 1.404 | 1.187 | 11.084 | 1.276 | 1.402 | 304 | 500 |
| shuf4+zstd3 | 1.168 | 1.404 | 1.187 | 11.257 | 1.331 | 1.402 | 183 | 533 |
| shuf4+zstd9 | 1.176 | 1.428 | 1.187 | 11.424 | 1.341 | 1.428 | 86 | 537 |

Columns: synthetic = Gaussian weights from `compress-lab synth ml` (32 MiB window per file), real = the downloaded model
files (one 32 MiB window each). Speeds in the last two columns are for real GPT-2 fp32, one thread.

- Plain zstd gets 1.08 on synthetic fp32 but 1.23 on real GPT-2 fp32 (real weights have structure: smaller exponent
  entropy, embedding rows). Synthetic bf16 (1.282) is close to real SmolLM2 bf16 (1.277). Treat the synthetic fp32 ratio
  as pessimistic.
- Byte-plane shuffle: shuf4+zstd3 on real GPT-2 fp32 gives 1.331 (7.8 percent fewer bytes than zstd 3). On bf16 the
  2-byte shuffle gives 1.402 (8.9 percent fewer bytes). On int8 the shuffle is a no-op. Compress speed drops from 324 to 183
  MB/s (fp32) and decompress from 590 to 533 MB/s. shuf4+zstd9 adds another 0.8 to 1.9 percent in ratio at 2 to 7 times lower speed.
- Sparse embedding table (90 percent zero rows): ratio 9.6 (lz4) to 10.9 (brotli 11) whatever the codec; shuffle
  gives +5 to +6.5 percent (11.26 for shuf4+zstd3, 11.42 for shuf4+zstd9); zstd 3 compresses at 1.1 GB/s.
- Brotli 11 beats plain zstd on bf16 (1.39 to 1.40 versus 1.28) and roughly ties shuffle+zstd 3 (1.40), at 1 MB/s.
- Peak memory (RSS growth during compression of one 32 MiB window): zstd 1/3 25 to 31 MiB (this is the output buffer),
  zstd 9 35 to 41 MiB, zstd 19 107 to 112 MiB, brotli 5 36 to 64 MiB, brotli 9 61 to 78 MiB, brotli 11 65 to 94 MiB.
  Decompression needs about the output size.

### 3.5 Dictionaries

Source text, zstd 3 with a 112 KiB dictionary (the 16 KiB dictionary and zstd 9 columns show the sensitivity). Test
files have paths disjoint from training files, but belong to the same repository: gains across unrelated repositories
were not tested.

| dataset | size bucket | files | zstd3 no dict KiB | zstd3 + 112 KiB dict KiB | gain % | zstd3 + 16 KiB dict gain % | zstd9 + 112 KiB dict gain % |
|---|---|---|---|---|---|---|---|
| ripgrep | [0,1024) | 114 | 35 | 20 | 41.3 | 25.8 | 43.7 |
| ripgrep | [1024,4096) | 122 | 113 | 86 | 24.0 | 15.3 | 29.2 |
| ripgrep | [4096,32768) | 281 | 1083 | 929 | 14.2 | 8.0 | 19.5 |
| ripgrep | [32768,262145) | 92 | 1037 | 920 | 11.3 | 4.7 | 15.5 |
| tokio | [0,1024) | 511 | 158 | 98 | 37.9 | 27.9 | 41.9 |
| tokio | [1024,4096) | 1072 | 888 | 629 | 29.1 | 22.8 | 35.0 |
| tokio | [4096,32768) | 1262 | 3880 | 3253 | 16.2 | 11.2 | 21.8 |
| tokio | [32768,262145) | 99 | 943 | 847 | 10.2 | 6.1 | 15.4 |
| django | [0,1024) | 634 | 190 | 117 | 38.5 | 32.7 | 42.6 |
| django | [1024,4096) | 904 | 783 | 567 | 27.6 | 21.0 | 33.5 |
| django | [4096,32768) | 1265 | 4518 | 3879 | 14.1 | 8.1 | 19.6 |
| django | [32768,262145) | 187 | 2713 | 2555 | 5.8 | 2.7 | 9.6 |
| linux_fs | [0,1024) | 126 | 40 | 27 | 32.7 | 26.7 | 38.5 |
| linux_fs | [1024,4096) | 374 | 372 | 277 | 25.7 | 20.9 | 33.4 |
| linux_fs | [4096,32768) | 905 | 3720 | 3283 | 11.8 | 8.2 | 17.7 |
| linux_fs | [32768,262145) | 248 | 4375 | 4141 | 5.3 | 3.0 | 8.6 |
| godot_demo | [0,1024) | 3229 | 968 | 421 | 56.5 | 50.1 | 60.7 |
| godot_demo | [1024,4096) | 1189 | 930 | 655 | 29.6 | 22.4 | 35.6 |
| godot_demo | [4096,32768) | 627 | 1178 | 975 | 17.2 | 13.5 | 23.5 |
| godot_demo | [32768,262145) | 66 | 611 | 593 | 2.9 | 2.6 | 5.5 |

Small binaries:

| dataset | size bucket | files | zstd3 no dict KiB | zstd3 + 112 KiB dict KiB | gain % |
|---|---|---|---|---|---|
| django | [0,1024) | 664 | 313 | 111 | 64.4 |
| django | [1024,4096) | 1037 | 1176 | 632 | 46.2 |
| django | [4096,32768) | 945 | 4615 | 3326 | 27.9 |
| django | [32768,262145) | 82 | 2277 | 2022 | 11.2 |
| godot_demo | [0,1024) | 58 | 26 | 24 | 8.1 |
| godot_demo | [1024,4096) | 71 | 130 | 127 | 2.8 |
| godot_demo | [4096,32768) | 74 | 978 | 977 | 0.1 |
| godot_demo | [32768,262145) | 50 | 5184 | 5193 | -0.2 |

- The gain is concentrated in files below 4 KiB: 24 to 57 percent. Above 32 KiB it is 3 to 11 percent. Over the whole
  test half (files up to 256 KiB, per-file zstd 3), the 112 KiB dictionary saves 9.2 (linux), 13.2 (django), 13.8
  (ripgrep), 17.8 (tokio) and 28.3 percent (godot text, many tiny scene and script files). Files under 32 KiB are 49 to 84
  percent of those compressed bytes.
- A 112 KiB dictionary beats a 16 KiB one by 0.3 to 15.5 percentage points (typically 4 to 8), so a larger dictionary
  is worth its extra fetch for small objects.
- Level and dictionary stack: zstd 9 + dictionary saves 2 to 6 more points than zstd 3 + dictionary.
- Not measured: dictionaries versus block grouping together (section 5 shows that grouping versions of a file in a
  block of 256 KiB or more gives larger gains than any dictionary), and dictionary distribution/versioning cost.

### 3.6 Adaptive per-type choice

Physical MiB per class; `probe` = LZ4 probe policy; `oracle` = best per file.

| dataset / class | zstd3 MiB | zstd19 MiB | oracle(none,lz4,zstd1,zstd3) MiB | probe(lz4 16K) MiB | oracle(all 9 codecs) MiB | probe vs zstd3 |
|---|---|---|---|---|---|---|
| ripgrep / source_text | 5.41 | 4.80 | 5.40 | 5.41 | 4.30 | +0.0% |
| tokio / source_text | 12.74 | 11.44 | 12.74 | 12.74 | 10.17 | +0.0% |
| django / source_text | 15.59 | 13.95 | 15.59 | 15.59 | 12.22 | -0.0% |
| linux_fs / source_text | 17.33 | 14.98 | 17.31 | 17.33 | 13.62 | +0.0% |
| godot_demo / source_text | 9.51 | 8.14 | 9.44 | 9.50 | 7.17 | -0.1% |
| django / small_binary | 17.64 | 16.07 | 17.63 | 17.67 | 14.47 | +0.2% |
| godot_demo / small_binary | 50.97 | 50.28 | 50.96 | 51.37 | 49.82 | +0.8% |
| godot_demo / large_binary | 40.22 | 36.40 | 40.22 | 42.82 | 34.87 | +6.5% |
| synth_ml_r1_fp32 / model_ckpt | 29.66 | 29.63 | 29.62 | 32.00 | 27.20 | +7.9% |
| synth_ml_r1_bf16 / model_ckpt | 24.95 | 24.65 | 24.95 | 32.00 | 22.40 | +28.2% |
| synth_ml_r1_q8 / model_ckpt | 26.96 | 26.97 | 26.96 | 32.00 | 26.84 | +18.7% |
| synth_sparse_r1 / model_ckpt | 2.98 | 2.99 | 2.98 | 2.98 | 2.80 | +0.0% |
| hf_gpt2 / model_ckpt | 26.08 | 26.07 | 26.06 | 32.00 | 23.87 | +22.7% |
| hf_smollm2-135m / model_ckpt | 25.05 | 24.68 | 25.05 | 32.00 | 22.41 | +27.7% |

- zstd 3 as a single choice is within 0.8 percent of the best per-file choice among {none, lz4, zstd1, zstd3} on every
  class (0.7 percent on godot text, under 0.2 percent elsewhere). Nothing is gained by choosing among those four per file.
- The probe policy is never better than zstd 3 and is clearly worse on large game assets (+6.5 percent), float weights
  (+7.9 to +28 percent) and small game binaries (+0.8 percent). Reason: LZ4 reports "incompressible" for data zstd still
  compresses 8 to 30 percent (floating point, structured binary).
- The oracle including brotli 11 is 20 to 25 percent smaller than zstd 3 on text, 2 to 18 percent on binaries and 0 to
  10 percent on float weights, but it is not implementable at write speed (1 MB/s) and needs all nine codecs per file.
  A per-class fixed choice (zstd 3 now, zstd 19 or brotli 11 in an offline compaction pass) is the realistic way to
  collect part of that gap.

## 4. Speed and memory summary (one thread, loaded box)

| codec | compress MB/s (text / large binary / float32) | decompress MB/s (text / large binary / float32) | RSS growth during compress (text files / 32 MiB float window) |
|---|---|---|---|
| lz4_flex | 296 / 590 / 1472 | 1399 / 3529 / 933 | 0.1 MiB / 28 MiB (output) |
| zstd 1 | 182 / 337 / 463 | 707 / 1397 / 654 | 0.2 MiB / 27 MiB (output) |
| zstd 3 | 136 / 137 / 324 | 701 / 1111 / 590 | 0 / 27 MiB (output) |
| zstd 9 | 34 / 53 / 274 | 736 / 1145 / 667 | 0.1 to 10 MiB / 36 MiB |
| zstd 19 | 2 / 5 / 5 | 697 / 690 / 623 | 9 to 17 MiB / 109 MiB |
| brotli 5 | 24 / 21 / 53 | 262 / 171 / 138 | 0.6 MiB / 64 MiB |
| brotli 9 | 2 / 5 / 22 | 260 / 177 / 136 | 31 MiB / 61 MiB |
| brotli 11 | 1 / 0.5 / 1 | 229 / 133 / 94 | 1.4 to 13 MiB / 65 MiB |

(text = django sample, large binary = godot sample, float32 = real GPT-2 window. Memory numbers are coarse (see 2.2):
the 27 to 28 MiB entries are the compressed output buffer of a 32 MiB window, not codec state.)

## 5. Independently compressed blocks: ratio

Ratio of the whole stream (unique blobs of the class) for zstd 3 as a function of block size. `path` = versions of a
file adjacent in the stream, `firstseen` = revision order. Index overhead is 0.1 percent of the compressed size at 64 KiB blocks and under 0.02 percent at 1 MiB and above
(12 bytes per block).

| dataset / class | order | 64K | 256K | 1M | 4M | 16M | solid |
|---|---|---|---|---|---|---|---|
| ripgrep / source_text | path | 5.67 | 9.51 | 13.02 | 14.43 | 14.76 | 14.84 |
| ripgrep / source_text | firstseen | 3.98 | 4.27 | 4.75 | 7.28 | 8.77 | 9.36 |
| tokio / source_text | path | 7.54 | 12.25 | 15.68 | 17.37 | 17.76 | 17.84 |
| tokio / source_text | firstseen | 4.26 | 4.68 | 4.98 | 5.36 | 5.55 | 5.56 |
| django / source_text | path | 6.02 | 10.00 | 13.82 | 15.75 | 16.33 | 16.48 |
| django / source_text | firstseen | 4.16 | 4.61 | 4.92 | 5.04 | 5.08 | 5.09 |
| linux_fs / source_text | path | 4.87 | 7.81 | 10.76 | 12.30 | 12.77 | 12.95 |
| linux_fs / source_text | firstseen | 3.70 | 4.00 | 4.22 | 4.31 | 4.33 | 4.34 |
| godot_demo / source_text | path | 9.51 | 11.19 | 13.25 | 14.42 | 14.88 | 14.90 |
| godot_demo / source_text | firstseen | 7.07 | 7.58 | 8.16 | 8.53 | 8.60 | 8.62 |
| django / small_binary | path | 3.38 | 4.21 | 4.97 | 5.35 | 5.44 | 5.47 |
| django / small_binary | firstseen | 2.80 | 3.29 | 3.61 | 3.75 | 3.78 | 3.79 |
| godot_demo / small_binary | path | 1.08 | 1.08 | 1.09 | 1.10 | 1.11 | 1.11 |
| godot_demo / small_binary | firstseen | 1.08 | 1.08 | 1.09 | 1.09 | 1.10 | 1.10 |
| godot_demo / large_binary | path | 1.20 | 1.25 | 1.25 | 1.26 | 1.26 | 1.26 |
| godot_demo / large_binary | firstseen | 1.20 | 1.25 | 1.25 | 1.26 | 1.26 | 1.26 |
| synth_ml / model_ckpt | path | 1.25 | 1.25 | 1.25 | 1.25 | 1.25 | 1.25 |
| synth_ml / model_ckpt | firstseen | 1.25 | 1.25 | 1.25 | 1.25 | 1.25 | 1.25 |
| synth_sparse / model_ckpt | path | 9.89 | 9.93 | 9.93 | 9.93 | 9.93 | 9.93 |
| synth_sparse / model_ckpt | firstseen | 9.89 | 9.93 | 9.93 | 9.93 | 9.93 | 9.93 |
| synth_assets / large_binary | path | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
| synth_assets / large_binary | firstseen | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |

- Text: ratio improves steeply with block size when similar content is adjacent: django 6.0 (64 KiB), 10.0 (256 KiB),
  13.8 (1 MiB), 15.8 (4 MiB), 16.3 (16 MiB), 16.5 (solid). A 1 MiB block already has 84 percent of the solid ratio and a
  4 MiB block 96 percent. Per-object zstd 3 on the same django text gives 3.64, so block compression with adjacent
  versions is 3.8 to 4.5 times smaller; with 64 KiB blocks it is still 1.65 times smaller.
- Order matters: with first-seen order (versions far apart, so the 2 MiB zstd 3 window rarely contains the earlier
  version) django 4 MiB blocks reach 5.04 instead of 15.75, and ripgrep 16 MiB 8.77 instead of 14.76. The gain therefore
  requires a packer that groups related files; block size alone does not give it.
- Binary or incompressible classes: the ratio is flat across block size (godot large binary 1.20 to 1.26, godot small
  1.08 to 1.11, synthetic checkpoint 1.25, random asset container 1.00). Block size is then a pure access-cost choice.
- LZ4 follows the same pattern at a lower level (django path order: 4.04 at 64 KiB, 5.45 at 16 MiB).

## 6. Random access and range-read cost

Django source text, zstd 3, blocks filled in path order (unique text stream 256 MiB). "Compressed bytes fetched" is
what a ranged GET of the covering block would transfer; "cold-file" is `pread` from a local file after evicting its
page cache plus decompress.

| block | ratio | compress MB/s | decompress MB/s | index KiB* | 4 KiB read warm us (median / p99) | compressed bytes fetched per 4 KiB read (mean KiB) | 4 KiB read cold-file us (median) | 1 MiB range warm us |
|---|---|---|---|---|---|---|---|---|
| 64K | 6.02 | 234 | 1331 | 48.0 | 49 / 133 | 11.4 | 135 | 885 |
| 256K | 10.00 | 337 | 1981 | 12.0 | 124 / 522 | 26.2 | 197 | 650 |
| 1M | 13.82 | 439 | 2559 | 3.0 | 405 / 849 | 74.2 | 512 | 870 |
| 4M | 15.75 | 473 | 2760 | 0.8 | 1547 / 2445 | 263.8 | 1758 | 1709 |
| 16M | 16.33 | 487 | 2699 | 0.2 | 5998 / 7775 | 993.2 | 6754 | 6688 |
| solid | 16.48 | 493 | 972 | 0.0 | 280057 / 281445 | 15908.5 | n/a | 280608 |

Same stream with LZ4:

| block | ratio | compress MB/s | decompress MB/s | index KiB* | 4 KiB read warm us (median / p99) | compressed bytes fetched per 4 KiB read (mean KiB) | 4 KiB read cold-file us (median) | 1 MiB range warm us |
|---|---|---|---|---|---|---|---|---|
| 64K | 4.04 | 440 | 2262 | 48.0 | 32 / 88 | 17.0 | 101 | 543 |
| 256K | 5.03 | 502 | 2601 | 12.0 | 92 / 197 | 52.4 | 210 | 511 |
| 1M | 5.35 | 521 | 2628 | 3.0 | 372 / 759 | 192.3 | 584 | 863 |
| 4M | 5.43 | 529 | 2551 | 0.8 | 1590 / 2858 | 754.4 | 2211 | 1686 |
| 16M | 5.45 | 526 | 2592 | 0.2 | 6666 / 9055 | 3018.5 | 9247 | 6735 |
| solid | 5.46 | 528 | 978 | 0.0 | 274404 / 277875 | 48032.9 | n/a | 275697 |

Large binary (godot, zstd 3; the ratio does not depend on block size, only cost does):

| block | ratio | compress MB/s | decompress MB/s | index KiB* | 4 KiB read warm us (median / p99) | compressed bytes fetched per 4 KiB read (mean KiB) | 4 KiB read cold-file us (median) | 1 MiB range warm us |
|---|---|---|---|---|---|---|---|---|
| 64K | 1.20 | 222 | 1206 | 44.6 | 69 / 179 | 56.8 | 181 | 1163 |
| 256K | 1.25 | 152 | 1081 | 11.2 | 292 / 626 | 209.1 | 507 | 1338 |
| 1M | 1.25 | 158 | 1046 | 2.8 | 1217 / 2019 | 832.0 | 1805 | 2610 |
| 4M | 1.26 | 144 | 1001 | 0.7 | 4580 / 8291 | 3254.9 | 6904 | 5596 |
| 16M | 1.26 | 139 | 987 | 0.2 | 16413 / 29300 | 12730.2 | 27763 | 20904 |
| solid | 1.26 | 132 | 590 | 0.0 | 426674 / 430542 | 193021.6 | n/a | 425789 |

Model-like synthetic checkpoint (zstd 3):

| block | ratio | compress MB/s | decompress MB/s | index KiB* | 4 KiB read warm us (median / p99) | compressed bytes fetched per 4 KiB read (mean KiB) | 4 KiB read cold-file us (median) | 1 MiB range warm us |
|---|---|---|---|---|---|---|---|---|
| 64K | 1.25 | 370 | 815 | 36.0 | 78 / 166 | 54.1 | 182 | 1381 |
| 256K | 1.25 | 299 | 809 | 9.0 | 323 / 653 | 209.5 | 556 | 1664 |
| 1M | 1.25 | 406 | 837 | 2.3 | 1230 / 1319 | 821.2 | 2041 | 2540 |
| 4M | 1.25 | 370 | 770 | 0.6 | 5008 / 6631 | 3285.5 | 7761 | 5239 |
| 16M | 1.25 | 367 | 785 | 0.2 | 21279 / 47939 | 13094.7 | 31967 | 29657 |
| solid | 1.25 | 370 | 613 | 0.0 | 250889 / 254375 | 157359.0 | n/a | 376301 |

- A 4 KiB random read costs one block decompression: 49 us (64 KiB block) to 6 ms (16 MiB) warm on text; time grows
  linearly with block size because decompression runs at about 1 to 2.7 GB/s (text) and 0.8 to 1.2 GB/s (float and
  assets). Read amplification in compressed bytes is 11 KiB (64 KiB blocks), 74 KiB (1 MiB), 264 KiB (4 MiB) per 4 KiB
  requested on text, and 57 KiB, 832 KiB, 3.2 MiB on large binaries (poorly compressible data transfers nearly the
  whole block).
- The solid stream makes a 4 KiB read cost 280 ms on 256 MiB of text: unusable for random access, usable for whole-stream
  scans.
- A 1 MiB range read is roughly flat at 0.65 to 0.9 ms for blocks up to 1 MiB (it touches 1 to 2 blocks), then grows
  with the block size.
- Compression speed rises from 234 MB/s (64 KiB blocks) to 487 MB/s (16 MiB) for zstd 3 on text (per-block setup is
  amortised; I did not isolate the cause). The RSS growth recorded in the JSON for the block experiment mostly
  measures the retained compressed blocks and is not a useful codec-memory number.
- Index overhead (MODEL, 12 bytes per block): 48 KiB for a 256 MiB stream at 64 KiB blocks (0.1 percent of the compressed
  size at ratio 6), 3 KiB at 1 MiB.
- Block sizes between: the Pareto points for text are 256 KiB (ratio 10.0, 124 us / 26 KiB fetched), 1 MiB (13.8, 405 us /
  74 KiB), 4 MiB (15.8, 1.5 ms / 264 KiB). 16 MiB buys 3.7 percent ratio over 4 MiB for 4 times the read cost.

## 7. Block compression versus git delta packs (sanity comparison)

`git pack-objects` (git version in the log) over exactly the unique blobs of the sampled revisions, delta search
disabled/enabled, `--no-reuse-delta --no-reuse-object` so that deltas stored in the clone's packs are not reused;
default zlib level. Log: `data/compress-lab/gitpack_noreuse.log`; script `data/compress-lab/gitpack.sh`.

| dataset | unique blobs | zlib, no deltas (MB) | delta window 10, depth 50 (MB) | delta window 50, depth 50 (MB) | pack time window 50 (s) |
|---|---|---|---|---|---|
| ripgrep | 1286 | 5.33 | 2.55 | 1.70 | 3.25 |
| tokio | 6186 | 12.41 | 8.54 | 6.08 | 12.86 |
| django | 46601 | 128.31 | 107.99 | 89.67 | 154.81 |
| godot_demo | 11499 | 302.89 | 295.90 | 294.91 | 125.39 |
| linux_fs | 16267 | 85.90 | 68.36 | 53.73 | 108.04 |

Compared with the path-ordered zstd 3 blocks above (MB = 10^6 bytes):

- ripgrep (text only; 1 MiB blocks 1.69 MB, 4 MiB blocks 1.52 MB): git pack window 50 is 1.70 MB, window 10 2.55 MB,
  no deltas 5.33 MB; per-file zstd 3 is 5.67 MB.
- tokio (text only; 1 MiB blocks 3.18 MB, 4 MiB blocks 2.87 MB): git window 50 6.08 MB, window 10 8.54 MB, no deltas
  12.41 MB; per-file zstd 3 is 13.36 MB.
- The django, linux and godot block streams are capped at 256 MiB per class while the pack covers all blobs, so they are
  not comparable; the git numbers are in the table for reference.

So grouping versions into 1 to 4 MiB zstd blocks gives, on these two corpora, a size comparable to (ripgrep) or 2 times
smaller than (tokio) a delta pack with window 50 and depth 50. This is not a claim that git is deficient: the pack also
has per-object headers and a separate index file, git's default window is 10, a larger window or depth was not tried,
the compared streams contain no commit or tree objects, and git decompression of a delta chain has different random
access costs which I did not measure. HYPOTHESIS: block-grouped compression can replace deltas for text at similar or
better size; unproven for write throughput and for heavy edit churn.

## 8. Negative results

1. Probe-based adaptive codec selection (LZ4 prefix) loses to fixed zstd 3 (section 3.6).
2. Dictionaries do not help game assets (godot): -0.2 to 8 percent, essentially 0 above 4 KiB. They also give only 3 to
   11 percent on source files above 32 KiB.
3. lz4_flex on synthetic and bf16 weights expands the data (0.996); on real fp32 GPT-2 it gains 13.7 percent (zstd 3:
   22.7 percent).
4. zstd 19 over zstd 9 buys 3 to 7 percent in ratio at 10 to 25 times lower speed; zstd 9 over zstd 3 buys 3 to 10
   percent at 3 to 4 times lower speed.
5. Raising the codec level does nothing for floating point weights (zstd 1 to 19: 1.080 to 1.080 synthetic fp32; 1.228 to
   1.227 real fp32). Only the byte shuffle or brotli's context modelling on bf16 helps.
6. Larger blocks do not help non-text classes: ratio flat from 64 KiB to solid on binaries and checkpoints.
7. First-seen block order: block compression of source text collapses to 4 to 9 (versus 12 to 18 with adjacent versions).
8. Big blocks have no access value for binaries: 16 MiB blocks cost 16 to 21 ms warm (28 to 32 ms cold-file) per 4 KiB
   read for no ratio gain.
9. The "cold" file reads are fast (100 to 190 us for 64 KiB blocks) because this VM's disk is fast; on object storage
   the fetch latency dominates and was not measured.
10. Peak RSS is an unreliable metric below about 1 MiB (the measurement shows 0 for several zstd rows).

## 9. Recommendation per object class

| class | per-object codec | block layout (if packed) | notes | status |
|---|---|---|---|---|
| source text | zstd 3; a trained 112 KiB dictionary for objects under 32 KiB stored individually; zstd 9/19 or brotli 11 only for offline compaction | 256 KiB to 1 MiB blocks, zstd 3, related versions adjacent (path order); 4 MiB when read amplification is acceptable | block grouping beats dictionaries; the packer must sort by path/version | CONFIRMED (ratios), HYPOTHESIS (packer cost) |
| small binary (< 1 MiB) | zstd 3 (raw fallback is automatic); skip dictionary except for catalog-like formats | 64 KiB to 256 KiB blocks (ratio flat) | | CONFIRMED |
| large binary (>= 1 MiB) | zstd 3 for compressible assets; no probing | whole-object or 64 KiB to 1 MiB blocks; ratio flat across sizes | CDC chunks (see chunking doc) are the dedup lever, not the codec | CONFIRMED |
| model checkpoint | byte-plane shuffle (width = element size: 4 for fp32, 2 for bf16/fp16) + zstd 3; none for int8; zstd 1 is nearly identical to 3 on weights | 64 KiB to 1 MiB blocks (ratio flat); aligned to the shuffle width | real ratios 1.28 to 1.40; synthetic data under-states fp32 | CONFIRMED on 2 real files and synthetic, HYPOTHESIS for other models |
| anything, offline compaction | zstd 19 (ratio +12 to 17 percent on text, +10 percent on large binary versus zstd 3) or brotli 11 (+25 to 31 percent on text) | | speed 1 to 5 MB/s per thread, memory 100 MiB | CONFIRMED (ratios, speeds) |

## 10. Limitations

- One run per dataset for ratios (deterministic); throughput has 3 repetitions on a loaded shared box. Treat 10 percent
  differences in speed as noise. Memory numbers are RSS deltas and coarse.
- Per-file sampling caps (64 MiB per class, 16 MiB windows) mean that large classes are represented by a subset; the
  sample size is in the JSON and tables. The 256 MiB block-stream cap truncates django, linux and godot text.
- Model results come from two real files (one 32 MiB window each) and synthetic data; real tensors differ by layer.
- Dictionary tests are within one repository; cross-repository generalisation is untested.
- Block experiments decompress from memory or a local file: no object-store latency or request cost; no concurrent
  readers; no cache of decompressed blocks.
- Only zstd 3, 9 and LZ4 were run for blocks. zstd with `--long`, multi-thread zstd, and zstd's seekable format were not tested.

## 11. Reproduce

```sh
cd crates/vcs-compress-lab && cargo build --release && cd ../..
sh crates/vcs-compress-lab/run_all.sh ripgrep tokio django godot_demo linux_fs synth_assets synth_sparse synth_ml
# model files (list: sources; 32 MiB window):
compress-lab codecs --name hf_gpt2 --source list:gpt2.txt --window-mb 64 --cap-mb 64 --out hf_gpt2_codecs.json
python3 -I crates/vcs-compress-lab/render.py codecs data/compress-lab/django_codecs.json     # full table with all dictionary rows
python3 -I crates/vcs-compress-lab/render.py blocks data/compress-lab/django_blocks_path.json
sh data/compress-lab/gitpack.sh                                                              # section 7, run from a shell on the box
```

`run_lab.sh` runs codecs, chunking and blocks (both orders) sequentially. The codec runs for synth_ml, synth_assets and
synth_sparse and the per-format model runs were redone after the sampling-window fix described in section 2.2; the JSON
in `data/compress-lab/` are the final runs.
