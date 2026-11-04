# Roadmap and acceptance criteria

Legend: ✅ done and tested · 🟡 partial · ⬜ not started. Status as of 2026-11-01 (main after PR #255). The epic is
issue #45; the storage-engine plan is [research/storage/implementation-roadmap.md](research/storage/implementation-roadmap.md)
(epic #190). Anything marked ✅ has an executable test; "never run" notes say what was not verified.

## M0 – Research & architecture 🟡
✅ architecture.md, object-model.md, decisions log, DAG/zone/transaction design, failure table, storage-engine study
(decision record, consolidated results, roadmap under `research/storage/`).
🟡 The older notes in research.md and `research/*.md` were written from prior knowledge, not live source inspection;
verify before relying on them. The storage study is measured and reproducible from committed data.

## M1 – Native local VCS ✅
init, blob/tree/commit objects, branches, tags (lightweight and annotated), index/staging, commit, log, status, diff,
show, blame, restore, rm, mv, `.vcsignore`, stash, `config`, `--json`, fsck, corruption detection, atomic ref update,
stale-lock error, symlink entries and collision checks, racy-stat handling. No Git involvement. Tests: `crates/*/tests`,
including a randomized model test of the local repository.

## M2 – Zones & snapshots ✅
Zone create/list/show/branch/checkout/add/remove/rename/delete; non-overlap enforced; independent heads; sparse
materialization with "unsubscribed ≠ deleted"; atomic multi-zone commit; snapshots (create/show/list/checkout).
See [zones.md](zones.md). ⬜ move across zones; dependency metadata; per-zone policies for storage and merge.

## M3 – Remote storage 🟡
✅ backend trait + conformance suite; Mem/Fs/S3/HTTP backends; manifest CAS protocol; model checker (publish, sharded
commit, GC race, fault injection); sparse and lazy fetch; publish/fetch/sync/clone; tags over the wire; remote packs
(one pack per publish, ranged reads, [remote-packs.md](remote-packs.md)); `s3://` and `http(s)://` remotes selectable
from `vcs remote set`.
🟡 Real cloud: S3 measured once (us-east-1, from a Mac). ⬜ Not done: cloud TLS interop test, R2/GCS/B2, S3 connection
reuse (#243), one manifest read per publish (#240), local object-cache eviction ([cache.md](cache.md)).

## M4 – Merge & operation history ✅ (known gaps below)
✅ merge-base (commit-graph accelerated), three-way tree merge, diff3 text merge, structured conflicts, `merge`,
`conflict`, `resolve`, `cherry-pick`, `revert`, `rebase` (keeps change ids, linear ranges), operation log + undo
(local state only, never rewinds published refs), opt-in rename-aware merge.
⬜ criss-cross merges, file/directory conflicts, copy detection. See [merge.md](merge.md), [rebase.md](rebase.md),
[operations.md](operations.md).

## M5 – Performance 🟡
✅ batched durability barrier, `repack`, chunk manifests for big files, commit-graph cache, directory-listing cache for
`status`, stat-cache `add`, cache-tree `commit`, parallel checkout and fsck, remote packs, real local and
directory-remote GC. Numbers: [benchmarks.md](benchmarks.md).
🟡 Storage engine: the study decided to keep the baseline and make targeted, versioned changes (#188, #189). Next:
pack format v2 with per-entry zstd (#245), pack index fanout (#246), automatic repack (#247), tiered remote
compaction (#248, needs S3/HTTP remote GC #232), manifest scaling data and decision ([manifest-scaling.md](manifest-scaling.md), #252).
⬜ filesystem monitor (#113, #154), changed-path Bloom filters (#114), range-read tightening (#227), 100k-commit
history benchmark, delta compression (gated, #250).

### Benchmark plan (partly run; only numbers in benchmarks.md are real)
Done: 1k/10k/100k-file datasets (flat, deep, binary) against git on a Linux box and a Mac, 10k-commit log, remote
request counts, one S3 latency run. Not done: 1M files, 100k-commit history, 1k branches, 50 zones, cold cache, RSS/CPU
and disk I/O, other clouds. Each performance change lands with before/after numbers.

## M6 – Semantic merging 🟡
✅ Tree-sitter structural merge for Rust, Python, TypeScript and Go behind cargo features (off by default), parse
guarantee on the output; generated-corpus and real-history replay measurements. 🟡 Assisted merge
(`vcs-assist-claude`, `merge.assist`): implemented and mock-tested, **never called against the live API** (#209).
⬜ merge quality on a large real-world corpus. Notes: [research/structural-merge.md](research/structural-merge.md), [assist.md](assist.md).

## M7 – Interop and server 🟡
✅ `import-git` (symlinks, tags, change-id trailer round trip), incremental `export-git`, `git-remote-allgit`
([import-git.md](import-git.md), [interop.md](interop.md)). ✅ `vcs-server`: validating writer, tokens and roles, path
policy, lock enforcement, audit chain, rate limits, TLS (rustls), directory or S3 backend, deploy recipe
([server.md](server.md), [deploy.md](deploy.md)); client `login/logout/credential`.
🟡 Locks are enforced only through `vcs-server`; with a plain directory or bucket they are advisory ([locks.md](locks.md)).
🟡 `import-p4` is fixture-tested only and has **never been run against a real Perforce server** ([import-p4.md](import-p4.md)).
⬜ Perforce-style `edit/submit` workflows; Windows is compile-checked in CI only, never run; S3/HTTP remote GC (#232);
path-level sparse sets (#118); virtual filesystem (#119).
