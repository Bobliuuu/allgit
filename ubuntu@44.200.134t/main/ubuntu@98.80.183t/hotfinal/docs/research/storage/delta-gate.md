# Delta gate on real allgit objects (issues #250, #244)

Status: measured, **gate passes**. The packing research (`packing-experiments.md`, #184) deferred deltas behind a data
gate: "deltas only if at least 1.15x smaller than per-object zstd at chain depth <= 8, with reconstruction reads
measured". That research ran on git objects and file contents. This document re-runs it on the objects allgit really
stores: `vcs import-git` of four real histories, then every stored object (blobs, canonical trees, commits, tags) read back
from the object store.

## Method

- Histories (bare clones taken 2026-11-01; heads below): ripgrep `91f5d22e` (2,343 commits), tokio `b499257e` (5,164),
  flask `49943b5a` (5,607; the Python project), cargo `809bcc52` (24,372). Stream made with
  `git fast-export --signed-tags=strip --all --show-original-ids`, imported with `vcs import-git` at `cc1fffb` plus this
  branch. Tags and every branch are included, so these are whole histories, not one checkout.
- Tool: `crates/vcs-storage-lab-delta` (own workspace, like the packing lab). It opens the repo's `LocalStore`, lists every
  object, takes its canonical encoding (`Object::encode`, the bytes that are hashed and stored), classifies it (text blob,
  binary blob = NUL in the first 8000 bytes, tree, commit, other = tags and the like, chunk manifests) and computes stored
  bytes for each variant. Delta engine: the packing lab's Git-format copy/insert delta (`delta.rs`, reused through a
  `#[path]` include).
- Variants (bytes are payload bytes; every variant pays the same 44-byte index entry and 4-byte frame per object, not
  counted):
  - `loose`: the encoded bytes (`raw`), and `loose_4k_blocks`: each rounded up to 4 KiB, which is what a file system
    allocates.
  - `pack v1`: encoded bytes as they are (same as `raw`).
  - `pack v2`: codec byte + zstd level 3 per object (`encode_payload`, the shipping format). **All ratios below are
    relative to this.**
  - `solid`: objects of a class laid out in path order (newest first within a path), concatenated into blocks of 64 KiB,
    256 KiB, 1 MiB or 4 MiB, each block zstd level 3, plus 4 bytes per object for the in-block length. A read must
    decompress its whole block.
  - `delta dN` (N = 4, 8, 16): per path, newest first (HEAD objects are whole, history is deltas), each object tried as a
    delta against the previous 8 members of its path group whose chain is shorter than N; the delta is zstd-3 compressed and
    kept only if (1 codec byte + 4-byte base reference + compressed delta) is smaller than the v2 payload.
  - `delta_sketch dN`: the same without paths. Objects of a kind are sorted by a content sketch (min and max of a rolling
    hash over every 16-byte window) and chopped into runs of 4,096; neighbours are tried as bases. This is what a pack
    writer can do when it only has bytes (a received pack, a repack of loose objects), and is what pack v3 ships.
- Where paths came from: the newest commit (by author time) that contains the object, walking each tree once; chunk
  blobs of a large file are keyed `path#chunk-index`.
- Sizes: Lambda box (30 cores, load 40, size does not depend on load). Timing: box b (8 vCPU), single thread, **load1 was
  3 to 5, not the "quiet" under 2 the protocol asks for**, because other agents were on it; treat timings as ordering and
  rough factors, not precise figures. One run of the tool per repo, each reconstruction timing is the median of 3 passes.
  Reads are from memory (payloads already loaded), so they measure decode and delta apply, not disk or page cache.
- Correctness: during timing every object of every class is reconstructed from its delta plan and compared with the
  original bytes: 0 mismatches over ripgrep, flask and tokio (cargo was sized on Lambda but not timed on b).

## Size results: shrink factor vs pack v2 (per-object zstd 3), larger is better

Per class, per repository (`data/deltagate/sizes.tsv` has every number in bytes). "v2 MiB" is the pack v2 size of the class.

