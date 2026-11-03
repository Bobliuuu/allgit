# Indexing experiments (experiment H)

Issue #185, epic #190. Code: `crates/vcs-storage-lab` (standalone workspace, no product code touched). Data: `data/idx_*.jsonl`,
`data/hist.jsonl`, `data/hist_cg.jsonl`, `data/treecost.jsonl`, `data/treeidx_*.jsonl`. Companion: [caching-experiments.md](caching-experiments.md).
**CONFIRMED** = measured here; **MODEL** = measured on a synthetic history whose parameters are printed in the data; **HYPOTHESIS** = not tested.
Environment for every run is recorded in the first `env` row of each data file: AMD EPYC 7J13, Linux 6.8.0-1046-nvidia, rustc 1.99.1, shared
box with load average 8 to 30 (so absolute ns are noisy upward; 5 trials, median reported, `*_max` kept in the data). All index lookups below are
warm, in-memory, single thread, unless a row says otherwise.

Read first: `crates/vcs-store/src/pack.rs` (per-pack `.idx`, 44 B per entry, flat sorted, whole-file BLAKE3 at open, one lookup per pack in
order, `LocalStore::packed` lib.rs 313), `crates/vcs-merge/src/cgraph.rs` (`CommitGraphFile`: ids, times, generation v1, parent positions, 49.2 B per
commit; `extend` rebuilds a `BTreeMap` of everything), `crates/vcs-repo/src/cgraph.rs`, `dircache.rs` (directory listing stamps), `docs/cache.md`,
`docs/research/commit-graph.md` (prior art on commit-graph and Bloom filters, not repeated here) and the audit `architecture-audit.md` (F-numbers).

## 1. Object-location indexes

Question: how should `id → (pack, offset, length)` be answered as the object count and pack count grow, and what does it cost to build?
Method (`idxbench`): N uniform 32-byte ids (object ids are BLAKE3 outputs, so uniform), split into P packs (one pack with 90 percent of the
objects, the rest shared evenly, like a repack plus incremental packs), each pack's entries sorted. Build time and heap bytes (measured with a
counting global allocator, not estimated, except `btreemap_mem` which is an estimate) per object; 2 M random hits and 2 M random absent ids,
5 trials. Correctness is checked: every hit must return the right location (`wrong_hits` = 0 everywhere) and false positives of absent ids are counted.

### 1.1 Pack-local versus global, in memory (N = 1,000,000, `data/idx_1000000.jsonl`)

| Layout | Packs | build | bytes/obj | hit ns | miss ns |
|---|---|---|---|---|---|
| per-pack flat sorted (**product today**) | 1 | 0.03 s | 48.0 | 713 | 721 |
| | 16 | 0.00 s | 48.0 | 1058 | 4260 |
| | 256 | 0.03 s | 48.0 | 2974 | **29021** |
| per-pack + 256 fanout (git .idx v2 style) | 1 / 16 / 256 | 0.03 / 0.01 / 0.01 s | 48.0 / 48.0 / 48.3 | 621 / 643 / 913 | 515 / 1744 / 4564 |
| global sorted array + 64K fanout | any | 0.08 to 0.13 s | 48.3 | 221 to 355 | 126 to 179 |
| global open-addressing hash, 64-bit prefix, 16 B slot, load 0.75 | any | 0.02 to 0.04 s | 33.6 | 31 to 40 | 34 to 41 |
| minimal perfect hash (BBHash, gamma 1.7) + 16 B slot with 32-bit fingerprint | any | 0.12 to 0.46 s | 16.4 to 16.7 | 84 to 198 | 77 to 171 |
| same, gamma 4.0 | any | 0.08 to 0.22 s | 16.7 | 55 to 97 | 60 to 80 |
| in-memory B-tree (`BTreeMap`) | any | 0.04 to 0.10 s | ~71.6 (estimate) | 317 to 406 | 319 to 423 |

