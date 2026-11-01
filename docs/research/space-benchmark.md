# Space benchmark: bytes stored by git vs vcs

MEASUREMENT ONLY. vcs optimizes for speed, not storage. This is a sanity check that vcs does not use a crazy
amount of extra space; it is informational and recommends nothing. No timing is claimed. No product code was
changed and no formats were tuned.

## Result in one paragraph

No cell is above 3x git (no FLAG, no RED) in any of the 6 dataset/size combinations. Across all measured cells
the largest vcs/git ratio is 1.85x (a fresh clone, flat 10k, allocated bytes: vcs leaves ~265 loose files that
`du` rounds up to 4 KiB blocks); apparent-byte ratios top out at 1.61x (the same cell). Remote stores are
1.01x-1.16x of git's. Because nothing is flagged, no per-object-kind byte breakdown was produced (the brief
asks for one only on flagged cells).

## Environment

| item | value |
|---|---|
| machine | Lambda box, AMD EPYC 7J13, 30 cores, ext4, Linux 6.8.0-1046-nvidia, shared box (load1 about 8 to 20 during the runs; sizes are load independent) |
| git | git version 2.56.0 |
| vcs | `vcs 0.1.0`, release build of origin/main 6ef5547 (`cargo build --release -p vcs-cli -p vcs-server -p vcs-bench`) |
| datasets | `datagen <layout> <n> <dir> 42`, layouts flat / deep / binary, n = 10000 and 100000 |
| history | 1 initial commit of everything, then 50 commits each appending one line to one file (index `(i*7919+42) % n`), as in `scripts/walgit_bench` |
| harness | `scripts/bench_space.py` (run: `bench_space.py <layout> <n> out.json`; table: `bench_space.py report out.json...`); raw JSON in `docs/research/data/space-bench/` |
| sizes | "apparent" = sum of file `st_size`; "alloc" = sum of `st_blocks*512` over files and directories (like `du`). Cell format: `MiB apparent / MiB alloc (file count)`; ratio = vcs/git, apparent then alloc |

Flag rule: FLAG if vcs > 3x git, RED if > 10x (taken as the larger of the apparent and alloc ratios).

## Configurations

- git: default config (`gc.auto` at its default, so auto-gc may fire during the 51 commits; only `gc.autoDetach=false` so a
  background gc cannot race the measurement). Remote = bare repo pushed over `file://`. Maintenance = `git gc` (client and bare).
- vcs "default": no configuration. Remote = directory remote (`vcs remote set <dir>; vcs publish`) and, separately,
  `vcs-server --root <dir>` published to over http (the server store is byte-identical in size to the directory remote in every cell).
  Maintenance = `vcs repack && vcs gc` (client), `vcs gc --remote --yes`, then `vcs gc --remote --compact --yes` (remote).
  Extra rows: `vcs repack --format v1|v2|v3|v4` on a copy of the client; `vcs gc --remote --compact --yes --pack-format v2` on a copy of the remote.
- vcs "loose": `vcs config set gc.autoPackLimit 0` right after `vcs init` (auto-pack disabled). IMPORTANT caveat: the numbers are
  indistinguishable from "default" (file counts differ by 1), so on these workloads this knob does not visibly change on-disk size.
  I did not verify what the add/commit write path does with loose objects, so do not read this column as "everything loose". It is
  reported only because the brief asked for the loose configuration; I did not test any other "fastest config" flag.
- Clones are made from the directory remote with default config.
- walgit: not re-run (needs RustFS and the walgit server). Stale numbers from `docs/research/walgit-benchmark.md` below.
- Not measured: the pack formats v3 on push (the push path picks its own format; v3 is only the default for remote compaction).

## Tables (this run)

