# Benchmarks (first baseline)

Everything here was measured by the harness in `crates/vcs-bench` and `scripts/bench_vs_git.sh`. Nothing is
extrapolated: sizes that were not run (100k files, 100k commits) have no numbers. This is a **first baseline on one
machine**, not a conclusion about vcs vs git.

## Reproduce

```sh
cargo bench -p vcs-bench --bench core            # criterion, sizes <= VCS_BENCH_MAX (default 10000)
VCS_BENCH_MAX=100000 cargo bench -p vcs-bench    # adds the 100k-file matrix (very slow, see Findings)
cargo run --release -p vcs-bench --bin datagen -- deep 10000 /tmp/ds   # layouts: flat | deep | bigdir
scripts/bench_vs_git.sh flat 1000 5              # git vs vcs CLI, uses hyperfine if installed
```

Datasets are deterministic (SplitMix64, seed 42, no clock/OS randomness, no external data). Each file is 100-1099
bytes of `a-z`. Layouts: `flat` = 100 files per directory; `deep` = depth 8, fan-out 4 (about one file per leaf
directory, so directory count is close to file count); `bigdir` = all files in one directory.

## Machine and method

- Apple M2 Pro, 10 cores, 16 GiB, macOS (Darwin 25.2.0, arm64), APFS (`/dev/disk3s5`, journaled), git 2.52.0.
- Rust 1.92, **release** profile (criterion `bench` profile). The workspace `dev`/`test` profiles use opt-level 1/3, so debug numbers are not reported.
- criterion was run with `-- --quick` (reduced sampling; `[low mid high]` intervals are NOT statistically rigorous, and
  heavy cases such as `add_all` at 10k have effectively a single sample). The table reports the middle estimate.
- Cache state: **warm** (files just written, page cache not dropped). No cold-cache runs were made.
- Machine was not otherwise idle-controlled (a normal desktop session).
- `repo/*` benches use the public `vcs_repo::Repo` API, not the CLI. `add_all` re-inits `.vcs` in the same generated tree before each iteration (setup untimed).
- `status_clean` runs after `add` + `commit` of the generated tree, so racy entries (mtime too close to the index write) are rehashed by `status`; `status_1pct_modified` rewrites every 100th file.
- `commit_1_change` = rewrite one file, `add` it (untimed), time `commit` of that single change.
- `log` history: 10,000 commits of one file in the root zone (not 100k; not run).

## Results: criterion (release, warm, `--quick`)

Object hashing (`blob_id`, BLAKE3 over the encoded blob):

| size | time | throughput |
|---|---|---|
| 1 KB | 1.39 us | 702 MiB/s |
| 64 KB | 37.4 us | 1.63 GiB/s |
| 1 MB | 557 us | 1.75 GiB/s |
| 16 MB | 8.97 ms | 1.74 GiB/s |
| 64 MB | 36.3 ms | 1.72 GiB/s |

Tree build / flatten (in-memory store, no disk):

| layout | files | build | flatten |
|---|---|---|---|
| flat | 1,000 | 198 us | 218 us |
| deep | 1,000 | 2.42 ms | 1.93 ms |
| bigdir | 1,000 | 201 us | 208 us |
| flat | 10,000 | 3.34 ms | 2.41 ms |
| deep | 10,000 | 21.9 ms | 13.0 ms |
| bigdir | 10,000 | 2.15 ms | 2.31 ms |

`Repo` operations (real files on APFS):

| op | layout | 1,000 files | 10,000 files |
|---|---|---|---|
| add . (fresh index) | flat | 3.86 s | 40.4 s |
| add . (fresh index) | deep | 3.43 s | 42.9 s |
| add . (fresh index) | bigdir | 3.39 s | 37.1 s |
| status, clean | flat | 3.63 ms | 40.2 ms |
| status, clean | deep | 254 ms | 3.69 s |
| status, clean | bigdir | 3.25 ms | 45.3 ms |
| status, 1% modified | flat | 3.55 ms | 40.4 ms |
| status, 1% modified | deep | 173 ms | 2.43 s |
| status, 1% modified | bigdir | 4.67 ms | 45.3 ms |
| commit, 1 changed file | flat | 20.8 ms | 73.7 ms |
| commit, 1 changed file | deep | 52.1 ms | 123 ms |
| commit, 1 changed file | bigdir | 22.9 ms | 22.2 ms |

`log` over a 10,000-commit history: limit 10 = 173 us; limit 1,000 = 14.3 ms; limit 10,000 = 188 ms.

