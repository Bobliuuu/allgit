# Research: commit-graph and path-history acceleration

Tracks issue #100. Docs only; nothing here is implemented. Sources were accessed on **2026-10-31**; claims I could not
confirm against a primary source are marked **[unverified]**. The only numbers quoted from our own code are the ones in
`docs/benchmarks.md`; numbers quoted from other projects are quoted from the cited page, not re-measured.

## 1. Problem

- `Repo::log` (`crates/vcs-repo/src/repo.rs`) walks parents with a max-heap keyed on author timestamp and decodes one
  commit object per step. `docs/benchmarks.md` measured `log` over 10,000 commits at 188 ms (limit 10 = 173 us,
  limit 1,000 = 14.3 ms), roughly linear in commits walked.
- `merge_base` (`crates/vcs-merge/src/base.rs`) computes the full ancestor set of both sides, intersects, then floods
  parents of the intersection. Its own comment says: "no generation numbers yet (M5)". Cost is O(all reachable commits),
  even when the answer is a few commits back.
- `blame` (planned) and `log -- <path>` must find, per commit, whether a path changed. Today that needs a tree diff
  against the parent, which means reading and decoding trees along the path for every commit.
- Hypothesis, **not measured**: most of the 188 ms is object read and decode (loose file open, framing, codec), not graph
  logic. Any proposal must be validated by profiling `log` first (follow-up task A in section 7).

## 2. How the prior art works

### 2.1 Git commit-graph file

Source: <https://git-scm.com/docs/gitformat-commit-graph> and <https://git.github.io/htmldocs/technical/commit-graph.html>
(accessed 2026-10-31).

- One file, `objects/info/commit-graph`, or a chain under `objects/info/commit-graphs/`. Header `CGPH`, version, hash
  version, chunk count, base-graph count; a chunk table of 12-byte entries (4-byte id, 8-byte offset); a trailing
  checksum. Multi-byte values are network byte order.
- Chunks: `OIDF` (256-entry fanout), `OIDL` (sorted commit ids), `CDAT` (per commit: root tree id, two parent
  *positions*, generation v1 in 30 bits, commit time in 34 bits), `EDGE` (extra parents for octopus merges), `GDA2` /
  `GDO2` (corrected-date generation and its overflow), `BIDX` / `BDAT` (changed-path Bloom filters), `BASE` (chain).
- Parents are stored as positions in the sorted-id table, so a parent walk is array indexing with no object reads.
- The format doc says a graph is ignored with a warning if its hash version differs from the repo's.

### 2.2 Generation numbers

Source: <https://git.github.io/htmldocs/technical/commit-graph.html> (accessed 2026-10-31).

- v1, topological level: roots are 1; otherwise 1 + max over parents. Capped at `0x3FFFFFFF` (30 bits).
- v2, corrected committer date: roots use their own date; otherwise max(own date, 1 + max corrected date of parents).
- Key property: if A has generation N, B has M and N <= M, then A cannot reach B. Walks use a priority queue on
  generation and stop early. Commit-time heuristics are unsafe under clock skew, so are not used where topological
  order is required (merge-base).
- Trade-off: v2 prunes better when dates are well-behaved (v1 can be far from the true distance in wide history; this
  is my reading, **[unverified]**: I did not find a primary measurement). v2 costs 4 more bytes per commit plus an
  overflow chunk.
