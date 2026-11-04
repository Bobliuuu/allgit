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
| 3 | PGO (`-Cprofile-generate` on the scoreboard workload, `llvm-profdata`) on top of LTO | not measured (needs a representative training run in CI; typical 5-15% on branchy code) | issue | 2-3 days (CI pipeline) | profile staleness |
| 4 | Remaining startup: 2.8 vs 2.6 ms floor means the Rust side is already near the floor on this host; what is left is dyld of libiconv (pulled by std/unicode-normalization users) | <= 0.3 ms | not worth an issue | | |
| 5 | `panic = "abort"` | measured with the LTO row: no visible gain beyond LTO on these commands (-0.3 MB size) | **rejected**: kills `vcs-server` on any handler panic | | |
| 6 | mimalloc/jemalloc | not measured; processes live 3-10 ms and allocate little, so allocator init and arena warmup would eat the gain; only the long-running server/gc/clone paths could benefit | issue (measure on clone/gc) | 1 day | extra dependency, `unsafe` global allocator |
| 7 | Command-level fast path for `status`/`diff` on the 20k repo: 8.6 ms CPU is now 6 ms above the process floor; remaining cost is index decode + stat of every file (see #493 mmap index, #294) | 2-3x on status/add | existing issues | | |
| 8 | `getattrlistbulk` (macOS) / `statx` via io_uring (Linux) for directory scans: one syscall per directory instead of lstat per file | estimate: first scan sys time (7.5 s of 5.8 s wall scan at 100k files in `10x-opportunities.md`) could drop 2-4x | issue | 1 week | platform code, unsafe |
| 9 | Bloom filter of changed paths per commit (commit-graph extension) for `log -- path` / `blame` | not measured; git's changed-path Bloom gives 5-10x on deep history path log | **done (filters existed; gc builds them eagerly, see history-walks.md)** | | |
| 10 | Multi-pack index analog and geometric repack | `gc` 1.8x loss; see 10x idea 9 | existing idea | 1-2 weeks | compaction correctness |
| 11 | Reachability bitmaps | fetch has no have/want walk; local gc mark 17.9 s -> 0.48 s CPU at 100k commits (docs/gc.md) | **done for gc mark** | 2-3 weeks | format |
| 12 | blake3 SIMD features per target | already enabled: `neon` feature in the workspace dep; x86 AVX2/AVX-512 are runtime-detected by the crate by default | no action | | |
| 13 | zstd level/dictionary tuning | speed-vs-ratio already chosen in `storage-speed.md`; dictionaries are a storage idea | skip | | |
| 14 | Persistent daemon for command-level result caching | a resident process saves the 2.6 ms floor only if clients talk to it by socket (roughly 0.3 ms), i.e. at best 2x on tiny commands, at the cost of a lifecycle/security surface | issue (design only) | 3-4 weeks | stale state, security |
| 15 | Thread-pool sizing | not measured; parallel paths already use `available_parallelism` | skip | | |

Not measured and why: Linux perf/samply is unavailable locally and the Linux box is off limits (no AWS). Numbers above are
macOS only; the CoreServices win (A) does not exist on Linux, the LTO win (B) should.
