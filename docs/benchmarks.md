# Benchmarks (first baseline)

Everything here was measured by the harness in `crates/vcs-bench` and `scripts/bench_vs_git.sh`. Nothing is
extrapolated: sizes that were not run (100k commits; see the 100k-file scale run below) have no numbers. This is a **first baseline on one
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

## After: parallel add and subtree-skipping staged status (#35), 2026-10-31

Same machine as above (M2 Pro, APFS, release, warm). **The machine was heavily loaded by other processes during
these runs (load average 8-55), so absolute times are noisy; before/after pairs were run alternately in the same
session with the pre-change binary built from `origin/main` (38c19fe).** Only measured values are listed.

Profile (macOS `sample`, 10k-file `deep` repo): `status` with a staged change spent essentially all samples in
`head_flat` (one object read per tree object); clean `status` was dominated by `open` in `walk_untracked` (one
`read_dir` per directory, about 250 of 340 samples); `add` was `open`/`read`/`write`/`close`/`rename` syscalls on a
single thread.

Changes: staged changes come from a hash-only index tree diffed against HEAD, reading only trees on changed paths;
`add` reads/hashes/writes blobs on up to 8 threads; the durability barrier fsyncs and renames on up to 8 threads;
the untracked walk uses a bounded thread budget with ordered output.

10k files, `deep` layout, CLI wall time (alternating runs, ms):

| case | before (runs) | after (runs) |
|---|---|---|
| `status`, clean | 846 898 925 950 981 984 | 612 615 628 659 665 764 |
| `status`, one staged change | 3465 3602 3608 3696 3773 3780 | 625 669 693 721 726 782 |
| `add .` (fresh index), 6 alternating runs | min 3570, median 4010 (with a 13.5 s outlier) | min 2310, median 3560 |

`scripts/bench_vs_git.sh` (median of 5, ms), under the same load. Only runs that completed are shown; the
10k `deep` pair did not finish and has no numbers.

| workflow | flat 10k vcs before | flat 10k vcs after | deep 1k vcs before | deep 1k vcs after | deep 1k git (same session) |
|---|---|---|---|---|---|
| add . (fresh) | 19325 | 5406 | 1499 | 1157 | 733 |
| status clean | 71.6 | 49.6 | 186 | 69.6 | 105 |
| status 1% modified | 62.1 | 48.7 | 182 | 71.3 | 104 |

Caveats:

- **Deep and flat `status` at 10k are not within 3x**: deep is about 0.6 s and flat about 35 ms (about 17x). What is
  left is one `open`+`getdirentries` per directory (about 25k directories in `deep`), which is kernel path-lookup
  bound; threads helped only partly. Git's own `status` is also several times slower on `deep` (above). Removing the
  rest would need a directory cache (fsmonitor/untracked-cache style) or `openat`-relative scanning; not done.
- The 10k-flat git `add` took 13-28 s in this loaded session (git's median is far above its earlier quiet-machine
  figures), so cross-tool comparisons from this run are not reliable.
- Linux was not measured. Criterion benches were not re-run for this change.

## After: directory listing cache for `status` (#102), 2026-10-31

`status` keeps a per-directory cache of directory listings in `.vcs/dircache` (stamp = inode, mtime, ctime; trusted
only after a 2 s racy window; see the module docs in `crates/vcs-repo/src/dircache.rs`). A directory whose stamp still
matches is not re-read; tracked files are still stat'ed every time, so this removes the `open`+`getdirentries`+`close`
per directory, not the per-file `stat`. `VCS_NO_DIRCACHE=1` turns it off for A/B runs.

Machine: the Mac used above (APFS, release build, warm cache), **heavily loaded by other jobs during the runs (load
average 17-31)**. CLI wall time of `vcs status` on a clean, committed, re-added 10k-file repo (`datagen`), in ms, three
alternating batches of 7 runs each (21 runs per cell, pooled):

| case | cache off: median (min-max) | cache warm: median (min-max) |
|---|---|---|
| `deep` 10k, clean | 757 (509-1321) | 161 (128-230) |

`flat` 10k, one batch of 7 runs each: cache off 81 (70-131), cache warm 81 (70-123). That is, no change for flat, as
expected (about 100 directories). The first run after the cache is missing (cold) took 863 ms (one sample) and writes
a 1.3 MB cache file.

