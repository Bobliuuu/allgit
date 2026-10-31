# Caching experiments (experiment J)

Issue #185, epic #190. Code: `crates/vcs-storage-lab` (a standalone workspace; no product crate depends on it, no default
behaviour changes). Raw data: `data/cache.jsonl`, `data/backend.jsonl`, `data/negcache.jsonl`, `data/modelcheck.jsonl`.
Companion: [indexing-experiments.md](indexing-experiments.md). Status words: **CONFIRMED** = measured here, **MODEL** =
measured on a synthetic model whose assumptions are listed, **HYPOTHESIS** = not tested. Sources were accessed on
**2026-10-31**; unverified ones say so.

## 0. Setup and honesty notes

- Machine for all runs: `ssh lambda-final`, AMD EPYC 7J13 (30 logical cores shown), Linux 6.8.0-1046-nvidia, rustc 1.99.1,
  216 GB RAM. The box is shared: load average was 8 to 30 during runs (each JSON row of `data/*.jsonl` starts with an `env` row that
  records host, kernel, CPU, load and date). Single-thread numbers are therefore noisy upward; spread is reported where it was
  measured (`*_max`). Page cache: **warm** unless a row says "cold"; cold rows drop the page cache with
  `echo 3 > /proc/sys/vm/drop_caches` (the earlier audit could not; sudo was available for this run).
- Build offline check (the brief asked): `rusqlite 0.32` (feature `bundled`, needs only `cc`), `redb 2`, `boomphf 0.6`, `xorf 0.11`,
  `fst 0.4` build from crates.io with no extra setup. `rocksdb 0.22` builds only with libclang: the box had none, so I installed
  `libclang` from PyPI into `~/rt/libclang` and exported `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/12/include`
  (`crates/vcs-storage-lab/labenv.sh`). Clean build of rocksdb: 53 s on the 30-core box. RocksDB is behind the `rocks` feature so the lab
  builds without it. Nothing needs the network at run time.
- Traces are generated, not captured from real users. The generator (`cache.rs`: `World`, `gen_trace`) builds per-zone histories with the
  same model as the index experiments (Zipf-skewed file popularity, 1 percent refactor commits touching 100 to 1000 files, side
  branches), lognormal blob sizes (median 3 KB, clipped 60 B to 1.5 MB) and trees of 40 B per entry. A client subscribes to 3 of 12 zones
  (Zipf over zones). Operation mix for `checkout_sync`: 55 percent sync (remote advances 1 to 5 commits, check out head), 30 percent switch to
  a commit 1 to 30 back, 8 percent bisect (10 jumps over history), 7 percent return to head; first touch of a zone is a full checkout.
  `checkout_sync_scan` adds a one-shot read of 1500 commits of a random zone every 400 operations. `ci_fresh` = every job is a fresh full
  checkout of a zone head, the head advancing 1 to 4 commits between jobs. All parameters are printed in the `cache_world` row.
  These workload shapes are my assumptions (**HYPOTHESIS** that they resemble real use).
- Network cost model (assumption): 40 ms RTT, 50 MB/s, either one object per round trip ("on demand", what `docs/cache.md`
  says lazy fetch does today) or batched per tree level with 16 parallel streams. `net_s_*` fields are modelled seconds for the whole
  trace, never measured network time.
- Capacities are fractions of the trace's unique bytes (575 MB for `checkout_sync`), so "10 percent" means a cache of 57 MB.

## 1. Backends (real implementations, `backendbench`)

100,000 objects, lognormal sizes (median about 2 KB, payload 413 MB total, incompressible random bytes), keys are 32-byte ids. All stores run with
durability off (no fsync, SQLite `synchronous=OFF`, redb `Durability::None`, RocksDB WAL disabled) because a cache is rebuildable;
this makes put numbers an upper bound. SQLite uses one connection behind a mutex (WAL readers could scale better; not tested).
Cold = page cache dropped, store reopened, first 3000 random gets. Source: `data/backend.jsonl`.