- Git history: earlier chunk ids `GDAT`/`GDOV` held possibly erroneous data, hence the `2` suffix.
  (<https://git-scm.com/docs/gitformat-commit-graph>.)

### 2.3 Changed-path Bloom filters

Sources: <https://git-scm.com/docs/gitformat-commit-graph>;
<https://devblogs.microsoft.com/devops/super-charging-the-git-commit-graph-iv-bloom-filters/> (both accessed 2026-10-31).

- Per commit, the set of paths changed against the **first parent** (each changed file also adds its parent
  directories) goes into a Bloom filter. A "definitely not" answer lets `log -- path` skip the tree diff for that commit.
- `BIDX` holds cumulative byte offsets (N x 4 bytes); `BDAT` has a 3-field u32 header (hash version, hash count, bits
  per entry) then concatenated filters. Hash is 32-bit MurmurHash3 with double hashing. Hash version 1 has a
  signed-char bug on bytes >= 0x80; version 2 is current and v1 is slated for removal
  (<https://git-scm.com/docs/gitformat-commit-graph>).
- Commits with no changes get a 1-byte all-zero filter; commits with more than 512 changes get a 1-byte all-ones
  ("too large", must diff) filter.
- The Microsoft (VSTS) write-up describes 10 bits reserved per expected element with 7 hashes, "roughly a 1%
  false-positive rate", a theoretical 100x speedup, "closer to a 6x speedup for a random sample of paths", up to 20x for
  rarely changed paths, and "less than 100MB of extra storage for a repository the size of the Linux kernel". These are
  Microsoft's numbers on their workload and their file-history algorithm, not ours.
- Trade-offs: false positives only cost an extra diff, never a wrong answer; filters only cover first-parent diffs, so
  they do not answer "changed relative to the second parent" (relevant to merge handling in blame); rename tracking
  cannot use them directly because the old path is unknown **[unverified for Git's exact behaviour]**.

### 2.4 Split (incremental) chains

Source: <https://git.github.io/htmldocs/technical/commit-graph.html> (accessed 2026-10-31).

- New commits go in a new top layer; layers merge when the lower layer has fewer than X (default 2) times the commits
  of the one above it, or a max-commits limit C is exceeded. This keeps level count logarithmic and bounds file size.
  The doc's only numbers are those design constants (2 and 64,000).
- Corrected dates are only written for a new layer if the topmost existing layer has them; readers that find the top
  layer lacking them fall back to topological levels.
- The graph is neither read nor written when replace objects, grafts or shallow commits exist. Our model has none of
  these, which simplifies things.

### 2.5 Sapling segmented changelog

Source: <https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/> (accessed 2026-10-31).

- Clone downloads "the high-level shape of the commit graph ... taking just a few megabytes"; the graph relationship
  between two commits can be queried in "O(number-of-merges) time"; `log` and `blame` on files can "bisect the segment
  graph" for O(log n) rather than O(n). The page claims smartlog stays "less than a second, regardless of how big the
  repository is". Those are Meta's claims, not reproduced here.
- Mechanism (segments of linear history joined at merges, with ancestry resolved on the segment graph) is from
  secondary summaries only: <https://sysdesign-wiki.pages.dev/concepts/segmented-changelog/> **[unverified]**; I did not
  read Sapling's source or its design doc (the Sapling internals page I tried returned 404).
- Relevance: strongest when history is mostly linear, and lets a client hold graph shape without commit contents, which
  fits our lazy/sparse-clone direction (`docs/cache.md`).

### 2.6 Mercurial

- Mercurial stores the changelog as a revlog with dense integer revision numbers where parents always have lower
  numbers, so topological order is implicit and ancestry has a cheap bound **[unverified: stated from background
  knowledge, no primary source fetched]**.
- Mercurial's changelog-v2 and revlog-v2 work stores extra per-revision data ("sidedata") in the revlog itself
  (found via search: <https://www.mercurial-scm.org/wiki/RevlogV2Plan>, a page that calls itself speculative; the
  sidedata commit summaries came from code.rhodecode.com mirrors). I did not verify whether this yields changed-file
  acceleration, and I found no Mercurial source for a path-history filter. **[unverified]**
- Takeaway kept: dense integer positions assigned in topological order are what make parent walks and generation-like
  bounds cheap. Our proposal gets the same property from the sorted position table plus generation.

## 3. Fit with our model

- Commits are per **zone** (`docs/object-model.md`): `Commit.parents` are always commits of the same zone, and refs are
  `(zone, branch)`. There is no cross-zone parent edge, so every zone's DAG is independent. A single global graph file
  would be rewritten whenever any zone advances and would couple unrelated zones; per-zone files do not.
- Object ids are 32 bytes (`ObjectId::from_bytes([u8; 32])`), so id-keyed tables cost 32 bytes per commit versus Git's
  20; positions (u32) are the way to avoid repeating ids.
- Writers publish through a single `refs` file replaced atomically, and objects are immutable and create-only. An
  immutable object store means a graph file is a pure function of the commit set: it can never be stale-wrong, only
  incomplete. That is the key simplification for invalidation.
- Local state lives in `.vcs/` as framed `magic || version || body` files rejected if malformed (`docs/architecture.md`).
  The graph should follow that framing, with a checksum.

## 4. Proposal

All of this is a **cache**: deleting it must change speed, never results. It is derived data, never an input to fsck's
reachability roots and never synced to remotes in v1.

### 4.1 File layout

```
.vcs/
  graph/
    <zone>/                      zone name, path-safe encoded (same encoding as other per-zone state)
      chain                      text: one layer file name per line, bottom to top; replaced atomically
      layer-<checksum-hex>.cg    immutable layer files, create-only, named by their own checksum
```

A layer file (all integers little-endian is fine since it is local-only; if it ever syncs, switch to big-endian, see
open questions):

| Part | Contents |
|---|---|
| Header | magic `AGCG`, format version u16, hash id u8 (32-byte ids), chunk count u8, base layer count u8, zone name hash |
| Chunk table | (chunk id u32, offset u64) x C, zero-terminated, like Git's |
| `IDS ` | sorted commit ids, N x 32 bytes, plus a 256-entry fanout (same idea as `OIDF`+`OIDL`) |
| `COMM` | per commit in id order: parent positions (2 x u32, sentinel for none), generation u32 (topological level), author timestamp u64 |
| `EDGE` | extra parents beyond two |
| `GENC` | optional corrected-date generation (u32 offset + overflow table), only if every layer below has it |
| `BLMI` / `BLMD` | optional changed-path Bloom filter index and data, first-parent diff, with header (hash version, hash count, bits per entry) |
| `BASE` | checksums of lower layers, bottom to top |
| Trailer | checksum over everything above |

Notes:
- Position space is cumulative across layers, as in Git (position = index in file + commits in all base layers), so a
  parent in a lower layer is just a smaller number.
- Per-zone files mean `log`, `merge_base` and `blame` open exactly the zone being queried.
- Whether to store root tree id (Git does) is an open question; `log` does not need it, tree-diff for blame does but
  reads the commit anyway.
- Bloom filter parameters (bits per entry, hash count, the 512-change cutoff) are **not chosen here**: they must come
  from measurement on our corpus. Git's cutoff is 512 (section 2.3) and is a reasonable starting value to test.

### 4.2 Use

- `log`: walk positions with a heap on (timestamp or generation); read commit objects only for the `limit` commits
  actually printed (message/author), not for every step. With a path filter, consult Bloom first, diff only on "maybe".
- `merge_base` / `is_ancestor`: priority queue by generation; stop when all frontier commits have generation at or
  below the target's, as described in the Git design doc. Falls back to the current algorithm if a layer is missing.
- `blame`: walk first-parent-first with Bloom filter skipping; commits whose filter says "no change" are passed
  through without tree reads.
- Commits not in the graph (new since last write) are handled by reading the object and treating generation as
  "infinity" (Git's `GENERATION_NUMBER_INFINITY`), so partially covered graphs stay correct.

### 4.3 Invalidation rules

1. **Immutability.** Commits never change, so a layer never becomes wrong when new commits arrive; it just does not
   cover them. No rewrite is needed for correctness.
2. **Write.** After a publish advances a zone head, a post-commit step (or `gc`) appends a layer covering commits
   reachable from the new head and not in any lower layer. Layer file is written to a temp name, fsynced, renamed, and
   only then is `chain` replaced atomically. A crash leaves an unreferenced layer file (garbage, collectable) or the old
   chain (valid).
3. **Merge policy.** Merge adjacent layers using Git's size-ratio rule (section 2.4) so layer count stays logarithmic;
   constants to be tuned by measurement.
4. **Read validation.** On open: check magic, version, hash id, zone hash, trailer checksum, and that each `BASE` entry
   equals the checksum of the layer below. Any failure means ignore that layer and everything above it, fall back to
   object walking, and (outside read-only commands) schedule a rebuild. Never an error to the user.
5. **Unreachable commits.** If gc removes commits (docs/gc.md), layers listing ids that no longer exist are not wrong
   for graph queries on reachable commits, but should be rebuilt by gc to avoid dangling positions; gc rebuilds the
   zone's graph from the surviving head set as its last step.
6. **Format version bump.** Old versions are ignored and rebuilt. No migration.
7. **Zone rename/removal.** Zone name is in commit ids (commits embed zone), so renames follow the existing zone-rename
   story in `docs/object-model.md`; the graph directory is simply dropped and rebuilt.
8. **Bloom-specific.** A filter with an unknown hash version is treated as "maybe" for every path; "too large" filters
   likewise. Filters are only trusted if the layer's header parameters match what the reader understands.
9. **Concurrency.** Readers see either the old or new `chain` (atomic rename); layer files are never modified, so no
   locking is required for readers. Writers take the existing repository lock (`.vcs/lock`).

### 4.4 Alternatives considered

| Option | Verdict |
|---|---|
| Single global graph across zones | Rejected: couples zones, rewritten on every publish by any zone |
| Mercurial-style dense revision numbers assigned at commit time | Rejected for v1: would need a mutable numbering shared by clones; positions in a derived cache give the same locality without making numbering part of the data model |
| Sapling-style segmented graph | Deferred: attractive for lazy history on remotes (strong fit for sparse clone) but a larger design; revisit if layers prove insufficient |
| Generation v1 only, no corrected dates | Candidate for the first cut: simpler, no overflow chunk; add v2 only if measurements show wide-history walks need it |
| Storing graph in objects store (synced) | Deferred: would need remote protocol and trust story; local-only first |

## 5. Risks and unknowns

- The 188 ms may be dominated by object decode, in which case the biggest win is not reading commits at all, which
  the position table gives, but Bloom filters give nothing for plain `log`.
- Bloom filters help path-limited history only; no estimate of benefit for our repos exists. Do not promise one.
- Rename-following in blame interacts badly with path filters (section 2.3).
- Mercurial and Sapling claims above are partly unverified; nothing in the proposal depends on them.

## 6. Source list

| URL | Used for | Accessed |
|---|---|---|
| <https://git-scm.com/docs/gitformat-commit-graph> | file format, chunks, Bloom layout, chains | 2026-10-31 |
| <https://git.github.io/htmldocs/technical/commit-graph.html> | generation numbers, split chain merge strategy, when graph is unused | 2026-10-31 |
| <https://devblogs.microsoft.com/devops/super-charging-the-git-commit-graph-iv-bloom-filters/> | Bloom sizing and measured speedups (Microsoft's) | 2026-10-31 |
| <https://engineering.fb.com/2022/11/15/open-source/sapling-source-control-scalable/> | segmented changelog claims (Meta's) | 2026-10-31 |
| <https://sysdesign-wiki.pages.dev/concepts/segmented-changelog/> | segment mechanism (secondary, unverified) | 2026-10-31 (search snippet only) |
| <https://www.mercurial-scm.org/wiki/RevlogV2Plan> | revlog v2 / sidedata (search snippet only, unverified) | 2026-10-31 |

Not found or not read: <https://github.blog/open-source/git/the-star-wars-of-git-commit-graph-generation-numbers/>
returned 404; Sapling internals page returned 404; no Mercurial primary source on changed-file indexes was read.

## 7. Follow-up tasks (to open as issues)

- A. Profile `log` and `merge_base` at 10k commits to split time into object read, decode, and graph logic (zone:perf, size:s).
- B. Implement per-zone commit-graph layer writer/reader with positions and generation v1, plus fallback and corruption tests (zone:perf, size:m).
- C. Use generation in `merge_base`/`is_ancestor` with differential test against the current implementation (zone:perf, size:s).
- D. Changed-path Bloom filters for `log -- path`/blame, with parameters chosen by measurement (zone:perf, size:m).
- E. Layer merge policy and gc integration (zone:perf, size:s).
