# vcs vs Jujutsu and Sapling, flat 100k (final run)

Build `845610a`, git version 2.43.0, jj 0.46.0-7d382314f9420b4b8d932245b49c99492c812b9f, Sapling 0.2.20261028-123539+7415fb2f, Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz, 16 vCPU, 2026-11-04T06:35:10Z, runs per cell 9 (interleaved), each cell gated on load1 < 1.0. Verdict rule as in command-scoreboard.md (5% band, wall gap above MAD noise, ratios wall/cpu).

| tool | WIN | TIE | LOSS |
|---|---|---|---|
| jj | 16 | 1 | 0 |
| sl | 15 | 1 | 0 |

## Losses

| cell | vs | wall ratio | cpu ratio | ref ms | vcs ms |
|---|---|---|---|---|---|

## All cells (median wall ms)

| cell | vcs | jj | verdict vs jj | sl | verdict vs sl | n |
|---|---|---|---|---|---|---|
| init (empty dir) | 14.1 | 30.7 | WIN (0.46/0.28) | 327.0 | WIN (0.04/0.01) | 9 |
| add/track (all files) | 715.6 | 113427.2 | WIN (0.01/0.02) | 6497.9 | WIN (0.11/0.30) | 9 |
| commit (all files) | 74.1 | 79.5 | WIN (0.93/0.17) | 13087.4 | WIN (0.01/0.01) | 9 |
| status (clean) | 41.9 | 49.8 | WIN (0.84/0.46) | 191.1 | WIN (0.22/0.34) | 9 |
| status (1% modified) | 41.8 | 487.3 | WIN (0.09/0.26) | 188.2 | WIN (0.22/0.36) | 9 |
| diff (1% modified) | 29.6 | 535.9 | WIN (0.06/0.15) | 723.6 | WIN (0.04/0.11) | 9 |
| log (50 commits) | 3.7 | 52.4 | WIN (0.07/0.01) | 231.5 | WIN (0.02/0.02) | 9 |
| blame/annotate (1 file) | 8.7 | 65.7 | WIN (0.13/0.02) | 213.1 | WIN (0.04/0.04) | 9 |
| branch/bookmark (create) | 2.6 | 78.7 | WIN (0.03/0.01) | 217.9 | WIN (0.01/0.01) | 9 |
| checkout/switch (1% differ) | 425.8 | 235.4 | TIE (1.81/1.00) | 402.2 | TIE (1.06/0.70) | 9 |
| undo (last commit) | 60.0 | 70.6 | WIN (0.85/0.44) | 550.3 | WIN (0.11/0.15) | 9 |
| merge (disjoint files) | 78.2 | 236.9 | WIN (0.33/0.39) | 961.2 | WIN (0.08/0.11) | 9 |
| rebase (1 commit onto main) | 33.0 | 235.5 | WIN (0.14/0.30) | 766.2 | WIN (0.04/0.09) | 9 |
| push (1 new commit) | 11.8 | 117.8 | WIN (0.10/0.01) | 256.3 | WIN (0.05/0.02) | 9 |
| clone (local remote, full history) | 1493.0 | 4962.4 | WIN (0.30/0.57) | 3167.0 | WIN (0.47/0.55) | 9 |
| fetch/pull (1 new commit) | 22.8 | 117.1 | WIN (0.20/0.04) | 270.9 | WIN (0.08/0.07) | 9 |
| gc (local) | 227.2 | 2503.0 | WIN (0.09/0.18) | n/a | N/A | 9 |

Raw data: [data/jj-sl-final/](data/jj-sl-final/). Tables: `python3 scripts/jj_sl_final_summary.py docs/research/data/jj-sl-final/jjsl-flat.json docs/research/jj-sl-final.md`. Charts (`docs/img/scoreboard-jj.svg`, `scoreboard-sapling.svg`): `python3 scripts/jj_sl_scoreboard.py docs/research/data/jj-sl-final docs/img /tmp/tables.md`.