### flat, 10000 files (working tree 5.7 MiB apparent, 39.5 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 5.6 / 6.7 (229) | 7.8 / 9.9 (413) | 1.39x / 1.47x | 7.8 / 9.9 (414) | 1.39x / 1.47x |
| client after maintenance | 5.1 / 5.2 (29) | 7.6 / 8.5 (12) | 1.50x / 1.63x | 7.6 / 8.5 (13) | 1.50x / 1.63x |
| client after `repack --format v2` | 5.1 / 5.2 (29) | 7.6 / 8.5 (12) | 1.50x / 1.63x | 7.6 / 8.5 (13) | 1.50x / 1.62x |
| client after `repack --format v3` | 5.1 / 5.2 (29) | 5.6 / 6.5 (12) | 1.10x / 1.24x | 5.6 / 6.4 (13) | 1.10x / 1.24x |
| client after `repack --format v4` | 5.1 / 5.2 (29) | 5.4 / 6.2 (12) | 1.06x / 1.20x | 5.4 / 6.2 (13) | 1.06x / 1.19x |
| client after `repack --format v1` | 5.1 / 5.2 (29) | 7.8 / 8.6 (12) | 1.53x / 1.66x | 7.8 / 8.6 (13) | 1.53x / 1.66x |
| remote store after push (directory) | 4.3 / 4.4 (22) | 5.0 / 5.0 (4) | 1.16x / 1.14x | 5.0 / 5.0 (4) | 1.16x / 1.14x |
| remote store after push (vcs-server) | 4.3 / 4.4 (22) | 5.0 / 5.0 (4) | 1.16x / 1.14x | 5.0 / 5.0 (4) | 1.16x / 1.14x |
| remote after `gc --remote` | 4.3 / 4.4 (22) | 5.0 / 5.0 (4) | 1.16x / 1.14x | 5.0 / 5.0 (4) | 1.16x / 1.14x |
| remote after `gc --remote --compact` | 4.3 / 4.4 (26) | 5.0 / 5.0 (4) | 1.15x / 1.13x | 5.0 / 5.0 (4) | 1.15x / 1.13x |
| remote after compact `--pack-format v2` | 4.3 / 4.4 (26) | 5.0 / 5.0 (4) | 1.15x / 1.13x | 5.0 / 5.0 (4) | 1.15x / 1.13x |
| fresh clone | 5.1 / 5.2 (28) | 8.1 / 9.6 (265) | 1.61x / 1.85x | 8.1 / 9.6 (265) | 1.61x / 1.85x |

vs working tree (apparent): git client 0.98x, vcs default client 1.36x, vcs loose client 1.36x


### flat, 100000 files (working tree 57.2 MiB apparent, 394.6 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 54.1 / 55.2 (229) | 75.3 / 77.1 (312) | 1.39x / 1.40x | 75.3 / 77.1 (313) | 1.39x / 1.40x |
| client after maintenance | 50.1 / 50.2 (29) | 75.1 / 75.8 (14) | 1.50x / 1.51x | 75.1 / 75.9 (15) | 1.50x / 1.51x |
| client after `repack --format v2` | 50.1 / 50.2 (29) | 75.1 / 75.8 (14) | 1.50x / 1.51x | 75.1 / 75.9 (15) | 1.50x / 1.51x |
| client after `repack --format v3` | 50.1 / 50.2 (29) | 53.8 / 54.5 (14) | 1.07x / 1.08x | 53.8 / 54.5 (15) | 1.07x / 1.09x |
| client after `repack --format v4` | 50.1 / 50.2 (29) | 53.2 / 54.0 (14) | 1.06x / 1.07x | 53.2 / 54.0 (15) | 1.06x / 1.07x |
| client after `repack --format v1` | 50.1 / 50.2 (29) | 75.3 / 76.0 (14) | 1.50x / 1.51x | 75.3 / 76.0 (15) | 1.50x / 1.51x |
| remote store after push (directory) | 42.4 / 42.5 (22) | 48.0 / 48.0 (4) | 1.13x / 1.13x | 48.0 / 48.0 (4) | 1.13x / 1.13x |
| remote store after push (vcs-server) | 42.4 / 42.5 (22) | 48.0 / 48.0 (4) | 1.13x / 1.13x | 48.0 / 48.0 (4) | 1.13x / 1.13x |
| remote after `gc --remote` | 42.4 / 42.5 (22) | 48.0 / 48.0 (4) | 1.13x / 1.13x | 48.0 / 48.0 (4) | 1.13x / 1.13x |
| remote after `gc --remote --compact` | 42.9 / 43.0 (26) | 48.0 / 48.0 (4) | 1.12x / 1.12x | 48.0 / 48.0 (4) | 1.12x / 1.12x |
| remote after compact `--pack-format v2` | 42.9 / 43.0 (26) | 48.0 / 48.0 (4) | 1.12x / 1.12x | 48.0 / 48.0 (4) | 1.12x / 1.12x |
| fresh clone | 50.1 / 50.2 (28) | 79.5 / 79.6 (15) | 1.59x / 1.58x | 79.5 / 79.6 (15) | 1.59x / 1.58x |

