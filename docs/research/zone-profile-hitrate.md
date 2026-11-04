# Zone predictor hit rate on real histories

Feature: [../zone-profile.md](../zone-profile.md). Harness: `crates/vcs-repo/examples/zone_hitrate.rs`, driver `scripts/zone_hitrate.sh`.
Raw output: [data/zoneprofile-hitrate.md](data/zoneprofile-hitrate.md). Issue #465 (the earlier [simulation](zone-profile-bench.md) used an access model written for the benchmark).

## Question and method

If a person's own earlier commits were their usage profile, how many of the zones they touch later would `sync --auto` have chosen
with `k` slots? Everything is taken from `git log --name-only` (no merges): who committed, when, which paths. Nothing is simulated.

* **Zones**: the first path component, or the first two under a workspace-style directory (`crates/x`, `packages/x`, `src/x`, `cmd/x`, ...).
  Top-level files are the root zone, which is always kept and not scored. A real zone layout may differ.
* **A person** is an author email. Their profile is one *edit* event per zone per commit at the commit time (reads and builds cannot be seen in a history).
  A person is scored once they have more than 10 commits, from their 11th commit on, and only if they have at least 5 scored buckets.
* **A scored bucket** is a day (the repository itself: 2 hours, because its history was written in 5 calendar days) in which that person commits.
  Truth = zones touched in that bucket. Hit rate = share of the truth that was among the `k` chosen zones, averaged over buckets
  (the "per bucket" column; averaging per person instead changes little, see the raw file, which also has p10..p90 per person).
* **Predictors**: random; recency (most recently edited); frequency (most edits); *ours* = `zoneprofile::rank` with its defaults (14-day half-life,
  floor of 0.05 of the busiest zone, **no dependency edges** because manifests are not in a log); *ours + co-change* = the same plus the co-change term mined
  from everybody's earlier commits (refreshed weekly, 1 hour for the repository itself, never future data); the last column removes the floor and fills the `k` slots.
* `k` = 6 and 3. The product default is 8 new zones.

## Results (hit rate, percent, per scored bucket)

k = 6

| repo | commits | zones | people | buckets | random | recency | frequency | ours | ours + co-change | ours + co-change, no floor |
|---|---|---|---|---|---|---|---|---|---|---|
| allgit (this repo) | 374 | 22 | 1 | 23 | 28.5 | 67.1 | 66.3 | 73.4 | 73.4 | 73.4 |
| clap | 5841 | 53 | 15 | 1163 | 11.9 | 76.4 | 68.8 | 78.7 | 78.4 | 78.6 |
| gin | 517 | 34 | 4 | 142 | 15.0 | 78.6 | 74.9 | 71.4 | 73.5 | 77.5 |
| nest | 9332 | 60 | 20 | 2489 | 10.3 | 85.2 | 84.8 | 81.8 | 81.9 | 84.6 |
| typeorm | 4516 | 51 | 20 | 888 | 11.9 | 60.6 | 63.0 | 62.7 | 62.5 | 62.9 |
| requests | 3547 | 13 | 11 | 713 | 45.5 | 98.5 | 98.5 | 92.7 | 93.2 | 98.8 |
| cobra | 300 | 6 | 3 | 35 | 100.0 | 98.6 | 98.6 | 92.9 | 92.9 | 98.6 |

k = 3

| repo | random | recency | frequency | ours | ours + co-change | ours + co-change, no floor |
|---|---|---|---|---|---|---|
| allgit (this repo) | 15.9 | 44.6 | 44.0 | 44.0 | 44.0 | 44.0 |
| clap | 5.8 | 54.0 | 46.7 | 57.6 | 57.3 | 57.4 |
| gin | 8.9 | 64.5 | 63.9 | 64.3 | 65.7 | 67.6 |
| nest | 4.8 | 70.8 | 72.8 | 72.2 | 71.0 | 71.9 |
| typeorm | 5.6 | 40.9 | 46.6 | 47.0 | 46.5 | 46.7 |
| requests | 22.7 | 95.1 | 93.1 | 90.5 | 91.2 | 95.0 |
| cobra | 44.3 | 98.6 | 97.1 | 92.9 | 92.9 | 98.6 |

(cobra has 6 zones, so random at k = 6 is trivially 100 percent; its row says nothing.)

## Reading (tie rule: within about 10 percent, or overlapping ranges, is a tie)

* A profile built from one's own history is a strong predictor: 62 to 93 percent at k = 6 against 10 to 45 percent for random (clear, 2 to 8 times).
* **Ours is a tie with recency and frequency** in every repository. It is behind by 4 to 6 points on requests and cobra and 7 on gin (k = 6) because of the
  floor: it stops before filling the slots when older zones have decayed below 0.05 of the busiest one. The "no floor" column recovers them (requests
  98.8, cobra 98.6, gin 77.5, nest 84.6). The floor is a bandwidth choice (do not download zones with no evidence), not a free lunch; it costs hit rate on
  small, tightly-knit repositories and costs nothing on large ones (clap, typeorm).
* **Co-change is a tie with ours** (largest gain +2.1 points on gin, -1.2 on nest at k = 3). On these histories it neither helps nor hurts measurably.
  Its value, if any, would be in a fresh clone with hints only, which this harness does not model (every person has a profile).
* The decay and dependency terms did not show a gain over plain frequency here, as in the simulation. Dependency edges were not part of this measurement.

## Limits

* Commits are a proxy for use: a person reads and builds many zones they never commit to, and that is what the real profile records. This measures the edit share.
* The people are the maintainers of public libraries (and one repository written by one account and agents), most with a few home areas. A company monorepo with
  hundreds of zones and people who rotate teams was not measured.
* Zone definitions are a path heuristic. `--depth 1|2` changes them; only `auto` was run here.
* One run per number, no confidence intervals; the per-person spread (p10..p90) in the raw file is wide (for example clap 68 to 100 percent).
* Real-user data from `.vcs/profile` of people using `--auto` does not exist yet; the harness reads git, not that file.

Reproduce: `scripts/zone_hitrate.sh /tmp/replay 6` (clones the six public repositories as blobless bare clones, commits and trees only).
