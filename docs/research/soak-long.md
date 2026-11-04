# Long soak with kill -9 during gc, push and checkout (2 h, EC2 c6i.4xlarge)

Follow-up to the short soak/chaos run on box g (`scripts/soak/`). Scripts changed here:

* **Sampling bug fixed.** `sampler()` was started with the server's original pid. After a `kill -9` + restart the pid changed, so RSS/fd/cpu read 0 or -1 for the rest of the run. It now re-reads the live server pid every tick, resets the cpu baseline on a restart, skips ticks where the process is mid-death, and writes a `pid` column. (A first version of the fix divided by zero right after a restart and killed the thread silently; also fixed.)
* `--chaos mixed`: server `kill -9` (timestamped with the number of client publish/sync/clone processes in flight), `kill -9` of `vcs gc --remote` and `vcs gc --remote --compact` at a random moment (alternating, every third run left to finish), a fresh-clone **audit** (clone + fsck + every file acked before the audit) after every gc kill and every 10 min.
* `--clientkill P`: SIGKILL of a publish/commit/sync client at a random moment (during push).
* `chaos_client.py` (kill during commit / switch(checkout) / publish / clone) looped alongside the soak.
* Harness repairs, each counted and reported, because the product has no recovery for them (see Findings): stale `.vcs/lock`, stale `gc/lock` (6 h lease), stale `.cas.lock`.

## Final run

80 collaborators (one process each, ~14 CLI ops/s sync, ~5 commits/s), one `vcs-server --root` on a 16 vCPU box (load average 50-90, so latencies are oversubscribed), 120 min, 98 server kill -9, 40 gc/compact kill -9 (plus 5 gc runs that completed), ~3% of publish/commit/sync clients killed, remote gc grace 900 s / hold 300 s. Data: `docs/research/soak-long-data/final/`.

| check | result |
| --- | --- |
| acked files (publish returned 0) | 8260 / 8260 present with the right hash |
| fresh clone fsck, clone of the store directory fsck | ok, ok (24041 objects) |
| audits during the run (after each gc kill, periodic) | 51, 0 bad |
| server alive at end | yes |
| lock-cycle "last wins" files | 34 of 50 correct, 15 older or missing: **issue #502, not explained** |

Server memory/fd trend (`final/trend.txt`, per server process between kills, 15 s samples): RSS climbs with the process age (5 -> 35 MB in 75 s, up to 178 MB at ~90 s under 80 clients, max 181 MB) and fds follow connections (max 417, threads max 415). Processes live under two minutes because of the kills, so this is not a leak test: no single process ran long enough to show a slope. The store grew from 8 to 515 packs (max 834) with the manifest at 131 KB; compaction by publishers and the gc thread kept it bounded.

Caveat on gc: with a 900 s grace only 5 sweeps completed and none deleted anything, so "kill during gc" mostly hit sweeps/compactions that were planning or rewriting, not mass deletion.

## Findings

1. **#480 stale `.vcs/lock`** after SIGKILL (also zero-byte lock when killed between create and write). Without the harness repair 152 of 200 clients were wedged within 12 min. 2056 repairs in the final run.
2. **#488 stale `.cas.lock`** in FsBackend: after a kill -9 the server stops accepting any manifest write (503 after 30 s) forever. First run: 15 minutes without a single successful publish until deleted by hand. Final run: 2 occurrences (13.5 s, 13.9 s before the harness removed them).
3. **gc/lock lease ~6 h.** A client killed inside auto-compaction (`vcs-auto-compact pid N`) or a killed gc blocks `gc --remote` and `--compact` for ~6 h; 90 manual clears in the final run.
4. **#502** acked lock-cycle writes reverted or missing in the head (above). Follow-up: not reproduced as a loss. Local repro (24 clients, lock cycles only / full mix / client SIGKILL 10% / server kill -9 every 8-25 s, 6-8 min each, debug build) showed 0 older-or-missing files in ~4000 acked publishes. The one mismatch seen (server kill -9 run) was a head NEWER than the last ack: an unacked lock-cycle commit lands later through a plain publish. The harness now reports `last_wins_newer_than_ack` separately; only older/missing counts as a mismatch. The original 15 older/missing are still unexplained and need the long run re-done with the new check.
   Commit kills (fsck 5/11 problems): reproduced and fixed, see the vcs-store barrier ordering fix: a kill -9 mid `sync` left a commit visible without its root tree because loose objects were renamed in HashMap order.
5. **Unsafe gc parameters lose data, as documented.** An earlier run used `--grace-secs 30 --hold-secs 10` under load average ~270. After a sweep (which rewrote a pack) and a killed compaction, a clone failed permanently with `object ... missing on remote`; the object was referenced from `tx/` records but in no pack. Publishers stalled by 30 s manifest timeouts exceed the 10 s hold the protocol assumes (docs/gc.md: grace must exceed twice the longest publisher hold), so this is a violated precondition, not a protocol bug, and I did not file it. It does show the operational risk: nothing in `gc --remote` checks the publishers' real hold time, and the failure mode is silent until a clone. Evidence: `run3-short-grace/`.
6. `chaos_client.py` aggregated over 16 loops of 30 iterations (`final/client-kill-total.json`): clone killed 469/480 times, always recoverable by cloning into a fresh directory (retry_failed 0). The message `destination ... exists and is not empty` is the product refusing to clone into the partial directory left by the killed clone, not a harness fault; the harness then deletes the directory and re-clones successfully. It is a usability gap (no cleanup or resume of a killed clone), not data loss. `no such branch root/other` (218) is a harness artifact: the scenario setup failed behind a stale lock. Commit kills: 4 of 480 left a repo where `vcs fsck` reported problems ("5 problem(s)", "11 problem(s)") right after the kill; the run did not keep the detail, worth a targeted repro. Publish `tree_mismatch` (37) is dominated by the stale-lock cascade and is not interpretable.

## Cost and box

c6i.4xlarge from 23:13 to 03:06 UTC, about 3.9 h, about US$2.7. Instance stopped (not terminated).

## Reproduce

```
VCS_BIN=target/release python3 scripts/soak/soak.py run --n 80 --minutes 120 --think 3 --ramp 120 \
  --chaos mixed --kill-min 30 --kill-max 120 --gc-first 300 --gc-every 45 --gc-grace 900 \
  --audit-every 600 --clientkill 0.03 --sample 15 --out ~/soak/long
for i in $(seq 0 39); do python3 scripts/soak/chaos_client.py --out ~/soak/ck$i --iters 30; done
```