Result: warm `deep` status is about 4.7x faster than without the cache, and about 2x the `flat` time at the same file
count (161 ms vs 81 ms), so the "within 3x of flat" criterion is met on this run, with wide noise from the load. The
speedup is the directory-listing part only; the remaining ~160 ms is per-file `stat` and index load. Not measured: Linux,
cold OS cache, 1% modified files, repositories with many untracked files, and the optional filesystem watcher (not done
in this change). Any change in a directory (new, removed or renamed entry) makes that directory be re-read; a status
after such a change was checked for correctness by the property test, not benchmarked.

## 100k-file scale run: git vs vcs CLI (`scripts/bench_scale.sh`), 2026-10-31

Method: `scripts/bench_scale.sh <layout> 100000 5` (one tool at a time; the dataset is generated into the tool's own
work dir by `datagen` and deleted afterwards; **5 runs per cell, median reported, raw runs in the script output**).
Per-run timings wrap the CLI process (`git ...` / `vcs ...`) with Python `perf_counter`; preparation (re-init,
re-add) is untimed. Warm cache. Workflows: `add .` into a fresh index; `commit` with everything staged; `status` clean;
`status` with every 100th file rewritten (1,000 files); `log` of a 50-commit history (`git log --oneline -n 50` /
`vcs log -n 50`); `checkout` = switch from a branch whose tree differs by 1,000 files (1%) to `main`
(`git checkout -q main` / `vcs switch main`; the switch away is untimed). Layouts: `flat` (100 text files of 100-1099
bytes per directory, 1,000 directories), `deep` (depth 8, fan-out 4, about one file per leaf directory),
`binary` (100 files per directory, 4 KiB random incompressible bytes each). Seed 42.

Machine for all 100k numbers: **Linux box, AMD EPYC 7J13, 30 cores, 216 GiB RAM, ext4 (not APFS), git 2.34.1,
vcs release build of `origin/main` at `c47fcda` (before later main commits).** The box is shared: load1 sampled around each cell ranged
1.5 to 34 (per-row values are in the script output; the run starts were load1 21.9 flat, 5.5 deep, 3.2 binary). So
timings are noisy; cells with a big spread between runs (e.g. git deep `commit`: 14.5-37.8 s) say so in the raw runs.
Not comparable in absolute terms with the earlier Mac (M2 Pro, APFS, git 2.52.0) tables above.

Median ms, git / vcs:

| workflow | flat 100k | deep 100k | binary 100k |
|---|---|---|---|
| add . (fresh index) | 22,170 / 47,741 | 15,189 / 39,804 | 23,634 / 29,297 |
| commit (all staged) | 729 / 684 | 19,041 / 30,405 | 652 / 637 |
| status, clean | 121 / 558 | 1,034 / 1,150 | 106 / 555 |
| status, 1% modified | 122 / 570 | 1,080 / 1,145 | 112 / 671 |
| log (50 commits) | 20.1 / 3.8 | 34.4 / 4.0 | 20.7 / 4.1 |
| checkout (1% differ) | 605 / 1,067 | 865 / 3,200 | 580 / 1,121 |

Store size on disk after the first commit (`du -sk` of `.git` / `.vcs`, KiB): flat 421,656 / 439,100; deep 777,884 /
791,608; binary 821,668 / 839,236. Both tools store loose objects here (git has not repacked), so sizes are close.

What the data says (only differences well outside the run-to-run spread):
- vcs `log` of 50 commits is faster than git in all three layouts (3.8-4.1 ms vs 20-34 ms).
- On these datasets git is faster at `status` on flat and binary (about 4.6-6.0x by medians), at `add` on flat and
  deep, and at `checkout`; vcs `checkout` on deep is about 3.7x slower. `commit` is roughly equal on flat and binary and slower
  for vcs on deep. On the binary layout `add` is within about 1.2x.
- `status` on deep is about 1.1 s for both tools.
- Nothing here shows a vcs speedup except `log`.

Caveats: single machine per number, shared and not idle; warm cache only; git ran with an empty config (no
fsmonitor/untracked cache, no repack); git writes zlib-compressed loose objects and vcs does not compress; the `log`
history is 50 single-file commits, not 100k commits; `checkout` moves 1% of files, not a whole tree; the `binary`
layout is 4 KiB files, not large binaries; the Mac was not used for 100k because of disk.

Small Mac comparison (flat, **10,000** files, 5 runs, median ms, git 2.52.0 / vcs; Apple M2 Pro, APFS, load1 about
14-35 during the run, 20 agents sharing the machine): add 8,160 / 2,030; commit 218 / 102; status clean 86.8 / 52.6;
status 1% modified 54.2 / 67.5; log 20.4 / 10.5; checkout 90.8 / 180. The git `add` figure is far above the quiet-machine
baselines earlier in this file (load), so this row pair is not reliable evidence about either tool.

