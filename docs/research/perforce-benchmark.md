# Perforce (Helix Core) benchmark and importer check

Status: **measured, one machine, shared and noisy**. Perforce is client/server: every `p4` command below talks to a
`p4d` over TCP on loopback. `git` and `vcs` are local programs. **These numbers are not an apples-to-apples comparison**;
they show what the same 100k-file datasets cost with Perforce's own workflows. The second half of this document records
what happened when `vcs import-p4` was run against real `p4` output for the first time.

## What was used

| item | value |
|---|---|
| machine | Linux box, AMD EPYC 7J13, 30 cores, 216 GiB RAM, ext4, kernel 6.8.0-1046-nvidia (shared with other jobs) |
| server | `p4d` Rev. P4D/LINUX26X86_64/**2026.1**/3089746 (2026/10/07), from `https://cdist2.perforce.com/perforce/r26.1/bin.linux26x86_64/` |
| client | `p4` Rev. P4/LINUX26X86_64/**2025.2**/3086021 (2026/10/05), from `.../r25.2/bin.linux26x86_64/` |
| location | `~/p4/bin` on the box (own directory; the servers ran with their root in `~/p4/work/...`) |
| server flags | `p4d -r <fresh dir> -J off -L /dev/null -p 127.0.0.1:11666`: **journal off**, no server log, loopback only, no licence file. No checkpoints were taken. Everything else is the default (RCS text storage, `lbr.autocompress` default, security level default). A fresh root is used for every layout; for the "initial submit" cell the server is wiped and restarted before every run |
| user | one user with a password and ticket: p4d 2026.1 refuses all access until the first user has set a password |
| workspace | one client, default view `//depot/... //ws/...`, default options |
| datasets | `datagen flat|deep|binary 100000`, seed 42, the same generator and layouts as `scripts/bench_scale.sh` |
| script | `scripts/bench_p4.sh <layout> 100000 5` (5 runs per cell, median, warm cache, prep untimed, load average sampled before and after each cell) |

### Download and verification

* `https://www.perforce.com/downloads/helix-core-p4d` now redirects to `portal.perforce.com/s/downloads` (a portal
  page); I did not use it. The public binary tree `cdist2.perforce.com/perforce/` is open and was used directly.
* The newest releases do not carry `p4d` in the Linux x86_64 directory listing: `r26.6` has no Linux directory,
  `r26.5`, `r26.4`, `r26.3`, `r26.2` list only add-on tools (`hth-cli`, `p4v`, `swarm`, `p4search`). The newest release with
  `p4d` and `p4` is **r26.1** (listing dated 2026-10-23, binaries 2026-10-07).
* `p4d` r26.1: SHA256 `e2c1e118...1847` equals the published `SHA256SUMS`, and the detached `p4d.asc` verifies against
  Perforce's package-signing key (`E581 31C0 AEA7 B082 C6DC 4C93 7123 CB76 0FF1 8869`, fetched from
  `package.perforce.com/perforce.pubkey`).
* `p4` r26.1 **failed both checks** (downloaded twice: SHA256 `f03c7c33...5383` vs published `bfbb838c...b4fc`;
  `gpg --verify p4.asc` reports BAD signature; the file is dated 2026-09-08 while the listing says 2026-10-07). I did not run
  it. `p4` r25.2 passes both checks (SHA256 `8f3c2688...457e`, good signature), so the 2025.2 client is used with the 2026.1 server.

### Licence

Relied on: the free version of Helix Core (no licence file). The `p4d` binary itself prints: "When using the free
version of the server (no license file) the server is limited to 5 users and 20 workspaces, or unlimited users and
workspaces when the repository has less than 1,000 files". `p4d -V` reports `License: none`. The runs used 1 user and at
most 7 workspaces (1 main plus 5 clone clients, only one at a time per server), and the importer capture 2 users and 12
workspaces. I could not read the licence terms on a Perforce web page (the download page is behind a portal), so the
terms above are the ones the binary states, not a reading of the EULA. The servers were stopped and their roots deleted after the runs.

