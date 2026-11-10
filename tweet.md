# Launch thread

Media for every tweet is in `docs/media/thread/` (numbered by tweet; mp4 for X, gif where noted). Every number is from the README and its linked raw data: one machine, one build, small run counts.

**1/**
I built my own version control system from scratch.
It beats git, perforce, sapling, and @tobi's walgit on performance. 136 wins, 68 ties, 14 losses.

Built for many writers and agents. All Rust, no git underneath.

1M files, add: 18.9s vs git's 215s

github.com/Bobliuuu/allgit 🧵

📎 01-demo.gif

**2/ The problem**
One big repo, a hundred people, a pile of AI agents, all pushing at once.

Most version control assumes everyone wants the whole repo and takes turns. vcs assumes neither.

Four ideas: zones, an S3 bucket as the whole backend, group commit, no data loss.

**3/ Zones**
A zone is a slice of the repo with its own branches and history.

`vcs clone --zone engine` and `assets/` never touches your disk.

You or your agent pick zones (`vcs zone describe --json`). A 50-zone repo cloned by usage profile moved 37 MB, not 189.5 MB.

📎 03-zones.mp4

**4/ S3 is the database**
No lock server, no database. Objects upload first, then one conditional write on a single manifest object moves every zone head at once. A cross-zone publish lands all heads or none.

The protocol is checked by an executable model that explores crashes and interleavings.

📎 04-atomic-publish.mp4

**5/ Many writers**
Every publish contends on one manifest key. Group commit merges disjoint publishes into one write.

Real S3, 256 to 1,024 writers: 157 to 348 pushes/s, p99 1.3 to 7.4s. Without batching it capped at 19 to 22.

128 pushers on S3-compatible storage: 42 pushes/s vs walgit's 15.

📎 05-many-writers.png (+ 05b-group-commit.mp4)

**6/ Agents on one branch**
32 pushers, same branch: only vcs landed every commit (with `publish --auto-merge`). git landed 60 to 75 of 160. walgit landed 35.

Honest loss: 128 pushers on separate branches into a local directory, git is faster (222 vs 85 pushes/s). vcs fsyncs every acknowledged object.

📎 06-highload.png

**7/ It does not lose data**
Server `kill -9`ed 157 times in 14 minutes while writers pushed: all 245 acknowledged files present, `fsck` clean.

Then 64 to 256 writers on a 64.7 GB, 200k-commit monorepo, server killed 8 times per run: 5,038 to 7,223 acked files per run, none missing.

📎 07-soak-chaos.png

**8/ Big files, fast status**
No LFS needed. A 4 MiB edit to a 4 GiB file pushed in 0.03s vs git's 126s, and the remote was 4.0 GiB vs 8.0 GiB.

Clean `status` on 100k files, fair run: 2.8 ms with the watcher vs 38.5 ms for git's fsmonitor hook.

📎 08-huge-scale.png

**8b/ Every metric**
Every command I measured, against git, Perforce, walgit and Jujutsu. Green = win, amber = tie, red = loss (5% or worse in wall and CPU), grey = not measured.

136 wins, 68 ties, 14 losses across all five tools. Raw tables: github.com/Bobliuuu/allgit/blob/main/docs/research/command-scoreboard.md

📎 08b-scoreboard-git.png, 08b-scoreboard-perforce.png, 08b-scoreboard-walgit.png, 08b-scoreboard-jujutsu.png

**8c/ Sapling**
Same scoreboard against Sapling (flat 100k files): 15 wins, 1 tie, 0 losses in the cells measured.

Every row is one machine, one build, interleaved runs. Ties are inside the noise by rule.

📎 08c-scoreboard-sapling.png

**9/ The rest**
`vcs op undo` reverts any local operation. Conflicts are saved state you can list and resolve. Change ids survive rebase. Lease locks for unmergeable files.

Coming from git? `vcs git commit -am "fix"` runs the native command; `vcs import-git` brings history in.

**10/ The losses, and the link**
Not uniformly faster. The README lists all 14 losses: tiny metadata commands (`branch`, `tag`), no-op `add` at 1M files, one-file `submit` vs Perforce, `blame` on 10k revisions.

Early research software, tested to 64.7 GB and 1,024 writers.

github.com/Bobliuuu/allgit

📎 10-scoreboard.png or 10-explainer.mp4