Reproduce: `cargo build --release -p vcs-cli -p vcs-bench && scripts/bench_scale.sh flat 100000 5` (also `deep`, `binary`;
needs about 1.5 GB free for flat, about 2.5 GB for deep/binary per tool at 100k; the script aborts below `MIN_FREE_GB`, default 4).

## Caveats

- One machine, one run session, `--quick` sampling; treat differences under ~2x as noise.
- Release build only; debug numbers not collected.
- Not run: 100k-commit history, 1k branches, 50 zones, large-binary mutation, RSS/CPU/disk I/O,
  cold cache, hyperfine, S3. The roadmap's benchmark plan lists these; they remain open.
- The git comparison covers a single workflow set at 1k files and is not a fair "who is faster" claim given the
  semantic differences above.

## Binary-heavy dataset: loose vs packed (measured)

Reproduce: `cargo run --release -p vcs-bench --bin packbench -- 100 262144 4 4 4096`.

Dataset: 100 files of 256 KiB deterministic random (incompressible) bytes, 4 versions; each new version overwrites 4
regions of 4 KiB in place in every file. The tool adds and commits each version, then times `fsck`, `repack`, `fsck`.
Same machine and release profile as above, warm cache, two runs, not idle-controlled; only one dataset size was run.

| Metric | Run 1 | Run 2 |
|---|---|---|
| add + commit, 4 versions | 2.54 s | 3.53 s |
| loose store: files / apparent bytes / allocated bytes | 421 / 104,877,906 / 106,598,400 | same |
| fsck, loose | 0.40 s | 0.44 s |
| `repack` (421 objects) | 1.59 s | 2.48 s |
| packed store: files / apparent bytes / allocated bytes | 2 / 104,898,242 / 104,902,656 | same |
| fsck, packed | 1.54 s | 1.12 s |

Findings:
- Packing does not shrink the data: there is no compression or delta yet, so apparent size grew by 20,336 bytes (index
  and framing). It does cut allocated bytes by about 1.6% (106.6 MB to 104.9 MB) and 421 files to 2.
- Packed `fsck` was slower than loose `fsck` in both runs because it also re-hashes the whole pack once.
- Every changed file is stored as a full new blob (about 4x the first version), which is the case content-defined chunking targets.

## Content-defined chunking prototype: dedup (measured)

Reproduce: `cargo run --release -p vcs-bench --bin chunkbench -- 100 262144 4 4 4096 overwrite` (or `insert`).
Prototype only: `vcs_store::chunk` (gear hash, normalized cut) finds boundaries; it is not wired into the object model,
so no chunk manifests exist yet and nothing here changes the stored repo. "Stored" counts each distinct BLAKE3 hash once
across all 4 versions of all 100 files (256 KiB random, per version 4 edits of 4 KiB per file). One run each, same
machine as above, warm cache; the dataset has no natural redundancy besides the edits.

| Edit kind | Chunk min/avg/max | Whole-blob stored | Chunk stored | Chunk / whole | Chunking time (all versions) |
|---|---|---|---|---|---|
| overwrite in place | 2 KiB / 8 KiB / 32 KiB | 104,857,600 B | 45,557,538 B | 43.4% | 164 ms |
| overwrite in place | 8 KiB / 32 KiB / 128 KiB | 104,857,600 B | 70,271,578 B | 67.0% | 78 ms |
| insert (shifts data) | 2 KiB / 8 KiB / 32 KiB | 114,688,000 B | 47,624,101 B | 41.5% | 74 ms |
| insert (shifts data) | 8 KiB / 32 KiB / 128 KiB | 114,688,000 B | 75,727,365 B | 66.0% | 78 ms |

Findings: smaller chunks dedup more on this edit pattern, and insertions that shift data dedup about as well as
in-place overwrites. Not measured: per-chunk metadata/manifest overhead (so the percentages overstate the saving), read
or checkout time, and the effect on `fsck`.

### Pack fsck after parallelizing verification

`Pack::verify` now hashes the whole pack and the per-object ranges on separate threads (at most 8). Re-run on the same
dataset after main moved on (loose `fsck` itself also changed upstream, so these are not comparable with the table above):
loose fsck 96 ms and 101 ms, packed fsck 217 ms and 202 ms (two runs). The earlier packed numbers (1.54 s, 1.12 s) came
from an older build, so no speedup factor is claimed.