## Results: Perforce on 100k files (ms, median of 5 runs)

Commands (all run in the workspace root, all with the server on 127.0.0.1):

| workflow | command timed | prep (untimed) |
|---|---|---|
| add | `p4 -q reconcile -a //...` | `p4 revert -k //...` |
| add (explicit list) | `find . -type f | p4 -q -x - add` | `p4 revert -k //...` |
| submit all (initial) | `p4 submit -d c1` (100k files opened for add) | fresh server, fresh client, `reconcile -a` |
| submit 1 file | `p4 submit -d one` | `p4 edit` + rewrite one file |
| status, clean / 1% modified | `p4 reconcile -n //...` (1,000 files modified: every 100th in sorted order, as `bench_scale.sh`) | none |
| log | `p4 changes -m 50` | 57 changelists exist |
| checkout (1% differ) | `p4 -q sync //...@2` | `p4 -q sync //...@1` (changelist 2 changes 1,000 files) |
| clone | `p4 -q sync //...` into a new empty client | create client and directory |

median ms
| workflow | flat | deep | binary |
|---|---|---|---|
| add: reconcile -a | 5347.9 | 20233.2 | 7694.1 |
| add: p4 -x - add | 44427.5 | 47418.1 | 47273.7 |
| submit all (initial) | 67985.4 | 105147.5 | 64296.5 |
| status clean (reconcile -n) | 2302.9 | 5578.4 | 2982.5 |
| status 1% mod (reconcile -n) | 2332.8 | 5495.2 | 3251.7 |
| submit 1 file (edit untimed) | 43.8 | 20.7 | 12.7 |
| log (changes -m 50) | 54.4 | 49.7 | 49.3 |
| checkout (sync @1->@2, 1%) | 719.1 | 653.2 | 405.2 |
| clone (sync into empty ws) | 32203.1 | 37013.2 | 16564.1 |

Load: the box was shared. load1 at the start of each layout run: flat 46.4, deep 87.2, binary 34.2; the per-cell
range is in the raw tables below (about 18 to 87). The first run of most cells is the slowest (cold, e.g. `reconcile -a`
14.9 s then 5.3 s, `reconcile -n` 6.2 s then 2.3 s); medians hide this. Single runs reach 2x the median under load (the
initial submit on flat: 55.6 to 113.9 s). Do not read differences below about 2x as real.

Sanity checks recorded by the script: `reconcile -n` printed 1 line ("no files to reconcile") on the clean tree and
1,000 lines with 1% modified; the clone contained 100,000 files; the history had 57 changelists for the `log` cell.

### Disk used after the initial submit (`du -sk`, KiB)

| layout | p4d root total | depot (archive files) | db.* | p4 workspace | `.git` / `.vcs` (from docs/benchmarks.md, other run) |
|---|---|---|---|---|---|
| flat | 872,944 | 804,032 | 68,852 | 404,032 | 421,656 / 439,100 |
| deep | 1,234,464 | 1,149,524 | 84,880 | 749,524 | 777,884 / 791,608 |
| binary | 1,272,924 | 1,204,016 | 68,848 | 404,032 | 821,668 / 839,236 |

Perforce keeps the server copy and the workspace copy separately (the workspace column is on top of the server
total; files are read-only after submit). The `.git`/`.vcs` stores hold the data once, next to the working tree, so the
comparable figure for Perforce is root total + workspace. The `p4d` root is larger than the git/vcs stores in all three layouts.
`du` counts 4 KiB blocks per small file.

### Reference: git / vcs on the same layouts

