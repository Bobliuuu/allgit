# Launch thread (v0.1.0)

Draft for X. Every number is from the README or the linked docs: one machine, one build, small run counts.
The explainer video (`docs/media/explainer.mp4`) attaches to tweet 9. The AI zone proposal feature
(`zone suggest --assist`) is not mentioned until it is merged.

**1/**
Monorepos are huge. You almost never need all of one.

I built vcs from scratch in Rust, with no git, libgit2 or Perforce underneath. Its idea: zones are malleable slices of the repo. You or your agent decide the boundaries, and you pull only the slices you need.

v0.1.0 is out. Numbers below, losses included 🧵

**2/ Pull less**
A 50-zone repo cloned by usage profile moved 37 MB instead of 189.5 MB for a full clone.

`vcs clone --zone engine` and `assets/` never touches your disk. Not downloaded, not deleted, just not there.

Boundaries can come from you, `vcs zone suggest`, or an agent reading `vcs zone describe --json`.

**3/ A lot of data**
One synthetic monorepo: 1,000,000 files, 200k commits, 64.7 GB of big binaries.

On 1M files (one c6i.4xlarge, one run per cell):
- clone: 40.5 s vs git 603 s
- add: 18.9 s vs 215 s
- diff of a 1% change: 3.7 s vs 19.7 s
- 4 MiB edit to a 4 GiB file, pushed: 0.03 s vs 126 s

The clone comparison is not like for like. The README says so.

**4/ A lot of concurrent writers**
Every publish contends on one manifest key. On real S3 the old path capped at 19 to 22 pushes/s at any of 64 to 1024 writers.

Batch commit (one conditional PUT carries every queued write) gives 157 to 348 pushes/s at 256 to 1024 writers, p99 1.3 to 7.4 s.

At 512 writers on one manifest, group commit takes publishes/s from 27 to 70 and p95 from 37 s to 8.5 s.

**5/ A lot of conflicts**
32 pushers on the same branch: only vcs landed every commit (with `publish --auto-merge`). git landed 60 to 75 of 160. walgit landed 35.

A per-branch ticket queue takes hot-branch p99 from 10 to 23 s to 0.2 to 1.1 s at 64 to 256 clients.

One hot branch is still a retry storm on real S3. Use zones or branches per writer.

**6/ Against the others**
Scoreboard: 218 measured cells vs git, Perforce, walgit, Jujutsu and Sapling. 136 wins, 68 ties, 14 losses. A win needs both wall time and CPU at least 5% lower.

- git (flat 100k files): 64 W / 59 T / 8 L. 100k-file `add` is 733 ms vs 5,383 ms.
- Perforce (p4d on loopback): 17 W / 7 T / 6 L. p4 is faster on one-file `submit`, `add`, `revert` and full-tree `sync`.
- walgit (remote workflows): 24 W / 0 T / 0 L. Initial push is 788 ms vs 8,398 ms, and a no-op fetch is 5 ms vs 19 ms.

Jujutsu and Sapling: 16 W / 1 T / 0 L and 15 W / 1 T / 0 L, flat layout only.

**7/ AI for the rest**
`vcs merge --assist` proposes a resolution. It never applies one without `vcs resolve --proposal`.

On 77 real conflicts from flask, requests and gin, scored against the human merge:
- sonnet: 49% exact (blind pick-a-side: 23 to 26%), p50 7.1 s, about $0.0135 per conflict
- haiku: 36% exact, about $0.0009, refuses 25 of 77

Small sample, strict byte-for-byte scoring.

**8/ Honest caveats**
Memory is where it loses:
- 1M-file add, publish and clone peak at 1.0 to 2.2 GiB RSS
- `fsck` needs 12.6 GiB at 200k commits and 35.1 GiB at 1M (git: 0.8 GiB)
- a no-op `add` on 1M files is 6.8 s vs git's 0.9 s
- on local disk git is faster in 6 of 9 push cells (git 222/s vs vcs 85/s at 128 pushers)
- vs walgit, vcs is worse on object-store request counts, client store size (1.5x) and server memory
- `blame` on a 10k-revision file didn't finish in 1,131 s (git: 3.7 s)

14 losses out of 218 scoreboard cells, all listed.

**9/ Durability and close**
The server was `kill -9`ed 157 times in 14 min, and again 8 times per run under 256 writers. No acknowledged file was lost, and `fsck` was clean.

Early research software, with no on-disk format promise yet.

github.com/Bobliuuu/allgit
[attach docs/media/explainer.mp4]
