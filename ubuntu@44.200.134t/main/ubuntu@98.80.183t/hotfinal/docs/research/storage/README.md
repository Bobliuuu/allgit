# Storage engine research (epic #190)

Index of documents under `docs/research/storage/`. Add new documents here, one line each.

| Document | Issue | Content |
|---|---|---|
| [architecture-audit.md](architecture-audit.md) | #178 | End-to-end audit of the current storage path at `cd0fd09`: diagram, source map, 23 findings (F1 to F23) with measured syscalls, hash counts and backend request counts from the Lambda box; probe `crates/vcs-bench/src/bin/auditprobe.rs`, data `data/audit_*` |
| [benchmark-methodology.md](benchmark-methodology.md) | #181 | Metric definitions, measurement protocol, Pareto scoring, result schema |
| [cost-model.md](cost-model.md) | #181 | Provider pricing (retrieved 2026-10-31) and the executable cost model |
| [provider-comparison.md](provider-comparison.md) | #181 | Capability matrix: S3, R2, GCS, B2, MinIO, RustFS, Ceph RGW |
| [conformance-test-plan.md](conformance-test-plan.md) | #181 | `Backend` trait contract tests and per-provider operator commands |
| [workloads.md](workloads.md) | #182 | The 16 seeded dataset generators of `storebench` (parameters, purpose, limits) |
| [git-analysis.md](git-analysis.md) | #179 | Git storage internals (loose, packs, deltas, idx, MIDX, bitmaps, commit-graph, cruft, partial clone, LFS) with costs on object storage and a measured git.git experiment |
| [walgit-analysis.md](walgit-analysis.md) | #179 | tobi/walgit read, built and tested at `80e9a20`: WAL and manifest CAS, remote reader, pack lifecycle, round-trip model, limits |
| [indexing-experiments.md](indexing-experiments.md) | #185 | Experiment H: object-location indexes (per-pack, fanout, global, hash, MPH, B-tree, LSM, SQLite), Bloom/cuckoo/fuse filters, commit reachability, per-zone commit-graph files, changed-path filters, tree indexes; lab crate `crates/vcs-storage-lab`, data `data/idx_*`, `hist*`, `tree*` |
| [caching-experiments.md](caching-experiments.md) | #185 | Experiment J: cache backends, LRU/ARC/W-TinyLFU on generated checkout/sync traces, two-level, prefetch, warming, safety under concurrent publish and GC (exhaustive model check, negative caching), data `data/cache.jsonl`, `backend.jsonl`, `negcache.jsonl`, `modelcheck.jsonl` |
| [packing-experiments.md](packing-experiments.md) | #184 | Experiments A, C, E: git-style delta packing baseline, bounded depth / checkpoints / blocks, placement policies (static vs adaptive) |
| [compaction-experiments.md](compaction-experiments.md) | #184 | Experiments F, G: LSM write buffers, tiered vs leveled compaction, hot/cold tiering (GC sections: meta agent) |
| [compression-experiments.md](compression-experiments.md) | #183 | Codecs, dictionaries, adaptive choice, independently compressed blocks, random-access cost |
| [chunking-experiments.md](chunking-experiments.md) | #183 | Whole-file, fixed, FastCDC, Rabin, hybrid chunking; dedup across revisions and zones; manifest/index overhead |
| [architecture-decision.md](architecture-decision.md) | #188 | Decision: keep the baseline plus targeted changes; audit findings already fixed on main with PRs; ten architectures compared from this folder's evidence, where each is worse than Git or walgit, candidate changes verified, cost view, migration and rollback, what remains unmeasured |
| [results.md](results.md) | #188 | Consolidated results with a pointer to each datum (document, section, data file, status, machine), grouped by axis for Pareto reading |
| [implementation-roadmap.md](implementation-roadmap.md) | #189 | Staged plan, roadmap issues with measurable success criteria, regression tests, deferred items and their triggers |

Scripts: `scripts/storage_cost_model.py`, `scripts/storage_score.py` (tests: `scripts/test_storage_*.py`). Data: `data/`.

Lab code for #184: `crates/vcs-storage-lab` (own workspace; table generator `tools/tables.py`). Data: `data/packing/`.
- [s3-aws-results.md](s3-aws-results.md): measured AWS S3 contract checks and latency (2026-11-01), including the ETag A-B-A finding.
- [delta-gate.md](delta-gate.md): #250 and #244: the delta gate re-run on real allgit-encoded objects (ripgrep, tokio, cargo, flask via `vcs import-git`): per-class sizes for loose, pack v1/v2, solid blocks and deltas at depth 4/8/16, reconstruction reads, tree compressibility and front coding; gate passes for text blobs and trees. Lab `crates/vcs-storage-lab-delta`, data `data/deltagate/`.
