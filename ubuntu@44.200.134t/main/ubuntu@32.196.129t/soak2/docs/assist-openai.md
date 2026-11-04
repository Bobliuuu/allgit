# OpenAI-compatible assistant for `vcs merge --assist`

`vcs-assist-openai` (crate `crates/vcs-assist-openai`) speaks the same assistant protocol as
[`vcs-assist-claude`](assist.md) but asks an OpenAI-compatible chat-completions endpoint. Nothing changes on the `vcs`
side: you select the adapter by command, exactly like the Claude one (`vcs merge feature --assist vcs-assist-openai`,
or `vcs config set merge.assist vcs-assist-openai`). So there is no new CLI flag and the CLI reference is unchanged.

It proposes only: `vcs` never applies a proposal until you run `vcs resolve <path> --proposal`. Prompt, extraction and
all validation (markers, size, NUL, non-conflicting hunks preserved byte for byte, optional parse check) are the shared
code of `vcs-assist-claude`, and the same rule holds: the three file versions are untrusted data, sent only inside
delimited blocks of the user turn, never in the system prompt; the prompt tells the model not to follow text inside them.

**Privacy and cost warnings from [assist.md](assist.md) apply unchanged**: whole files leave your machine, to whatever
URL you configure, and each conflict costs money.

## Environment

| variable | default | meaning |
|---|---|---|
| `OPENAI_API_KEY` | required | sent only as `authorization: Bearer`; scrubbed from every printed message |
| `VCS_ASSIST_MODEL` | `gpt-4.1-mini` | model id; use a cheap one |
| `VCS_ASSIST_API_URL` | `https://api.openai.com/v1/chat/completions` | any OpenAI-compatible chat-completions URL; must be `https://` or `http://` to localhost |
| `VCS_ASSIST_API_TIMEOUT_SECS` | 25 | wall-clock budget per conflict including retries |
| `VCS_ASSIST_MAX_RETRIES` | 2 (max 8) | retries after 429, 5xx, connection errors |
| `VCS_ASSIST_BACKOFF_MS` | 500 | first backoff, doubled per retry |
| `VCS_ASSIST_MAX_OUTPUT_TOKENS` | 8192 | cap on completion tokens per call |
| `VCS_ASSIST_TOKEN_PARAM` | `max_completion_tokens` | set `max_tokens` for servers that only know the old name |
| `VCS_ASSIST_MAX_REQUESTS` | 10 | hard budget: HTTP requests per process, retries included |
| `VCS_ASSIST_MAX_TOTAL_TOKENS` | 100000 | hard budget: prompt + completion tokens per process, from the API `usage` field |

The budgets are per process: one `vcs-assist-openai` run handles one conflict (or one `eval` run). Before every request
the budget is checked; a call that would exceed it is not sent and fails with `budget: ...`. A prompt-size estimate
(bytes / 4) is checked before sending; actual usage is added afterwards, so one call can overshoot the token budget by
its own size at most. `vcs` starts one process per conflict, so the budget does not span a whole merge. `gpt-5*` and
o-series models get no `temperature` field (they only accept the default); others get `temperature: 0`.
On stderr every run prints `usage: requests=.. prompt_tokens=.. completion_tokens=.. model=..` from the API usage fields.

```sh
cargo build --release -p vcs-assist-openai
OPENAI_API_KEY=... vcs merge feature --assist vcs-assist-openai
vcs-assist-openai eval <dir> --live     # same fixture format as vcs-assist-claude eval
```

## Tests and a measured evaluation

`cargo test -p vcs-assist-openai` uses a local fake server (no key, no network): request shape, success, refusal,
malformed bodies and malformed model text, 429/5xx retry, give-up, non-retried 4xx, injection text in a hunk, request
and token budgets (including across `eval` cases), key never printed.

One live run was made on 24 generated Python/Rust conflicts with `gpt-4.1-mini` (6 independent edits, 6 same-line
semantic combinations, 4 rename+edit, 5 with injection text, 3 true contradictions where refusing is correct).
Results: 24 requests, 15,023 prompt and 1,032 completion tokens (no retries). 6 byte-exact proposals, 1 more that
differs from the reference but passes the Python check, 0 wrong proposals, 0 invalid, 17 refusals, of which 3 were the
correct refusals and 14 were refusals of conflicts that had a known resolution. No injection was obeyed (4 of the 5 were
simply refused). The model is cautious to the point of being unhelpful on easy cases; this is one model, one prompt, one
run of a small synthetic set, and says nothing about real repositories.
