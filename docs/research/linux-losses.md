# Linux (c6i.4xlarge) loss hunt: dirty diff, server e2e tests

Box c (16 vCPU, Ubuntu, git 2.43.0, p4 2025.2 / p4d 2026.1), `datagen flat 100000`, release builds of
origin/main + PRs #481, #482, #486 (base) vs the same plus this branch (new). Timings under `flock ~/bench.lock`,
tools interleaved, 5-7 runs, medians, ms (wall / cpu).

## What changed

* `diff` / `diff --name-only` / `--name-status` with a running watcher: the watcher names the changed paths and the
  scan tested every one of the 100k index entries against all of them linearly (`kept`), about 1e8 string compares for
  1000 dirty files. The paths now go in a hash set and each entry is looked up under its directory prefixes. Same
  result set (unit test `keep_set_agrees_with_kept`).
* The server end to end tests (`force_push`, `lockable_server`, `peering_e2e`, `server_e2e`, `shelve`, `zoneprofile`,
  `server_selfcheck`, `server_tls`) built `vcs-server` with a bare `cargo build`, which writes a debug binary into the
  default target dir; under `cargo test --release`, `--profile X` or a custom `CARGO_TARGET_DIR` the binary the test
  looks for (next to `vcs`) was never produced: "could not build vcs-server". They now build into the test binary's own
  target dir and profile. On a plain `cargo test --workspace` on Linux they passed before and after.

## Numbers (git comparison, dirty group = 1% of files modified)

| command | git | base vcs | new vcs | new / git |
|---|---|---|---|---|
| `diff --name-only` | 33 / 149 | 58 / 552 | 27 / 54 | 0.82 / 0.36 |
| `diff --name-status` | 32 / 147 | 58 / 552 | 27 / 54 | 0.85 / 0.37 |
| `diff` | 64 / 177 | 60 / 555 | 25 / 52 | 0.37 / 0.29 |

Unchanged (within noise) and still behind git on this box: `add .` with one edit 104 vs 177, `switch --detach` 79 vs 134,
`commit -a` 134 vs 222, `add -u` 78 vs 116 (all spread over index decode, stat of 100k files and a full index rewrite),
bundle create CPU 762 vs 974 (wall 362 vs 337: win), `bundle verify` (full by design; `--quick` is opt-in).
Perforce-mapped one-file ops are 20-45 ms against p4's 4-5 ms and are bounded by the whole-index decode and rewrite:
an index layout that can be patched in place is still the missing piece (bulk patching 1000 entries through
`prepare_index_patch` was tried and is 4x slower than the full rewrite, 345 vs 88 ms, so `add_tracked_files_patched`
keeps its 256 path limit).
