# Agent benchmark: understanding a repository through vcs vs git

Status: harness implemented and run once (two live runs, below). Small sample, one model: read the caveats before quoting any number.

Question: does a zone-aware tool, plus the [zone ontology](../zone-ontology.md), let an agent answer repository questions in fewer steps
and more correctly than the same agent using plain git? Code: `scripts/agent_bench/`. Raw data: `docs/research/data/agent-bench/`.

## What was measured

* **Fixture.** One scripted history (16 commits, 5 authors, 8 zones plus root, Cargo path dependencies, CODEOWNERS, one deletion,
  several multi-zone commits) is applied twice by `fixture.py`: with the git CLI to a git repo, and with the vcs CLI to a vcs repo
  whose zones are created first. Same files, same commit order, same subjects, authors and timestamps. Ground truth for every task is computed from the
  script itself, never from either tool's output.
* **24 tasks** in 6 categories, each with an exact-match grader (strings case-folded, lists compared as sets, integers):
  path ownership (4), ontology facts: purpose, CODEOWNERS (2), dependencies: direct, transitive, dependents, leaves (6), history: top author, commit count, find the commit
  that introduced a function / changed a constant (4), changes: files of a commit, zones a commit touches, zones of the latest commit, files of a zone between two commits (4),
  safe merge: which zones / do files overlap for two commits (4). `python3 scripts/agent_bench/harness.py --list` prints them.
* **Same agent, same questions, different tool.** The agent gets exactly one tool, `run(command)`, which executes a read-only `git ...` or
  `vcs ...` command (argv, no shell, write commands refused, output truncated at 40,000 bytes, at most 14 calls). The git agent is told what a zone
  means *for this repo* (directories under `crates/`, plus `tools/gen` and `docs`; dependencies = `path =` lines of Cargo.toml), because git has no zone concept; this
  is a generous baseline. The vcs agent is told the ontology commands exist (prompt v1) and, in run 2, also how to get per-zone history (prompt v2).
* **Metrics per episode:** correct (exact match), tool calls, bytes of tool output consumed (tokens proxy), prompt tokens reported by the API (live), wall seconds
  (live: includes model latency and an ssh round trip per call, identically for both tools), full recorded transcript (every command and its output) in the JSONL.

### Modes

* `--mode offline`: scripted **oracle strategies** per task and tool (`tasks.py`), no LLM, deterministic. They are what an efficient agent would
  do (for git: `git grep`, `git log -S`, `git show --name-only`, ...; for vcs: `zone graph`, `zone describe --fields ...`, `log --zone Z -S`). They make the harness testable
  (`scripts/agent_bench/test_bench.py`, 7 tests: all 48 oracle episodes are graded correct, graders reject wrong answers, executor refuses writes, both repos share the history).
* `--mode live`: `gpt-4.1-mini` through the OpenAI API (`urllib`, only api.openai.com is contacted, key from the environment only, never written anywhere), temperature 0.7,
  3 repeats per task and tool. Tools ran on a Linux box (build box E, 16 vCPU) over ssh because there is no Mac vcs build; the key never left the Mac. No Anthropic API was used.

## Results

### Offline oracles (deterministic; box E, 16 vCPU, load1 about 20 from other agents, so timings are indicative only)

| | episodes/tool | correct (git / vcs) | tool calls | tool output bytes | wall s |
|---|---|---|---|---|---|
| **all tasks** | 24 | 100% / 100% | 2.2 / 1.4 | 834 / 5,648 | 0.0 / 0.0 |
| changes | 4 | 100% / 100% | 3.0 / 1.0 | 1,331 / 10,044 | 0.0 / 0.0 |
| dependencies | 6 | 100% / 100% | 2.0 / 1.0 | 635 / 3,395 | 0.0 / 0.0 |
| history | 4 | 100% / 100% | 1.0 / 3.5 | 28 / 1,962 | 0.0 / 0.1 |
| merge | 4 | 100% / 100% | 4.5 / 1.0 | 2,314 / 12,975 | 0.0 / 0.0 |
| ontology | 2 | 100% / 100% | 2.0 / 1.0 | 316 / 846 | 0.0 / 0.0 |
| ownership | 4 | 100% / 100% | 1.0 / 1.0 | 221 / 3,395 | 0.0 / 0.0 |


