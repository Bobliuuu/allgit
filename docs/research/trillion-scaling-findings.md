# Scaling toward a trillion: findings so far (interim, run still in progress)

Box: i4i.8xlarge (32 vCPU, 7 TB NVMe), 2026-11-06. Raw data: `docs/research/data/scaling-trillion/` (re-synced as the run
continues). Fits and projections: `scripts/scaling/analyze.py` (tables in `data/scaling-trillion/out/tables.md`). Every
"1e12" number is a PROJECTED extrapolation, not a measurement.

## What holds at scale (measured)

* Pack index: 1e10 objects, 44 B/object (440 GB, 4 packs), build 292 s, open 0.5 ms with ~93 MB anonymous memory,
  cold lookup p50 3.4 ms. Lookups are flat/log in N.
* Commit-graph point queries (`index_of`, merge-base at small fork distance), reach-bitmap membership, path-filter
  lookup, AGI5 `open`, sharded manifest commit (230 us, flat): all flat or sub-linear.
* Index staging (`add` of one file) is 7 ms from 10k to 30M files. Deep history (one file per commit) commits in 7 ms at
  5M commits.

## What breaks first (measured, linear in N or worse)

| structure | cost at largest measured N | class |
|---|---|---|
| commit-graph decode_validate | 17 s, 30.5 GB at 3e8 commits | O(n) time and RAM |
| reach-bitmap load_verify | 19 s, 32 GB at 1e9 | O(n) time and RAM |
| path-filter load_parse | 4.2 s, 6.5 GB at 1e8 | O(n) time and RAM |
| commit-graph extend (1 commit) | 546 ms at 1e7 (after PR #556; 1.1 s at 1e6 before) | O(n) |
| manifest decode / locks decode | 1.7 s at 1e6 zones/refs; 9 s at 1e7 locks | O(n) |
| **one-file `commit` in a wide repo** | 0.76 s @1M files, 7.6 s @10M, 21 s and 8.5 GB @30M | O(n) |

Commit-graph, reach-bitmap and path-filter are whole-file decodes: they must become mmap-style lazy readers to reach 1e12.

## Diagnosis: one-file commit is O(files) (wide repos)

`Repo::commit_signed` (`crates/vcs-repo/src/repo.rs`) pays two linear costs even when one file changed:

1. `self.index()` is `index_image()?.to_index()`: decodes every entry into a `BTreeMap<String, IndexEntry>`; that map is
   what `treeops::IndexTrees` walks. RSS 8.5 GB at 30M files is about 285 B/file.
2. After the commit it stores the tree cache with `Op::SetTrees(trees_snapshot())`, which clones and re-encodes the whole
   cached-tree map (one entry per directory; 300k at 30M files) instead of a delta.

`add` is fast because it already appends log ops. The index log already carries `DropTrees` (dirs invalidated by an edit),
so the dirty directories are knowable without decoding the index.

Proposed fix (not done): lazy commit. Load cached trees (O(dirs)), take dirty dirs = ancestors in `DropTrees` since the last
`SetTrees`, rebuild only those from the block `IndexView` children ranges on top of the parent tree, and append a delta
`SetTrees`. Must keep the invariant that a cache never changes an answer: property-test against the full-map path.

## Wide import is linear but slow-ish

vcs import of N files in N/100 dirs: 18 s @1M, 197 s @10M, 593 s @30M (36 GB RSS at 30M). First publish: 110 s, 11.4 GB at 30M.