Copied from the "100k-file scale run" in [docs/benchmarks.md](../benchmarks.md) (different day, different `vcs` build,
git 2.34.1, also a shared box); not re-run here. Median ms, git / vcs: add . 22,170 / 47,741 (flat), 15,189 / 39,804
(deep), 23,634 / 29,297 (binary); commit 729 / 684, 19,041 / 30,405, 652 / 637; status clean 121 / 558, 1,034 / 1,150, 106
/ 555; status 1% 122 / 570, 1,080 / 1,145, 112 / 671; log (50) 20.1 / 3.8, 34.4 / 4.0, 20.7 / 4.1; checkout 605 / 1,067,
865 / 3,200, 580 / 1,121 (flat, deep, binary).

### What the numbers say, and what they do not

* The Perforce equivalent of "add everything and commit" costs `reconcile -a` + `submit`: 73 s (flat), 125 s (deep),
  72 s (binary), as sums of medians. git `add .` + `commit` is 22.9 / 34.2 / 24.3 s and vcs 48.4 / 70.2 / 29.9 s in the
  referenced run. The submit is where the server copies and stores every file (about 64 to 105 s).
* `reconcile -n` (status) takes 2.3 to 5.6 s; the referenced git status takes 0.1 to 1.1 s and vcs 0.56 to 1.15 s. It
  does not get faster with fewer changes (clean and 1% modified are within noise), because Perforce compares the whole workspace with the server's have list.
* A 1-file submit takes 13 to 44 ms median at 100k files, `p4 changes -m 50` about 50 ms, and a 1% `sync` between two
  changelists 0.4 to 0.7 s (the referenced git checkout is 0.6 to 0.9 s, vcs switch 1.1 to 3.2 s). `changes` is a process
  start plus a loopback round trip; do not compare it with the in-process `log` costs.
* A full `p4 sync` into an empty workspace takes 16.6 to 37.0 s. No local `git clone` / `vcs clone` of the same data was
  measured, so there is no comparison number.
* `p4 -x - add` is about 8x slower than `reconcile -a` on flat (44 s vs 5.3 s) and 2.3x on deep: it sends one request per file.
* Not measured: multiple clients at once, a remote server (network latency), a cold OS cache, `p4 sync --parallel`, `p4 diff`,
  history at 100k changelists, server memory (the `VmHWM` printed by the script is the listener process only, not the per-connection workers).

### Workflows with no real equivalent

| Perforce | git / vcs | note |
|---|---|---|
| `p4 submit` (server-side, atomic, numbered) | `commit` (local) then `push`/`publish` | Perforce has no local commit; the commit and the publish are one step, so "submit 100k" includes work git/vcs do at `add` and at `push` |
| `reconcile -a` / `-n` | `add .` / `status` | the workspace is not tracked locally; Perforce asks the server what the workspace has (the "have" list) |
| `sync //...@N` | `switch` / `checkout` | no branches in the git sense; `sync` moves a workspace to a changelist |
| `changes -m 50` | `log -n 50` | server query, not a local graph walk |
| clone | `clone` | `sync` into a new client; the server must create a client spec first |
| (none) | `branch`, local history, offline work, `stash` | not measurable with Perforce here (shelving and streams are not the same thing and were not tried) |

## Importer against real output

`scripts/p4_capture.sh <dir>` builds a history in a throwaway p4d (two users, two clients, 11 changelists, the last one
pending): add; edit; delete; `move`; branch (`p4 integ` into `//depot/rel`, action `branch`); integrate (action
`integrate`); `+x` file; symlink; random binary; type changes (`text+x` to `text`, `text` to `text+x`); empty file; CRLF text
without trailing newline; `+k` keyword file; a `utf16` file; names with a space, non-ASCII `é`, and `@ # % *`; a deleted path
re-added as a regular file where a symlink was. It then records `p4 -G describe -s N`, `p4 describe -s N`, `p4 -G changes -l`,
`p4 changes -l` and `p4 print -q` for every file revision, and the ground truth: `p4 sync //depot/...@N` into a fresh
client for every N, as a manifest (`mode path sha256 size`, or `L path target`). The capture (240 KiB, 55 files) is
committed in `crates/vcs-repo/tests/fixtures/p4real`.