Both toolsets can answer all 24 tasks correctly (that is what the oracles show). vcs needs fewer calls (1.4 vs 2.2) but its JSON output is about 7x larger in bytes
(5,648 vs 834 per task). Summed tool time over the 24 tasks, 3 passes on a loaded box: git median 0.10 s, vcs median 0.23 s (the vcs commands flatten every zone tree).

### Live, run 1: prompt v1 (vcs told only that `zone graph` / `zone describe` exist)

| | episodes/tool | correct (git / vcs) | tool calls | tool output bytes | wall s | prompt tokens |
|---|---|---|---|---|---|---|
| **all tasks** | 72 | 82% / 72% | 5.2 / 4.9 | 483 / 4,663 | 4.5 / 4.8 | 2,868 / 8,623 |
| changes | 12 | 100% / 8% | 2.6 / 8.2 | 168 / 6,082 | 3.0 / 9.1 | 1,503 / 18,285 |
| dependencies | 18 | 61% / 100% | 9.0 / 1.3 | 891 / 5,088 | 7.1 / 1.7 | 5,368 / 2,964 |
| history | 12 | 100% / 75% | 2.6 / 7.1 | 330 / 5,479 | 2.8 / 6.3 | 1,899 / 14,839 |
| merge | 12 | 50% / 50% | 6.8 / 10.8 | 810 / 2,244 | 5.2 / 8.9 | 3,036 / 10,682 |
| ontology | 6 | 100% / 100% | 4.8 / 1.3 | 230 / 6,296 | 3.8 / 1.7 | 2,044 / 3,600 |
| ownership | 12 | 100% / 100% | 3.6 / 1.0 | 134 / 3,395 | 3.2 / 1.5 | 1,694 / 1,686 |


Per task (git / vcs):

| task | category | correct (of repeats) | mean calls | mean bytes |
|---|---|---|---|---|
| own1 | ownership | 3/3 / 3/3 | 2.0 / 1.0 | 99 / 3,395 |
| own2 | ownership | 3/3 / 3/3 | 1.0 / 1.0 | 20 / 3,395 |
| own3 | ownership | 3/3 / 3/3 | 1.0 / 1.0 | 9 / 3,395 |
| list | ownership | 3/3 / 3/3 | 10.3 / 1.0 | 409 / 3,395 |
| purpose | ontology | 3/3 / 3/3 | 5.0 / 1.7 | 223 / 7,065 |
| deps_direct | dependencies | 3/3 / 3/3 | 2.0 / 1.0 | 193 / 5,778 |
| build_cli | dependencies | 3/3 / 3/3 | 7.0 / 1.3 | 795 / 5,208 |
| build_net | dependencies | 3/3 / 3/3 | 4.7 / 1.0 | 404 / 4,976 |
| dependents_direct | dependencies | 1/3 / 3/3 | 14.0 / 2.3 | 1,615 / 7,774 |
| dependents_trans | dependencies | 1/3 / 3/3 | 13.0 / 1.0 | 1,145 / 3,395 |
| no_deps | dependencies | 0/3 / 3/3 | 13.3 / 1.0 | 1,197 / 3,395 |
| owners | ontology | 3/3 / 3/3 | 4.7 / 1.0 | 238 / 5,528 |
| top_author | history | 3/3 / 3/3 | 1.0 / 1.0 | 52 / 4,920 |
| commit_count | history | 3/3 / 3/3 | 1.0 / 1.0 | 143 / 260 |
| commit_files | changes | 3/3 / 0/3 | 2.0 / 3.0 | 79 / 336 |
| commit_zones | changes | 3/3 / 0/3 | 2.0 / 6.7 | 87 / 1,985 |
| latest_zones | changes | 3/3 / 1/3 | 1.3 / 11.7 | 56 / 6,853 |
| range_zone | changes | 3/3 / 0/3 | 5.0 / 11.3 | 451 / 15,152 |
| find_func | history | 3/3 / 2/3 | 1.7 / 13.7 | 58 / 11,055 |
| find_const | history | 3/3 / 1/3 | 6.7 / 12.7 | 1,069 / 5,681 |
| merge_a_zones | merge | 0/3 / 0/3 | 6.7 / 13.3 | 605 / 4,185 |
| merge_a_files | merge | 3/3 / 0/3 | 6.3 / 11.0 | 531 / 794 |
| merge_b_zones | merge | 0/3 / 3/3 | 8.0 / 12.3 | 1,809 / 3,481 |
| merge_b_files | merge | 3/3 / 3/3 | 6.0 / 6.3 | 296 / 515 |