vs working tree (apparent): git client 0.95x, vcs default client 1.32x, vcs loose client 1.32x


### deep, 10000 files (working tree 5.7 MiB apparent, 138.5 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 8.8 / 11.8 (579) | 9.8 / 13.2 (662) | 1.12x / 1.11x | 9.8 / 13.2 (663) | 1.12x / 1.11x |
| client after maintenance | 7.8 / 7.9 (29) | 9.8 / 10.8 (14) | 1.25x / 1.36x | 9.8 / 10.7 (15) | 1.25x / 1.35x |
| client after `repack --format v2` | 7.8 / 7.9 (29) | 9.8 / 10.8 (14) | 1.25x / 1.36x | 9.8 / 10.7 (15) | 1.25x / 1.35x |
| client after `repack --format v3` | 7.8 / 7.9 (29) | 8.1 / 9.1 (14) | 1.04x / 1.14x | 8.2 / 9.1 (15) | 1.05x / 1.15x |
| client after `repack --format v4` | 7.8 / 7.9 (29) | 7.9 / 8.8 (14) | 1.01x / 1.11x | 7.9 / 8.8 (15) | 1.01x / 1.11x |
| client after `repack --format v1` | 7.8 / 7.9 (29) | 9.8 / 10.7 (14) | 1.25x / 1.35x | 9.8 / 10.7 (15) | 1.25x / 1.35x |
| remote store after push (directory) | 6.1 / 6.2 (22) | 6.9 / 7.0 (4) | 1.13x / 1.12x | 6.9 / 7.0 (4) | 1.13x / 1.12x |
| remote store after push (vcs-server) | 6.1 / 6.2 (22) | 6.9 / 7.0 (4) | 1.13x / 1.12x | 6.9 / 7.0 (4) | 1.13x / 1.12x |
| remote after `gc --remote` | 6.1 / 6.2 (22) | 6.9 / 7.0 (4) | 1.13x / 1.12x | 6.9 / 7.0 (4) | 1.13x / 1.12x |
| remote after `gc --remote --compact` | 6.3 / 6.4 (26) | 6.9 / 7.0 (4) | 1.10x / 1.09x | 6.9 / 7.0 (4) | 1.10x / 1.09x |
| remote after compact `--pack-format v2` | 6.3 / 6.4 (26) | 6.9 / 7.0 (4) | 1.10x / 1.09x | 6.9 / 7.0 (4) | 1.10x / 1.09x |
| fresh clone | 7.8 / 7.9 (28) | 11.2 / 11.3 (15) | 1.44x / 1.42x | 11.2 / 11.3 (15) | 1.44x / 1.42x |

vs working tree (apparent): git client 1.53x, vcs default client 1.71x, vcs loose client 1.71x