## Chunk manifests wired into the object model (measured)

Reproduce: `cargo run --release -p vcs-bench --bin packbench -- 20 2097152 4 4 4096` on the base commit (whole blobs) and
on the chunk-manifest branch. Dataset: 20 files of 2 MiB deterministic random bytes (above the 1 MiB chunking
threshold), 4 versions, 4 in-place edits of 4 KiB per file per version. Chunking: gear-hash CDC, 4/16/64 KiB min/avg/max.
Same machine, release profile, warm cache, two runs each; the machine was shared with other builds, so timings are noisy.

| Metric | Whole blobs (base) | Chunk manifests |
|---|---|---|
| objects stored | 101 | 2,543 |
| loose apparent bytes | 167,777,426 | 49,570,606 (29.5%) |
| packed apparent bytes | 167,782,402 | 49,692,798 (29.6%) |
| add + commit, 4 versions (2 runs) | 2.39 s, 0.82 s | 10.05 s, 10.18 s |
| loose fsck (2 runs) | 127 ms, 232 ms | 1.87 s, 1.57 s |
| packed fsck (2 runs) | 237 ms, 831 ms | 989 ms, 1.13 s |

Findings:
- Size: random data with small edits stores 29.5% of the whole-blob bytes (the manifests are about 36 bytes per chunk,
  included here). This is the best case: the data has no other redundancy.
- Time: chunking is not free. Add + commit was roughly 4x to 12x slower in these runs (about 2,540 objects written
  instead of 101, each a separate loose file and hash, plus chunking every file), and `fsck` is about 7x to 15x slower
  for loose objects for the same reason. We have not profiled which part dominates, so the cause is a hypothesis.
  The saving is bytes (and network transfer), not speed. A larger average chunk would cut object counts but dedup less.
- Not measured: checkout/clone time, remote transfer time, non-random binaries, or other thresholds.

## Touch-on-dedup cost (`add_dedup`, measured)

`LocalStore::put` refreshes an existing object's mtime only when it is older than `touch_after` (default 3.5 days). Bench
`repo/add_dedup`: empty index, every object already stored (and just written, so "recent"), so each file takes the dedup path
(index reset untimed). 10,000 files, release, warm, criterion default sampling (10 samples). Three process runs, each timing
all three variants in one process (run 2 in reverse order), via a temporary env override of `touch_after` that is not in the
tree: `never` = no touch (the old behaviour), `default` = 3.5 days (what ships), `always` = touch on every hit.
Table cells are the **median of the three runs' mid estimates** (individual runs ranged widely, see below).

| layout (10,000 files) | never | default | always |
|---|---|---|---|
| flat | 244 ms | 291 ms | 674 ms |
| deep | 1.16 s | 1.15 s | 1.32 s |
| bigdir | 170 ms | 150 ms | 346 ms |

Per-run mid estimates (ms), run1 / run2 / run3: flat never 244/153/261, default 291/142/296, always 938/377/674; deep never
1156/1067/1715, default 1151/1028/1493, always 1113/1322/3190; bigdir never 142/170/401, default 142/150/288, always
329/346/576.

Reading: the shipped default is indistinguishable from no touch at this noise level (the machine was shared with other
builds, so run-to-run spread is up to 2-3x; the `default` and `never` numbers swap order between runs). Touching on every hit
roughly doubles flat and bigdir dedup-adds, which is why the age threshold exists. Not measured: dedup hits on objects older
than `touch_after` (these do pay one open and `futimens` each, the `always` column is their upper bound), cold cache,
quiet machine.

## Commit-graph cache (#103), 10,000 commits

Measured on the Linux x86_64 build box (30 cores), `cargo bench -p vcs-bench --bench core -- log`, criterion medians.
History: 10,000 commits, two branches diverging 10 commits before each tip. Graph file present and current.

| operation | without graph | with graph |
|---|---|---|
| merge base of the two tips | 325 ms | 2.32 ms |
| is_ancestor, negative (walks all history without graph) | 154 ms | 6.01 ms |
| is_ancestor, positive near the tip | 149 us | 144 us |

`log` does not use the graph (limit 10 = 114 us, limit 1,000 = 7.95 ms, limit 10,000 = 86.6 ms on this box; an
experimental graph-driven log was measured at 2.42 ms, 10.8 ms and 93.9 ms, i.e. slower, so it was dropped). The ~2.3 ms
floor with the graph is loading and validating the 10k-entry file; building it cold was not measured.
These numbers are from a different machine than the earlier tables in this file and are not comparable with them.