Total API cost (list price estimate): $0.373; mean LLM seconds per episode git/vcs: 4.1 / 4.4

Note: `merge_*_zones` were worded ambiguously in run 1 (several git episodes answered the union instead of the intersection); the wording was fixed before run 2.

### Live, run 2: prompt v2 (vcs also told how to read per-zone history) and fixed merge wording

This run's prompt and one task were edited **after seeing run 1**, so run 2 is tuned on the test set and flatters vcs. Run 1 is the cleaner comparison; run 2 shows what a better
prompt buys.

| | episodes/tool | correct (git / vcs) | tool calls | tool output bytes | wall s | prompt tokens |
|---|---|---|---|---|---|---|
| **all tasks** | 72 | 81% / 83% | 5.5 / 3.2 | 341 / 8,211 | 4.7 / 3.3 | 3,125 / 6,333 |
| changes | 12 | 92% / 50% | 2.9 / 3.4 | 180 / 13,344 | 3.3 / 4.1 | 1,562 / 10,260 |
| dependencies | 18 | 50% / 94% | 9.4 / 1.1 | 544 / 4,370 | 7.7 / 1.7 | 5,722 / 2,551 |
| history | 12 | 100% / 83% | 2.9 / 5.0 | 356 / 12,816 | 3.0 / 4.6 | 1,930 / 9,114 |
| merge | 12 | 67% / 75% | 7.9 / 6.9 | 552 / 9,572 | 6.0 / 6.0 | 4,266 / 10,528 |
| ontology | 6 | 100% / 100% | 4.8 / 1.8 | 182 / 7,168 | 4.1 / 2.1 | 2,096 / 4,783 |
| ownership | 12 | 100% / 100% | 2.7 / 1.0 | 50 / 3,395 | 2.6 / 1.5 | 1,358 / 1,876 |


Per task (git / vcs):

| task | category | correct (of repeats) | mean calls | mean bytes |
|---|---|---|---|---|
| own1 | ownership | 3/3 / 3/3 | 1.0 / 1.0 | 24 / 3,395 |
| own2 | ownership | 3/3 / 3/3 | 1.0 / 1.0 | 20 / 3,395 |
| own3 | ownership | 3/3 / 3/3 | 1.0 / 1.0 | 9 / 3,395 |
| list | ownership | 3/3 / 3/3 | 7.7 / 1.0 | 147 / 3,395 |
| purpose | ontology | 3/3 / 3/3 | 3.7 / 2.7 | 154 / 8,807 |
| deps_direct | dependencies | 3/3 / 2/3 | 2.0 / 1.0 | 193 / 5,778 |
| build_cli | dependencies | 3/3 / 3/3 | 8.3 / 1.7 | 687 / 5,282 |
| build_net | dependencies | 3/3 / 3/3 | 5.3 / 1.0 | 385 / 4,976 |
| dependents_direct | dependencies | 0/3 / 3/3 | 14.0 / 1.0 | 299 / 3,395 |
| dependents_trans | dependencies | 0/3 / 3/3 | 14.0 / 1.0 | 656 / 3,395 |
| no_deps | dependencies | 0/3 / 3/3 | 13.0 / 1.0 | 1,046 / 3,395 |
| owners | ontology | 3/3 / 3/3 | 6.0 / 1.0 | 210 / 5,528 |
| top_author | history | 3/3 / 3/3 | 1.0 / 1.0 | 52 / 7,353 |
| commit_count | history | 3/3 / 3/3 | 1.0 / 1.0 | 153 / 6,991 |
| commit_files | changes | 3/3 / 1/3 | 2.0 / 3.0 | 79 / 5,440 |
| commit_zones | changes | 3/3 / 2/3 | 2.0 / 3.7 | 86 / 13,101 |
| latest_zones | changes | 3/3 / 3/3 | 4.0 / 2.0 | 366 / 22,765 |
| range_zone | changes | 2/3 / 0/3 | 3.7 / 5.0 | 189 / 12,072 |
| find_func | history | 3/3 / 2/3 | 3.3 / 10.0 | 88 / 18,773 |
| find_const | history | 3/3 / 2/3 | 6.3 / 8.0 | 1,130 / 18,147 |
| merge_a_zones | merge | 3/3 / 1/3 | 11.3 / 5.0 | 702 / 7,301 |
| merge_a_files | merge | 2/3 / 2/3 | 5.0 / 7.3 | 399 / 9,792 |
| merge_b_zones | merge | 0/3 / 3/3 | 9.3 / 6.0 | 732 / 9,584 |
| merge_b_files | merge | 3/3 / 3/3 | 6.0 / 9.3 | 375 / 11,611 |