### deep, 100000 files (working tree 57.2 MiB apparent, 732.0 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 66.4 / 69.5 (579) | 82.5 / 85.9 (662) | 1.24x / 1.24x | 82.5 / 85.9 (663) | 1.24x / 1.24x |
| client after maintenance | 61.4 / 61.6 (29) | 82.5 / 83.5 (14) | 1.34x / 1.36x | 82.5 / 83.5 (15) | 1.34x / 1.36x |
| client after `repack --format v2` | 61.4 / 61.6 (29) | 82.5 / 83.5 (14) | 1.34x / 1.36x | 82.5 / 83.5 (15) | 1.34x / 1.36x |
| client after `repack --format v3` | 61.4 / 61.6 (29) | 63.7 / 64.7 (14) | 1.04x / 1.05x | 63.7 / 64.7 (15) | 1.04x / 1.05x |
| client after `repack --format v4` | 61.4 / 61.6 (29) | 63.3 / 64.2 (14) | 1.03x / 1.04x | 63.3 / 64.2 (15) | 1.03x / 1.04x |
| client after `repack --format v1` | 61.4 / 61.6 (29) | 82.4 / 83.3 (14) | 1.34x / 1.35x | 82.4 / 83.3 (15) | 1.34x / 1.35x |
| remote store after push (directory) | 49.2 / 49.3 (22) | 54.5 / 54.5 (4) | 1.11x / 1.11x | 54.5 / 54.5 (4) | 1.11x / 1.11x |
| remote store after push (vcs-server) | 49.2 / 49.3 (22) | 54.5 / 54.5 (4) | 1.11x / 1.11x | 54.5 / 54.5 (4) | 1.11x / 1.11x |
| remote after `gc --remote` | 49.2 / 49.3 (22) | 54.5 / 54.5 (4) | 1.11x / 1.11x | 54.5 / 54.5 (4) | 1.11x / 1.11x |
| remote after `gc --remote --compact` | 50.0 / 50.1 (26) | 54.5 / 54.5 (4) | 1.09x / 1.09x | 54.5 / 54.5 (4) | 1.09x / 1.09x |
| remote after compact `--pack-format v2` | 50.0 / 50.1 (26) | 54.5 / 54.5 (4) | 1.09x / 1.09x | 54.5 / 54.5 (4) | 1.09x / 1.09x |
| fresh clone | 61.4 / 61.6 (28) | 90.1 / 90.2 (15) | 1.47x / 1.47x | 90.1 / 90.2 (15) | 1.47x / 1.47x |

vs working tree (apparent): git client 1.16x, vcs default client 1.44x, vcs loose client 1.44x


### binary, 10000 files (working tree 39.1 MiB apparent, 39.5 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 41.3 / 42.5 (229) | 41.3 / 43.5 (413) | 1.00x / 1.02x | 41.3 / 43.4 (414) | 1.00x / 1.02x |
| client after maintenance | 40.6 / 40.7 (29) | 41.1 / 42.0 (12) | 1.01x / 1.03x | 41.1 / 42.0 (13) | 1.01x / 1.03x |
| client after `repack --format v2` | 40.6 / 40.7 (29) | 41.1 / 42.0 (12) | 1.01x / 1.03x | 41.1 / 42.0 (13) | 1.01x / 1.03x |
| client after `repack --format v3` | 40.6 / 40.7 (29) | 41.4 / 42.3 (12) | 1.02x / 1.04x | 41.4 / 42.2 (13) | 1.02x / 1.04x |
| client after `repack --format v4` | 40.6 / 40.7 (29) | 40.8 / 41.7 (12) | 1.01x / 1.02x | 40.8 / 41.7 (13) | 1.01x / 1.02x |
| client after `repack --format v1` | 40.6 / 40.7 (29) | 41.3 / 42.2 (12) | 1.02x / 1.04x | 41.3 / 42.1 (13) | 1.02x / 1.03x |
| remote store after push (directory) | 39.8 / 39.9 (22) | 40.4 / 40.5 (4) | 1.02x / 1.01x | 40.4 / 40.5 (4) | 1.02x / 1.01x |
| remote store after push (vcs-server) | 39.8 / 39.9 (22) | 40.4 / 40.5 (4) | 1.02x / 1.01x | 40.4 / 40.5 (4) | 1.02x / 1.01x |
| remote after `gc --remote` | 39.8 / 39.9 (22) | 40.4 / 40.5 (4) | 1.02x / 1.01x | 40.4 / 40.5 (4) | 1.02x / 1.01x |
| remote after `gc --remote --compact` | 39.8 / 40.0 (26) | 40.4 / 40.5 (4) | 1.01x / 1.01x | 40.4 / 40.5 (4) | 1.01x / 1.01x |
| remote after compact `--pack-format v2` | 39.8 / 40.0 (26) | 40.4 / 40.5 (4) | 1.01x / 1.01x | 40.4 / 40.5 (4) | 1.01x / 1.01x |
| fresh clone | 40.6 / 40.7 (28) | 41.5 / 43.0 (265) | 1.02x / 1.06x | 41.5 / 43.0 (265) | 1.02x / 1.06x |

vs working tree (apparent): git client 1.06x, vcs default client 1.06x, vcs loose client 1.06x


