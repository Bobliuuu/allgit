# Research notes

> **Provenance warning.** Written from the author's prior knowledge during the M0/M1 session. No source code, papers, or
> documentation were fetched or re-verified in this session, and WalGit (github.com/tobi/walgit) in particular was **not
> read**. Treat every entry as an unverified lead to confirm against primary sources, not as a citation. Items that shaped
> the implementation are linked to decisions in architecture.md.

| System | Properties worth keeping | Limits / avoid | Used for |
|---|---|---|---|
| Git | immutable content-addressed DAG, cheap branches, offline commits; packfiles+deltas; commit-graph file; partial clone/promisor; sparse-checkout; reftable | SHA-1 history, whole-repo mindset, index/stat cost at scale, one ref namespace, no stable change identity | object model shape; racy-stat rule (index) |
| Perforce Helix | huge repos, server-side file locks, workspace views, atomic multi-file changelists, binary handling | central server is a SPOF, no offline commits | zones + sparse subscription; Transaction as changelist; locks need a validating writer |
| Sapling / EdenFS | scalable commit graph, lazy history/content, virtual FS, watchman-style status | needs server infra; EdenFS is platform-specific | lazy-fetch direction; FUSE deferred, not an MVP dependency |
| Jujutsu | change ids vs commit ids, operation log + undo, first-class conflicts, auto-snapshotted working copy | git-backend constraints on its conflict encoding | change_id field now; op log + structured Conflict in M4 |
| WalGit (unread) | per task statement: S3 as authority, immutable uploads, WAL, atomic manifest publish, CAS, stateless servers | – | protocol shape in architecture.md was derived from first principles + the task statement, **not** from its code |
| Pijul / Darcs | patch theory, commutation, conflicts as data | general commutation is hard; perf pitfalls | only restricted, provably commuting op classes (rename/import/add-decl) as *metadata over snapshots* – experiment, not MVP |
| Mercurial / Fossil / Plastic-Unity | revlogs/changegroups; Fossil's sqlite single-file repo & autosync; Plastic's large-file + lock workflows | – | Plastic/Perforce workflow ergonomics for `edit/lock/submit` |
| Tree-sitter | incremental error-tolerant parsing, many grammars | syntax ≠ semantics; parse success doesn't validate a merge | M6: structural merge + mandatory test validation |
| FastCDC | fast content-defined chunking | extra metadata, hurts small files | M5: benchmark vs fixed-size; only for large/binary |
| Merkle B-tree / HAMT / radix trees | O(log n) update, wide fan-out ⇒ fewer S3 requests for huge directories | complexity | keep plain Merkle dir trees until M5 benchmarks show large-directory pain |
| S3 conditional writes | `If-None-Match`/`If-Match` give CAS on a key | provider-dependent; only one key per CAS | basis of manifest design; capability-detect, never assume |

## Experiments proposed (not run)
1. Large-directory (100k entries) tree update cost: plain vs HAMT, S3-request amplification.
2. FastCDC vs fixed chunks on mutated binary corpora: dedup ratio vs metadata overhead vs random-read latency.
3. Conflict-rate of single-zone-bumps-global-manifest vs separate CAS targets under N concurrent writers (model first).
4. Commuting operation classes: take real rename/add-import histories from open-source repos and measure how often
   snapshot 3-way merge conflicts but op-based merge would not.
5. Semantic merge precision: false-accept rate of "parses + compiles" as a validator on a mutated-merge corpus.