## Remote packs: requests and bytes (`CountingBackend` over `MemBackend`)

Measured by `cargo test -p vcs-remote --test packs -- --nocapture` on the Linux build box. Requests = every backend call
(GET, ranged GET, HEAD, PUT, LIST); bytes are payload bytes. "loose" is the legacy layout (`with_loose_objects()`), "packed"
the new default; both rows run the same code and workload, so only the layout differs. In-memory backend: no latency.

| workload | layout | requests | bytes down | bytes up |
|---|---|---|---|---|
| publish 1 edited file, directory of 5,000 entries (second commit) | loose | 5,009 | 81 | 200,304 |
| same | packed | 6 | 81 | 200,576 |
| lazy clone (commits and trees), 1,000 commits x 100 files | loose | 2,002 | 3,899,939 | 0 |
| same | packed | 9 | 4,063,554 | 0 |
| full fetch (all objects), same history | loose | 3,101 | 3,909,413 | 0 |
| same | packed | 4 | 4,058,293 | 0 |

Reading: the 1-file publish upload is 200 KB in both layouts because the 5,000-entry tree itself is rewritten (that is the
real changed object); only the request count changes. The packed clone moves about 4% more bytes (read-ahead past the metadata
prefix, pack framing) for about 220x to 780x fewer requests; on a real network the win is latency, not bytes, and it was not
measured here. The loose-layout "before" was produced with the new fetch code against a loose remote (one extra LIST), not
by checking out the old tree, so the audit's figures (5,012 and 3,103) differ by that small amount. Not measured: S3 or any
real network, first-publish cost, many accumulated packs (one index GET per pack per fetch).

## Perf pass: big-file add/fsck and checkout at 100k files (measured, Linux box)

Machine: the shared 30-core Lambda box (ext4, load average 10-22 during these runs, so absolute numbers are noisy; each
pair was run interleaved, base and new alternating). "base" = `origin/main` at `8b94091`; "new" = this branch.

What the profile said (callgrind, instructions; timers for wall time):
- Chunked add: every chunk was hashed in `ChunkedBlob::plan`, hashed again in `put` (`id_of_encoded`), copied twice
  (`to_vec`, `encode`), and `add` with fewer than 64 files ran on one thread. The first `sync` (fsync of about 2,000
  loose files) was about 85% of add wall time on ext4.
- Loose `fsck`: every chunk was read and hashed twice (once as a listed object, once more by its manifest), serially.
- `switch` at 100k flat: decoding the index twice (one `Index::insert` with two `format!` allocations per entry), the
  untracked-file walk inside the clean check (not needed for it), the whole-tree case-fold collision scan, and
  flattening both full trees.

Changes (on-disk format and ids unchanged; chunk boundaries are pinned by a test against the old scan):
single hash per chunk and no copy on write; `chunk_ends` as two branch-free scans; `add` parallel (at most 8 threads)
when any file is 256 KiB or more; fsync pass preceded by a parallel `sync_file_range` write-back on Linux and up to 32
fsync threads; fsck reads objects on 8 threads and checks manifests against what it already read; index decode builds
the map in one pass; case-fold scan skips shared leading directories; `switch` loads the index once, skips the
untracked walk, diffs the two trees skipping equal subtrees, checks collisions per changed directory, and writes
files on up to 8 threads; `status` stats files on up to 8 threads.

`packbench 20 2097152 4 4 4096` (20 files of 2 MiB, 4 versions; 7 runs each, medians):

| metric | base | new |
|---|---|---|
| add + commit, 4 versions (includes file mutation, about 0.2 s) | 2.44 s | 0.85 s |
| loose fsck | 385 ms | 54 ms |
| packed fsck | 410 ms | 109 ms |

Add + commit of one version (first sync dominated), 9 interleaved runs: base about 0.85 s, new about 0.32-0.39 s.

`vcs switch main` with 1% of 100k flat files different (`~/swbench`, same dataset as `scripts/bench_scale.sh flat`):
base runs 1194-2652 ms (median of 9: 1.44 s), new 324-402 ms (median 0.35 s). The earlier table on main has git at
605 ms for this cell on the same box; this was not re-measured against git in this pass.

Not measured: deep and binary layouts, macOS/APFS, a cold cache, `add` of 100k small files (unchanged: still reads and
hashes every file), parallel chunking inside a single huge file. Caveat on the switch change: the case-fold collision
check now looks at directories that differ between the two branches, so a collision that already exists inside
unchanged directories of both branches is no longer reported by `switch` (it is still reported by `add`).