| class | repo | objects | v2 MiB | loose 4K | solid 64K | solid 1M | solid 4M | delta d4 | delta d8 | delta d16 | sketch d8 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| text blob | ripgrep | 5,140 | 29.1 | 0.23x | 1.32x | 8.83x | 13.24x | 7.15x | 8.44x | 10.28x | 8.42x |
| text blob | flask | 9,285 | 49.7 | 0.28x | 1.39x | 10.37x | 17.68x | 7.45x | 9.80x | 13.15x | 8.89x |
| text blob | tokio | 21,058 | 59.2 | 0.22x | 1.85x | 9.29x | 11.88x | 5.87x | 7.06x | 8.25x | 6.85x |
| text blob | cargo | 59,864 | 321.5 | 0.21x | 1.34x | 9.45x | 16.02x | 8.29x | 10.54x | 13.35x | 9.73x |
| binary blob | ripgrep | 7 | 0.00 | 0.07x | 1.09x | 1.09x | 1.09x | 1.00x | 1.00x | 1.00x | 1.14x |
| binary blob | flask | 38 | 1.54 | 0.93x | 1.00x | 1.22x | 1.22x | 1.21x | 1.21x | 1.21x | 1.21x |
| binary blob | tokio | 1 | 0.00 | 0.83x | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x | 1.00x |
| binary blob | cargo | 278 | 3.58 | 0.52x | 1.02x | 1.09x | 1.10x | 1.00x | 1.00x | 1.00x | 1.01x |
| tree | ripgrep | 6,419 | 3.57 | 0.14x | 9.03x | 10.31x | 10.41x | 3.55x | 4.13x | 4.83x | 3.30x |
| tree | flask | 11,435 | 9.08 | 0.20x | 11.02x | 14.29x | 14.57x | 4.22x | 5.02x | 6.02x | 4.10x |
| tree | tokio | 25,467 | 16.8 | 0.16x | 7.28x | 10.56x | 10.82x | 3.58x | 4.19x | 4.93x | 3.52x |
| tree | cargo | 110,653 | 91.5 | 0.20x | 6.96x | 14.12x | 15.25x | 4.65x | 5.53x | 6.46x | 4.78x |
| commit | ripgrep | 2,343 | 0.59 | 0.06x | 1.46x | 1.57x | 1.57x | 1.04x | 1.05x | 1.05x | 1.03x |
| commit | flask | 5,607 | 1.13 | 0.05x | 1.66x | 1.75x | 1.75x | 1.12x | 1.14x | 1.15x | 1.07x |
| commit | tokio | 5,164 | 1.23 | 0.06x | 1.45x | 1.55x | 1.56x | 1.04x | 1.05x | 1.05x | 1.02x |
| commit | cargo | 24,372 | 7.61 | 0.08x | 1.62x | 1.81x | 1.84x | 1.11x | 1.12x | 1.12x | 1.07x |
| all objects | ripgrep | 14,186 | 33.3 | 0.21x | 1.46x | 8.27x | 11.06x | 5.86x | 6.77x | 7.98x | 6.49x |
| all objects | flask | 26,404 | 61.4 | 0.25x | 1.59x | 8.35x | 11.58x | 5.53x | 6.72x | 8.14x | 6.05x |
| all objects | tokio | 51,706 | 77.3 | 0.20x | 2.19x | 8.69x | 10.55x | 4.83x | 5.68x | 6.56x | 5.26x |
| all objects | cargo | 195,281 | 424.3 | 0.20x | 1.62x | 8.82x | 12.64x | 6.15x | 7.38x | 8.72x | 6.74x |

Reading it:

- "loose 4K" is below 1x everywhere: a loose store is 3 to 5 times bigger than pack v2 on disk (cargo: 2.17 GB loose in
  4 KiB blocks, 1.56 GB raw, 424 MiB pack v2, 57 MiB with delta d8 over all objects).
- **Text blobs**: 7x to 10.5x at depth 8, 8x to 13x at depth 16, on all four histories. Far above the 1.15x gate.
- **Trees**: 4.1x to 5.5x at depth 8 (3.3x to 4.8x without paths). Far above the gate. Per-object zstd does nothing for
  a tree (0.94x to 1.00x of raw) because about 75% of a tree's bytes are 32-byte BLAKE3 ids (see below), which are
  incompressible; a delta against the previous version of the directory removes the unchanged entries wholesale.
- **Binary blobs**: 1.00x to 1.21x, and there are almost none in these histories (1 to 278 objects, under 1% of bytes).
  Below the gate on 3 of 4; not worth the code path, but harmless.
