# Resident command daemon (issue #510): built, measured, not merged

Result: a per-repo resident daemon does **not** beat the in-process path on the commands it was meant to speed up, so
it is not in `main`. The working implementation is kept on branch `perf/resident-daemon-impl`
(`crates/vcs-cli/src/daemon.rs`, `crates/vcs-cli/tests/daemon.rs`).

## What was built
`vcs daemon start|stop|status` (opt-in; `VCS_DAEMON=1` auto-starts, `VCS_DAEMON=0` disables). A thin client at the top of
`main` connects to `.vcs/daemon/sock` (dir 0700, socket 0600, peer-uid check), passes its stdin/stdout/stderr with
`SCM_RIGHTS` plus cwd, umask, env and argv. The daemon `fork`s per request and the child runs the same `real_main` the
plain binary runs, so output is byte-identical by construction and the daemon keeps no semantic state: status freshness
(fsmonitor journal plus stat validation), the `flock` repo locks and crash safety are exactly the in-process ones, and the
idle daemon holds no repo lock. Falls back in-process when no daemon, stale socket, wrong build id, or unsupported argv
(allowlist: status diff log show branch add rm mv restore ls-files cat-file commit -m, p4 opened/revert/add/edit/delete).
Auto-exits after `VCS_DAEMON_IDLE` seconds (default 300). Tests (5): differential output over a command corpus,
external-edit freshness, 8 concurrent clients, kill -9 of the daemon, 0600/0700 modes, idle exit, version-mismatch
fallback, unsupported-command fallback.

One bug found by stress (10% of fast commands printed twice): macOS `setsockopt(SO_RCVTIMEO)` fails with EINVAL once the
peer has closed, and the client treated that as "daemon unavailable" and re-ran the command in-process after the
daemon had already run it. After the daemon acknowledges a request the client must never fall back.

## Why it does not win (macOS arm64, 100k-file flat repo, shared host, load average 38 to 70)
After AGI5 (mmap index, O(touched) lookups) the index work for a one-file command is already small; what remains is the
process floor (about 2.8 ms) which a fork-per-request daemon replaces with fork + fd passing + SIGCHLD round trip.
Keeping a decoded index warm would need a long-lived state that must be re-validated against the filesystem on every
request, which is what the mapped index already does for free.

Interleaved, 41 runs each, wall ms, min / median (daemon vs in-process, same binary):

| command | in-process | daemon |
|---|---|---|
| `branch` | 3.4 / 4.6 | 4.1 / 5.8 |
| `diff <one file>` | 3.4 / 4.6 | 4.5 / 5.6 |
| `log -n 1` | 6.0 / 18.3 | 10.2 / 26.9 |
| `status` clean | 30.9 / 56.6 | 34.9 / 62.9 |
| `diff` clean | 23.2 / 44.3 | 17.1 / 43.0 |

Mutating ops (9 interleaved runs, median wall ms, in-process / daemon / git): add 64 / 43 / 187, rm 142 / 86 / 284,
mv 91 / 85 / 166, revert (`restore`) 81 / 79 / 197, status dirty 132 / 132 / 550, `p4 opened` 78 / 110 / 496. The
add/rm gaps are inside the noise of a loaded host (the same bench had `log -n 1` at 31 ms in-process against 6 ms min), so
no win is claimed. vcs (either path) is already 2 to 4x faster than git on these ops at 100k files; `git log -n 1` is
7 ms min.

## What would change the answer
Skipping process start only matters when it is a large share of the command. A daemon that serves the command **inside
the daemon process** (no fork; shared decoded state; own stdout capture) could reach roughly 0.3 ms per request, but then
needs per-request output capture, cooperative locking across threads and a staleness proof for cached status; that is the
risk the issue warned about and is not justified by a 2 to 4 ms saving. Revisit only if a workload runs thousands of
tiny commands per second (an IDE or agent loop); for that, a library API is the better answer than a socket.