| Backend | put obj/s | disk / payload | get p50 (warm) | get p99 | absent-key p50 | 8-thread kget/s | cold get mean | reopen |
|---|---|---|---|---|---|---|---|---|
| memory (64-shard HashMap) | 275,000 | n/a | 0.88 us | 3.4 us | 0.07 us | 4,235 | n/a | n/a |
| local files, git-style fanout | 13,900 | 1.00 | 11.5 us | 24 us | 9.6 us | 181 | 177 us | 0 s |
| SQLite WAL, WITHOUT ROWID | 22,100 | 1.41 | 33 us | 61 us | 31.7 us | 26 | 517 us | 0 s |
| redb (B-tree) | 31,900 | 5.24 | 3.9 us | 16.9 us | 1.9 us | 539 | 5.7 us (after reopen) | 5.8 s |
| RocksDB (LSM, 10-bit Bloom, no compression) | 46,800 | 1.01 | 3.1 us | 14.3 us | 0.43 us | 925 | 82 us | 0.02 s |

Findings.

- CONFIRMED: for object-sized values the embedded LSM beats loose files on every axis measured here except simplicity: 3.4x the put rate,
  3.7x lower warm get latency, 5x the 8-thread read rate, 22x faster absent-key check (its Bloom filters answer without I/O), and a better cold read.
  The absent-key result matters for negative lookups and `contains` during publish.
- CONFIRMED: SQLite is the slowest reader in this configuration (single connection, a statement per get); it is a poor cache backend for
  hot reads. Its advantage, one portable file, is real but not a performance one.
- CONFIRMED, with a caveat: redb read latency is good, but its file was 5.2x the payload after non-durable bulk inserts (copy-on-write
  garbage, not compacted) and reopening took 5.8 s after the page cache was dropped (I did not find out whether that is an integrity
  scan; **unverified** cause). The 5.7 us "cold" get is after that scan had pulled the file into cache, so it is not comparable to the
  other cold figures.
- CONFIRMED: loose files cost one `open` per get (11.5 us warm) and a directory entry per object; at 100k objects that is fine, but the
  same layout in the product (`LocalStore`) also stats the loose path first for every packed object. Not measured: millions of files.
- NOT TESTED: RocksDB write stalls and compaction CPU under sustained load, space after overwrite, behaviour with many processes
  opening the same directory (RocksDB takes an exclusive lock; allgit CLI invocations are separate processes, so an LSM cache would need a
  daemon or per-process shards; this is a design cost, not a number).

Recommendation (**HYPOTHESIS** until a product prototype exists): a cache in front of a remote store should be (1) a bounded in-process
memory layer for hot trees and commits, over (2) either loose files (zero dependencies, process-friendly, 11 us) or, if an exclusive
daemon is acceptable, an LSM. SQLite and redb do not pay for themselves for this access pattern.

## 2. Policies on generated traces (`cachebench`)

