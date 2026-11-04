# More optimizations: build profile, process startup, and the ideas not built

Scope: second hunt after `10x-opportunities.md`. Speed over storage. macOS arm64, 10 cores, load average 20-60 from other
agents, so **CPU time (user+sys of the child, `getrusage`) is the metric; wall is shown but inflated**. Method: 20,000-file
repo (200 dirs x 100 files, 6 commits), a small python harness (subprocess + `getrusage`): each binary run alternately, 40-60 runs,
medians, one warmup. Floor for comparison: an empty C `main` costs 2.6 ms CPU / 5.6 ms wall here under this load.

## Measured and implemented (PR with this document)

| Change | Measured effect | Notes |
|---|---|---|
| **A. Do not link CoreFoundation/CoreServices into `vcs`.** The `notify` FSEvents backend made dyld load two frameworks in every process, watcher or not. The watcher moved to crate `vcs-watchd` and binary `vcs-watchd`; `vcs watch` and the automatic watcher spawn it (sibling of `vcs`). | `--version` 4.3 -> ~2.3 ms CPU (no-watcher build vs default, same profile); the 3 other dylibs left are libSystem/libiconv | The biggest single startup cost found. Linux inotify has no such cost but uses the same layout. `vcs-repo` no longer has a `watcher` feature or `notify` dependency. |
| **B. `[profile.release] lto = "fat", codegen-units = 1`** (panic stays `unwind`, because `vcs-server` isolates a panicking connection thread) | in isolation (fat LTO + cgu1 + abort vs old release): `log` 6.4 -> 5.9, `status` 11.6 -> 10.9, `branch` 4.2 -> 4.1 ms CPU: 2-8%; binary 15.7 -> 10.3 MB | Costs build time: 6.3 min -> 9.8 min for a cold release build of `vcs-cli` under load. |
| A + B together, final binaries, 60 runs, CPU ms (wall ms) | | |
| `vcs --version` | 4.0 (5.2) -> **2.8 (3.9)** | -30% |
| `branch` | 3.8 (4.9) -> **2.6 (3.8)** | -32% |
| `log --oneline` | 5.0 (6.2) -> **3.6 (4.8)** | -28% |
| `show` | 4.6 (5.9) -> **3.2 (4.5)** | -30% |
| `ls-tree HEAD` | 4.1 (5.3) -> **2.8 (4.1)** | -32% |
| `status` (20k files) | 11.2 (19.8) -> **8.6 (17.5)** | -23% |
| `diff` (20k files) | 8.6 (19.1) -> **5.9 (17.6)** | -31% |

Why it matters: the p4 comparison gap on one-file operations is dominated by process start (`p4-small-ops.md`). About 1.2-1.4 ms
CPU per invocation comes off every command, on every machine that runs `vcs` many times (agents, hooks, shell prompts).
Compatibility: `vcs-watchd` must be installed next to `vcs` (a normal `cargo build`/`cargo install -p vcs-cli` does both);
if it is missing, `vcs watch` says so and the automatic watcher silently stays off (`status` is then a plain scan).

## Ranked table: ideas evaluated

Gain is on our own cases. "Measured" means a number in this document; everything else is an estimate and says so.