### binary, 100000 files (working tree 390.6 MiB apparent, 394.6 MiB allocated)

Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown in the row (git maintenance column paired with vcs maintenance).

| cell | git | vcs default | ratio | vcs loose | ratio |
|---|---|---|---|---|---|
| client after 51 commits | 409.3 / 410.4 (229) | 409.0 / 410.8 (312) | 1.00x / 1.00x | 409.0 / 410.8 (313) | 1.00x / 1.00x |
| client after maintenance | 405.3 / 405.4 (29) | 408.7 / 409.5 (14) | 1.01x / 1.01x | 408.7 / 409.5 (15) | 1.01x / 1.01x |
| client after `repack --format v2` | 405.3 / 405.4 (29) | 408.7 / 409.5 (14) | 1.01x / 1.01x | 408.7 / 409.5 (15) | 1.01x / 1.01x |
| client after `repack --format v3` | 405.3 / 405.4 (29) | 410.1 / 410.9 (14) | 1.01x / 1.01x | 410.1 / 410.9 (15) | 1.01x / 1.01x |
| client after `repack --format v4` | 405.3 / 405.4 (29) | 406.1 / 406.9 (14) | 1.00x / 1.00x | 406.1 / 406.9 (15) | 1.00x / 1.00x |
| client after `repack --format v1` | 405.3 / 405.4 (29) | 408.9 / 409.7 (14) | 1.01x / 1.01x | 408.9 / 409.6 (15) | 1.01x / 1.01x |
| remote store after push (directory) | 397.6 / 397.8 (22) | 400.9 / 400.9 (4) | 1.01x / 1.01x | 400.9 / 400.9 (4) | 1.01x / 1.01x |
| remote store after push (vcs-server) | 397.6 / 397.8 (22) | 400.9 / 400.9 (4) | 1.01x / 1.01x | 400.9 / 400.9 (4) | 1.01x / 1.01x |
| remote after `gc --remote` | 397.6 / 397.8 (22) | 400.9 / 400.9 (4) | 1.01x / 1.01x | 400.9 / 400.9 (4) | 1.01x / 1.01x |
| remote after `gc --remote --compact` | 398.1 / 398.2 (26) | 400.9 / 400.9 (4) | 1.01x / 1.01x | 400.9 / 400.9 (4) | 1.01x / 1.01x |
| remote after compact `--pack-format v2` | 398.1 / 398.2 (26) | 400.9 / 400.9 (4) | 1.01x / 1.01x | 400.9 / 400.9 (4) | 1.01x / 1.01x |
| fresh clone | 405.3 / 405.5 (28) | 413.0 / 413.1 (15) | 1.02x / 1.02x | 413.0 / 413.1 (15) | 1.02x / 1.02x |

vs working tree (apparent): git client 1.05x, vcs default client 1.05x, vcs loose client 1.05x



## walgit remote store (STALE: copied from docs/research/walgit-benchmark.md, a different run, du of the S3 store dir per repo after the initial push)

| layout | files | git bare | walgit | vcs-server (older build) |
|---|---|---|---|---|
| binary | 10000 | 39.9 MiB | 39.8 MiB | 40.5 MiB |
| deep | 10000 | 6.2 MiB | 6.0 MiB | 8.9 MiB |
| flat | 10000 | 4.4 MiB | 4.3 MiB | 7.0 MiB |
| binary | 100000 | 397.7 MiB | 397.3 MiB | 400.5 MiB |
| deep | 100000 | 49.3 MiB | 48.6 MiB | 73.7 MiB |
| flat | 100000 | 42.5 MiB | 42.1 MiB | 66.8 MiB |

The vcs column there is an older build served through RustFS (S3 layout); the current-build vcs-server numbers are in the tables above.

## Notes

- The working-tree allocated size is inflated for small files (4 KiB blocks), which is why "alloc" for the stores can be well under
  the working tree's.
- Clone rows: vcs clones leave many loose objects (about 265 files at 10k, 15 at 100k), so the allocated ratio is higher than the apparent one there.
- Reproduce: build release binaries, put `datagen vcs vcs-server git` on PATH, `VCS_COMMIT=$(git rev-parse --short HEAD) python3 scripts/bench_space.py flat 10000 out.json`.
