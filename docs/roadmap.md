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
⬜ rename of zones that have history; move-across-zones; dependency metadata; per-zone policies (storage/merge/lock/ACL).

## M3 – Remote storage 🟡
✅ backend trait + conformance suite, Mem/Fs backends, manifest CAS protocol, model checker, sparse fetch, publish/fetch/sync/clone in `vcs-repo`.
⬜ S3 backend + MinIO tests, CLI commands, local object cache policy (tracked in GitHub issues).
Acceptance: state-machine model of the publish protocol checked over all interleavings and crash points *before* the S3 code;
MinIO integration tests for every row of the failure table; capability detection; idempotent retry by txid; partial-fetch of
one zone downloads no bytes of others (assert via request log).

## M4 – Merge & operation history ⬜
Merge-base, 3-way tree merge, text merge (compare Myers / patience / histogram on a corpus), structured `Conflict` objects,
operation log + undo (local ops only; never rewinds published refs), rebase with change-id preservation.

## M5 – Performance ⬜
Packfiles + range reads, zone-aware packing, FastCDC vs fixed chunking, subtree-skipping tree diff, commit-graph index,
fs watcher. Each change lands with before/after numbers.

### Benchmark plan (not run yet; no numbers claimed anywhere in this repo)
Datasets generated deterministically by script: 10k / 100k / 1M files, deep trees, 100k-entry directory, 100k-commit
history, 1k branches, 50 zones, large binaries with mutation patterns. Metrics: wall time, CPU, peak RSS, disk I/O,
S3 request count and bytes, cache hit rate, p50/p99. Compare with `git` (sparse-checkout + partial clone for the partial
cases), same machine, cold and warm cache, hardware recorded. First candidates: `add .` and `status` on 100k files
(index/stat cache), `commit` tree build, `log -n` traversal.

## M6 – Semantic merging ⬜ · M7 – Interop ⬜
Tree-sitter structural merge for one language with parse + test validation; Git import first (explicit ID mapping table; fast-export import shipped, see import-git.md),
then export projection rules (prototype and design: [interop.md](interop.md)); Perforce-style `edit/submit/lock` as thin workflows over the same model.
Server-enforced locks are only meaningful with the M3 validating-writer component; local-only locks will be labelled advisory.