| Rank | Idea | Gain | Status | Effort | Risk |
|---|---|---|---|---|---|
| 1 | Drop CoreServices from the main binary (A) | 1.2-2 ms CPU on every command (-30% on small ones) | **done, measured** | done | missing `vcs-watchd` disables the watcher |
| 2 | Fat LTO + cgu=1 (B) | 2-8% CPU, -35% size | **done, measured** | done | build time +50% |
| 3 | PGO (`-Cprofile-generate` on the scoreboard workload, `llvm-profdata`) on top of LTO | **measured: no gain** (-1.3% .. +0.9% CPU, inside noise; see follow-up below) | **script shipped, not adopted** (#505) | done | profile staleness |
| 4 | Remaining startup: 2.8 vs 2.6 ms floor means the Rust side is already near the floor on this host; what is left is dyld of libiconv (pulled by std/unicode-normalization users) | <= 0.3 ms | not worth an issue | | |
| 5 | `panic = "abort"` | measured with the LTO row: no visible gain beyond LTO on these commands (-0.3 MB size) | **rejected**: kills `vcs-server` on any handler panic | | |
| 6 | mimalloc/jemalloc | **measured: no consistent win, server +19..23% CPU worse** (follow-up below) | **rejected** (#508) | done | |
| 7 | Command-level fast path for `status`/`diff` on the 20k repo: 8.6 ms CPU is now 6 ms above the process floor; remaining cost is index decode + stat of every file (see #493 mmap index, #294) | 2-3x on status/add | existing issues | | |
| 8 | `getattrlistbulk` (macOS) / `statx` via io_uring (Linux) for directory scans: one syscall per directory instead of lstat per file | **macOS measured: status -23%, diff -43% CPU at 100k files**; Linux not done (cannot be measured here) | **macOS done** (#506) | done | platform code, unsafe |
| 9 | Bloom filter of changed paths per commit (commit-graph extension) for `log -- path` / `blame` | not measured; git's changed-path Bloom gives 5-10x on deep history path log | **done (filters existed; gc builds them eagerly, see history-walks.md)** | | |
| 10 | Multi-pack index analog and geometric repack | `gc` 1.8x loss; see 10x idea 9 | existing idea | 1-2 weeks | compaction correctness |
| 11 | Reachability bitmaps | fetch has no have/want walk; local gc mark 17.9 s -> 0.48 s CPU at 100k commits (docs/gc.md) | **done for gc mark** | 2-3 weeks | format |
| 12 | blake3 SIMD features per target | already enabled: `neon` feature in the workspace dep; x86 AVX2/AVX-512 are runtime-detected by the crate by default | no action | | |
| 13 | zstd level/dictionary tuning | speed-vs-ratio already chosen in `storage-speed.md`; dictionaries are a storage idea | skip | | |
| 14 | Persistent daemon for command-level result caching | a resident process saves the 2.6 ms floor only if clients talk to it by socket (roughly 0.3 ms), i.e. at best 2x on tiny commands, at the cost of a lifecycle/security surface | issue (design only) | 3-4 weeks | stale state, security |
| 15 | Thread-pool sizing | not measured; parallel paths already use `available_parallelism` | skip | | |

Not measured and why: Linux perf/samply is unavailable locally and the Linux box is off limits (no AWS). Numbers above are
macOS only; the CoreServices win (A) does not exist on Linux, the LTO win (B) should.

## Follow-up: PGO, allocators, bulk directory stat (issues #505, #508, #506)

Method: `scripts/pgo/ab.py` (new). 100,000-file flat repo from `datagen` (1000 dirs x 100 files), 5 small commits, no
watcher (`VCS_NO_AUTOWATCH=1 VCS_NO_FSMONITOR=1`, so `status`/`diff` are real cold scans), every variant runs once per round in
shuffled order, CPU time = user+sys of the child from `wait4` rusage, warmup round dropped, median and MAD% reported.
macOS arm64, 10 cores. Load average is noted per table; the allocator runs were under load 50-75 (other agents), the
PGO and bulk-stat confirmation runs under load 3-6. Linux and AWS were not used.

### #506 getattrlistbulk: adopted (macOS)

`StatCursor` (used by the sorted-path stat loops of `status`, `diff`, and `add`) reads a directory with one
`getattrlistbulk(2)` after 8 files of it were stat-ed one by one, and answers the remaining lookups from that map. Entries it
cannot describe exactly as `fstatat` does (symlinks, directories, devices, a missing attribute) are left out and fall back to
`fstatat`. `VCS_BULKSTAT=0` disables it. Equivalence test: `stat_seq_bulk_matches_std` compares against `symlink_metadata`
for files (executable, empty, non-ASCII names), a symlink, a directory, missing paths, big and small directories, twice.
CPU ms, median of 21 interleaved runs, load 3-5, MAD 1-3%:

| scenario | before (main) | after | same binary, `VCS_BULKSTAT=0` |
|---|---|---|---|
| `status` (clean) | 1316 | **1017 (-22.7%)** | 1305 (-0.9%) |
| `status` (1% dirty) | 1306 | **1018 (-22.1%)** | 1338 (+2.4%) |
| `diff` | 769 | **437 (-43.3%)** | 764 (-0.7%) |
| `add .` (no change) | 1183 | 1196 (+1.1%, noise) | 1196 |
| `--version` (startup floor) | 2.9 | 2.9 | 2.8 |

Linux: `statx` has no per-directory form, and the io_uring batching variant could not be built or timed here (no Linux box, no
AWS), so Linux keeps `fstatat`; the `cfg(target_os = "macos")` gate leaves that path untouched. Not measured, not claimed.

### #505 PGO: script shipped, not adopted

`scripts/pgo/pgo-build.sh [OUT_DIR]` is the reproducible flow: instrumented build (`-Cprofile-generate`, explicit `--target` so
build scripts stay uninstrumented), training = `scripts/pgo/ab.py` scenarios on a generated 100k-file repo (floor, status,
dirty status, add, diff, log, clone, gc, client + `vcs-server` over HTTP), `llvm-profdata merge` using the toolchain's own
copy (`rustup component add llvm-tools`; a system llvm-profdata of another major version rejects the profile), then the
optimized build (`-Cprofile-use`). No CI, no checked-in profile (it is tied to the exact source and rustc). Release recipe:

```
rustup component add llvm-tools
scripts/pgo/pgo-build.sh dist/        # ~3 release builds of CPU time; vcs, vcs-watchd, vcs-server land in dist/
```

Result, PGO vs the same source built without it (CPU ms, 21 runs for the first six rows, 5 for clone/gc, load 5-6):

| scenario | no PGO | PGO | delta |
|---|---|---|---|
| `--version` | 2.9 | 2.8 | -1.0% |
| `log --oneline` | 3.8 | 3.8 | -0.1% |
| `status` | 751.6 | 758.8 | +0.9% |
| `status` (1% dirty) | 722.7 | 720.5 | -0.3% |
| `add .` | 1201.5 | 1186.1 | -1.3% |
| `diff` | 467.3 | 466.0 | -0.3% |
| `clone` (file remote) | 37554 | 37761 | +0.6% |
| `gc` | 983.6 | 979.6 | -0.4% |

Every delta is inside the 1-7% MAD. The big commands are syscall-bound (sys time dominates, see the bulk-stat table) and the
small ones are at the process-start floor, so there is little branchy user code left for PGO to improve. Not adopted; the
script stays so the question can be re-run when the hot paths change.

### #508 mimalloc / jemalloc: rejected

Both were built as opt-in cargo features on `vcs` and `vcs-server` (fat LTO, same flags), then removed again. CPU ms,
interleaved, load 50-75, 7 runs (server: 5 runs, load 7-22):

| scenario | system allocator | mimalloc | jemalloc |
|---|---|---|---|
| `--version` | 7.3 | 6.2 (-15%, MAD 18% on base) | 6.1 (-16%, MAD 29%) |
| `status` | 1628 (MAD 50%) | 1832 (+12.5%) | 1599 (-1.8%) |
| `clone` (100k files) | 31977 | 34306 (+7.3%) | 28534 (-10.8%, MAD 11%) |
| `gc` | 851 | 739 (-13.2%) | 828 (-2.7%) |
| server CPU (publish + 2 clones served) | 205 | 244 (**+18.9%**) | 253 (**+23.5%**) |
| client clone from server (2x) | 71554 | 74073 (+3.5%) | 73540 (+2.8%) |

No allocator wins on server, clone and gc together; the server, the one long-running process, is 19-24% worse with either, and
the clone and `status` movements flip sign between the two. The startup-floor differences are noise (the base's own spread
is 18%). Adding a dependency plus a global allocator for that is not justified, so nothing was merged.