Unified diff of a large text file with ~1% of lines changed (`diff::unified`): 1,000 lines 1.64 ms; 10,000 lines
583 us; 100,000 lines 6.40 ms (415 MiB/s). The 1,000-line case being slower than 10,000 lines is unexplained (likely
`--quick` noise and the specific random edit set); it should be re-run with full sampling before being relied on.

## Results: git vs vcs CLI (`scripts/bench_vs_git.sh`, 1,000 files, median of 5 runs, ms, warm)

Builtin timing loop (hyperfine was not installed). Process start-up is included for both tools; `git` run with empty
global/system config.

| workflow | git flat | vcs flat | git deep | vcs deep |
|---|---|---|---|---|
| init | 15.6 | 23.1 | 15.0 | 23.6 |
| add . (fresh) | 519 | 3701 | 685 | 4052 |
| commit (all staged) | 31.0 | 54.8 | 2003 | 15669 |
| status clean | 12.4 | 12.7 | 96.0 | 244 |
| status 1% modified | 13.0 | 12.2 | 97.4 | 177 |
| log (1 commit) | 10.6 | 7.6 | 11.2 | 7.3 |

Only 1,000-file datasets were run through the script (the 10k `add` alone takes ~40 s per run for vcs).

### Semantic differences (affect comparability)

- git packs/zlib-compresses; vcs writes loose objects, each with `fsync` (see Findings). git loose objects are not fsynced by default.
- Hash functions differ (git: SHA-1 here; vcs: BLAKE3).
- vcs `commit` rebuilds all tree objects from the index (no cache-tree / subtree skipping yet); git reuses the index cache-tree.
- git `status` here has no fsmonitor/untracked cache; vcs `status` stat-caches but rehashes racy entries.
- No `.gitignore` in the dataset, so ignore handling is not exercised.
- Warm cache only.

## Findings

1. **`add`/`commit` are dominated by one `fsync` per loose object** (`LocalStore::put` calls `sync_all`). `add` costs
   about 3.4-4.3 ms per file and scales linearly (1k: ~3.4-3.9 s, 10k: 37-43 s), ~7x slower than git `add .` at 1k.
   `commit` on `deep` (many tree objects) is ~8x slower than git. Not quadratic. Tracked in #21.
2. **`status` on the `deep` layout is ~70-90x slower than flat/bigdir** at the same file count (3.7 s vs 40 ms at 10k),
   and grows 14.6x for 10x files. Cause undiagnosed (directory-count-driven work, or racy rehash). Tracked in #22.
3. Tree build/flatten are linear-looking in file count for flat/bigdir (about 11-14x for 10x files); `deep` costs
   ~7-10x more than flat per file, consistent with creating many small tree objects.
4. `log` limit scaling is roughly linear in commits walked (14 ms per 1,000, 188 ms per 10,000).

## After: batched object sync (#21) and status fast path (#22), 2026-10-31

Same machine, same flags as above (criterion `--quick`, release, warm, APFS, `VCS_BENCH_MAX` default 10,000). The
baseline above is kept untouched. "Before" here is the pre-change code (`origin/main` at 21a5cbf) **re-measured in the
same session** with the same harness, so the two columns are directly comparable; they agree with the first baseline
within normal `--quick` noise. Time-per-op, lower is better. 10k `add_all` has effectively one sample per cell.

`repo/add_all` (fresh index, every object written):

| layout | 1,000 before | 1,000 after | 10,000 before | 10,000 after |
|---|---|---|---|---|
| flat | 3.43 s | 256 ms | 37.6 s | 2.63 s |
| deep | 4.01 s | 374 ms | 40.2 s | 3.67 s |
| bigdir | 4.76 s | 265 ms | 45.1 s | 2.74 s |

`repo/commit_1_change`:

| layout | 1,000 before | 1,000 after | 10,000 before | 10,000 after |
|---|---|---|---|---|
| flat | 21.2 ms | 10.9 ms | 25.0 ms | 17.9 ms |
| deep | 47.0 ms | 21.7 ms | 128 ms | 93.2 ms |
| bigdir | 22.3 ms | 10.0 ms | 29.8 ms | 20.7 ms |

`repo/status_clean`:

| layout | 1,000 before | 1,000 after | 10,000 before | 10,000 after |
|---|---|---|---|---|
| flat | 3.43 ms | 2.91 ms | 34.9 ms | 33.3 ms |
| deep | 157 ms | 75.0 ms | 3.85 s | 674 ms |
| bigdir | 3.21 ms | 3.32 ms | 45.1 ms | 39.6 ms |

