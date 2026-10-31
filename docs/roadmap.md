# Roadmap and acceptance criteria

Legend: ✅ done and tested · 🟡 partial · ⬜ not started.

## M0 – Research & architecture 🟡
✅ architecture.md, object-model.md, decisions log, DAG/zone/transaction design, failure table.
🟡 research.md is written from prior knowledge; **no live source inspection was done in this session** – needs a pass against
primary sources before any claim in it is relied on. ⬜ benchmark plan (below).

## M1 – Native local VCS ✅
Acceptance (all covered by tests in `crates/*/tests`, `cargo test --workspace`):
init, blob/tree/commit objects, branches, index/staging, commit, log, status, unified diff, checkout (`switch`),
no Git involvement, fsck, corruption detection, atomic ref update, stale-lock error, symlink-escape refusal,
racy-stat handling. CLI demo from the task statement runs verbatim (`crates/vcs-cli/tests/cli.rs`).

## M2 – Zones & snapshots 🟡
✅ zone create/list/show/branches/branch/checkout/add/remove; non-overlap enforced; independent branch heads per zone;
sparse materialization with "unsubscribed ≠ deleted"; multi-zone atomic local commit via Transaction; Snapshot create/show/list.
✅ snapshot checkout, zone rename/delete, branch delete (see [docs/zones.md](zones.md)).
✅ rename of zones that have history (display-name alias, see [docs/zones.md](zones.md)).
⬜ move-across-zones; dependency metadata; per-zone policies (storage/merge/lock/ACL).

## M3 – Remote storage 🟡
✅ backend trait + conformance suite, Mem/Fs backends, manifest CAS protocol, model checker, sparse fetch, publish/fetch/sync/clone in `vcs-repo` and the CLI (directory remotes).
✅ S3 backend (`vcs-store-s3`) tested on MinIO. ⬜ S3 is not selectable from `vcs remote set`; local object cache policy ([cache.md](cache.md)).
Acceptance: state-machine model of the publish protocol checked over all interleavings and crash points *before* the S3 code;
MinIO integration tests for every row of the failure table; capability detection; idempotent retry by txid; partial-fetch of
one zone downloads no bytes of others (assert via request log).

## M4 – Merge & operation history 🟡
✅ merge-base, three-way tree merge, diff3 text merge, structured conflicts, `merge/conflict/resolve`, `cherry-pick`, `revert`,
`rebase` with change-id preservation (linear ranges), `stash`, operation log + undo (local ops only; never rewinds published refs).
⬜ rename detection, criss-cross merges, file/directory conflicts, histogram diff comparison on a corpus. See [merge.md](merge.md), [rebase.md](rebase.md), [operations.md](operations.md).

## M5 – Performance 🟡
✅ `repack` (one pack + index), batched durability barrier, first benchmark baseline ([benchmarks.md](benchmarks.md)).
⬜ range reads, zone-aware packing, delta compression / FastCDC chunking, subtree-skipping tree diff, commit-graph index, fs watcher,
destructive GC (only `gc --dry-run` exists, [gc.md](gc.md)). Each change lands with before/after numbers.

### Benchmark plan (partly run; only numbers in benchmarks.md are real)
Datasets generated deterministically by script: 10k / 100k / 1M files, deep trees, 100k-entry directory, 100k-commit
history, 1k branches, 50 zones, large binaries with mutation patterns. Metrics: wall time, CPU, peak RSS, disk I/O,
S3 request count and bytes, cache hit rate, p50/p99. Compare with `git` (sparse-checkout + partial clone for the partial
cases), same machine, cold and warm cache, hardware recorded. First candidates: `add .` and `status` on 100k files
(index/stat cache), `commit` tree build, `log -n` traversal.

## M6 – Semantic merging ⬜ · M7 – Interop 🟡
⬜ Tree-sitter structural merge for one language with parse + test validation (notes: [research/structural-merge.md](research/structural-merge.md)).
✅ Git import (`import-git`, fast-export streams, id mapping table; [import-git.md](import-git.md)); 🟡 Git export prototype (`export-git`, one zone branch; [interop.md](interop.md)).
🟡 Locks: `lock/unlock/locks` exist and are model-checked ([locks.md](locks.md)), but enforcement is by the client's own validator, so they are advisory
until a validating writer holds the only manifest credential. ⬜ Perforce-style `edit/submit` workflows and any Perforce mapping: design only.
