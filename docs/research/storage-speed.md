# Storage engine speed settings (perf/storage-speed)

Owner rule: speed beats space; a size change is accepted only if no operation gets slower.

Harness: `scripts/bench_storage_speed.py <flat|deep|binary> <n> --vcs name=BIN[,ENV=VAL]` (git vs vcs configurations,
interleaved in random order per round, wall and CPU from wait4 rusage; cells add, commit, commit_1pct_modified, status,
blame, checkout, clone, fetch; store sizes are recorded as a side effect). Machine: Lambda box (30 cores, **shared, load1
about 23 to 29 at start**, so read CPU time first), git 2.56.0, 10,000 files, 7 runs, medians. The AWS boxes were down.

## Change: barrier packs start at 64 pending objects (was 256), capped at 32 packs

A durability barrier with fewer than `PACK_MIN` objects writes each object as a loose file (tmp file, fsync, rename:
hundreds of fsyncs for a commit of a few hundred objects). From `PACK_MIN` objects it writes ONE pack (one fsync).
`VCS_PACK_MIN` overrides the threshold (benchmarks only). Past `MAX_BARRIER_PACKS` (32) packs barriers write loose files
again, because every reader consults every pack (measured: 151 small packs made `log` 5 to 11 ms, `blame` 15 to 24 ms,
`switch` 27 to 32 ms on a 3,000 file repo, so no unbounded pack count). Object ids and formats are unchanged.

CPU time vs git (lower is better; m256 = old default, m64 = this change, m32 for comparison):

| layout | cell | m256 | m64 | m32 |
|---|---|---|---|---|
| flat | commit (first, 215 objects) | 0.55 | 0.15 | 0.15 |
| flat | commit 1% modified | 1.41 | 0.45 | 0.45 |
| binary | commit (first) | 1.00 | 0.31 | 0.31 |
| binary | commit 1% modified | 1.52 | 0.44 | 0.44 |
| deep | commit 1% modified | 0.34 | 0.35 | 0.36 |
| all | add, status, checkout, blame, fetch | unchanged within noise | | |

Wall time for commit: flat 0.67 -> 0.45 of git, binary 0.95 -> 0.82. Size cost: none (smaller: source store 19.7 -> 13.1 MiB
on flat, 18.0 -> 14.8 on deep, 54.0 -> 47.4 on binary, because loose objects take a 4 KiB block each).

## Findings that were measured but not changed

- Pack format v3 (`VCS_PACK_FORMAT=v3`, publish and compaction) is speed neutral on add, commit, checkout, clone, fetch
  (flat, 5 runs) and shrinks the remote 5.3 -> 4.7 MiB. It is not the default only because of the pending format decision.
- Remaining gaps vs git on CPU: clone is 1.35 to 1.8x git CPU (but 0.5 to 0.65x wall: it parallelises), status 1.1 to 1.3x
  (flat and binary), checkout 1.2x on flat and binary (0.73x on deep).
- Not run: AWS b quiet-box timings (down), 100k-file datasets, Windows/macOS.

Tests: `cargo test --workspace` passes on the Lambda box; new unit test for the pack cap.
