# Git storage internals as a reference for allgit (research #179, part 1)

Status: research, branch `research/storage-engine`, epic #190. Written 2026-10-31.
Companion: [walgit-analysis.md](walgit-analysis.md). Sibling work: #178 (audit of allgit's current path), #180 (other systems), #181 (cost model).

Vocabulary: this repository's unit of independent history and sparse sync is the **zone**.

## 0. Method, sources, revisions

Primary sources, all read from a local clone, not from memory:

| Source | Revision | Accessed |
|---|---|---|
| Git source tree `Documentation/` (technical docs, `gitformat-*`, `git-*` man pages, `config/*.adoc`), https://github.com/git/git | tag `v2.56.0` (tag object `2478544319491bbd2a42cc57a83e3d2f4730a280`, commit `a018953688f1b10bddf91bff8747068f5f4746a4`, 2026-09-27) | 2026-10-31 |
| Git LFS specification, https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md | `main` as fetched on the access date, header read only (pointer file rules) | 2026-10-31 |
| allgit design, `docs/object-model.md` in this repo | `cd0fd09` | 2026-10-31 |

Experiments (section 4) ran on the Lambda box `lambda-final`: AMD EPYC 7J13, 30 cores, kernel `6.8.0-1046-nvidia`, Rust 1.99.1 (not used by these runs), **system git 2.34.1** (the box's package; a newer git was not built there). Therefore every measured number is for git 2.34.1. Documentation claims are for v2.56.0. Features documented but newer than 2.34 (cruft packs, incremental MIDX, `--path-walk`, name-hash v2, bitmap lookup tables) were **not exercised**.

Evidence labels: **CONFIRMED** = read in the cited primary doc or measured by us (data committed); **HYPOTHESIS** = our reasoning about cost on object storage, not yet measured; **[unverified]** = recalled, not re-read.

What allgit has today (from `docs/object-model.md` at `cd0fd09`): BLAKE3 256-bit ids over `kind||version||payload`, loose objects plus a single-format local pack (`VCSPACK1`, no compression, no deltas, 44-byte idx entries, one idx per pack), chunk manifests for files of 1 MiB or more (gear CDC, min/avg/max 4/16/64 KiB), per-zone commit DAGs, Snapshot and Transaction objects, an atomic publish through one CAS-ed manifest. Applicability notes below refer to this.

## 1. Summary table

| # | Technique | Exists to | Object-storage pain (HYPOTHESIS unless noted) | Take for allgit |
|---|---|---|---|---|
| G1 | Loose objects | simple writes | one PUT per object | keep as local write buffer only |
| G2 | Packfile | few files, zlib per object | needs idx to be useful | already adopted in simplified form |
| G3 | OFS/REF delta, thin pack | cross-version compression | chain reads = extra ranged GETs | measure; maybe bounded-depth, size-gated |
| G4 | Delta search heuristics (window, depth, name-hash sort) | find good bases cheaply | CPU/RSS at repack, not at read | only if delta chosen |
| G5 | Delta chain depth (default 50, max 4095) | trade read cost for size | mean chain 14 at depth 50 (CONFIRMED on git.git) | cap far below 50 for remote packs |
| G6 | Pack `.idx` v2 | O(log n) lookup | 8 to 11% of pack (CONFIRMED), one fetch per pack | adopt fanout; consider smaller footprint |
| G7 | Reverse index `.rev` | offset to object map | rebuilt in RAM if absent | only if offset-ordered scans are needed |
| G8 | Multi-pack-index (MIDX), incremental chains | lookup across many packs | a second global object that must be rewritten | strong candidate shape for the pack catalog |
| G9 | Reachability bitmaps, pack reuse | fast "what to send" and verbatim copy | bitmap build cost, tied to one closure | HYPOTHESIS: the zone-scoped analogue is cheaper |
| G10 | commit-graph, generation numbers, Bloom filters | history walks without reading commits | small file, big win | allgit already has a commit-graph (docs/research/commit-graph.md) |
| G11 | Cruft packs and mtimes | safe pruning of unreachable objects | GC race with in-flight writers | the grace-window idea matters for allgit GC |
| G12 | Geometric repack, gc thresholds | amortized compaction | rewrite cost, S3 request and egress cost | adopt geometric policy |
| G13 | Delta islands | keep per-audience packs servable by reuse | none new | maps to per-zone placement |
| G14 | Partial clone, promisors, filters | do not download what is not needed | one request per missing object unless batched | allgit's lazy fetch already is this |
| G15 | Large object promisors | big blobs elsewhere | n/a | confirms size-split placement |
| G16 | Packfile URIs, bundle URIs | offload bulk bytes to a CDN or object store | client support, auth | the bucket is the CDN for allgit |
| G17 | Sparse checkout, sparse index | smaller worktree and index | none for storage | allgit zones are the structural version |
| G18 | Shallow | truncate history | breaks reachability assumptions | do not mimic |
| G19 | Git LFS | keep big files out of packs | second system, second auth | allgit's chunk manifests replace it |
| G20 | reftable | scalable refs | refs are not objects | informs manifest or ref sharding, #186 |

## 2. Techniques in detail

Every entry: what it does, cost, applicability to allgit's object model, evidence.

### G1. Loose objects

- What: each object is `<type> <size>\0<data>`, zlib-compressed, in `objects/aa/bbbb...`. The id hashes the uncompressed form.
- Cost: one file (on S3: one PUT and one GET per object). Git's own docs say loose objects are compacted into packs "to improve disk space usage and speed up the lookup" and sharded by first two hex characters because some filesystems degrade with large directories.
- Applicability: allgit already uses loose objects as the local write path (`objects/<aa>/<rest>`), and ships them individually on publish. On an object store this is the worst layout for per-request cost. Keep loose only as a local staging area.
- Evidence: CONFIRMED, `Documentation/gitformat-loose.adoc` @ v2.56.0. `gc.auto` default 6700 loose objects triggers `gc --auto` (`Documentation/config/gc.adoc`).

### G2. Packfile

- What: header (`PACK`, version, count), then entries: a variable-length (3-bit type, then 7-bit groups) size, then zlib data, or a delta (G3); trailer is a checksum of all preceding bytes. Object ids are not stored in the pack; ids live only in the idx. Pack size encoding is not limited to 32 bits.
- Cost: a pack alone cannot be searched; reading one object needs the idx (G6) and then a range read of the entry. Entry boundaries are known only from the next offset or from `.rev` (G7). Whole-pack integrity needs a full read.
- Applicability: allgit's `VCSPACK1` is a deliberately simpler version (u32 length prefix, no compression, sorted by id). The Git design shows what allgit omits: per-entry compression and deltas, a CRC per entry so repack can copy compressed bytes without recompression, and the "pack is self-contained on disk" rule.
- Evidence: CONFIRMED, `gitformat-pack.adoc` @ v2.56.0 ("pack-*.pack", "Object encoding", "Deltified representation").

### G3. OFS_DELTA, REF_DELTA, thin packs

- What: an entry can be a delta against a base. `OFS_DELTA` stores a negative offset inside the same pack; `REF_DELTA` stores the base object id and can point outside the pack (a "thin pack", legal on the wire, but "when stored on disk however, the pack should be self contained to avoid cyclic dependency"). Delta data begins with base size and result size and then copy/insert instructions.
- Cost: reconstructing an object requires reading and inflating the whole chain to its base. Writing a good delta requires a search (G4).
- Applicability: allgit has no deltas. Candidates for deltas are text trees and text blobs across versions of the same path. For large binaries allgit already chose content-defined chunking; whether delta beats CDC per data class is exactly experiment area D/I in the epic and is not answered here.
- Evidence: CONFIRMED (doc). Chain read amplification on object storage: HYPOTHESIS (see G5 for the measured chain lengths).

### G4. Delta search heuristics

- What: `pack-objects` sorts candidate objects by type, then by a hash of the path name (so versions of the same file are neighbors), then by size descending, and tries each object against a sliding `--window` of preceding objects. Defaults: `--window=10`, `--depth=50`; maximum depth 4095. `--window-memory` caps window RAM per thread; `--threads` multiplies memory. Existing deltas are reused unless `--no-reuse-delta`; `--no-reuse-object` also recompresses. `--path-walk` (newer) walks per path instead of sorting by name hash.
- Cost, measured (CONFIRMED, our run, git 2.34.1, one trial each, box not idle, see section 4): on a full repack of git.git (422,883 objects): window 10 depth 50 gave 147.5 MB in 53 s and 1.49 GB RSS; window 250 depth 50 gave 107.5 MB in 82 s and 3.60 GB RSS; window 10 depth 10 gave 172.0 MB in 39 s and 1.35 GB RSS. The pack that GitHub sent on clone was 325.3 MB. Window size is a cost paid at repack, never at read.
- Applicability: any delta layer in allgit needs a path-aware candidate ordering; allgit trees already carry names, so the name-hash trick is available. Because allgit's content is zone-scoped, search windows can be zone-local.
- Evidence: CONFIRMED docs (`git-pack-objects.adoc`, `config/pack.adoc`, `technical/pack-heuristics.adoc`); measurements `data/git-structure-gitgit.json`.

### G5. Delta chain depth

- What: `--depth` bounds the length of a delta chain. Default 50.
- Cost, measured (CONFIRMED, same runs): with depth 50 the **mean chain length was 14.18** and 102,165 of 422,883 objects (24.2%) were undeltified; with depth 10 the mean was 4.84 and the pack was 16.6% larger than depth 50 (172.0 vs 147.5 MB); with window 250 at depth 50 the mean was 12.85 and 97,474 objects were undeltified. About a quarter of all objects are base objects at any setting. Histogram per depth is in the JSON.
- Object-store reading of that number (HYPOTHESIS): a random read of one object averages about 14 delta applications, each needing a base that may lie in a different block than the delta. Over ranged GETs this multiplies requests unless a block cache holds the neighborhood. walgit guards chains at 4096 and caches decoded objects (see walgit-analysis W12).
- Applicability: if allgit adds deltas, depth should be a first-class tunable and probably far below 50 for data served by range reads, with deltas placed near their bases. The size/depth tradeoff above is a Pareto point list, not a ranking: depth 10 costs 17% size for 3x shorter chains on this data. Text-code data only; do not extrapolate to binaries.
- Evidence: CONFIRMED measurement on one repository; extrapolation HYPOTHESIS.

### G6. Pack index v2 (`.idx`)

- What: magic and version, 256-entry fanout table, sorted object ids, a CRC32 per entry, 4-byte offsets (31-bit, high bit means "index into the 8-byte table"), an 8-byte offset table (empty under 2 GiB), pack checksum and idx checksum. The lookup narrows by the first id byte via fanout, then binary searches.
- Cost, measured (CONFIRMED): the idx was 11,841,796 bytes for 422,883 objects (28 bytes per object), i.e. 8.0% of the 147.5 MB pack, 11.0% of the 107.5 MB pack and 6.9% of the 172.0 MB pack. The idx size does not depend on pack size, so better-compressed packs have proportionally larger indexes.
- Object-store reading (HYPOTHESIS): a cold reader must GET the idx (about 28 bytes per object) before the first object; the idx is a fixed cost per pack that grows with object count and can rival a sparse workspace's useful bytes.
- Applicability: allgit's idx entry is 44 bytes (`id[32] + offset u64 + len u32`), 57% larger per object than Git's 28 (20-byte SHA-1 id + 4 + 4) and about the size of Git's SHA-256 form (32+4+4 = 40). A fanout table plus 4-byte offsets plus truncated-id prefixes are available size reductions; the epic's index experiments (#185) should measure them. allgit also loads the whole idx in memory; a fanout lets a reader range-read only the needed slice of a remote idx (2 GETs per lookup instead of one big GET) at the cost of one more round trip.
- Evidence: CONFIRMED format (`gitformat-pack.adoc` "pack-*.idx"), CONFIRMED sizes (data JSON).

### G7. Reverse index (`.rev`)

- What: a table mapping position in offset order to position in idx (name) order, with trailer checksums. Needed to learn an entry's end offset and for bitmaps and `OFS_DELTA` resolution in reverse. `pack.writeReverseIndex` defaults to true; if missing Git rebuilds it in memory each time (`pack.readReverseIndex`).
- Cost: 4 bytes per object. walgit reports that a missing `.rev` cost 2.85 s per fetch on a 60 M-object repository (walgit-reported, not reproduced here).
- Applicability: only relevant if allgit packs are read in offset order (streaming a whole pack, or computing entry length without storing a length). allgit stores `len` in its idx instead, which makes `.rev` unnecessary for point reads.
- Evidence: CONFIRMED format; the 2.85 s figure is walgit-reported.

### G8. Multi-pack-index (MIDX), incremental MIDX

- What: one file listing all packs, one sorted id table with (pack id, offset) per object, optional pseudo-pack ordering for bitmaps, reverse index chunk, and a `BTMP` chunk mapping packs to bitmap ranges to allow multi-pack verbatim reuse. It gives O(log N) lookup over any number of packs; if an object is in several packs one copy is chosen ("preferred pack" else newest). The pack idx files stay and the MIDX can be deleted without data loss. Newer Git can write an incremental MIDX chain whose layers are geometrically merged (`repack.midxSplitFactor`, default 2).
- Cost, measured (CONFIRMED): for one pack, the MIDX was 11,841,892 bytes (96 bytes more than the sole idx, so a MIDX over N packs costs about one id table total, not per pack).
- Applicability: HYPOTHESIS but strong. allgit currently has one idx per pack. Many small packs (one per publish) force a reader to fetch many idx files. A MIDX-shaped catalog (one object, rewritten or layered by a compactor) turns "which pack has this id" into one lookup. On object storage the MIDX is a second mutable-by-replacement object that must be versioned with the manifest; the incremental chain is the append-only variant and is the closer fit to immutable objects.
- Evidence: CONFIRMED docs (`technical/multi-pack-index.adoc`, `gitformat-pack.adoc`, `git-repack.adoc` @ v2.56.0); size CONFIRMED.

### G9. Reachability bitmaps and pack reuse

- What: an EWAH-compressed bit per object (in pack or MIDX order) for selected commits, XOR-compressed against earlier bitmaps; optional hash cache (4 bytes per object) to feed delta heuristics, and optional lookup table (loads individual bitmaps lazily). With `pack.allowPackReuse` (`true`/`single`, or `multi` with a MIDX bitmap) `pack-objects` sends whole ranges of an existing pack verbatim when all requested objects in the range are present with their bases.
- Cost, measured (CONFIRMED): the bitmap for the 147.5 MB, 422,883-object pack was 2,470,810 bytes (1.7% of the pack). Build cost was part of a full repack; not separated.
- Applicability: allgit's serving unit is a zone, not a repository closure. HYPOTHESIS: a per-zone, per-snapshot "which objects does this zone version need" structure is the allgit analogue, and its job (serve a sparse clone with few requests) is the same. The Git constraint that a bitmap needs a full-closure pack (`BITMAP_OPT_FULL_DAG`) maps poorly to a zone-sharded store and filtered (sparse) packs.
- Evidence: CONFIRMED docs (`technical/bitmap-format.adoc`, `config/pack.adoc`) and size.

### G10. commit-graph, generation numbers, Bloom filters

- What: a file with per-commit parents, root tree, date and generation number, plus optional Bloom filters of changed paths (`BIDX`/`BDAT` chunks). Generation number v1 is topological level; v2 is corrected committer date. A commit has generation greater than any ancestor, so a walk can stop early. Split chains let new layers be appended.
- Cost, measured (CONFIRMED): for git.git, 5,157,668 bytes without and 5,975,339 bytes with changed-path Bloom filters (+15.9%).
- Applicability: allgit already has a commit-graph (`docs/research/commit-graph.md`); the Git doc is the reference for generation-number semantics. Per-zone history is the natural shard for allgit. The changed-paths filter is the part with a storage tradeoff and applies to path-restricted history queries.
- Evidence: CONFIRMED (`technical/commit-graph.adoc`, `gitformat-commit-graph.adoc`).

### G11. Cruft packs

- What: unreachable objects are stored in a dedicated pack with a sidecar `.mtimes` file recording per-object mtimes, replacing loose-unreachable storage, so grace-period pruning no longer needs one file per object. Default in `git gc` (`gc.cruftPacks` true). Expiry via `--cruft-expiration`.
- Why it exists: unreachable objects cannot be deleted immediately "since doing so could race with an incoming push which may reference an object which is about to be deleted". Per-object mtimes are required so freshening one object does not renew the whole pack's life.
- Applicability: this is the core of allgit's GC design problem (concurrent publishes vs sweeper). The doc is the clearest statement of the race and of why per-object timestamps (not per-pack) matter. For an object store, mtime per object is free (object metadata) only if objects are not packed; once packed, a sidecar is needed. See #186 for protocol experiments.
- Evidence: CONFIRMED docs (`gitformat-pack.adoc` "cruft packs", `git-repack.adoc`, `config/gc.adoc`). Not exercised (git 2.34.1 on the box predates default cruft).

### G12. Geometric repack and automatic gc thresholds

- What: `git repack --geometric=<factor>` chooses the smallest set of packs to roll into one so that each successive pack has at least `factor` times the objects of the next larger one; large packs are left intact, loose objects are rolled in. `gc.autoPackLimit` default 50 packs; `gc.bigPackThreshold` keeps large packs; `--keep-pack` excludes packs.
- Cost: amortized rewrite cost is O(n log n) object-bytes over the life of a repository (HYPOTHESIS, standard argument for geometric merging). On object storage every rewrite is read + write egress and PUT requests; deletions of the old packs must wait for readers (G11 race).
- Applicability: allgit has no multi-pack merge today (`docs/object-model.md`: "Not done: ... multi-pack merging"). Geometric merging is the established policy; walgit adopts it (W16). The epic should test the factor and the interplay with zone-sharded packs.
- Evidence: CONFIRMED docs; the cost remark is HYPOTHESIS.

### G13. Delta islands

- What: `pack.island` groups refs into islands; `pack-objects` refuses to make a delta from object A against a base that is not present in all of A's islands, so a pack for one island can be reused without recomputing deltas. `pack.islandCore` orders a core island first.
- Applicability: this is the Git mechanism for the allgit problem "a zone's pack should be self-contained". If allgit adds deltas, base selection must stay within a zone (or within a shared base set) so a sparse workspace fetches self-contained ranges. Blobs shared across zones (dedup) conflict with that and need an explicit rule.
- Evidence: CONFIRMED docs (`git-pack-objects.adoc` "DELTA ISLANDS").

### G14. Partial clone, promisor remotes, object filters

- What: the client sends a filter spec (`blob:none`, `blob:limit=`, `tree:<depth>`, sparse filters); the server omits objects; the omitted-object packs are marked promisor packs; missing objects are demand-fetched. Multiple promisor remotes are supported.
- Cost (HYPOTHESIS for object storage): without batching, one request per missing object; Git mitigates by batching fetches for known sets (for example sparse checkout prefetch).
- Applicability: allgit's lazy blob fetch with batched prefetch is the same idea; its distinguishing property is that omission is structural (zones) rather than a negotiated filter. The partial-clone doc lists the two motivating cases that match allgit's workloads: files outside the working cone and large binary assets.
- Evidence: CONFIRMED docs (`technical/partial-clone.adoc`).

### G15. Large object promisors

- What: a design (documented, partly implemented) in which large blobs live on a separate promisor remote backed by object storage, so the main remote stores only code-sized objects. The document cites GitLab statistics that over 75% of disk is blobs larger than 1 MB, often binary, and argues deltas and zlib rarely help those.
- Applicability: supports allgit's size-split decision (chunk manifests at 1 MiB) and suggests placing large chunks in a separate prefix or tier. The document explicitly excludes chunking and dedup from its scope, which is where allgit goes further.
- Evidence: CONFIRMED docs (`technical/large-object-promisors.adoc`). The 75% figure is the document's claim about GitLab data; not verified by us.

### G16. Packfile URIs and bundle URIs

- What: protocol v2: the server may send, before the `packfile` section, URIs of packs the client downloads and indexes itself (`fetch.uriProtocols`); the experimental server config `uploadpack.blobPackfileUri` excludes listed blobs from the dynamic pack. Bundle URIs let a client bootstrap from bundles at any location (creation tokens for incremental bundles).
- Applicability: the transfer-offload pattern is native to an object store: pack and idx objects are already addressable. walgit builds on exactly this (W19). For allgit, the equivalent is returning object-store keys (or presigned URLs) for the packs a sparse workspace needs instead of proxying bytes.
- Evidence: CONFIRMED docs (`technical/packfile-uri.adoc`, `technical/bundle-uri.adoc`).

### G17. Sparse checkout and sparse index

- What: cone mode limits the worktree to chosen directories (plus ancestors); sparse index collapses out-of-cone directories into tree entries in the index. They shrink the working tree and index; they do not change object storage.
- Applicability: client-side only in Git. In allgit, zones are the structural counterpart and also decide what is stored and synced, which Git's sparse checkout cannot.
- Evidence: CONFIRMED docs (`technical/sparse-checkout.adoc`).

### G18. Shallow

- Truncates history to a depth or date; breaks the closure invariant (bitmaps, connectivity) and needs special negotiation (`technical/shallow.adoc`). Not recommended as a model for allgit.

### G19. Git LFS

- What: files are replaced by pointer files of fewer than 1024 bytes (`version`, `oid sha256:<hex>`, `size`); the bytes live on a separate server through a batch API with basic transfer.
- Cost: a second service with its own auth, no cross-repo dedup unless the server provides it, no deltas, and a pointer that can drift from content if hooks are missing. walgit documents its LFS handling, including a read-through to an upstream (walgit-analysis W24).
- Applicability: allgit's chunk manifests give the same separation inside one content-addressed store with dedup by chunk id, so LFS adds nothing but import compatibility.
- Evidence: CONFIRMED (spec header read: pointer rules, sha256 only; batch API details not re-read here, [unverified] beyond pointer format).

### G20. reftable

- What: a block-structured sorted ref table designed for near-constant-time lookup, `O(size_of_update)` atomic multi-ref updates and combined reflog. Motivated by `packed-refs` rewriting the whole file (the doc's example: 62 MB in, 62 MB out for a 2-ref update at 866k refs).
- Applicability: refs are not objects, but the same pressure applies to allgit's single manifest whose CAS is the linearization point: its size must not grow with history, and a multi-zone atomic update must stay O(update). This is the reference problem statement for #186 (metadata sharding).
- Evidence: CONFIRMED docs (`technical/reftable.adoc`). Not benchmarked.

## 3. Where Git's design hurts on object storage (consolidated)

These are HYPOTHESES derived from the formats above; each names the experiment that would settle it.

1. **Index fetch before data.** Every pack needs its idx first; many small packs mean many idx fetches (G6, G8). Settle with #185 and #184: cold-start request count for sparse and full clones against pack count.
2. **Delta chain read amplification.** Mean chain 14.2 on git.git with default depth (G5). Settle with a ranged-read replay over the produced packs against a latency-injected local store.
3. **Whole-pack rewrite on compaction** with egress and request cost, plus a GC race with readers (G11, G12). Settle with #186.
4. **Closure-based accelerators** (bitmaps, pack reuse) assume one repository-wide closure (G9); zone-sharded stores need a different unit.
5. **Mutable-by-replacement files** (MIDX, commit-graph chain head, `packed-refs`) have no atomic multi-file update on an object store; the manifest CAS must name the exact set (this is what walgit does, W2).

## 4. Measurements

Script (run on `lambda-final`, under `~/rt/gitexp/run.sh`): bare clone of https://github.com/git/git (HEAD `50fe5cc63f9cd6e89b87a2c9ccf96a444f4cfdb7`, 422,883 objects, source pack 325,274,586 bytes), then `git repack -adf -q --window=W --depth=D --threads=24` for (10,50), (250,50), (10,10), `git verify-pack -s` for chain histograms, then `repack -adf -b`, `commit-graph write --reachable` with and without `--changed-paths`, `multi-pack-index write`.

| window / depth | pack bytes | idx bytes | idx/pack | undeltified | mean chain | max chain | wall (s) | max RSS (GB) |
|---|---|---|---|---|---|---|---|---|
| 10 / 50 | 147,493,220 | 11,841,796 | 8.0% | 102,165 | 14.18 | 50 | 53.4 | 1.49 |
| 250 / 50 | 107,491,971 | 11,841,796 | 11.0% | 97,474 | 12.85 | 50 | 81.8 | 3.60 |
| 10 / 10 | 172,034,626 | 11,841,796 | 6.9% | 103,036 | 4.84 | 10 | 39.0 | 1.35 |

Side files for the (10,50) pack: bitmap 2,470,810 B; commit-graph 5,157,668 B (5,975,339 B with changed paths); MIDX 11,841,892 B.

Caveats: **one trial per configuration**; the box was not idle (load average about 31 at the start, a concurrent `cargo test` of walgit); wall time and RSS are therefore indicative only, sizes and chain histograms are deterministic for a given input and settings up to thread nondeterminism, which we did not test. One repository (C source, text, long history), git 2.34.1. Data: [data/git-structure-gitgit.json](data/git-structure-gitgit.json). Raw log stayed on the box (`~/rt/gitexp/log.txt`, 23 KB; dataset `g.git`, about 150 MB after repack, deleted after use; re-create it with the script).

Negative or unexpected results: the pack served by GitHub on clone (325 MB) is 2.2x larger than a local `repack -adf` at default window/depth (147 MB); the clone pack reuses whatever deltas the server had, so a downloaded pack is not a good size baseline. We did not determine why beyond that.

## 5. What this means for allgit (HYPOTHESES to test, not decisions)

1. The biggest structural lesson is G8 plus G11: a catalog object (MIDX-like) to find an object across many immutable packs, and a grace-window GC. Test both in #184 and #186.
2. If deltas are added, cap depth for remotely read packs and keep bases near their deltas (G5, G13); measure depth 5, 10, 20, 50 against ranged-read cost, per data class.
3. The idx is a cold-start cost of 28 B/object in Git and 44 B/object in allgit; a sliceable (fanout) layout lets a reader range-read part of an idx.
4. Do not copy: shallow, LFS, closure bitmaps tied to repository-wide DAGs.

## 6. References

- Git documentation tree at tag v2.56.0: https://github.com/git/git/tree/v2.56.0/Documentation (files named inline).
- Rendered equivalents: https://git-scm.com/docs and https://git-scm.com/docs/pack-format (not separately fetched; the source files above were read).
- Git LFS spec: https://github.com/git-lfs/git-lfs/blob/main/docs/spec.md