(N = 10,000,000, `data/idx_10000000.jsonl`: per-pack flat 1 pack: hit 1284 ns; global sorted 507 ns; global hash 52 ns at 26.8 B/obj; MPH 128 ns at 16.5 B/obj;
with 16 packs the flat layout's miss is 12.0 us against 0.49 us for the global sorted array. The 256-pack rows at 10 M may be missing from the file if that run
had not finished when this document was written; the 1 M and 100 k tables are complete.)

Findings.

- CONFIRMED: with many packs the product's layout makes **absent-object lookups the slow case**: 29 us at 256 packs and 1 M objects, 40x the single-pack
  cost, because every pack is probed. The audit measured the same effect on the real store (`audit_openpacks.jsonl`: 96 us per `contains` miss with 1000 packs). Absent lookups are what a
  dedup check in `put` and a lazy fetch in a sparse clone do. A fanout table (the git .idx v2 idea, <https://git-scm.com/docs/gitformat-pack>, fetched 2026-10-31) cuts the 256-pack miss
  from 29 us to 4.6 us and costs 4 bytes x 256 per pack; a single global index removes the dependence on pack count entirely (126 to 179 ns).
- CONFIRMED: a flat global sorted array with a 16-bit fanout is within 2% of the per-pack memory (48.3 vs 48.0 B/obj) and 3x faster on hits even at one pack
  (221 vs 713 ns), since the fanout removes about 16 levels of binary search. A 256-entry per-pack fanout alone gives 1.15x.
- CONFIRMED: a hashed index is the fastest (31 to 40 ns) but pays 33.6 B/obj (load 0.75, 16 B slots) and needs the length elided (read from the 4-byte length prefix that precedes every pack
  entry) and a verify-on-read of the object hash, which `LocalStore::get` already does, so the 64-bit-prefix ambiguity costs nothing extra. Not ordered, so it cannot serve prefix
  (abbreviated id) lookup or `list`.
- CONFIRMED: the minimal perfect hash is the smallest (16.4 B/obj, one third of today's 48; the BBHash structure itself retains about 4 bits per key: 16.4 minus the 16 B slot, see
  `Mph::bytes`) and answers in 55 to 198 ns, **but** it is static and costs 0.1 to 0.5 s per million keys to build, and a fingerprint of 32 bits lets an absent id through with probability 2^-32 (0 false
  positives in 2 M probes). It fits an **immutable pack** (build it once at repack, store next to the pack) and does not fit a global index that changes at every publish. Source for the MPH
  technique: Limasset et al., BBHash, <https://arxiv.org/abs/1702.03154> (fetched 2026-10-31; 3.7 bits per key for 10^10 keys, from the abstract; our measured retained size is for 10^5 to 10^7 keys).
- HYPOTHESIS: the design that dominates on these axes is "per-pack MPH or hash, plus a small in-memory global overlay for packs younger than the last repack, plus a tiered repack
  policy bounding pack count". Not built.
- The in-memory B-tree is a control, not a candidate: 5 to 10x slower than the global array and 1.5x its memory.

### 1.2 Open cost of the product index (`data/idx_*.jsonl`, row `product_pack_index`)

`vcs_store::pack::PackIndex::parse` on an in-memory 44 B x N file, using the real code: 2 ms at N = 100 k, **47 ms at 1 M** (12 ms of it BLAKE3 over the file), so about 0.47 s at 10 M plus the read of a 440 MB
file (against about 165 MB for the 16.5 B/object MPH layout). The audit measured 90 ms for a real 1 M-object pack including file read. Opening happens on every process start (every CLI call), for every pack. CONFIRMED.
Option: skip the whole-file hash at open and verify lazily (hash is a `verify`/`fsck` job; each object read is already hashed). HYPOTHESIS: this removes about a quarter of the open cost; the rest is parse and copy
(a memory-mapped, in-place format would remove it).

### 1.3 Negative-lookup filters in front of an index (N = 1 M, `row exp=filter`)

| Filter | bits/key | build | hit query ns | miss query ns | measured false-positive rate |
|---|---|---|---|---|---|
| Bloom 8 bits, k=5 | 8.0 | 11 ms | 10 | 19 | 2.2 % |
| Bloom 10 bits, k=7 | 10.0 | 14 ms | 14 | 21 | 0.8 % |
| Bloom 16 bits, k=11 | 16.0 | 23 ms | 21 | 21 | 0.1 % |
| Cuckoo, 16-bit fingerprints, 4-way buckets (power-of-two table) | 33.6 at 1 M (21.0 at 100 k) | 12 ms | 14 | 7 | 0 in 2 M probes (theory ~1e-4) |
| Binary fuse 8 (xorf crate) | 9.0 | 73 ms | 8 | 8 | 0.4 % |

The cuckoo figure is inflated by rounding the bucket count to a power of two (the XOR trick needs it): at 1 M keys the table is almost half empty. Binary fuse filters: Graf and Lemire,
<https://arxiv.org/abs/2201.01174>, fetched 2026-10-31 ("within 13% of the storage lower bound"); cuckoo filters: Fan et al., CoNEXT 2014 [unverified, not fetched].

**False-positive cost** = one wasted probe of a pack index: 0.7 to 3 us for the product layout (table 1.1) or one wasted ranged read if the index is remote (not measured; ~one RTT, i.e. 10 to 100 ms on object storage). With
a filter in front of each pack the expected miss cost at P = 256 packs and 1 M objects is 256 x (8 ns filter) + 256 x 0.008 x 1 us (Bloom 10) = about 2 us + 2 us, versus 29 us unfiltered (**computed from the measured parts, not
a separate run**). A global index reaches 0.13 us with no filter at all, so **filters are the right tool only if indexes stay per pack and remote**, where one avoided range read is worth 10 to 100 ms.

### 1.4 On-disk B-tree, LSM, SQLite as location index (N = 1 M, 48-byte key+value, `loc_index_ondisk` rows; page cache warm unless "cold")

| Store | insert order | build | disk bytes/obj | hit ns | miss ns | cold first-2000 hit |
|---|---|---|---|---|---|---|
| redb (B-tree) | random / sorted | 6.3 s / 2.4 s | 1078 / 135 | 1759 / 1512 | 1450 / 1415 | 3.6 / 3.9 us |
| SQLite WAL, WITHOUT ROWID | random / sorted | 3.2 s / 1.1 s | 61 / 62 | 4213 / 4244 | 4108 / 4184 | 82 / 139 us |
| RocksDB (LSM, Bloom 10, no compression) | random / sorted | 2.1 s / 0.7 s | 59 / 59 | 2013 / 2160 | **393 / 448** | 74 / 76 us |

Findings: CONFIRMED that all three are 25x to 200x slower to build and about 9x slower per hit than the in-memory global array, and 59 to 1078 bytes on disk per object against 44 for the flat file; RocksDB has the best absent-key
latency because of its Bloom filters (0.4 us, 4x better than the in-memory per-pack flat layout at 16 packs); redb's file bloats 8x under random-order inserts (non-durable commits; **not investigated further**), 
and its "cold" figure is after the file had been read in on reopen (5.8 s in the cache bench), so it is not comparable. **An embedded KV does not pay as the object-location index** of a store whose
objects are in immutable packs: you pay 9x latency and 1.3x to 22x space to get incremental updates that an immutable-pack design does not need. It could pay for the *mutable* global overlay at very large scale (**HYPOTHESIS**).
Results for N = 100 k are in `data/idx_100000.jsonl` with the same ranking.

## 2. Commit-graph, reachability, changed-path filters on multi-zone histories (`histbench`, MODEL)

Model (printed in `data/hist.jsonl`): 8 zones, 50,000 commits per zone (400,000 total), 20,000 files per zone, 8 percent side-branch probability (mean branch length 6),
3 changed files per commit on average chosen with Zipf popularity, 1 percent "refactor" commits touching 100 to 1000 files, 2 percent of commits with clock skew of up to 48 h. A parent always lives in its child's zone, so every zone is an independent DAG
(`vcs-repo/src/cgraph.rs` enforces this). Measured changed entries per commit (files plus all ancestor directories, as git does): mean 38.4, median 15, p99 812, max 2563. 15,640 merges.

### 2.1 Reachability: `is_ancestor(a, d)` on one zone (visited commits per query; each visit is an object read plus decode in the product without a graph file)

| Strategy | extra bytes/commit | near (≤20 back) | far | newer-than (false) | random pair | wrong answers |
|---|---|---|---|---|---|---|
| no index (walk) | 0 | 9,350 | 26,940 | 12,315 | 17,137 | 0 |
| commit-time cutoff | 8 | 9 | 25,854 | 1 | 7,017 | **892 of 8,000** |
| generation v1 (topological level; the product's `CommitGraphFile` has it) | 4 | 9.9 | 24,058 | 1 | 6,546 | 0 |
| generation v2 (corrected date) | 8 | 9.8 | 24,058 | 1 | 6,546 | 0 |
| GRAIL k=3 labels + gen v1 | 28 | 9.1 | 24,057 | 1 | 6,545 | 0 |

Findings.
- CONFIRMED (MODEL): generation numbers cut the near and false-answer cases by 3 orders of magnitude (9,350 → 10; 12,315 → 1). Time-based cutoffs are fast but **wrong** (892 wrong of 8,000 queries, 11 percent) once clocks are skewed: negative result, do not use commit time for pruning.
- Negative result: generation v2 (corrected date) gives no extra pruning over v1 on this history, and 4 bytes more per commit; GRAIL labels (24 more bytes per commit) gave a 1 to 7 percent further reduction only on near queries and nothing on far queries.
  For true ancestors far back every strategy must still visit ~24,000 commits (the answer is "yes", you must find a path), so none of these help `merge-base` of distant commits; only generation-bounded walks help. The existing v1 numbers in the product are enough.
- Product graph file (real `CommitGraphFile`, all 400 k commits): 49.2 bytes per commit (19.7 MB); `extend` from scratch 4.7 s; `decode` 80 ms.

### 2.2 Zone partitioning of the graph file (`data/hist_cg.jsonl`; `extend` timed alone, loader map prepared beforehand)

| Layout | file size | extend with 10 new commits on top of existing | bytes written per publish |
|---|---|---|---|
| one monolithic file for all zones (product) | 19.7 MB | 437 ms (399,990 existing) | 19.7 MB |
| one file per zone | 2.46 MB | 31.5 ms (49,990 existing) | 2.46 MB |

CONFIRMED (MODEL): zone partitioning is a **14x CPU and 8x I/O win per publish** for the same data, because `extend` re-materialises every existing commit (O(existing)) and the file is rewritten whole. A zone-scoped query decodes 1/8 of
the bytes (9 ms vs 80 ms decode). It follows directly from the zone invariant, which makes the partition exact: no edge crosses zones. Even better (HYPOTHESIS, not built): an append-only chain of small files per zone (git's split commit-graph) makes extend O(new).

### 2.3 Changed-path filters and `log -- path` (one zone, 50,000 commits, 2,000 queries per class)

Tree diffs needed per query (each is a path-limited tree comparison for one commit). True matches in brackets. No filter always needs 50,000.

| Filter | bytes/commit | uniform file (17.9) | directory (361) | hot file (739) | FP rate over negatives | scan time/query |
|---|---|---|---|---|---|---|
| Bloom 6 bits/entry, k=4 | 21.6 | 3,303 | 3,359 | 3,417 | 5.4 to 6.6 % | 0.65 ms |
| Bloom 10 bits/entry, k=7 (git default) | 33.0 | 1,008 | 1,074 | 1,112 | 0.8 to 2.0 % | 0.71 ms |
| Bloom 16 bits/entry, k=11 | 50.1 | 714 | 928 | 1,139 | 0.8 to 1.4 % | 0.72 ms |
| exact inverted index (path → sorted commit list, varint-delta) | 48.6 | **17.9** | **361** | **739** | 0 | **2.2 us** |

The measured Bloom FP rate (0.8 to 2 percent at 10 bits) is above the textbook 0.8 percent for the larger classes because filters are tiny (median 15 entries = 19 bytes) and double hashing modulo a tiny
bit count shares structure (git's scheme, which this reproduces). Commits with more than 512 entries get no filter and always return "maybe".

Real cost of what a filter saves (`treecost`, a real repo built with `vcs-repo` and `vcs-bench` generators: 5,000 files in the `deep` layout, 400 commits of 3 skew-chosen file edits, path depth 9, warm cache, hash-verified reads, `data/treecost.jsonl`):
one path-limited tree diff costs **30.5 us** from loose objects and **46.3 us** from a pack (4.1 tree reads); reading one commit object from the pack costs 10.7 us. So, applying the 10-bit filter to 50,000 commits (computed, not
measured end to end): 50,000 x 46 us = 2.3 s for an unfiltered `log -- path` against 1,008 x 46 us + 0.71 ms = 47 ms with the filter (49x), and 17.9 x 46 us = 0.8 ms with the inverted index (2,800x). The Bloom
scan itself, 0.7 ms, is a lower bound on `log` of 50,000 commits that the inverted index avoids.

Findings.
- CONFIRMED (MODEL): for point-path history a Bloom filter at 10 bits per entry removes 98 percent of the tree diffs, for 33 bytes per commit (1.7 MB per 50 k commits). 6 bits is 3x worse on FPs for 36 percent fewer bytes.
- CONFIRMED (MODEL): an exact inverted path→commits index is at least as small (48.6 bytes per commit with my rough size estimate that adds 2 bytes per posting list; **estimate**, not a serialised file), has zero false positives, and answers in microseconds.
  Its limits: it needs a path dictionary (unbounded growth with churn), cannot answer "what changed in this commit" without a second structure, and appending a commit touches up to ~38 posting lists. The right answer
  depends on whether `log -- path` or `blame` dominates; it is an open decision, so I label the result "Bloom is good enough, an inverted index is better for read-heavy use".
- Zone effect (`zone_partition` row): a query scoped to one zone scans 50,000 commits with per-zone filter sets and 400,000 with a global interleaved one: 8x. Per-zone sets are also what makes a sparse workspace cheap: an unsubscribed zone's
  filters are never loaded.
- Caveat: merge commits' first-parent diff and renames are modelled crudely (merge = union of side-branch changes, no renames).

## 3. Tree / path indexes (`treeidx`, 200 k and 1 M files, one zone, depth 7, `data/treeidx_*.jsonl`)

| Structure | heap bytes/file | lookup hit | miss | list a subtree (5,556 files at 200 k) | bytes to write for a 1-file change |
|---|---|---|---|---|---|
| nested sorted directories (the shape of trees; in memory, **no object reads**) | 127 | 1.5 us | 1.1 us | 20 us (3.6 ns/file) | 3,773 (estimate: 40 B per entry of each ancestor) |
| flat sorted path array | 62 | 0.9 us | 0.7 us | 20 us | whole array (12.4 MB) |
| FST map (`fst` crate) | 21.6 heap, 8.5 serialised | 0.58 us | 0.40 us | 1,365 us (246 ns/file) | rebuild (1.7 MB, 0.1 s) |
| hash map | 74 | 0.15 us | 0.04 us | cannot | cannot |

At 1 M files the same ranking holds (nested hit 1.8 us, flat 1.4 us, FST 0.68 us at 8.7 B serialised/file, hash 0.31 us; listing a 27,778-file subtree 63 us nested, 97 us flat, 6.9 ms FST).

Findings.
- CONFIRMED (MODEL): the FST is the **smallest** (8.5 to 8.7 bytes per file, 7x smaller than the flat array) and has good point lookups but is 70x slower to enumerate a subtree and is immutable; nested directories (what the object model already is) are the best for subtree enumeration
  (the sparse-workspace operation) and for incremental update (a one-file change re-encodes ~3.8 KB, about 4 directories) — the content-addressed tree is already a good index for what allgit needs. A flat derived path index only wins on **deep point lookups** (1.5x), and loses badly on update (rewrite
  everything) unless layered.
- The in-memory comparison ignores the dominant real cost: each nested level is a tree-object read (30 to 46 us per tree diff above). A flat/FST index would remove the object reads for point lookup: HYPOTHESIS that a per-commit
  FST of paths pays for repeated `blame` and sparse checkout planning; not measured. For zone-aware queries nothing requires a *global* tree index: each zone's tree is already its own root.

## 4. Which indexes help allgit's zone-aware queries

| Query | Helps (measured here) | Does not help / unnecessary |
|---|---|---|
| Object `get`/`contains` with many packs | global or fanout location index; MPH per immutable pack | embedded B-tree, LSM or SQLite as the location index (10x slower, large) |
| Absent-object checks (dedup `put`, lazy fetch) | any global index (0.13 us at 1 M); per-pack Bloom or fuse filters if indexes are remote | per-pack flat scan (29 us at 256 packs) |
| Pack open at process start | skipping the whole-file BLAKE3, mmap format (HYPOTHESIS) | – |
| `log`, `merge-base`, "ahead/behind" in one zone | generation v1 (already in the product) | corrected-date generation, GRAIL, commit-time cutoffs (wrong under skew) |
| Graph file maintenance at publish | **per-zone graph files** (14x less CPU, 8x less I/O for 8 zones) | monolithic file |
| `log -- path`, `blame` skeleton | 10-bit changed-path Bloom per commit, or exact inverted index | 6-bit filters (3x the false diffs) |
| Sparse workspace subtree listing | nested trees as they are | FST (70x slower to enumerate), hash map |
| Snapshot/point lookup by path | flat or FST index (HYPOTHESIS payoff) | – |

## 5. Limits and not run

- Histories are synthetic; no real repository (git.git, a monorepo) was replayed. 8 zones x 50 k commits is smaller than a real monorepo; the claims are about ranking and order of magnitude.
- 10 M-object runs for the per-pack and filter experiments finished only for the rows in `data/idx_10000000.jsonl`; on-disk stores were run to 1 M only (`--ondisk-max`).
- Remote-resident indexes (ranged reads from S3/MinIO) were not run: no cloud, MinIO not started; their costs are only reasoned in §1.3.
- Thread scaling of lookups was not measured; all index numbers are single-thread. The box was shared (see header), so absolute numbers carry a wide error band; ratios of 3x or more are the claims.
- Repeated seeds: one seed per configuration. A different history model (fewer hot files, bigger merges) was not tried.
- Real `blame` was not built; only its path-history skeleton.

## 6. Reproduce

```sh
cd crates/vcs-storage-lab        # standalone workspace; rocksdb needs libclang (labenv.sh)
cargo build --release --features rocks
LAB_DIR=/big/disk ./target/release/idxbench --n 1000000 --out data/idx_1000000.jsonl
./target/release/histbench --zones 8 --commits 50000 --files 20000 --queries 2000 --out data/hist.jsonl
LAB_DIR=/big/disk ./target/release/treecost --files 5000 --commits 400
./target/release/treeidx --files 200000
cargo test --release
```
