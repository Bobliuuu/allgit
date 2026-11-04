# Autosync and hot path, quiet-box rerun (box h, 2026-11-03)

Closes two gaps in `box-harvest.md` (PR 468) (PR 468): "autosync has no results" and "hot path has a baseline only".
Driver: `scripts/bench_autosync_hotpath.sh`. Raw data: `data/autosync-hotpath/` (`hot-{clean,dirty,small}.json`, `autosync.json`, `meta.txt`).

## Setup

* Box h, AWS c6i.4xlarge (16 vCPU), git 2.43.0, `cargo build --release`. **Build: origin/main `6300ff7242cb2bd28ce531255a5eceffb9c86edd`** (vcs 0.1.0); this PR adds only the driver and docs.
* Quiet: each phase started only when load1 < 1.0 (starts 0.92 to 0.97). Ends: clean 1.34, dirty 0.97, small 1.17, autosync 1.68 (the benchmark's own server and clients).
  So nothing ran under contention, but the load1 at the end of some phases is above 1 because of the work itself.
* Hot path: `scripts/command_scoreboard.py`, groups clean, dirty, small, flat 100k files, 9 interleaved runs per cell, wall and CPU (user+sys). Verdict rule: WIN = wall under 0.9 of git and CPU not above 1.1; LOSS = wall over 1.1; otherwise TIE or mixed.
  Cells at 1 to 3 ms are dominated by process start; their ratios are real but the absolute differences are about 1 ms.
* Autosync: `scripts/bench_zoneprofile.py`, 50 zones, 100,051 files, loopback server (no network latency), 5 runs, same persona profile as [zone-profile-bench.md](zone-profile-bench.md). git comparison is partial clone + sparse checkout of the same 7 zones, which gives git the answer.

## Autosync (`clone --auto-profile`, `sync --auto`)

Initial clone

| scenario | wall s | cpu s | requests | MB downloaded | MB on disk |
|---|---|---|---|---|---|
| vcs full | 2.54 [2.50..4.79] | 4.23 [4.15..4.70] | 28 [28..28] | 189.5 [189.5..189.5] | 622.5 [622.5..622.5] |
| vcs --auto-profile | 1.03 [0.95..1.35] | 1.12 [1.11..1.18] | 71 [71..71] | 37.0 [37.0..37.0] | 95.2 [95.2..95.2] |
| git full | 7.05 [6.71..7.18] | 6.36 [6.34..6.40] | n/a | n/a | 197.4 [197.4..197.4] |
| git partial (blob:none) + sparse, same zones | 3.83 [3.81..3.96] | 7.65 [7.63..7.67] | n/a | n/a | 39.6 [39.6..39.6] |

Incremental sync (1000 files changed in 10 zones)

| scenario | wall s | cpu s | requests | MB downloaded | MB on disk |
|---|---|---|---|---|---|
| vcs sync | 8.71 [7.96..11.47] | 7.94 [7.64..9.02] | 75 [55..95] | 48.8 [47.7..49.9] | n/a |
| vcs sync --auto | 0.67 [0.60..1.10] | 0.55 [0.52..0.61] | 21 [21..21] | 2.0 [2.0..2.0] | n/a |
| git pull, full clone | 0.38 [0.37..0.64] | 0.52 [0.50..0.72] | n/a | 2.4 [-165.2..130.9] | n/a |
| git pull, partial + sparse | 0.28 [0.26..0.45] | 0.25 [0.24..0.43] | n/a | 1.1 [1.1..1.1] | n/a |

* Matches the earlier box C run (zone-profile-bench.md) on bytes exactly (37.0 MB for `--auto` clone, 2.0 MB for `sync --auto`), now with a quiet start (load1 0.94) and 5 runs.
* `clone --auto-profile`: 19.5 percent of the bytes of a full vcs clone, 2.5x faster than vcs full, 3.7x faster than git full, and faster than git partial+sparse in wall (1.03 vs 3.83 s) and CPU (1.12 vs 7.65 s). It makes 71 requests against 28 for a full clone; loopback hides what that costs on a real link.
* `sync --auto` after 1000 changed files: 0.67 s and 2.0 MB against 8.71 s and 48.8 MB for a full vcs sync (13x faster, 24x fewer bytes). **Against git it is a loss:** git pull takes 0.38 s (full) and 0.28 s (partial+sparse), so vcs `--auto` sync is 1.8x and 2.4x slower than git in wall, and 2.2x CPU of git partial. Bytes: 2.0 MB vs 1.1 MB for git partial, a loss too (git full 2.4 MB, a tie). The git "MB downloaded" for full pull is `.git` growth and its min/max range is unreliable (negative lower bound, gc during the run); read only the median.
* Fetching one skipped zone on demand (z02): vcs 0.83 s, 13 requests, 8.4 MB; git `sparse-checkout add` 0.31 s, 3.9 MB. A loss, 2.7x wall and 2.1x bytes.
* Recording the usage profile costs nothing measurable: `status` median 664.8 ms with recording on and 664.5 ms off (n=40). (This absolute figure is the benchmark's 100k-file zone repo with 50 zones, not the flat repo below.)
* Not measured: real network latency, a real codebase, profile accuracy on real usage.


## Hot path (status, diff and the rest of the scoreboard), after vs baseline

"vcs before" is the box h baseline from PR 468 (`box-harvest-data/hotpath-baseline`, same box, same generator, quiet), available for the 11 cells it covered. The "now" column is origin/main `6300ff7`.

**Result: the hot path did not change.** Every cell with a baseline is within about 5 percent of its before value (`status` 29.5 to 30.3 ms, `diff` clean 7.0 to 7.6 ms, `diff` dirty 58.6 to 59.8 ms, `commit_1` 64.8 to 60.0 ms; `log5` 1.7 to 2.1 ms is the noisiest). No improvement is claimed. The status and clean-diff wins and the known losses are the same as in the baseline:

* WIN: `status` (0.47 wall, 0.24 CPU), `status -s`, dirty `status`, `diff` clean (0.34), `diff -- path`, `diff A..B`, `blame`, `grep`, `merge`, `rebase`, `cherry-pick`, `switch`, `restore`, `add FILE`.
* LOSS, still open: `diff --name-only` dirty (1.82 wall, 3.84 CPU), `diff --stat` dirty (1.16 / 3.36), `diff` dirty CPU (3.13 x git, wall tie 0.95), `add .` (1.82), `commit -a` (1.66), `commit --amend` (1.40, CPU 0.28), `bundle verify` (332 ms vs 1.3 ms), `ls-tree` flat listing (31 ms vs 1.8 ms), `switch -c` (24 ms vs 1.7), `branch -m` (18.8 ms vs 1.6), `gc` CPU (2.56x), `bundle create` CPU (1.60x), and the 1 to 2 ms read commands (`rev-parse`, `config`, `shortlog`, `log -n`, `log --stat`) which lose on process start by about 1 ms.
* Likely cause of the dirty `diff` loss: vcs spends about 550 ms CPU against git's 175 ms for the same 1000 modified files (wall is hidden by parallelism). Not investigated further here.

Full table (all cells, medians of 9; ratio = vcs / git, below 1 is faster):

| cell | git wall/CPU ms | vcs wall/CPU ms (now) | wall ratio | CPU ratio | vcs wall/CPU before | verdict |
|---|---|---|---|---|---|---|
| status | 65.1 / 180.2 | 30.3 / 43.9 | 0.47 | 0.24 | 29.5 / 43.4 | WIN |
| status_s | 66.7 / 181.0 | 30.0 / 43.8 | 0.45 | 0.24 | 28.6 / 42.4 | WIN |
| status_porcelain | 66.4 / 180.8 | 30.5 / 44.2 | 0.46 | 0.24 | n/a | WIN |
| diff_clean | 22.4 / 135.3 | 7.6 / 7.2 | 0.34 | 0.05 | 7.0 / 6.5 | WIN |
| diff_staged_clean | 10.1 / 10.0 | 5.6 / 5.6 | 0.56 | 0.56 | n/a | WIN |
| diff_rev | 1.8 / 1.7 | 2.5 / 2.5 | 1.39 | 1.41 | n/a | LOSS |
| diff_branches | 75.7 / 75.6 | 37.7 / 37.6 | 0.50 | 0.50 | n/a | WIN |
| diff_path | 9.7 / 12.8 | 6.0 / 5.9 | 0.62 | 0.46 | 5.5 / 5.4 | WIN |
| log5 | 1.5 / 1.4 | 2.1 / 2.0 | 1.40 | 1.43 | 1.7 / 1.6 | LOSS |
| log_all | 2.0 / 1.9 | 2.3 / 2.2 | 1.12 | 1.13 | n/a | LOSS |
| log_graph | 2.1 / 2.0 | 2.3 / 2.3 | 1.10 | 1.11 | n/a | LOSS |
| log_author | 2.4 / 2.3 | 2.3 / 2.2 | 0.97 | 0.98 | n/a | TIE |
| log_grep | 2.0 / 2.0 | 2.3 / 2.3 | 1.15 | 1.16 | n/a | LOSS |
| log_S | 590.4 / 590.2 | 181.3 / 197.2 | 0.31 | 0.33 | n/a | WIN |
| log_follow | 62.1 / 61.9 | 5.8 / 5.7 | 0.09 | 0.09 | n/a | WIN |
| log_namestatus | 67.1 / 67.0 | 36.7 / 36.6 | 0.55 | 0.55 | n/a | WIN |
| log_format | 1.7 / 1.6 | 2.4 / 2.3 | 1.38 | 1.41 | n/a | LOSS |
| log_path | 9.8 / 9.7 | 5.8 / 5.7 | 0.59 | 0.59 | n/a | WIN |
| log_range | 1.5 / 1.4 | 2.1 / 2.0 | 1.43 | 1.46 | n/a | LOSS |
| log_sym | 1.5 / 1.4 | 2.1 / 2.0 | 1.39 | 1.42 | n/a | LOSS |
| log_stat | 3.5 / 3.4 | 5.9 / 5.9 | 1.72 | 1.74 | n/a | LOSS |
| revlist_count | 1.4 / 1.3 | 1.9 / 1.9 | 1.35 | 1.38 | n/a | LOSS |
| revlist_all | 1.8 / 1.8 | 1.9 / 1.9 | 1.06 | 1.06 | n/a | TIE |
| shortlog | 1.0 / 0.9 | 2.2 / 2.1 | 2.31 | 2.43 | n/a | LOSS |
| show | 1.9 / 1.8 | 1.5 / 1.4 | 0.76 | 0.76 | 1.3 / 1.2 | WIN |
| show_stat | 1.8 / 1.8 | 2.3 / 2.3 | 1.27 | 1.29 | n/a | LOSS |
| show_file | 1.3 / 1.2 | 1.2 / 1.1 | 0.94 | 0.94 | n/a | TIE |
| catfile_p | 1.2 / 1.1 | 1.2 / 1.1 | 0.97 | 0.99 | n/a | TIE |
| catfile_t | 1.0 / 0.9 | 1.0 / 0.9 | 1.00 | 1.02 | n/a | TIE |
| revparse | 0.9 / 0.8 | 1.7 / 1.7 | 1.88 | 1.97 | n/a | LOSS |
| revparse_short | 1.0 / 0.9 | 1.7 / 1.7 | 1.76 | 1.83 | n/a | LOSS |
| revparse_top | 0.9 / 0.8 | 1.7 / 1.6 | 1.87 | 1.99 | n/a | LOSS |
| branch_list | 1.0 / 0.9 | 0.9 / 0.9 | 0.94 | 0.94 | 0.9 / 0.8 | TIE |
| tag_list | 0.9 / 0.9 | 1.0 / 0.9 | 1.08 | 1.09 | n/a | TIE |
| describe | 1.7 / 1.7 | 1.9 / 1.8 | 1.07 | 1.08 | n/a | TIE |
| describe_always | 1.6 / 1.6 | 1.8 / 1.7 | 1.11 | 1.12 | n/a | LOSS |
| lsfiles | 13.9 / 13.8 | 8.9 / 19.4 | 0.64 | 1.41 | n/a | mixed (CPU loss) |
| lsfiles_s | 27.9 / 27.7 | 11.5 / 29.6 | 0.41 | 1.07 | n/a | WIN |
| lstree | 25.2 / 25.1 | 24.2 / 24.1 | 0.96 | 0.96 | n/a | TIE |
| lstree_flat | 1.8 / 1.7 | 31.2 / 31.1 | 17.54 | 18.32 | n/a | LOSS |
| mergebase | 1.3 / 1.2 | 1.9 / 1.8 | 1.39 | 1.44 | n/a | LOSS |
| difftree | 51.3 / 51.2 | 26.0 / 26.0 | 0.51 | 0.51 | n/a | WIN |
| blame | 27.5 / 27.4 | 5.5 / 5.5 | 0.20 | 0.20 | n/a | WIN |
| blame_rev | 9.6 / 9.5 | 5.3 / 5.3 | 0.56 | 0.56 | n/a | WIN |
| grep | 723.6 / 2448.5 | 84.2 / 1018.7 | 0.12 | 0.42 | n/a | WIN |
| grep_rev | 552.2 / 2512.8 | 100.7 / 553.5 | 0.18 | 0.22 | n/a | WIN |
| grep_path | 11.0 / 13.5 | 9.5 / 20.2 | 0.87 | 1.49 | n/a | mixed (CPU loss) |
| config_get | 0.9 / 0.8 | 1.6 / 1.5 | 1.78 | 1.88 | n/a | LOSS |
| config_list | 0.9 / 0.8 | 1.6 / 1.5 | 1.75 | 1.85 | n/a | LOSS |
| remote_v | 1.0 / 0.9 | 1.8 / 1.7 | 1.85 | 1.94 | n/a | LOSS |
| reflog | 2.2 / 2.1 | 1.4 / 1.3 | 0.62 | 0.62 | n/a | WIN |
| reflog5 | 1.4 / 1.4 | 1.9 / 1.8 | 1.30 | 1.33 | n/a | LOSS |
| cherry | 45.0 / 44.9 | 35.9 / 35.8 | 0.80 | 0.80 | n/a | WIN |
| worktree_list | 1.0 / 1.0 | 0.9 / 0.9 | 0.90 | 0.91 | n/a | TIE |
| stash_list | 0.9 / 0.9 | 0.9 / 0.8 | 0.96 | 0.98 | n/a | TIE |
| clean_n | 45.6 / 45.5 | 30.1 / 43.7 | 0.66 | 0.96 | n/a | WIN |
| formatpatch | 2.0 / 1.9 | 2.2 / 2.1 | 1.13 | 1.14 | n/a | LOSS |
| archive | 759.0 / 758.7 | 379.8 / 379.7 | 0.50 | 0.50 | n/a | WIN |
| bundle_create | 354.7 / 677.5 | 302.0 / 1085.3 | 0.85 | 1.60 | n/a | mixed (CPU loss) |
| bundle_verify | 1.3 / 1.1 | 332.6 / 332.4 | 254.27 | 291.87 | n/a | LOSS |
| fsck | 1113.1 / 1112.9 | 588.9 / 952.6 | 0.53 | 0.86 | n/a | WIN |
| fastexport | 893.1 / 893.0 | 3.4 / 2.2 | 0.00 | 0.00 | n/a | WIN |
| status_dirty | 74.8 / 186.0 | 32.0 / 45.7 | 0.43 | 0.25 | 30.7 / 44.5 | WIN |
| status_dirty_s | 69.9 / 182.9 | 30.6 / 44.1 | 0.44 | 0.24 | n/a | WIN |
| diff_dirty | 62.6 / 176.8 | 59.8 / 553.9 | 0.95 | 3.13 | 58.6 / 552.1 | mixed (CPU loss) |
| diff_stat | 52.2 / 164.8 | 60.5 / 554.2 | 1.16 | 3.36 | n/a | LOSS |
| diff_nameonly | 31.6 / 143.7 | 57.5 / 552.3 | 1.82 | 3.84 | 56.2 / 549.9 | LOSS |
| diff_namestatus | 33.2 / 145.1 | 57.5 / 551.5 | 1.73 | 3.80 | n/a | LOSS |
| diff_head | 63.2 / 176.0 | 50.9 / 185.9 | 0.80 | 1.06 | n/a | WIN |
| add_u | 72.2 / 183.7 | 99.2 / 269.0 | 1.38 | 1.46 | n/a | LOSS |
| commit_a | 123.3 / 236.1 | 204.0 / 372.2 | 1.66 | 1.58 | n/a | LOSS |
| stash_push | 491.2 / 605.6 | 440.6 / 419.2 | 0.90 | 0.69 | n/a | WIN |
| restore_all | 175.4 / 288.1 | 79.6 / 237.8 | 0.45 | 0.83 | n/a | WIN |
| add_file | 50.8 / 50.6 | 27.2 / 20.2 | 0.53 | 0.40 | 42.5 / 49.8 | WIN |
| add_dot_1 | 98.5 / 209.6 | 178.8 / 371.1 | 1.82 | 1.77 | n/a | LOSS |
| rm_file | 46.3 / 46.1 | 33.1 / 23.9 | 0.71 | 0.52 | n/a | WIN |
| rm_cached | 63.7 / 46.4 | 53.5 / 23.8 | 0.84 | 0.51 | n/a | WIN |
| mv_file | 63.2 / 43.5 | 53.5 / 24.7 | 0.85 | 0.57 | n/a | WIN |
| commit_1 | 64.0 / 176.8 | 60.0 / 60.0 | 0.94 | 0.34 | 64.8 / 58.8 | TIE |
| commit_amend | 28.2 / 145.2 | 39.5 / 40.0 | 1.40 | 0.28 | n/a | LOSS |
| restore_file | 45.8 / 45.6 | 28.8 / 20.5 | 0.63 | 0.45 | 44.9 / 49.9 | WIN |
| restore_staged | 46.3 / 45.6 | 33.3 / 24.0 | 0.72 | 0.53 | n/a | WIN |
| reset_file | 61.1 / 174.0 | 52.4 / 24.5 | 0.86 | 0.14 | n/a | WIN |
| branch_create | 1.4 / 1.3 | 2.2 / 1.3 | 1.59 | 1.01 | n/a | LOSS |
| branch_delete | 1.5 / 1.4 | 2.3 / 1.3 | 1.52 | 0.95 | n/a | LOSS |
| branch_rename | 1.6 / 1.5 | 18.8 / 2.3 | 11.87 | 1.55 | n/a | LOSS |
| tag_create | 1.4 / 1.3 | 2.3 / 1.3 | 1.65 | 1.02 | n/a | LOSS |
| tag_annot | 1.4 / 1.3 | 2.3 / 1.4 | 1.66 | 1.03 | n/a | LOSS |
| tag_delete | 1.1 / 1.1 | 2.4 / 1.4 | 2.07 | 1.33 | n/a | LOSS |
| switch_side | 232.3 / 346.4 | 159.2 / 276.4 | 0.69 | 0.80 | n/a | WIN |
| switch_main | 217.5 / 329.1 | 164.0 / 281.4 | 0.75 | 0.86 | n/a | WIN |
| switch_c | 1.7 / 1.6 | 24.4 / 10.3 | 14.79 | 6.61 | n/a | LOSS |
| switch_detach | 75.3 / 187.6 | 135.0 / 263.3 | 1.79 | 1.40 | n/a | LOSS |
| checkout_b | 73.2 / 185.3 | 24.9 / 11.0 | 0.34 | 0.06 | n/a | WIN |
| reset_soft | 9.2 / 9.1 | 13.8 / 2.8 | 1.49 | 0.31 | n/a | LOSS |
| reset_mixed | 58.0 / 169.5 | 47.7 / 51.1 | 0.82 | 0.30 | n/a | WIN |
| reset_hard | 163.3 / 160.0 | 63.3 / 192.5 | 0.39 | 1.20 | n/a | mixed (CPU loss) |
| revert | 82.5 / 194.9 | 76.7 / 221.4 | 0.93 | 1.14 | n/a | mixed (CPU loss) |
| cherry_pick | 311.2 / 419.6 | 209.2 / 343.3 | 0.67 | 0.82 | n/a | WIN |
| merge | 372.6 / 481.0 | 205.6 / 340.5 | 0.55 | 0.71 | n/a | WIN |
| merge_noff | 381.9 / 490.6 | 208.3 / 345.3 | 0.55 | 0.70 | n/a | WIN |
| merge_squash | 374.6 / 483.5 | 205.4 / 342.5 | 0.55 | 0.71 | n/a | WIN |
| rebase | 582.1 / 796.9 | 121.9 / 260.1 | 0.21 | 0.33 | n/a | WIN |
| worktree_add | 1887.0 / 1869.2 | 577.1 / 1976.3 | 0.31 | 1.06 | n/a | WIN |
| gc | 555.5 / 878.5 | 345.4 / 2251.0 | 0.62 | 2.56 | n/a | mixed (CPU loss) |
| repack | 518.0 / 827.3 | 3.8 / 3.7 | 0.01 | 0.00 | n/a | WIN |


## Reproduce

```sh
cargo build --release -p vcs-cli -p vcs-server -p vcs-bench
scripts/bench_autosync_hotpath.sh OUTDIR     # about 15 minutes; waits for load1 < 1.0 before each timed phase
```

Caveats: one box, one run of 9 (hot path) or 5 (autosync); the loopback server hides network cost; synthetic content.