`crates/vcs-repo/tests/import_p4_real.rs` imports it with `Repo::import_p4` and compares the tree of every commit with
`expected/N.manifest`; it also checks author, e-mail, timestamp, the multi-line description, the ignored pending change, and
that the text-form describes (no file types) give the same paths and timestamps. Result on the Linux box: all 10
submitted changelists match, except for the two discrepancies below (the test skips those two paths explicitly).

| # | discrepancy | cause | status |
|---|---|---|---|
| 1 | depot paths with `@ # % *` stay percent-escaped (`we%40ird%23na%25me%2A.txt`); `p4 sync` writes `we@ird#na%me*.txt` | `describe` output uses p4's escapes; the importer used them as zone paths | fixed in a separate PR (failing test first) |
| 2 | `utf16` file: `p4 print -q` returns UTF-8 text (3 bytes), `p4 sync` writes the UTF-16 file (8 bytes with BOM); the importer stores the print output | print converts for the client charset; the importer only warns about the type | fixed (#302): the importer converts utf16 print output to UTF-16LE with BOM; the real-fixture test no longer skips `u16.txt` |

Format findings that needed no change: `p4 -G` output is Python-marshalled dictionaries with byte-string keys and values
(handled); the change `time` field is epoch seconds (UTC); the text describe header is `Change N by user@client on
YYYY/MM/DD HH:MM:SS` in server time (the capture server ran in UTC, so the text and epoch timestamps agree only for that reason); `p4 print -q`
of a symlink gives the target plus a newline (the importer strips it); `+k` expansion is applied identically by `print` and `sync`, so
the expanded text is what both give; a `*pending*` change appears in `p4 changes -l` and is ignored. Not covered:
unicode-mode servers, large files, streams, obliterated or renumbered history, `apple`/`resource` types, locks, `p4 print` of
files whose type needs special handling.

## Reproduce

```bash
# on a Linux x86_64 box with python3; downloads and checksums as in "Download and verification"
mkdir -p ~/p4/bin && cp p4d p4 ~/p4/bin/
cargo build --release -p vcs-bench          # datagen
scripts/bench_p4.sh flat 100000 5           # also deep, binary; needs about 3 GB free per layout
scripts/p4_capture.sh /tmp/p4real           # then compare with crates/vcs-repo/tests/fixtures/p4real
```

## Raw runs

### flat
| workflow | raw runs (ms) | median | load1 |
|---|---|---|---|
| add: reconcile -a | 14876.1 5547.7 5347.9 5259.4 5260.7 | 5347.9 | 48.06 -> 44.88 |
| add: p4 -x - add | 39940.9 43660.9 82554.0 49794.4 44427.5 | 44427.5 | 44.88 -> 36.24 |
| submit all (initial) | 67985.4 55623.2 113883.0 79782.0 61994.5 | 67985.4 | 36.24 -> 57.31 |
| status clean (reconcile -n) | 6161.5 2281.7 2253.5 2350.4 2302.9 | 2302.9 | 64.37 -> 68.09 |
| status 1% mod (reconcile -n) | 3067.0 2322.0 2332.8 2344.1 2317.1 | 2332.8 | 64.40 -> 58.78 |
| submit 1 file (edit untimed) | 30.1 36.4 48.9 65.0 43.8 | 43.8 | 57.35 -> 59.25 |
| log (changes -m 50) | 50.5 55.7 54.4 52.9 54.7 | 54.4 | 71.61 -> 74.60 |
| checkout (sync @1->@2, 1%) | 1054.0 842.1 719.1 621.8 661.4 | 719.1 | 74.60 -> 77.19 |
| clone (sync into empty ws) | 18031.5 33826.9 47503.5 26141.4 32203.1 | 32203.1 | 77.19 -> 61.62 |
(workspace size: 404032 KiB; files: 100000)
(opened files after add: 100000)
(server root after initial submit: total 872944 KiB = depot 804032 + db.* 68852; workspace 404032 KiB; free 354.17 GiB)
(reconcile -n lines on clean tree: 1)
(reconcile -n lines with 1% modified: 1000)
(changes now: 2)
(changes now: 57)
(cloned files in last clone: 100000)

### deep
| workflow | raw runs (ms) | median | load1 |
|---|---|---|---|
| add: reconcile -a | 91427.1 20233.2 86151.0 12744.9 11467.7 | 20233.2 | 41.30 -> 73.68 |
| add: p4 -x - add | 46551.0 47418.1 59516.4 50731.8 44375.7 | 47418.1 | 73.68 -> 57.14 |
| submit all (initial) | 118838.0 125170.9 67071.6 105147.5 65812.4 | 105147.5 | 57.14 -> 49.32 |
| status clean (reconcile -n) | 10715.3 5560.7 5785.2 5511.0 5578.4 | 5578.4 | 42.34 -> 36.11 |
| status 1% mod (reconcile -n) | 7119.0 5491.9 5528.9 5495.2 5476.8 | 5495.2 | 35.96 -> 32.28 |
| submit 1 file (edit untimed) | 15.9 39.8 20.6 22.3 20.7 | 20.7 | 33.31 -> 32.77 |
| log (changes -m 50) | 49.5 49.4 49.7 50.2 50.4 | 49.7 | 33.44 -> 33.44 |
| checkout (sync @1->@2, 1%) | 743.5 648.4 643.2 653.2 813.6 | 653.2 | 33.44 -> 33.04 |
| clone (sync into empty ws) | 90391.6 37013.2 30821.3 27327.8 38687.8 | 37013.2 | 33.04 -> 44.93 |
(workspace size: 749524 KiB; files: 100000)
(opened files after add: 100000)
(server root after initial submit: total 1234464 KiB = depot 1149524 + db.* 84880; workspace 749524 KiB; free 340.92 GiB)
(reconcile -n lines on clean tree: 1)
(reconcile -n lines with 1% modified: 1000)
(changes now: 2)
(changes now: 57)
(cloned files in last clone: 100000)

### binary
| workflow | raw runs (ms) | median | load1 |
|---|---|---|---|
| add: reconcile -a | 14969.7 7633.0 7600.8 7740.2 7694.1 | 7694.1 | 32.04 -> 32.89 |
| add: p4 -x - add | 47672.0 44769.7 47273.7 45669.0 54230.9 | 47273.7 | 32.89 -> 40.98 |
| submit all (initial) | 114401.3 60184.8 64296.5 83441.3 59163.4 | 64296.5 | 40.98 -> 20.76 |
| status clean (reconcile -n) | 7459.4 2930.3 2982.5 2960.6 3080.1 | 2982.5 | 19.73 -> 18.39 |
| status 1% mod (reconcile -n) | 3472.8 2969.9 3023.3 3768.4 3251.7 | 3251.7 | 19.24 -> 20.81 |
| submit 1 file (edit untimed) | 12.6 12.6 12.7 14.4 15.6 | 12.7 | 21.09 -> 21.09 |
| log (changes -m 50) | 50.1 49.3 50.4 49.3 48.8 | 49.3 | 19.64 -> 19.64 |
| checkout (sync @1->@2, 1%) | 354.0 350.0 405.2 483.6 436.5 | 405.2 | 19.64 -> 21.19 |
| clone (sync into empty ws) | 33652.1 16564.1 8932.8 15381.6 17729.2 | 16564.1 | 21.19 -> 21.59 |
(workspace size: 404032 KiB; files: 100000)
(opened files after add: 100000)
(server root after initial submit: total 1272924 KiB = depot 1204016 + db.* 68848; workspace 404032 KiB; free 330.24 GiB)
(reconcile -n lines on clean tree: 1)
(reconcile -n lines with 1% modified: 1000)
(changes now: 2)
(changes now: 57)
(cloned files in last clone: 100000)