- **Commits**: 1.04x to 1.14x at depth 8 with paths, 1.02x to 1.07x with the path-free sketch. Fails the gate. Solid
  blocks do better (1.5x to 1.8x) but commits are 1% to 2% of the bytes. Not deltified.
- **Solid blocks** beat deltas on size (10x to 13x overall at 4 MiB) but a read costs a whole block (next section). At
  64 KiB blocks they are barely better than per-object zstd for blobs (1.3x to 1.9x).
- **Depth** buys a lot: d4, d8, d16 give about 4.8x to 6.2x, 5.7x to 7.4x, 6.6x to 8.7x on all objects. Depth 8 is the gate's limit.
- **Path-free sketch** costs about 10% to 25% of the path-ordered result at the same depth (d8 overall 6.05x to 6.74x vs
  6.72x to 7.38x with paths), still 5x to 7x. Good enough to ship without any graph knowledge.

## Reconstruction reads (box b, in memory, single thread)

Average cost to materialise one object, no cache, relative to the v2 decode of the same class. `HEAD` = objects of the
newest commit, which newest-first ordering stores whole.

| class (repo) | v2 decode | delta d4 | delta d8 | delta d16 | solid 64K | solid 1M | solid 4M |
|---|---|---|---|---|---|---|---|
| text blob (ripgrep) | 27.7 us | 103 us (3.7x) | 146 us (5.3x) | 165 us (5.9x) | 84 us (3.0x) | 776 us (28x) | 2,665 us (96x) |
| text blob (flask) | 26.0 us | 82 us (3.1x) | 112 us (4.3x) | 197 us (7.6x) | 82 us (3.1x) | 709 us (27x) | 2,601 us (100x) |
| text blob (tokio) | 16.8 us | 56 us (3.3x) | 76 us (4.5x) | 107 us (6.4x) | 76 us (4.5x) | 731 us (44x) | 2,761 us (165x) |
| tree (ripgrep) | 0.43 us | 44 us | 81 us | 164 us | 56 us | 700 us | 2,354 us |
| tree (flask) | 1.73 us | 61 us | 95 us | 148 us | 86 us | 862 us | 3,135 us |
| tree (tokio) | 1.52 us | 51 us | 73 us | 122 us | 83 us | 705 us | 2,805 us |

- Text blobs at depth 8: 4.3x to 5.3x the cost of a v2 read, 76 to 146 us in absolute terms. HEAD blobs cost the same as a
  v2 read (12 to 28 us), since they are stored whole.
- Trees are tiny (v2 decode 0.4 to 1.7 us), so the delta read is dominated by decompressing and applying the chain:
  73 to 95 us at depth 8. HEAD trees are whole (0.25 to 1 us).
- Solid blocks: a 1 MiB block costs 27x to 44x for a blob read and 4 MiB costs 100x to 165x, so they only fit scans, not
  random reads (`log`, `blame`, partial checkout, remote range fetches).
- Product path, flask, every object through `LocalStore::get` (file open, range read, decode, hash verification), box b,
  load1 about 5, 5 passes: v2 pack 27.6 us per object, v3 pack 49.7 to 50.3 us per object (1.8x). Pack sizes of the whole
  object directory: 65.7 MB (v2), 11.9 MB (v3), 182.8 MB as imported. Lambda box (load 40): 28.9 us vs 52.0 us.

## Trees: how compressible, does front coding help

Trees are 11% to 22% of the v2 pack bytes (ripgrep 11%, flask 15%, tokio 22%, cargo 22%). A tree is `kind, version, then per entry name, mode,
32-byte id`, entries sorted by name. Measured over all trees:

| repo | entries | id bytes / tree bytes | zstd-3 per tree | front-coded names + zstd-3 | columnar (names and modes zstd, ids raw) |
|---|---|---|---|---|---|
| ripgrep | 88,066 | 75.1% | 1.00x of v2 (baseline) | 0.99x | 1.02x |
| flask | 219,769 | 71.6% | baseline | 1.00x | 1.04x |
| tokio | 416,402 | 71.4% | baseline | 1.00x | 1.04x |
| cargo | 2,261,747 | 69.4% | baseline | 1.00x | 1.05x |