Replayed policies: FIFO, LRU, ARC (byte-weighted, my implementation of Megiddo and Modha's algorithm, FAST 2003, [unverified: paper
not fetched]), and W-TinyLFU (1 percent window, SLRU main with 20/80 split, 4-row count-min sketch with 4-bit saturating counters and
periodic halving; after Einziger, Friedman and Manes, <https://arxiv.org/abs/1512.00727>, fetched 2026-10-31, submitted 2 December 2015).
Belady (evict farthest next use) is an offline reference, exact only for equal sizes. Unit tests (`cargo test cache`) check the capacity
invariant, scan resistance and that Belady is never worse than LRU. Source: `data/cache.jsonl`; hit rates are over object accesses, byte hit rate over bytes.
Traces: `checkout_sync` 10.5 M accesses, 345 k unique objects, 575 MB unique, 32.5 GB read in total; `ci_fresh` 19.3 M accesses, 204 k unique, 326 MB unique.
These are one seed each; I did not run repeated seeds, so differences under about 2 points are not claimed.

`checkout_sync`, hit rate (byte hit rate in brackets):

| Cache size | FIFO | LRU | ARC | W-TinyLFU | Belady bound |
|---|---|---|---|---|---|
| 1 % | 0.008 (0.004) | 0.008 (0.004) | 0.022 (0.016) | **0.087 (0.032)** | 0.16 |
| 3 % | 0.022 | 0.022 | 0.055 | **0.194 (0.089)** | 0.30 |
| 10 % | 0.364 | **0.372 (0.433)** | 0.334 | 0.297 (0.289) | 0.57 |
| 30 % | 0.671 | **0.696 (0.762)** | 0.695 | 0.660 | 0.83 |

`ci_fresh` (a loop larger than the cache):

| Cache size | LRU | ARC | W-TinyLFU | Belady bound |
|---|---|---|---|---|
| 1 % | 0.000 | 0.000 | **0.169** | 0.30 |
| 3 % | 0.000 | 0.081 | **0.336** | 0.38 |
| 10 % | 0.321 | 0.311 | **0.654 (byte 0.483)** | 0.745 |
| 30 % | **0.989** | 0.989 | 0.971 | 0.989 |

Findings.

- CONFIRMED (MODEL): the best policy depends on cache size relative to the working set, so no single policy wins. W-TinyLFU is clearly
  best when the cache is much smaller than the working set (1 to 3 percent: 9x to 11x the LRU hit rate on checkout traces; on the CI loop LRU
  and FIFO get exactly 0 hits because each job's scan evicts everything before it is reused, while W-TinyLFU holds a stable subset). At 10 to 30 percent
  on the interactive trace LRU is as good or better (0.372 vs 0.297 at 10 percent) because W-TinyLFU's admission filter rejects objects that
  are about to be reused in the regular sync pattern.
- ARC sits between: better than LRU at tiny sizes, slightly worse at 10 percent, never the winner. It also needs ghost lists (extra memory). I
  would not pick it.
- Gap to the offline bound stays large (0.37 vs 0.57 at 10 percent): recency and frequency are weak predictors for checkout traffic;
  the reuse is structural (objects of the commit being checked out), which motivates prefetch and warming below.
- Policy CPU cost is small: 87 to 170 ns per access single-threaded (W-TinyLFU highest at 150 to 170 ns), against a remote miss of tens of ms.
  Peak policy bookkeeping memory was 36 to 68 MB for the sketch/slab at these sizes (`peak_policy_heap_mb`, includes trace-independent
  allocations; coarse).
- Negative result: the periodic history scan in `checkout_sync_scan` did **not** change the ranking (hit rates within 0.01 of
  `checkout_sync` at every size). The scan was about 0.1 percent of accesses, too small to pollute a cache; scan resistance is shown only by
  the unit test and by the `ci_fresh` loop. A heavier scan (`fsck`, `gc_plan` read about 2x the store, see audit F11) was not simulated.
- Modelled time for the whole `checkout_sync` trace, batched fetch: 28,068 s with no cache, 9,211 s with a 30 percent LRU cache
  (3.0x), 25,766 s with 1 percent W-TinyLFU. On-demand (one RTT per object) is 15x worse than batched with no cache (421,281 s vs
  28,068 s). **Batching the tree-level fetch matters more than any policy.** This follows from the model's constants (40 ms RTT, 16 streams);
  the ratio is MODEL, the direction is not in doubt.

## 3. Two-level, prefetch, warming

Source: `data/cache.jsonl`, rows `two_level`, `prefetch`, `warming`.

- Two-level (memory L1 over a 30 percent LRU L2, inclusive fill): adding a 3 percent W-TinyLFU L1 moved 19.4 percent of accesses to L1
  and reduced remote misses from 30.4 to 28.4 percent; an LRU L1 of the same size caught 2.2 percent and changed remote misses by
  0.0. So a small memory layer only helps if it is frequency-aware, and then mostly by saving disk reads (L2 hit 0.696 → 0.522), not
  remote traffic. At 40 ns versus 3 to 11 us per local hit (section 1) the saving is real but small in absolute terms.
- Prefetch: after a sync, speculatively fetch the next 3 or 6 commits' objects of that zone (assumes the remote advances 1 to 5 commits
  between syncs, so the model is favourable to prefetch). With LRU 10 percent: demand hit rate 0.372 → 0.379 (3 ahead) → 0.380 (6 ahead);
  prefetch fetched 195 MB (3 ahead) and was useful for 60 percent of objects, costing 1.0 percent extra total bytes (18.44 → 18.54 GB); with W-TinyLFU only
  17 percent (3 ahead) and 10 percent (6 ahead) of prefetched objects were ever used and hit rate did not improve. Verdict: **prefetching next-commit objects
  is a marginal win (about 1.5 percent less modelled demand time with LRU) and a waste with a frequency-admission cache.** Likely better
  value: prefetch only the objects of the *same level batch* in one round trip (that is batching, section 2) not speculation about future commits.
- Warming, session 2 = the same client after a restart with an empty cache vs pre-filled with 10 percent / 2 = 29 MB: first 1500 accesses
  hit rate 0.000 cold, 0.027 warmed with the previous session's most frequent objects, **0.151 warmed with its most recently used objects**
  (`checkout_sync`; 0.073 on the scan variant). Recency beats frequency here because a checkout workload resumes at the head it left. Net time
  for that window: 4.67 s cold, 4.14 s warm (11 percent). Warming cost is off the critical path in the model; if it were paid up front it
  would be 29 MB of transfer.

## 4. Safety of caches under concurrent publish and GC

Background (from the product docs): objects are immutable and content-addressed (`docs/object-model.md`); the remote manifest is the
only mutable object and publish is `Collect → Upload* → PutTx → Load → Cas` (`docs/gc.md`); GC deletes an object only if unreachable
from a mark taken after the listing **and** untouched for the grace period, with `put` refreshing an existing object's time (touch-on-dedup).
The audit (F8, section 3.6) records that the manifest is never cached.

### 4.1 Classification

| Cache | What it stores | Safe under publish + GC? | Why |
|---|---|---|---|
| Content cache (bytes of object `id`) | `id → bytes`, verified by `hash(bytes) == id` on read | **Yes, unconditionally** | Content never changes. A hit is correct whether or not the remote still has the object or a GC has since deleted it: the reader is served true bytes. A concurrent publish only adds ids. Eviction is always safe: a miss refetches. Corruption on disk is caught by the hash check. |
| Structural cache derived from immutable inputs (commit-graph, changed-path filter, tree index) | function of a set of objects | Yes, if keyed by the exact input (commit id set) and verified or rebuilt on doubt | Already the rule in `cgraph.rs`. A graph entry for a commit that GC later deletes only matters if the commit is still referenced, in which case it was never deleted. |
| Positive remote-existence cache | `id → "remote has it"` | **No, unless guarded** | Used to skip upload/touch. Stale after GC deleted the object; or fresh but skipping the touch lets GC delete an object a publisher is about to reference. Creates a dangling reference, i.e. data loss. |
| Negative cache | `id → "remote lacks it"` | **No, unless guarded** | Stale as soon as anyone publishes the object. Causes spurious "missing object" failures and, in negotiation, re-uploads. |
| Manifest / refs cache | mutable pointer | Only with validation (ETag / generation check) | Mutable by design. |

Why positive caching of *bytes* is safe but of *existence* is not: bytes are a function of the id alone; existence is a function of time.

### 4.2 Exhaustive interleaving check (`modelcheck`, `data/modelcheck.jsonl`)

A breadth-first search over every interleaving of: publisher P (observe, upload-or-touch, CAS manifest), GC G (mark, optional re-read of roots,
sweep), cache filler C, a clock that ages the object, with at most one clock tick during a publish and one during a GC pass
(that is "publish and GC pass each shorter than grace minus touch-after"), grace = 5 ticks, touch-after = 2. The invariant:
whenever the manifest references X, the remote has X. Delete is unconditional (S3 DELETE is not conditional), so the "re-read roots then
delete" window stays open; protection must come from the grace period and touch.

| Mode | Initial object age 0, 2, 4, 5 ticks; GC with and without root re-check | Result |
|---|---|---|
| No cache (touch-on-dedup, product design) | 8 runs, 3.3 k to 4.6 k states each | **invariant holds** |
| Positive cache, TTL only, hit skips upload and touch | 8 runs | **violated** in all 8 (shortest: GC marks, client caches "X exists", publisher trusts the cache, CAS, GC sweeps the old X) |
| Positive cache guarded: valid only while GC epoch unchanged **and** estimated age ≤ touch-after | 8 runs | **invariant holds** |
| Negative control: guard trusts entries up to touch-after + 1 | 8 runs | **violated** for initial ages 0 and 2 (age 4 and 5 are never trusted) |

The negative control shows the check can fail and that the margin is real: **touch-after + publish duration + GC pass duration must be
smaller than grace.** With the product's 14-day grace and a quarter-grace touch-after (3.5 days) the margin leaves about 10 days, which a
publish or a GC pass will not use up; but a positive cache TTL must then be at most the touch-after, and in that regime a hit saves only
the existence check (the product already skips the touch for recent objects). So **a positive existence cache saves at most one HEAD per
dedup hit and carries the largest risk in the whole design: I recommend not building one.**
The tick abstraction (3 to 5 levels, one object, one publisher) is a small instance, not a proof for the system; it reproduces the argument
in `docs/gc.md` and shows which side conditions are load-bearing.

### 4.3 Negative caching under concurrent publish (Monte Carlo and model check)

Model check (`negative_cache_vs_publish`): actors N (cache "X absent"), Q (upload X, CAS manifest), R (read manifest, look X up).
Without a negative cache, or with one guarded by the manifest generation, no reader reports a missing object that exists. With a TTL-only
negative cache the shortest counterexample is 5 steps: N caches the absence, Q uploads and CASes, R reads the manifest and trusts the cache, reporting "missing" while the
remote has X.

Monte Carlo (`negcache`, 2 M samples per cell, `data/negcache.jsonl`): an object is absent now, published with probability 0.5 after an
exponential delay; a client probes it again after an exponential gap. Selected rows (mean gap between probes 60 s):

| Mean publish delay | TTL | Re-probes answered from cache | Stale "absent" answers (share of all re-probes) |
|---|---|---|---|
| 60 s | 10 s | 15 % | 0.6 % |
| 60 s | 60 s | 63 % | 9.9 % |
| 60 s | 600 s | 100 % | 25 % |
| 600 s | 60 s | 63 % | 1.3 % |
| 3600 s | 600 s | 100 % | 0.8 % |

Guarded variant: store the manifest generation with the entry and revalidate with one generation read per batch of 20 probes. Stale answers are
0 by construction (every publish CASes the manifest, so it changes the generation). The price is reuse: with 1 publish per minute and a 60 s gap, an entry is reusable 50 percent of
the time (`exp(-gap × rate)`), with 10 publishes per minute and a 60 s gap 9 percent, with 0.1 per minute 91 percent; plus 0.05 generation reads per probe. So guarded negative caching pays when the remote
is quiet and is nearly worthless when it is busy.

Positive-cache-versus-GC Monte Carlo (same file; **MODEL**, a conditional rate over orphan re-adoptions, intended to show scale and not
to be read as a production failure probability): with 14-day grace, an unguarded positive cache with a 1-day TTL creates a dangling reference
in 4.5 percent of the hits it considered, 3.5 days 14 percent, 14 days 50 percent; with 1-day grace and 1-day TTL 34 percent.

### 4.4 Rules

1. Cache object **bytes** freely, always verify `hash == id` on a hit that came from disk, and evict by any policy. No coordination with publish or GC is
   needed. (CONFIRMED by construction; the product already does this.)
2. Never cache existence on the remote across a GC epoch; if it must be cached, validate with the GC epoch/generation, bound its age by
   touch-after, and still touch (model check).
3. A negative entry must carry the manifest generation (or ETag) it was read under and is valid only while that generation is current;
   otherwise do not cache negatives. A TTL alone is never sufficient.
4. Mutable pointers (manifest, refs) are cached only behind a conditional request.
5. Derived structures (commit graph, filters, indexes) are keyed by their immutable inputs and rebuilt, never trusted, when the input set differs.
6. Local eviction of content (docs/cache.md "Cache" class) stays restricted to objects reachable from a remote-tracking head, as specified there; this experiment adds no reason to change it.

## 5. What was not run

- Real network, real S3 or MinIO latency (no cloud; MinIO not started). All network time is modelled.
- Real user traces; only generated ones. Repeated seeds; multi-client sharing of a cache; cache behaviour across processes.
- Concurrency stress of a real cache with a real GC (no destructive GC exists in the product, `docs/gc.md`); safety evidence is the model check and Monte Carlo above.
- Cold-start of memory-mapped formats, huge (>10 M object) caches, RocksDB under write pressure.

## 6. Reproduce

```sh
cd crates/vcs-storage-lab && ./sync.sh "cargo build --release --features rocks"   # box-specific wrapper; see labenv.sh
./target/release/cachebench --ops 3000 --commits 4000 --files 4000 --out data/cache.jsonl
LAB_DIR=/path/with/space ./target/release/backendbench --n 100000
./target/release/negcache --samples 2000000 && ./target/release/modelcheck
```
