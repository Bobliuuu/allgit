# Storage landscape: what other systems teach allgit

Issue: #180 (epic #190). Branch: `research/storage-engine`. Baseline: `cd0fd09`.
Status: survey, docs only. No experiments were run for this document.

Evidence convention. **CONFIRMED** means the statement was read in the cited primary source on the access date
(2026-10-31). **HYPOTHESIS** means it is our inference about allgit and must be tested by an experiment issue.
**[unverified]** means not read in a primary source during this pass (from memory or a secondary summary). Page text
was read through a summarizing fetch tool, not the raw page, so quoted numbers are the tool's extraction of the
source; re-check against the source before putting a number in a decision document. No revision hash is available for
live documentation pages; where the page prints a version or date we give it, otherwise "rev: live page".

## allgit constraints used for the fit column

From `docs/object-model.md`, `docs/zones.md` and the epic (#190):

- Objects are immutable, identified by BLAKE3 over `kind || version || payload`. Kinds: Blob, Chunked (manifest of
  `(chunk_id, len)` for files of 1 MiB or more), Tree, Commit (one per zone), Snapshot (`zone -> commit`),
  Transaction, Operation.
- A **zone** has its own commit chain; a Snapshot pins consistent zone versions; a Transaction is one atomic multi-zone
  publication. Sparse workspaces subscribe to a subset of zones.
- Remote: backends with `get`, `get_range`, `exists`, `put_if_absent`, `put_if_match`, `list`. A single manifest object
  whose compare-and-swap is the linearization point.
- Local: loose objects plus packs with one index each; no deltas, no general compression; lazy blob fetch with
  batched prefetch.

"Fit" below always means fit with those constraints. Because ids are hashes of the logical object, any technique that
changes only the *physical* encoding (compression, delta, packing, placement) is compatible with the id scheme.
Techniques that change *what is hashed* (for example a different chunker) change ids and need a format bump.

## Summary matrix

| System | Technique worth studying | Physical-only? | Fit |
|---|---|---|---|
| Jujutsu | lock-free op log in content-addressed store; `StackedTable` | yes | high (already close to allgit Operation) |
| Sapling / Mononoke | indexedlog; inherent vs derived data; multiplexed blobstore with WAL and healer; packblob (zstd + delta); metadata DB split | yes | high |
| Perforce Helix | metadata DB separate from archive files; lazy copies; RCS delta vs full | yes | medium |
| Mercurial | revlog delta chains, snapshot bound, sparse-revlog | yes | medium |
| Fossil | per-artifact zlib + delta in SQLite; delta format | yes | low-medium |
| Nix | store object content addressing; CA derivations early cutoff | n/a | low (different problem) |
| BorgBackup | append-only segments, compaction by sparsity, buzhash CDC | mostly | medium |
| restic | packs with trailing header, supersedable index files, write order, zstd v2 | yes | high |
| Kopia | 20-40 MB packs, index blobs, per-policy compression, epoch manager | yes | high |
| Duplicacy | object-per-chunk, no index, two-step fossil deletion | yes | medium (GC idea high) |
| Venti | write-once log arenas, hash-table index, hash trees, no delete | yes | medium |
| IPFS | Merkle DAG, chunk layouts, UnixFS | no (changes ids) | low |
| Bazel REAPI CAS | FindMissingBlobs, batch RPCs, GetTree, compressed-blobs, chunk mapping | yes (protocol) | high for remote protocol |
| RocksDB / Pebble / LSM | leveled compaction, key-value separation (WiscKey/BlobDB) | yes | medium-high for indexes and small objects |
| FoundationDB | strict-serializable multi-key txn; hard limits | n/a | medium (metadata sharding reference) |
| SQLite | blob-in-DB vs files, rollback journal, WAL | yes | medium (local index/metadata) |
| GitHub | Spokes quorum replication, geometric repack, MIDX + bitmaps | yes | medium-high |
| Google (Piper) | single repo on Bigtable/Spanner, cloud workspaces | n/a | context only |
| Meta (Sapling/Mononoke/Eden) | see Sapling row; segmented changelog, virtual FS | yes | high |

## Per-system notes

### 1. Jujutsu

- **Technique.** Storage is split into pluggable backends: commits, operations/views, op heads, commit index, working
  copy. The Git backend stores commits in a Git repo and keeps jj-only data (change id, predecessors) in a
  `StackedTable`, a custom key-value format: fixed-size sorted keys, "we never update a table in place", child tables
  link to parents so lookup/insert are O(log N) amortized, and no GC of unreachable tables yet. Operations and views
  live in content-addressed storage; the current op head is a small file whose creation makes an operation visible.
  Divergent op heads are merged by a 3-way merge of views. To keep commits alive for Git GC it writes a ref per commit
  under `refs/jj/keep/`. CONFIRMED.
- **Tradeoff.** Lock-free concurrency, at the price of visible divergence (conflicted bookmarks) and untested behavior
  on rsync/NFS/Dropbox-style sharing; the docs list Git-backend corruption as a known issue with `jj debug reindex` as
  recovery. CONFIRMED.
- **Fit.** allgit already has Operation objects (kind 6) and a CAS manifest. jj's op-head-as-file and view-merge is the
  model for multi-writer *local* operations; the manifest CAS replaces it remotely. `StackedTable` is a candidate
  immutable on-disk index that never rewrites (HYPOTHESIS: could replace one-index-per-pack with layered tables).
- **Sources.** https://docs.jj-vcs.dev/latest/technical/architecture/ and
  https://docs.jj-vcs.dev/latest/technical/concurrency/ (rev: live page, latest; accessed 2026-10-31).

### 2. Sapling and Mononoke (Meta)

- **IndexedLog.** Append-only Log is the single source of truth; indexes are derived from it through application
  functions and can be deleted and rebuilt; the standalone index is a radix tree with an atomic root-pointer replace
  giving lock-free reads; writes buffered in memory; XXH32/XXH64 checksum per log entry, index checksum per 1 MB;
  `repair` truncates to valid entries and rebuilds indexes; RotateLog bounds client cache size. CONFIRMED
  (https://sapling-scm.com/docs/dev/internals/indexedlog, accessed 2026-10-31).
- **Mononoke storage.** Immutable blobstore (Bonsai changesets, file contents, derived data) plus a SQL metadata DB
  (bookmarks, VCS mappings, commit-graph index with generation numbers and skip-tree pointers). Blobstore is a trait
  with composable decorators: Prefixblob, Cacheblob (cachelib then memcache), Multiplexedblob, Packblob. Multiplexed
  writes succeed on quorum, failures are logged in a WAL, and a healer job copies missing blobs. Packblob packs related
  blobs (versions of one file) using Zstd with delta against one blob as dictionary, with an envelope marking
  packed/unpacked. Placement rule: immutable content-addressed large data in the blobstore, mutable or index-heavy data
  in the DB. CONFIRMED
  (https://github.com/facebook/sapling/blob/main/eden/mononoke/docs/2.4-storage-architecture.md, accessed 2026-10-31;
  the repo's README notes the open-source export omits some internal functions).
- **Bonsai.** Commits store only inherent data (file-change list plus content ids); manifests and VCS-specific
  structures are *derived* asynchronously, not part of content hashes, and regenerable. CONFIRMED
  (.../2.1-bonsai-data-model.md, accessed 2026-10-31).
- **Client scalability.** Lazy history fetch; Segmented Changelog downloads commit-graph shape in "a few megabytes";
  sparse profiles committed in repo; virtual filesystem. Meta monorepo "tens of millions of files". Blog dated
  2022-11-15 (https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/). CONFIRMED.
- **Tradeoff.** Derived data adds a derivation pipeline and a lag before a commit is visible ("a few seconds" per the
  doc); multiplexing slows writes for redundancy; packblob needs read-side unpack cost [cost not measured in source].
- **Fit.** High, with a caveat. allgit hashes Trees into Commit identity, so it cannot adopt Bonsai wholesale, but the
  *layering* maps: Transaction/Snapshot as small mutable-ish metadata, blobs/chunks as the immutable bulk,
  commit-graph as derived and rebuildable (allgit already has a commit-graph). Packblob-style "delta/dictionary-
  compress versions of one path inside a pack" is a physical-only change. Healer + WAL is a model for GC/repair of a
  future multi-backend remote.

### 3. Perforce Helix Core

- **Technique.** Metadata lives in `db.*` files (each a binary-encoded table) separate from versioned "archive files"
  in depot directories. Lazy copies are references to existing depot files instead of duplicates (branching is cheap).
  `p4 archive` moves revisions to an archive depot; RCS-format revisions are skipped unless `-z`, and many RCS deltas
  carry notable compute cost. `p4 verify` checks archive integrity. Sizing guidance: P4ROOT volume at least 3x the
  current versioned files plus about 0.5 KB per user per file for the database. CONFIRMED via search-result summary of
  the admin guide and command reference
  (https://www.perforce.com/perforce/doc.current/manuals/p4sag/install-disk-space-allocation.html,
  https://www.perforce.com/manuals/cmdref/Content/CmdRef/p4_archive.html; accessed 2026-10-31; the page body itself
  was not re-read, the redirect target was not fetched). The internal RCS delta layout was not found: [unverified].
- **Tradeoff.** Metadata DB is a single scaling and failover point; archive files are per-file (not content-addressed),
  so dedup is only by lazy copy.
- **Fit.** Medium. The metadata/bulk separation is the same placement rule as Mononoke. Lazy-copy = allgit already gets
  this for free from content addressing. Useful lesson: delta storage with an escape hatch to full storage for hot
  files.

### 4. Mercurial

- **Technique.** Revlog: each file version stored as a compressed delta against a previous version, with a full
  snapshot when reconstruction would cost too much; a fixed-size index record per version (node id, parents, length,
  offset, chain base, linkrev); append-only so adding a version needs O(1) seeks; reconstruction takes one index lookup
  and one read. CONFIRMED for the original revlog (https://wiki.mercurial-scm.org/Revlog, archived wiki; says RevlogNG
  replaced it in Mercurial 0.9; accessed 2026-10-31). generaldelta and sparse-revlog are [unverified]: the internals
  doc (repo.mercurial-scm.org) returned an access-denied page.
- **Tradeoff.** Space saved by deltas vs read cost bounded only by the snapshot rule; delta chains require ordered,
  per-path storage which fits poorly with object stores that offer only whole-object GET (HYPOTHESIS; range reads help).
- **Fit.** Medium. Delta-per-path inside a pack works with allgit ids (ids stay on the logical object). Needs a
  path/lineage hint at pack time because ids are path-agnostic. Experiment: #184.

### 5. Fossil

- **Technique.** Whole repository is one SQLite file. Artifacts are BLOBs "compressed using zlib compression and, where
  applicable, using delta compression"; the canonical format is the unordered set of artifacts and compression is "not
  an inherent part of the Fossil file format". Median stored size after delta and zlib is 156 bytes (site's own repo,
  figure from the page). Shunning is a hash list in a `shun` table. Derived cross-references are rebuildable
  (`fossil rebuild`). Delta format: header (target size), copy/insert segments, checksum trailer, base-64-variant
  integers; no compression inside the delta. CONFIRMED
  (https://fossil-scm.org/home/doc/trunk/www/tech_overview.wiki,
  https://fossil-scm.org/home/doc/trunk/www/delta_format.wiki; accessed 2026-10-31).
- **Tradeoff.** Single-file DB gives atomicity and simplicity but caps concurrency (single writer) and offers no
  partial/object-store layout. The page does not quantify cost.
- **Fit.** Low-medium: a compact copy/insert delta format is small enough to reimplement; the "physical encoding is an
  optimization, canonical identity is the artifact set" principle matches allgit's separation of id and storage.
  Shun-by-hash resembles Mononoke redaction.

### 6. Nix

- **Technique.** Store objects are content-addressed or input-addressed; a content-addressed path depends only on the
  file-system-object graph, references, store dir and name. Hash methods: flat, text, NAR (references including self
  allowed only with SHA-256), git (experimental, SHA-1 only, no references). Self-references handled by a sentinel
  rewrite. Floating content-addressed derivations (`ca-derivations`, experimental) mean a rebuild with identical output
  does not change dependents (early cutoff effect). CONFIRMED (Nix 2.34.9 manual,
  https://nix.dev/manual/nix/stable/store/store-object/content-address and
  .../derivation/outputs/content-address; accessed 2026-10-31). Thesis: Dolstra, "The Purely Functional Software
  Deployment Model", Utrecht 2006 (https://dspace.library.uu.nl/handle/1874/7540; abstract-level only).
- **Tradeoff.** Reference rewriting and determinism requirements; early cutoff needs reproducible builds.
- **Fit.** Low for storage. The only transferable piece is the *serialization-first* rule: hash a canonical
  serialization (NAR) of a directory graph, with a stable canonical encoding. allgit's canonical codec already does
  this. A cache-key view (hash of inputs to find outputs) is relevant to #185 caches, not to object storage.

### 7. BorgBackup

- **Technique.** Repository of append-only segment files (~500 MB, 4 GiB cap from 32-bit offsets) with PUT/DELETE/COMMIT
  entries; in-memory HashIndex maps key to (segment, offset), regenerated if corrupt; buzhash CDC with defaults
  min 2^19, max 2^23, mask 2^21, window 4095 (chunks around 2 MiB), seeded per repository; chunks cache with refcounts;
  compaction forward-moving, only for sparse segments over a threshold, oldest to newest; files cache skips unchanged
  files at significant RAM cost (doc estimate: 1 Mi files / 1 TiB is about 0.31 GiB with defaults and about 2.8 GiB with
  chunker params 10,23,16,4095). CONFIRMED
  (https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html; accessed 2026-10-31, stable docs).
- **Tradeoff.** Large chunks give a small index and poor dedup granularity for small edits; small chunks multiply
  index memory. Single-client locking (not shown on this page; [unverified]).
- **Fit.** Medium. Sparse-segment compaction is the right shape for pack GC. Chunk-size memory arithmetic is directly
  relevant to allgit's 1 MiB manifest threshold. The files-cache idea matches allgit's directory cache.

### 8. restic

- **Technique.** Packs hold independently encrypted blobs followed by an encrypted header and 4-byte length, so a pack
  can be indexed from its tail and written as a stream. Blobs 512 KiB to 8 MiB, files under 512 KiB are not split,
  average about 1 MiB. Index files (JSON) kept below 8 MiB, with `supersedes` listing replaced indexes. Snapshot files
  point at a root tree. Rabin CDC with a 64-byte window and a repo-random polynomial; a 2025 attack that recovers the
  polynomial from chunk sizes was mitigated in 0.18.0 by random pack assignment. Write order: packs, then indexes, then
  snapshots; removal order: drop from index, then delete pack; only `prune` deletes, under an exclusive lock; locks
  are files, stale after 30 minutes. Repository format v2 adds zstd with an encoding-version byte. CONFIRMED
  (https://github.com/restic/restic/blob/master/doc/design.rst, master, accessed 2026-10-31).
- **Tradeoff.** Many small index files and listing cost on object stores; prune needs an exclusive lock, so no
  concurrent writers during GC.
- **Fit.** High. The "children before parents" write order is exactly the safety rule a manifest-CAS remote needs: upload
  packs and indexes, then CAS the manifest. Trailing-header packs support streaming upload and range-read of a single
  object via the index. Locks-as-files is the thing allgit should avoid (its CAS is better).

### 9. Kopia

- **Technique.** Layers: BLOB storage, content-addressable block storage, content-addressable object storage,
  label-addressed manifests. Blocks combined into packs of 20-40 MB; pack names are random with type prefixes
  (`p` data, `q` metadata, `x` index); an index maps block id to blob, offset, length; each pack also ends with a local
  index for recovery; large objects split and linked by indirect JSON. Compression per policy (s2 default family, zstd
  levels, pgzip, deflate; falls back to storing raw if compressed is larger; min size and never-compress extensions;
  LZ4 deprecated). Default splitter `DYNAMIC-4M-BUZHASH`. CONFIRMED
  (https://kopia.io/docs/advanced/architecture/, .../compression/, .../splitter/; accessed 2026-10-31). Epoch manager and
  maintenance modes: page returned 404, [unverified].
- **Tradeoff.** Metadata packs (`q`) separated from data packs keeps hot small objects together; compression choice
  depends on data (the doc itself says no single choice is best).
- **Fit.** High. Separating metadata packs from data packs is a direct candidate for allgit: Trees/Commits/
  Snapshots/Transactions are small and hot; Blobs and chunks are bulk. Zone-aware variant: one metadata pack family per
  zone (HYPOTHESIS, #184).

### 10. Duplicacy

- **Technique.** Each chunk is stored as its own file named by its hash, so existence is a file lookup and no index
  database or distributed lock is needed. Deletion is two-step "fossil collection": unreferenced chunks are renamed to
  fossils; backups ignore fossils (re-upload if needed); fossils are deleted only when every snapshot id has produced a
  new snapshot not seen by the collection step and that finished after it. Needs only rename as primitive. CONFIRMED
  (https://github.com/gilbertchen/duplicacy/wiki/Lock-Free-Deduplication; accessed 2026-10-31). `DESIGN.md` is a stub
  pointing to the wiki.
- **Tradeoff.** Deletion depends on later backups finishing; one object per chunk costs requests and listing on object
  stores (HYPOTHESIS; the page does not quantify it); fossil overhead not quantified by source.
- **Fit.** The GC protocol is the most relevant piece for #186: allgit needs a safe GC under concurrent writers, and the
  fossil rule ("mark, wait for a witness that every writer has seen the mark, then delete") can be recast over the
  manifest CAS: the manifest version is the witness. Needs a rename or equivalent; S3 has no rename, so use a tombstone
  key or manifest-listed "pending delete" set (HYPOTHESIS).

### 11. Venti

- **Technique.** Write-once blocks addressed by SHA-1 fingerprint; append-only log divided into self-contained sealed
  arenas (header, block directory, trailer); blocks up to 52 KB, compressed; disk hash table index with one-block buckets
  ("one disk access ... in almost all cases"); block cache and index cache; hash trees via pointer blocks give version
  sharing. Reported: virgin write 3.7 MB/s uncached, sequential read 0.9 MB/s uncached vs 14.8 raw RAID; dedup+compress
  saved 59.7% (bootes) and 76.5% (emelie); no deletion. CONFIRMED (Quinlan and Dorward, FAST 2002,
  https://plan9.io/sys/doc/venti/venti.html; accessed 2026-10-31). 2002 hardware, so numbers are historical only.
- **Tradeoff.** Random index I/O bounds throughput; fixed-size blocks miss shifted data; no reclamation.
- **Fit.** Medium. Arenas with a self-describing directory are the same idea as allgit packs; the paper's lesson that
  the *index lookup, not the data read*, dominates cold sequential reads motivates a prefetch/locality-aware layout
  (#184, #185).

### 12. IPFS

- **Technique.** Merkle DAG with CIDs; any change to a node changes all ancestors. UnixFS chunks files into nodes with
  fixed-size or Rabin chunking; balanced layout with default `max-width` 174; raw leaves default for CIDv1. CONFIRMED
  (https://docs.ipfs.tech/concepts/merkle-dag/, https://docs.ipfs.tech/concepts/file-systems/; accessed 2026-10-31).
  Default chunk size (256 KiB), block size limit and trickle layout were not in the fetched pages: [unverified].
- **Tradeoff.** Smaller chunks dedupe better but add nodes; per-block network exchange has high overhead.
- **Fit.** Low. allgit's Chunked manifest is a one-level version of the same thing. Not a source of new physical
  techniques; the layout choice (balanced vs one flat list) matters only for multi-GB files and range reads.

### 13. Bazel Remote Execution CAS

- **Technique.** Digest is (hash, size). Service: `FindMissingBlobs` (skip uploads), `BatchUpdateBlobs` and
  `BatchReadBlobs` for small blobs (per-blob status, size cap), `GetTree` (stream a whole directory tree from a root
  digest, partial results allowed), ByteStream for large blobs with resume (`write_offset`), compressed variants with
  the uncompressed digest in the resource name, servers must treat the empty blob as always present. Chunked-transfer
  RPCs `GetChunkMapping`/`RegisterChunkMapping` (client uploads chunks then registers composition, server verifies the
  digest, capability flags advertise support). CONFIRMED for the API shape
  (https://github.com/bazelbuild/remote-apis/blob/main/build/bazel/remote/execution/v2/remote_execution.proto, main,
  accessed 2026-10-31; first ~60 KB read through a summarizer). Version history of the chunking RPCs (which release
  added or removed SplitBlob) is [unverified].
- **Tradeoff.** Protocol-only: leaves layout and GC to the server; CAS lifetime is implementation-specific.
- **Fit.** High for the remote protocol. `FindMissingBlobs` (batched exists), `GetTree` (one round trip per subtree, which
  maps to a zone's root tree) and batch-read are the request-count levers for sparse clone, and are what allgit's
  lazy-fetch batching already approximates. Compressed-blob resource naming shows how to carry "logical id, physical
  encoding" on the wire.

### 14. RocksDB, Pebble and LSM trees

- **Technique.** Leveled compaction: L0 flushes, each deeper level one sorted run, targets grow by a multiplier;
  dynamic level bytes (default since 8.4) sizes levels from the last level so about 90% of data sits in the last level
  and 9% in the second last; write amplification "is often larger than 10". BlobDB (key-value separation): values at or
  above `min_blob_size` go to immutable blob files with pointers in the SSTs; GC is folded into compaction; trades
  space amplification for much lower write amplification. WiscKey (FAST 2016) is the paper behind this: keys in the LSM,
  values in a log; reported 2.5x-111x faster loading and 1.6x-14x faster random lookups than LevelDB; range queries need
  parallel random reads; crash semantics: a lost vLog value loses all later values. Pebble is a Go
  RocksDB-inspired store (RocksDB file format at v1; v2+ not openable by RocksDB directly), used by CockroachDB, adds
  range keys; value-separation details are not in its README. CONFIRMED
  (https://github.com/facebook/rocksdb/wiki/Leveled-Compaction, .../BlobDB, https://github.com/cockroachdb/pebble,
  https://www.usenix.org/system/files/conference/fast16/fast16-papers-lu.pdf; accessed 2026-10-31; paper text extracted
  locally). O'Neil et al. LSM-tree 1996: [unverified, not fetched].
- **Tradeoff.** Compaction write amplification vs read amplification vs space amplification; background compaction CPU.
- **Fit.** Medium-high but narrow. Do not store multi-MB objects in an LSM. Use it for the *small, hot, ordered*
  data: object index (id to pack/offset), commit-graph, per-zone path indexes, ref history. The WiscKey/BlobDB idea
  is already the shape of "index in LSM, bulk in packs". Interesting to test: replacing one-index-per-pack with a
  single LSM index (fewer index files to open on lookups) versus layered immutable tables (jj's `StackedTable`).

### 15. FoundationDB

- **Technique.** Ordered key-value store with strictly serializable multi-key transactions. Documented limits (version
  8.0.0 page): transactions longer than 5 seconds fail, transaction size at most 10,000,000 bytes of affected data,
  keys at most 10,000 bytes, values at most 100,000 bytes. The transaction manifesto argues ACID lets higher layers be
  composed on it. CONFIRMED (https://apple.github.io/foundationdb/known-limitations.html,
  https://apple.github.io/foundationdb/transaction-manifesto.html; accessed 2026-10-31). The manifesto page does not
  describe conflict detection; resolver details [unverified].
- **Tradeoff.** Strong transactions with small-value, short-transaction limits; operational cost of a cluster.
- **Fit.** Medium, as a reference rather than a dependency: it shows that multi-key atomicity and large blobs do not mix
  (the 100 KB value limit forces blobs elsewhere). For allgit, the interesting question (#186) is whether the single
  manifest object can be split per zone while a Transaction still commits atomically across zones. FDB-style answer: a
  small transactional metadata tier holding ref heads, bulk data in a CAS tier. On an object store without
  multi-key transactions the options are a single manifest CAS (current), a two-level manifest (root CAS over per-zone
  manifests, root is the commit point), or an external transactional DB (needs infrastructure the brief rules out).

### 16. SQLite

- **Technique.** Rollback journal: original pages saved to a journal; journal flushed before the database is touched;
  deleting (or truncating or zeroing the header of) the journal is the commit point; a super-journal makes a multi-file
  commit atomic. WAL: readers do not block writers, one writer, needs shared memory so not valid on network
  filesystems, checkpoint at about 1000 pages, long readers can stop checkpoints. "Faster than the filesystem" test:
  about 35% faster reads/writes of ~10 KB blobs and about 20% less disk than individual files; threshold in the Gray
  study was 250 KiB to 1 MiB; page size 4 KiB; measured week of 2017-06-05, SQLite 3.19.2-3.20.0, mostly consumer
  hardware; writes 5-15x slower than reads. CONFIRMED (https://www.sqlite.org/atomiccommit.html,
  https://www.sqlite.org/wal.html, https://www.sqlite.org/fasterthanfs.html; accessed 2026-10-31).
- **Tradeoff.** Great for small objects, bad for bulk and for multi-machine writers.
- **Fit.** Medium. Strongest argument: allgit's loose-object directory for objects below about 100 KB is the exact
  case SQLite measured against (many small files, per-file filesystem overhead, ~20% disk saving from page packing).
  Candidate local object store for small objects and indexes (HYPOTHESIS; needs a benchmark on Linux ext4/xfs/APFS).
  The multi-file super-journal commit point is a template for allgit's local multi-zone publish ordering.

### 17. GitHub

- **Spokes (distributed Git).** Replicates at the Git application level (replacing block-level DRBD), every push passes
  a proxy that replicates to several fileservers, writes guarantee "at least a quorum", three-phase commit with replicas
  as a lock, per-replica "Spokes checksum" (XOR of hashes of each ref name/value pair, incrementally updatable) after
  every update, ref update about a third of a second, geo-replicas non-voting. CONFIRMED (published 2017-10-13, updated
  2025-06-03: https://github.blog/engineering/infrastructure/stretching-spokes/; accessed 2026-10-31).
- **Repack and indexes.** Single-pack repack became a quadratic cost for large repositories and reachability bitmaps
  only covered one pack; fixes were multi-pack indexes with bitmaps, reverse indexes (`.rev`), and
  `git repack --geometric` (packs kept in a geometric progression, only small recent packs rewritten, full repack every
  eighth). Reported: average 5.67 CPU days saved per hour and per-repository repack time about one minute to 15
  seconds. CONFIRMED (Taylor Blau, 2021-04-29, updated 2022-06-30:
  https://github.blog/engineering/infrastructure/scaling-monorepo-maintenance/; accessed 2026-10-31). Incremental MIDX
  layers appear in the Git 2.55 highlights (search snippet only, [unverified]).
- **Tradeoff.** Geometric repacking bounds write amplification per push at the cost of more packs to search.
- **Fit.** Medium-high. allgit has packs with one index each and no repack policy; geometric merging of packs is
  directly applicable and testable (#184). A quorum checksum over refs is analogous to verifying the manifest against
  replicas. Bitmaps = a precomputed reachability index; allgit's analogue would be per-zone reachability for sparse
  fetch (#185).

### 18. Google (Piper)

- **Technique.** Single repository ("tens of thousands" of developers) on Google infrastructure, originally Bigtable,
  now Spanner, with trunk-based development and atomic changes across projects. CONFIRMED at abstract level
  (Potvin and Levenberg, CACM 59(7), 2016, https://research.google/pubs/why-google-stores-billions-of-lines-of-code-in-a-single-repository/;
  accessed 2026-10-31). Scale numbers vary between secondary summaries (one billion files/lines vs two billion), full text
  returned 403, so no number is cited here: [unverified]. Cloud workspace (CitC) and FUSE details: [unverified].
- **Tradeoff.** Needs a custom backend and heavy tooling; no public storage format.
- **Fit.** Context only: confirms "metadata in a transactional database, workspace virtualized, atomic multi-project
  commit" is the design point that zones are aiming at, but offers no portable physical technique.

### 19. Meta (summary of rows 2 and Eden)

Covered under Sapling/Mononoke above. Additional primary point: the 2022 blog says the history is three quarters of the
5.5 GB Linux repo, and lazy history fetch makes terabyte-scale repos workable; Eden/virtual filesystem is mentioned there
without a name. The Mononoke docs index lists packblob, walker/validation ("compression analysis"), redaction and
microwave (cache warming); those docs were not all read.

### Also consulted: FastCDC

Xia et al., USENIX ATC 2016: gear-hash CDC, about 10x faster than Rabin-based CDC and about 3x faster than other
Gear/AE CDC in the authors' experiments (https://www.usenix.org/system/files/conference/atc16/atc16-paper-xia.pdf, text
extracted locally; accessed 2026-10-31). Relevant to allgit's chunker choice (#183). The paper's dedup-ratio effect was not
extracted: [unverified].

## Cross-cutting patterns

1. **Separate immutable bulk from small mutable/ordered metadata** (Mononoke, Perforce, Kopia `p`/`q`, jj). allgit
   already does this at the remote (objects vs manifest); it does not locally.
2. **Index is derived and rebuildable** (indexedlog, Borg, Fossil crosslinks, restic index, Mononoke derived data).
   Implication: indexes can use whatever format is fastest and be treated as cache.
3. **Write children before parents; commit by a single small atomic step** (restic order, SQLite journal delete, jj op
   head file, allgit manifest CAS).
4. **GC needs a witness that writers have moved on** (Duplicacy fossils, restic exclusive lock, Borg segment hints,
   Mononoke WAL+healer).
5. **Packs are merged on a size schedule, not all at once** (GitHub geometric, Borg sparse compaction).
6. **Compression and delta are physical and optional** (Fossil states this outright; Mononoke packblob; Kopia
   fallback-to-raw). Keep allgit ids over uncompressed logical objects.

## Negative and null results

- No source gave a head-to-head number comparing delta compression with CDC for source code, binaries or ML
  checkpoints. That question stays open for #183/#184; do not infer it from this survey.
- Perforce's internal RCS layout, Mercurial's generaldelta/sparse-revlog internals, Kopia's epoch manager and Google's
  Piper scale numbers could not be read in this pass.
- Two fetch targets returned 404 or 403 (Kopia epoch manager, CACM full text); one returned a bot wall (Mercurial
  internals); Mononoke `packblob.md` path does not exist (the content is in `2.4-storage-architecture.md`).
- Several figures above come from 2002-2017 hardware (Venti, SQLite) and must not be used as allgit predictions.

## Ranked shortlist of techniques to prototype

Ranking weighs expected gain for allgit's stated workloads, risk to atomicity, and cost to prototype. All are
HYPOTHESES until an experiment issue produces data. Each item names the experiment issue it feeds.

1. **Metadata/bulk pack split plus trailing-header packs (Kopia `p`/`q`, restic).** Put Tree, Commit, Snapshot,
   Transaction in metadata packs and Blob/chunks in data packs, one-pass streaming write with the index at the tail.
   Why first: physical-only, no id change, targets GET count for sparse and historical lookup, easy to measure.
   Feeds #184, #185.
2. **Geometric pack merging (GitHub, Borg sparse compaction).** Why: allgit has packs with one index each and no
   repack policy; the policy bounds write amplification and read fan-out and is a prerequisite for any GC. Feeds #184, #186.
3. **Safe GC via witness (Duplicacy fossils recast on the manifest CAS, restic write order).** Why: GC is dry-run only
   today, and concurrent-writer safety is the riskiest unsolved correctness item; this has a model-checkable
   protocol. Feeds #186.
4. **General compression layer with raw fallback and a per-kind policy (Kopia, restic v2, Fossil).** Why: cheapest
   large win candidate for source-like objects; Kopia's fallback to raw handles already-compressed data. Compare
   zstd levels and block sizes (small-object pack blocks vs whole object). Feeds #183.
5. **Delta/dictionary compression within a pack across versions of a path (Mononoke packblob, Mercurial revlog,
   Fossil delta).** Why: addresses source history where CDC does little; bounded chain/snapshot rule from revlog
   limits read cost. Higher risk: needs path hints at pack time. Feeds #184.
6. **Batched remote protocol primitives (Bazel `FindMissingBlobs`, `GetTree`, batch read).** Why: GET count is the
   dominant sparse-clone cost on object stores; check whether allgit's batched prefetch already reaches the same
   request count as `GetTree`-style subtree fetch. Feeds #185 and the S3 backend work.
7. **Derived, rebuildable indexes with a layered immutable table or LSM (indexedlog, StackedTable, RocksDB).** Why:
   replace one index per pack, benchmark lookup latency and open cost with N packs. Medium payoff, low risk since
   indexes are rebuildable. Feeds #185.
8. **Two-level manifest for per-zone sharding (FDB-style reference, Mononoke metadata DB split).** Why last: highest
   potential for large repos with many zones, but highest risk to the model-checked atomic multi-zone commit. Prototype
   only inside the model checker first. Feeds #186.
9. **Small-object local store in SQLite (SQLite fasterthanfs).** Cheap to try for loose objects under about 100 KB;
   expected to matter mostly on platforms with costly small files. Feeds #184.

Not recommended for prototyping: IPFS-style DAG layout (no gain over Chunked manifest, changes ids), Nix-style
input-addressing (different problem), FoundationDB or any external transactional database as a dependency (infra
outside the brief).

## Reproduction notes

This document is literature and documentation review, produced 2026-10-31 by fetching each URL once and reading the
extracted text; PDFs for Venti, WiscKey and FastCDC were text-extracted locally with `pypdf`. No benchmarks were run.
No machine-readable data is attached. Quoted figures were taken from the tool's extraction of each page and not
re-checked against the raw HTML.