(Factors are v2 size divided by variant size; 1.00x means no change.) Ids are about 70% to 75% of the bytes and cannot
shrink; zstd already handles the sorted-name prefix redundancy, so **sorted-entry front coding gains nothing** (0.99x to
1.00x) and even splitting ids into their own stream gains 2% to 5%. A per-tree transform is not worth a format change.
What does work for trees is cross-version redundancy: deltas (4x to 5.5x at depth 8) or solid blocks. The 69% to 75%
id share also bounds per-object tree compression at 1.33x to 1.44x of raw whatever the codec.

## Decision against the gate

| gate condition | result |
|---|---|
| >= 1.15x smaller than pack v2 at depth <= 8 | text blobs 7.1x to 10.5x, trees 4.1x to 5.5x, all objects 5.3x to 7.4x (path-free: 5.3x to 6.7x), on all four histories |
| reconstruction reads measured | yes: 3.1x to 5.3x a v2 read for blobs at depth 8 (76 to 146 us), 0 mismatches over 3 histories; HEAD objects unchanged |
| per class | pass: text blob, tree. Fail: commit (1.02x to 1.14x), tag/other (~1.1x), binary blob (1.0x to 1.2x, under 3% of the bytes) |

**The gate passes** for text blobs and trees, which together are over 95% of the bytes. Commits, tags and binary blobs
stay whole. Solid blocks compress better but cost 27x to 165x per read at the sizes where they win, so they are not a fit
for the read paths allgit cares about; deltas keep the HEAD path at v2 speed.

Relation to pack v3 (#249, landed while this ran): v3 is the solid-block variant measured here as `solid`, used by
compaction with 4 MiB blocks. Its size win (10x to 13x) is larger than deltas (6x to 8x) and this data agrees with that
choice for cold, scan-style packs; its random-read cost (27x to 165x a v2 read at 1 to 4 MiB, without a block cache) is
why deltas are still interesting for local hot packs. The two are not stacked: a v3 block has no per-object entry to
delta against.

What shipped (separate PR): pack **v4** in `vcs-store` (v3 was taken by blocks), a codec byte 2 for delta entries on
per-object entries like v2, opt-in via `vcs repack --format v4` or `VCS_PACK_FORMAT=v4`, local packs only, path-free
sketch ordering, depth 8, window 8, readers accept chains up to 50 and verify the object hash. v2 stays the default;
rollback is `vcs repack --format v2`. Whether to make v4 the default for local packs, or to keep v3 blocks as the only
space-saving format, is a product decision this data informs but does not make.

## Not measured, caveats

- Disk and cold-cache reads (everything above is in memory or warm page cache); remote ranged reads of deltas (v3 is local
  only because of this); concurrent readers; write cost beyond the single `repack --format v3` timing (flask: 4.5 s wall
  on the 30-core box at load 40, including conversion of the existing 183 MB pack; ratio of write time to v2 not
  measured on the quiet box).
- Four repositories, all open source, all text heavy. A repo with large binary assets (game art, models) would show the
  binary-blob row, not the text one. Large files are chunked (content-defined, 16 KiB average); chunk blobs were grouped
  per `path#chunk-index` and are included in the blob rows, but no repository here has many of them.
- The tool deltifies with a window of 8 and the greedy 16-byte matcher; Git's own heuristics and a better matcher could
  shrink further. Cargo was sized (Lambda) but not timed on box b.
- Box b load was 3 to 5 during timing instead of under 2, so timings are indicative; sizes do not depend on load (the tool is deterministic) but I did not diff the two machines' outputs.
- Base reference cost is counted as 4 bytes in the lab; pack v3 stores a varint distance (1 to 3 bytes), slightly smaller.

## Reproduce

```text
git clone --bare <repo> r.git; git -C r.git fast-export --signed-tags=strip --all --show-original-ids > r.stream
mkdir r.vcs && cd r.vcs && vcs init . && vcs import-git ../r.stream && cd ..
cargo build --release --manifest-path crates/vcs-storage-lab-delta/Cargo.toml
deltagate r r.vcs/.vcs/objects [--time]      # R lines: repo class variant objects bytes ratio-to-v2 shrink-vs-v2
deltagate r r.vcs/.vcs/objects --readall     # product read path through LocalStore::get
```

Data: `data/deltagate/sizes.tsv` (R lines, all four repos) and `data/deltagate/timing_box_b.tsv` (T lines, box b).
