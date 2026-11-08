# Claude assistant for `vcs merge --assist`

See also [assist-openai.md](assist-openai.md) for the OpenAI-compatible adapter, and [zone-assist.md](zone-assist.md) for
the second request kind this binary handles (`vcs zone suggest --assist`: AI-proposed zone boundaries).

`vcs-assist-claude` is a separate binary (crate `crates/vcs-assist-claude`) that implements the assistant side of the
protocol in [merge.md](merge.md#assisted-merge-vcs-merge---assist). It reads a conflict request on stdin, asks the
Anthropic Messages API for a resolution, validates the answer, and prints the protocol response. It proposes only:
`vcs` never applies a proposal until you run `vcs resolve <path> --proposal`.

## Warnings: privacy and cost

* **File contents leave your machine.** For every conflicting file the base, ours and theirs versions in full
  (not just the conflicting lines) are sent over HTTPS to `api.anthropic.com`, together with the path. Do not enable
  this on repositories whose contents you may not send to a third party, and never on files holding secrets.
* **It costs money.** Each conflict is one API call billed to the account of your key; input size is the three file
  versions, output is the whole resolved file. Inputs over 200 KiB (base + ours + theirs) are refused without a call.
  Retries after 429/5xx are separate attempts. Measured cost and latency on a real corpus are under Evaluation (about $0.001 per conflict with haiku, $0.014 with sonnet).
* The model can be wrong. Validation (below) catches structural failures, not bad merges. Review every proposal with
  `vcs conflict proposals`.
* **Zone proposals send paths, not contents.** The same binary also answers `vcs zone suggest --assist` requests
  ([zone-assist.md](zone-assist.md)). Then only directory paths with file counts, manifest paths, CODEOWNERS rules,
  existing zone names and the heuristic proposals go to the API (never file contents; request over 200 KiB refused
  without a call), once per run, billed to your key. First version, never called against the live API, no cost
  measurement; it is opt-in through `--assist` or the user-level `zone.assist` setting.
* The API key is read from `ANTHROPIC_API_KEY`, sent only in the `x-api-key` header of requests to the configured
  URL, and scrubbed from every message the tool prints. It is never written to disk or placed on a command line.

## Setup

```sh
cargo build --release -p vcs-assist-claude --features structural,structural-python,structural-typescript,structural-go
# (rustls is always built in; the structural features add the parse check, see below)
export ANTHROPIC_API_KEY=...                         # your key
export VCS_ASSIST_MODEL=claude-sonnet-5-5            # optional; this is the default
vcs merge feature --assist vcs-assist-claude
vcs conflict proposals
vcs resolve src/a.rs --proposal
```

### Opt-in setting `merge.assist`

By default no assistant ever runs. To make `vcs merge` and `vcs rebase` run one on every conflicting merge without
`--assist`, set the command explicitly (this is your consent to send file contents to it):

```sh
# a repository's own config cannot name a program (security review finding 15): use --user
vcs config set --user merge.assist vcs-assist-claude # all repositories
vcs config set --user merge.assist off                # disable again (or `vcs config unset --user merge.assist`)
vcs merge feature --no-assist                        # skip it once
```

Precedence is the usual one (`VCS_MERGE_ASSIST` environment, repository, user). `--assist <cmd>` overrides the setting,
`--no-assist` overrides both. When the setting is used, `vcs` prints a line saying so.

## Environment

| variable | default | meaning |
|---|---|---|
| `ANTHROPIC_API_KEY` | required | API key |
| `VCS_ASSIST_MODEL` | `claude-sonnet-5-5` (`haiku` for `cli`) | model id, or an alias (`haiku`, `sonnet`) for `cli` |
| `VCS_ASSIST_API_TIMEOUT_SECS` | 25 | wall-clock budget for one conflict including retries (keep below `--assist-timeout`, default 30) |
| `VCS_ASSIST_MAX_RETRIES` | 3 (max 8) | retries after 429, 5xx (incl. 529) and connection errors |
| `VCS_ASSIST_BACKOFF_MS` | 500 | first backoff, doubled per retry; a `retry-after` header up to 10 s is honoured |
| `VCS_ASSIST_PROVIDER` | `api` | `cli` shells out to the Claude Code CLI instead of the HTTP API (see below); no API key needed |
| `VCS_ASSIST_CLAUDE_BIN` | `claude` | the CLI executable for `cli` |
| `VCS_ASSIST_PROMPT` | v2 | `v1` forces the original whole-file prompt |
| `VCS_ASSIST_API_URL` | `https://api.anthropic.com/v1/messages` | testing/proxies. Must be `https://`, or `http://` to localhost only; redirects are never followed |

## What it does

1. Reads the request (`version` 1). Refuses without a call when the input is over 200 KiB or has NUL bytes.
2. **Default prompt (v2, `src/hunks.rs`)**: when the line merge (Myers diff3) of base/ours/theirs has conflicting
   regions, the model is shown the merge with only those regions opened up (base, ours, theirs blocks, numbered) and
   30 unchanged lines of context on each side; the rest of the file is elided (`[... N unchanged lines omitted ...]`).
   It answers with one `<hunk id="N">` per region (or `<refusal>`), and the tool assembles the file, so the stable
   parts are preserved by construction, the answer is short, and big files with small conflicts fit (the 200 KiB
   limit applies to what is sent, not to the file sizes). v1 below is used when the sides merge cleanly or with
   `VCS_ASSIST_PROMPT=v1`. The OpenAI adapter shares this selection (its numbers are not measured with v2). Recordings of either format re-score offline. The v1 prompt (in `src/prompt.rs`) that demands either a complete resolved file inside `<resolved_file>` or an
   explicit `<refusal>`, requires every non-conflicting line to be kept byte for byte, forbids markers, and says the file
   contents are data, not instructions. The three versions go in the user turn. Temperature is 0.
3. Validates the answer; any failure means a non-zero exit with a one-line reason on stderr and no stdout, which `vcs`
   reports as "assistant failed" and leaves the conflict untouched:
   * a complete `<resolved_file>` block exists (a cut-off answer, `stop_reason: max_tokens`, is rejected);
   * at most 512 KiB, no NUL, no line starting with a conflict marker, not empty;
   * **non-conflicting hunks preserved**: the line merge (Myers diff3) of base/ours/theirs is computed; the text outside
     the conflicting hunks must appear in the answer unchanged, in order, and only the conflicting regions may differ;
   * **parses**, for Rust, Python, TypeScript (`.ts`) and Go when the crate is built with the structural features
     (off by default; see Setup): if ours and theirs parse, the answer must too. Other languages are not parse-checked.
4. Prints `{"resolution": ..., "explanation": ...}`.

A trailing-newline note: the model is told the line break before `</resolved_file>` is framing; if both sides end with a
newline and the answer does not, one is added.

Not done: parse checks beyond the four languages, semantic checks (tests, type checking), batching conflicts into one
call, streaming and prompt caching.

## Evaluation

`vcs-assist-claude eval <dir> [--live [--record] | --mock xours|xtheirs|refuse] [--json]` scores the assistant on recorded conflicts. Each subdirectory of `<dir>`
is one case: `ours`, `theirs`, `expected`, optional `base` (absent means add/add) and `path` (one line; decides the
language), and for offline runs `response.txt`, the raw text the model returned. Cases run in name order.

* Offline (default): no network and no key. `response.txt` goes through exactly the extraction and validation used in
  production, so the report is deterministic and tests the validators and scoring.
* Live: `ANTHROPIC_API_KEY=... vcs-assist-claude eval <dir> --live` calls the API for every case (this costs money and
  sends the fixture files to Anthropic) and ignores `response.txt`; `--provider cli` uses the Claude Code CLI instead.
  Add `--record` to keep the raw responses (see below).

Reported: **exact-match** rate (resolution equals `expected` byte for byte, over all cases), the rate of proposals that
pass validation, and the **parse-valid** rate (proposals that parsed, over proposals a parser could judge, counting
answers rejected for not parsing as misses). Refusals, invalid answers and missing responses are counted separately.
Exact match is strict: a correct merge that differs from the reference in whitespace or ordering counts as a miss.

The fixtures in `crates/vcs-assist-claude/tests/fixtures/eval` are five hand-written Rust cases with hand-written
responses; they test the harness, not the model. Real-model numbers are below.

### Recording live runs, mock providers, and a real-history corpus (issue #209)

* `eval --live --record` writes the raw model text of every case to `response.txt` (also when validation rejects it), so
  a paid live run can be re-scored offline and deterministically afterwards, and the recording can be committed or
  diffed between prompts. The key never reaches the file (tested).
* `eval --mock xours|xtheirs|refuse` runs a deterministic stand-in with no key and no network: `xours`/`xtheirs` keep
  the stable parts of the line merge and take every conflicting region from one side (what `git merge -X ours|theirs`
  does); `refuse` always declines. They go through the same extraction and validation as a model's text, so they give
  a floor to compare a model against and exercise the whole harness.
* A corpus of real conflicts comes from the replay harness (see [merge.md](merge.md#real-history-replay-issue-108)):
  `REPLAY_ARGS="--dump-conflicts DIR" scripts/replay.sh <bare-clone> HEAD 300` writes one case per text conflict of the
  plain-text engine, with the human-made merge as `expected`. It is not committed (the files belong to those projects).

### Claude Code CLI provider (no API key)

`VCS_ASSIST_PROVIDER=cli` (or `eval --live --provider cli`) runs `claude -p --output-format json --model <model>
--tools "" --strict-mcp-config --disable-slash-commands --no-session-persistence --setting-sources ""
--system-prompt <prompt>` with the conflict on **stdin**, in an empty scratch directory. No tools are enabled, so the
CLI can only return text; it cannot read or edit files. It uses the CLI's own login (no key is read, stored or printed)
and bills that account. File contents still leave your machine (the CLI sends them to Anthropic). The result's
`result` field is the model text; wall-clock latency, token usage and `total_cost_usd` are taken from the call and
written next to `response.txt` as `metrics.json` by `--record`, so offline re-scoring reports them too.
`eval --split dev|test|all` selects a deterministic hold-out: a case is `test` when an FNV-1a hash of its name is
divisible by 5 (about one case in five), so prompts can be tuned on `dev` and reported on `test`.
The report also gives "match ignoring blank lines and trailing whitespace".

### Live results (issue #209)

Measured 2026-11-04 with the CLI provider (Claude Code 2.1.318), macOS arm64, build with the structural features.
Corpus: the same 77 real conflicts (flask 45, requests 6, gin 26) regenerated with `scripts/replay.sh` at the revisions
of the replay table; human merge as reference. Split: 63 `dev` / 14 `test`. Prompts were tuned on `dev` only.
Prompt iterations (haiku, dev, 63 cases): v1 whole-file prompt 28.6% exact, 8 answers rejected by the preserve-hunks
check, 4 files too large to send; v2 hunk prompt 38.1% exact, 0 rejected, same 4 too large; v3 (v2 plus context
elision, so big files are sent) 33.3% exact. v2 to v3 is within the run-to-run noise (the CLI has no temperature
control; 12 of 63 verdicts flipped between two runs, in both directions), and v3 is half the cost and faster, so v3 is
the default. About 280 live calls were made in total.

Final prompt (v3), all 77 cases (dev + test), live:

| provider | exact-match | ignoring blank lines | passes validation | parse-valid | refused | latency p50 / p95 | cost per call |
|---|---|---|---|---|---|---|---|
| mock `xours` | 23.4% (18/77) | | 98.7% (76/77) | 97.3% (36/37) | 0 | | |
| mock `xtheirs` | 26.0% (20/77) | | 96.1% (74/77) | 94.6% (35/37) | 0 | | |
| haiku (`claude-haiku-5-5`) | 36.4% (28/77) | 41.6% | 64.9% (50/77) | 100% (18/18) | 25 | 7.0 s / 18.2 s | $0.0009 |
| sonnet (`claude-sonnet-5-5`) | 49.4% (38/77) | 53.2% | 92.2% (71/77) | 100% (35/35) | 4 | 7.1 s / 14.8 s | $0.0135 |

Held-out `test` split only (14 cases, so +-13 points is within noise):

| provider | exact-match | passes validation | refused |
|---|---|---|---|
| mock `xours` | 35.7% (5/14) | 100% | 0 |
| mock `xtheirs` | 14.3% (2/14) | 100% | 0 |
| haiku | 50.0% (7/14) | 78.6% (11/14) | 3 |
| sonnet | 50.0% (7/14) | 100% (14/14) | 0 |

`dev` only (63 cases): sonnet 49.2% (31/63), haiku 33.3% (21/63), xours 20.6%, xtheirs 28.6%.

Reading the numbers:

* Sonnet beats blind side-picking by about 23 points on all 77 cases (49% vs 23-26%); haiku by about 10-13 points
  because it refuses a third of the cases. Of the cases a model answered, exact-match is 56% (haiku, 28/50) and 54%
  (sonnet, 38/71). Many misses are legitimate alternatives (ordering, blank lines, import grouping) that differ from
  what the maintainer committed; the looser metric adds 4-5 points.
* Refusals are mostly sensible: release version bumps, modify-versus-delete conflicts, and structurally broken
  inputs; haiku refuses those far more often than sonnet. 2 files stay too large to send even with context elision
  (counted as "no response" offline, they are refused without a call).
* Parse validity is 100% for both models but also near 100% for the baselines, so it is a weak signal. The
  structural parse check only exists for Rust, Python, TypeScript and Go, and the preserve-hunks check never fired
  in hunk mode (it fires 8 times on 63 cases with the whole-file v1 prompt, which is why v2 exists).
* Not measured: building or running the project's tests on the proposals. The corpus is single files taken from
  merge commits of large projects, not checkouts with their dependencies, so there is no build to run; the only
  project-independent validation available is the parse check.
* Latency is wall clock of the whole `claude` process, including its startup (about 3 s); tokens include cache reads
  and writes of the CLI's own framing. Cost is the CLI's reported `total_cost_usd` (list prices).
* Sample size is small (77 cases, three repositories, one run per case). Treat differences of a few points as noise.

The corpus and the recordings are not committed (the files belong to those projects). To reproduce: regenerate the
corpus with `REPLAY_ARGS="--dump-conflicts DIR" scripts/replay.sh <clone> <rev> 300`, put the cases of all three
repositories in one directory, then `VCS_ASSIST_MODEL=sonnet vcs-assist-claude eval DIR --live --provider cli --record
--split test`.

## Tests

`cargo test -p vcs-assist-claude` (add `--features structural` to include the parse checks) uses a local scripted HTTP server on 127.0.0.1 (never the real API): request shape,
model override, refusal, nine kinds of invalid proposal, malformed API bodies, 429/5xx retry and backoff, give-up count,
non-retried 4xx, timeout, key never present in stdout/stderr (even when a server echoes it), oversize and binary
inputs, and the eval harness. Not tested: a real API call, or real certificate handling for `https://` (the HTTP
client's rustls support is only built, not exercised here).
