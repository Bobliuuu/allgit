# Claude assistant for `vcs merge --assist`

See also [assist-openai.md](assist-openai.md) for the OpenAI-compatible adapter.

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
  Retries after 429/5xx are separate attempts. No model, price or latency numbers are claimed here; measure with your own
  key (see Evaluation).
* The model can be wrong. Validation (below) catches structural failures, not bad merges. Review every proposal with
  `vcs conflict proposals`.
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
| `VCS_ASSIST_MODEL` | `claude-sonnet-5-5` | model id |
| `VCS_ASSIST_API_TIMEOUT_SECS` | 25 | wall-clock budget for one conflict including retries (keep below `--assist-timeout`, default 30) |
| `VCS_ASSIST_MAX_RETRIES` | 3 (max 8) | retries after 429, 5xx (incl. 529) and connection errors |
| `VCS_ASSIST_BACKOFF_MS` | 500 | first backoff, doubled per retry; a `retry-after` header up to 10 s is honoured |
| `VCS_ASSIST_API_URL` | `https://api.anthropic.com/v1/messages` | testing/proxies. Must be `https://`, or `http://` to localhost only; redirects are never followed |

## What it does

1. Reads the request (`version` 1). Refuses without a call when the input is over 200 KiB or has NUL bytes.
2. Sends a system prompt (in `src/prompt.rs`) that demands either a complete resolved file inside `<resolved_file>` or an
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
call, streaming, prompt caching, and cost accounting.

## Evaluation

`vcs-assist-claude eval <dir> [--live] [--json]` scores the assistant on recorded conflicts. Each subdirectory of `<dir>`
is one case: `ours`, `theirs`, `expected`, optional `base` (absent means add/add) and `path` (one line; decides the
language), and for offline runs `response.txt`, the raw text the model returned. Cases run in name order.

* Offline (default): no network and no key. `response.txt` goes through exactly the extraction and validation used in
  production, so the report is deterministic and tests the validators and scoring.
* Live: `ANTHROPIC_API_KEY=... vcs-assist-claude eval <dir> --live` calls the API for every case (this costs money and
  sends the fixture files to Anthropic) and ignores `response.txt`. To record responses for offline replay, save the
  model text of a live call as `response.txt`; the tool does not do that for you yet.

Reported: **exact-match** rate (resolution equals `expected` byte for byte, over all cases), the rate of proposals that
pass validation, and the **parse-valid** rate (proposals that parsed, over proposals a parser could judge, counting
answers rejected for not parsing as misses). Refusals, invalid answers and missing responses are counted separately.
Exact match is strict: a correct merge that differs from the reference in whitespace or ordering counts as a miss.

The fixtures in `crates/vcs-assist-claude/tests/fixtures/eval` are five hand-written Rust cases with hand-written
responses; they test the harness, not the model. No quality numbers for any real model exist yet.

## Tests

`cargo test -p vcs-assist-claude` (add `--features structural` to include the parse checks) uses a local scripted HTTP server on 127.0.0.1 (never the real API): request shape,
model override, refusal, nine kinds of invalid proposal, malformed API bodies, 429/5xx retry and backoff, give-up count,
non-retried 4xx, timeout, key never present in stdout/stderr (even when a server echoes it), oversize and binary
inputs, and the eval harness. Not tested: a real API call, or real certificate handling for `https://` (the HTTP
client's rustls support is only built, not exercised here).