`repo/status_1pct_modified`:

| layout | 1,000 before | 1,000 after | 10,000 before | 10,000 after |
|---|---|---|---|---|
| flat | 3.50 ms | 5.18 ms | 35.4 ms | 38.9 ms |
| deep | 163 ms | 79.5 ms | 3.79 s | 647 ms |
| bigdir | 3.23 ms | 3.22 ms | 46.5 ms | 42.2 ms |

New cases (no "before": they did not exist in the old harness):

| case | layout | 1,000 | 10,000 |
|---|---|---|---|
| `add_unchanged` (re-add of an already stored tree) | flat | 22.9 ms | 244 ms |
| | deep | 102 ms | 1.00 s |
| | bigdir | 22.2 ms | 281 ms |
| `status_1_staged` (one staged change: slow path) | flat | 3.94 ms | 37.7 ms |
| | deep | 161 ms | 2.97 s |
| | bigdir | 3.58 ms | 46.0 ms |

### What changed and why

- **#21.** `LocalStore::put` no longer fsyncs. It writes to `objects/tmp/` and `ObjectStore::sync()` (called by
  `Repo::add` before the index is saved, by `commit`/`snapshot_create` before refs, and by fetch before refs move)
  flushes all pending files, renames them into place and flushes the directories. On macOS that is plain `fsync` per
  file plus two `F_FULLFSYNC` per barrier instead of one `F_FULLFSYNC` per object. Objects that already exist are
  skipped (one `stat`); the cost of that check is the `add_unchanged` row (about 22 us/file flat). The guarantee and
  crash outcomes are documented in `docs/architecture.md` ("Durability"). `add` is ~13-16x faster at 10k files; it
  is still ~0.26 ms/file (file create/write/rename plus per-file `fsync`), so it is not free.
- **#22.** Diagnosis (macOS `sample` on the CLI, `status` of a clean 10k-file `deep` repo): the time was not in
  `ix.under()` allocation. About 47% of samples were in `head_flat` (one object-file read per tree object, about 25k
  trees for 10k deep files) and about 41% in `walk_untracked` (one `read_dir` per directory, about 25k directories);
  the rest was per-file stats. Both are syscall-bound and scale with directory count, which is why flat/bigdir (about
  100 and 1 directories) were unaffected. Fix: `status` first hashes the tree the index would commit (no I/O) and, if
  it equals HEAD's root id, skips flattening HEAD entirely (the two path maps are then identical, so staged is empty).
  `walk_untracked` also uses a non-allocating `Index::has_under` instead of `under()`; its separate effect was not
  measured. Output is unchanged (regression tests in `crates/vcs-repo/tests/status_deep.rs`).

### Notes and caveats for these numbers

- `deep` status is still ~17x flat at 10k (674 ms vs 33 ms): the per-directory scan in `walk_untracked` remains
  (git's own `status` is also ~8x slower on `deep` at 1k, see the CLI table above).
- The fast path only helps when nothing is staged. With a staged change (`status_1_staged`) `deep` status still takes
  about 3 s at 10k, essentially the old cost. A subtree-skipping HEAD-vs-index diff is the real fix and is not done.
- Harness change: the persistent repo now sleeps 2.2 s and re-runs `add` after the initial commit. Reason: with the
  faster `add`, the generated files were still inside the 2 s racy window when the index was written, so every
  `status` re-hashed every file (an early run measured `status_clean/flat/1000` at 15.6 ms instead of 2.9 ms). In the
  old code `add` itself took seconds, so entries were never racy. A real consequence worth knowing: `status` never
  rewrites the index, so racy entries stay re-hashed until the next `add`/`commit`.
- Durability was exercised by simulated crashes (dropping the store before the barrier), not by power-loss testing. The
  batching was measured on macOS/APFS only; Linux behaviour is unmeasured. The CLI git-vs-vcs table was not re-run.
- `add_all` and 10k cells are single-sample `--quick` estimates; `status_1pct_modified/flat/1000` moved from 3.50 to
  5.18 ms with no code path change for flat layouts, which shows the noise level to expect.

## Caveats

- One machine, one run session, `--quick` sampling; treat differences under ~2x as noise.
- Release build only; debug numbers not collected.
- Not run: 100k files, 100k-commit history, 1k branches, 50 zones, large-binary mutation, RSS/CPU/disk I/O,
  cold cache, hyperfine, S3. The roadmap's benchmark plan lists these; they remain open.
- The git comparison covers a single workflow set at 1k files and is not a fair "who is faster" claim given the
  semantic differences above.