Total API cost (list price estimate): $0.308; mean LLM seconds per episode git/vcs: 4.3 / 3.0

## Reading the results

* **Where vcs clearly helps: dependency questions.** Direct/transitive dependencies and dependents, "what must be subscribed to build X", "zones with no
  dependencies" are answered from one `zone graph --json` call, 94-100% correct vs 50-61% for git. The git agent has to read every Cargo.toml and compute closures by hand and often runs out of its 14 calls or gets
  the closure wrong. Ownership questions are equal (100% both) and vcs uses fewer calls; the ontology also answers "list all zones" in 1 call vs 8-10.
* **Where vcs is worse today: history and change questions.** The agent could not find a cross-zone view of history. `vcs log` is per zone (no `--all` across zones), so
  "which commit introduced X" costs one call per zone, and run 1 agents often gave up or ran into the call limit (changes: 8% correct vs 100% for git). With the
  v2 hint the changes category rose to 50% and history stayed at 83%, still below git (92% / 100%). This is a product gap (a cross-zone `log -S` and a smaller
  `zone describe` history view would help), not only a prompt problem.
* **Bytes.** vcs consumed 10-24x more tool output per episode than git (run 1: 4,663 vs 483 bytes; run 2: 8,211 vs 341). `zone describe` is verbose even with `--fields`: it prints
  full 64-hex ids, change ids, parents and file lists. Fewer calls did not mean fewer tokens. Prompt tokens per episode were 3.0x (run 1) and 2.0x (run 2) higher for vcs.
* **Overall accuracy** over all 24 tasks: run 1 git 82% / vcs 72%; run 2 git 81% / vcs 83%. These two numbers are within what 3 repeats of one model on 24 tasks can move by, and run 2 is
  tuned. **Do not read them as "vcs is better" or "worse"; read the per-category pattern.**

## Caveats (read these)

* One model (gpt-4.1-mini), one small synthetic repository (16 commits, 9 zones), 24 tasks, 3 repeats at temperature 0.7: 72 episodes per tool per run. Episode counts per category are 6-18; expect swings of
  tens of percentage points from sampling alone. No confidence intervals were computed.
* The fixture is synthetic and favourable to zones: clean path-dependency manifests, one zone per directory, a CODEOWNERS file. Real repositories are messier.
* The git baseline is given the zone definition in its prompt; the vcs agent finds it with a tool. A git agent without that hint would do worse on ownership and dependency tasks.
* Task wording, the vcs prompt (v2) and `zone describe --fields` were developed while looking at oracle output and run 1. The oracles use features written for this benchmark (`--fields`).
  The oracle strategies are my best guess at efficient use of each tool, not proven optimal.
* Tool wall time live includes an ssh round trip and the model's latency (about 3-4 s per episode of model time), so it says little about tool speed. Offline oracle timings are from a shared, loaded box.
* Cost: about US$0.70 in total at gpt-4.1-mini list prices (0.40 / 1.60 USD per million tokens in/out, which I did not re-verify against the current price list). The harness stops at a `--budget-usd` limit.
* Not run: any other model, any real repository, a git agent allowed to use a shell (`grep`, `cat`, pipes), repeats beyond 3, remote/network repositories.

## Reproduce

```sh
# build vcs (box), then offline: deterministic, no key, no network
cd scripts/agent_bench && VCS_BIN=/path/to/vcs python3 -m unittest test_bench
VCS_BIN=/path/to/vcs python3 harness.py --mode offline --vcs-bin /path/to/vcs
# fixture only
python3 harness.py --vcs-bin /path/to/vcs --build-fixture /tmp/fx
# live (OPENAI_API_KEY from the environment; add --ssh-target/--ssh-key if the repos live on a box)
OPENAI_API_KEY="$(cat keyfile)" python3 harness.py --mode live --prompt v1 --vcs-bin /path/to/vcs --fixture /tmp/fx --repeats 3 --out out.jsonl
python3 report.py out.jsonl
```
