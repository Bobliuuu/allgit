# AI-assisted zone proposals: `vcs zone suggest --assist`

Status: **first version.** It is mock-tested (a fake assistant script for `vcs`, a local fake HTTP server for the
assistant binaries) and **has never been called against a live model API**. There are no accuracy, cost or latency
measurements, and no claim that the proposals are good. Treat every proposal as a suggestion to read, not an answer.

`vcs zone suggest` already proposes zone boundaries from manifests, CODEOWNERS and directory structure, offline and
deterministically ([zones-autodetect.md](zones-autodetect.md)). With `--assist <cmd>` it additionally asks an external
command, normally an LLM wrapper, for more proposals and prints them next to the heuristic ones. It follows the design of
the merge assistant ([merge.md](merge.md#assisted-merge-vcs-merge---assist), [assist.md](assist.md)): opt-in, an external
program you name, a strict protocol, validation on the `vcs` side, and **proposals only**.

## Rules

* **Off by default.** No assistant runs unless you pass `--assist <cmd>` or set the user-level `zone.assist` (below).
  `--no-assist` overrides both.
* **Never applies anything.** `zone suggest` stays read-only, with or without `--assist`: no zone is created, no commit
  is made, nothing under `.vcs/` is written. Adopting a path is a separate, explicit step you run yourself:
  `vcs zone apply <path|name>` for a heuristic suggestion, or `vcs zone apply --dir <path> --name <name>` for a
  proposal that only the assistant made. Both print a plan until you add `--yes`.
* **The assistant is not trusted.** `vcs` validates every proposal itself (see Validation) and drops the bad ones.
  A failing, slow or lying assistant never changes the heuristic output: it adds an "assistant" section with the reason.

## Privacy and cost: what leaves your machine

`vcs` writes one JSON request to the assistant's stdin. **Exactly this is in it, and nothing else:**

| field | content |
|---|---|
| `tree.dirs` | directory paths of the root zone down to depth 4, each with the number of committed files under it (at most 1,500 directories, shallowest first; `truncated` says if some were cut). Dot-directories and `node_modules`, `target`, `vendor`, `dist`, `build`, `out`, `__pycache__`, `third_party` are left out |
| `manifests` | paths (not contents) of manifest files: `Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, `pom.xml`, `build.gradle(.kts)`, `BUILD`, `BUILD.bazel` (at most 500) |
| `codeowners` | the rules of the CODEOWNERS file: pattern and owner handles or emails (at most 200 rules, 10 owners each) |
| `existing_zones` | name and path of the zones that already exist |
| `heuristic` | the deterministic proposals: name, path, file count, confidence, and the signals behind them (kind and a one-line detail such as a manifest path) |
| `repo`, `limits` | total and root-zone file counts, `min_files`, `max_zones` |

**No file contents are sent. Individual file names are not sent** (other than manifest paths and what a CODEOWNERS
pattern or a signal detail happens to name). Directory names, manifest paths and owner handles can still be sensitive
for a private project; do not enable this where they may not go to a third party. The request is capped at 200 KiB;
`vcs` shrinks the tree to fit, and the assistant binaries refuse a larger request without making a call. The
repository's own contents are never in it, so there is no equivalent of the merge assistant's "whole files leave your
machine" risk, but the request does describe your repository's layout and ownership.

**It costs money**: one API call per `zone suggest --assist`, billed to the account of the key; the input is the request
above (at most 200 KiB) and the output is a short JSON list (output limit 4,096 tokens). No measurement of the actual
cost has been made. Retries after 429/5xx are separate attempts (see the environment variables in [assist.md](assist.md)).

The key is read from the environment by the assistant (not by `vcs`), sent only in the request header, scrubbed from
every message and refused if it appears in an answer; it is never written to disk or put on a command line.

## Using it

```sh
cargo build --release -p vcs-assist-claude          # or vcs-assist-openai
export ANTHROPIC_API_KEY=...                        # or OPENAI_API_KEY for vcs-assist-openai
vcs zone suggest --assist vcs-assist-claude
vcs --json zone suggest --assist vcs-assist-claude
vcs zone apply --dir web --name web-ui              # plan only
vcs zone apply --dir web --name web-ui --yes        # do it
```

`--assist-timeout SECS` (default 30) bounds the assistant. The command runs through `sh -c` in the repository root
(`cmd /C` off unix), like the merge assistant; its stderr is discarded by `vcs` (so a missing key shows up as
"assistant failed: exited with ..."; run the assistant by hand with a saved request to see its message).

### Opt-in setting `zone.assist`

```sh
vcs config set --user zone.assist vcs-assist-claude   # all repositories (your consent to send the request above)
vcs config set --user zone.assist off                  # disable again
vcs zone suggest --no-assist                           # skip it once
```

Same security rule as `merge.assist` ([security-review.md](security-review.md) finding 15): a repository's own config
cannot name a program. `vcs config set zone.assist ...` without `--user` is refused, and a `zone.assist` written by hand
into `.vcs/config` is ignored with a warning. Only the user config and the environment (`VCS_ZONE_ASSIST`) may set it.
When the setting is used, `vcs` prints `zone.assist is set: running ...` (on stderr with `--json`). Precedence is the
usual one; `--assist <cmd>` overrides the setting.

## Protocol

Versioned like the merge protocol (`"version":1`) and told apart by a new field, **`"kind":"zone-suggest"`**. The merge
conflict request has no `kind` (and `"kind":"merge-conflict"` is accepted as the same thing), so existing merge
assistants and the merge code are unchanged. An assistant that only knows the merge request and is sent a zone request
fails (it finds no `path`) rather than misbehaving; `vcs` shows that as "assistant failed". An assistant that knows both
looks at `kind` first. Unknown fields must be ignored by both sides, so fields can be added without a version bump;
anything that changes the meaning of a field bumps `version`.

Request:

```json
{"version":1,"kind":"zone-suggest","limits":{"min_files":3,"max_zones":20},
 "repo":{"total_files":120,"root_files":120},
 "tree":{"max_depth":4,"truncated":false,"dirs":[{"path":"web","files":5},{"path":"web/ui","files":3}]},
 "manifests":["svc/go.mod"],
 "codeowners":[{"pattern":"*.js","owners":["@alice"]}],
 "existing_zones":[{"name":"docs","path":"docs"}],
 "heuristic":[{"name":"svc","path":"svc","files":4,"confidence_pct":86,
               "signals":[{"kind":"go-module","detail":"svc/go.mod"}]}]}
```

Response (stdout, one JSON object, exit status 0; stdout over 1 MiB is refused; `version` and `kind` optional but, if
present, must be `1` and `"zone-suggest"`):

```json
{"zones":[{"name":"web-ui","path":"web","rationale":"one short sentence","confidence":0.7}]}
```

The zone ontology ([zone-ontology.md](zone-ontology.md)) is **not** sent in this version: it describes existing zones, of
which only name and path matter here.

### Validation

In `vcs`, per proposal; failures are listed as "rejected" with the reason:

* `name`: the ordinary zone-name rule (1 to 64 of letters, digits, `.`, `_`, `-`, not starting with `.` or `-`), not an
  existing zone id or display name, not used by an earlier proposal, and not the name of a heuristic proposal for
  another path.
* `path`: relative, no `.` or `..` or empty component, no backslash or control character, no component starting with `.`
  or named `node_modules`, `target`, `vendor`, `dist`, `build`, `out`, `__pycache__`, `third_party`; a single trailing
  `/` is accepted and removed; the repository's path rules apply.
* the path must exist: at least `--min-files` (default 3) committed files of the root zone under it. A path that is
  already inside a zone, or inside the root zone's checked-out branch but not committed, is rejected.
* no overlap with an existing zone, and none between proposals (the more confident one wins; ties go to the shorter path).
* `confidence` a number from 0 to 1, `rationale` a string (control characters become spaces, cut at 500 bytes).
* at most 50 proposals are read and at most 20 kept. A response that is not UTF-8, not JSON, nested deeper than 32,
  over 1 MiB, or without a `zones` array is ignored as a whole.

Overlap with a *heuristic* proposal is allowed and shown (`overlaps_heuristic`): they are alternatives, pick one.

### Output

Text: after the usual heuristic listing, an "assistant" section with each accepted proposal (`source assist`, or
`source both` when its path equals a heuristic proposal's), its rationale, and the rejected ones with reasons.

`--json`: `zones[]` is unchanged except that each entry now has `"source":"heuristic"` (an added field, no version bump),
and a new top-level object appears only when an assistant was used:

```
"assist":{"used":true,"command_from":"flag"|"setting","ok":bool,"error":str|null,
          "proposals":[{"name","path","files","confidence_pct","rationale","source":"assist"|"both","overlaps_heuristic":[name]}],
          "rejected":[{"name","path","reason"}],"notes":[str]}
```

Assist-only proposals are deliberately **not** put into `zones[]`, so a script that applies everything in `zones[]` can
never apply a model's proposal by accident.

## The assistant binaries

`vcs-assist-claude` and `vcs-assist-openai` handle both request kinds ([assist.md](assist.md),
[assist-openai.md](assist-openai.md)); the environment variables are the same. For a zone request the model is asked for
the JSON above, the answer is checked again by the binary (path must be in the request's tree, no overlap with existing
zones or each other, name rules, confidence range, at most `max_zones`), and dropped proposals are reported on stderr.
The directory names in the request are untrusted data: they go only into a delimited block of the user turn, never into
the system prompt, and the prompt tells the model not to follow text inside them. A request over 200 KiB is refused
without a call. `vcs-assist-openai` keeps its per-process request and token budgets.

## Tests

* `vcs-repo` unit tests (`zoneassist`): request building (no file names, no vendor or dot directories, cap and
  truncation, oversize refusal) and response validation (absolute and `..` paths, backslashes, dot and vendor
  directories, nonexistent paths, too few files, existing zones, overlaps, names, confidence, limits, malformed JSON).
* `crates/vcs-cli/tests/zone_assist.rs`: the real `vcs` binary with a fake assistant script: off by default, request
  shape (no contents), read-only, labelling, failures keep the heuristic output, timeout, `zone.assist` user-only
  opt-in (a repository config cannot set it), `--no-assist`, `apply --dir`.
* `crates/vcs-assist-claude/tests/zones.rs` and `crates/vcs-assist-openai/tests/zones.rs`: the binaries against the
  local fake server: checked proposals, layout only in the user turn, injection text as data, bad answers, oversize
  request never sent, key never in any output, merge requests unchanged. No real API call is made by any test.

## Limits and not done

* Never called against a live API; quality is unknown. The prompt is a first draft and has not been tuned or evaluated.
* Only the root zone's committed files are considered; zones nested in existing zones are not proposed.
* The tree stops at depth 4 and 1,500 directories; in a very large repository the model sees a truncated layout.
* File contents are not sent, so a boundary that only the code shows (imports between directories, for example) cannot be
  found except through the heuristic's manifest dependencies in the `heuristic` field. There is no option to send content.
* `zone apply --dir` splits one directory per call, like `apply`; it has no preview of what collaborators will see.
* stderr of the assistant is not shown by `vcs`.
