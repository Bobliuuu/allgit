#!/usr/bin/env python3
"""Agent benchmark harness: how well does an agent understand a repo through vcs vs git?

Modes
  offline  scripted oracle strategies (no LLM, deterministic); `--mode offline`
  live     gpt-4.1-mini via the OpenAI API (key from env OPENAI_API_KEY only); `--mode live`

Tool execution: locally, or on a box over ssh (`--ssh user@host --remote-dir DIR`) so a Mac can drive
repos that live on Linux. Only read-only git/vcs commands run, with argv (never a shell).
"""
import argparse
import json
import os
import shlex
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fixture  # noqa: E402
import tasks as TK  # noqa: E402

MAX_OUT = 40000
MAX_CALLS = 14
MODEL = "gpt-4.1-mini"
PRICE_IN, PRICE_OUT = 0.40 / 1e6, 1.60 / 1e6  # USD per token for gpt-4.1-mini (list price; check before relying)

GIT_OK = {"log", "show", "ls-tree", "ls-files", "grep", "rev-list", "diff", "cat-file", "branch", "rev-parse",
          "shortlog", "blame", "status", "tag", "describe", "name-rev", "diff-tree", "show-ref", "version", "help"}
VCS_OK = {"zone", "log", "show", "diff", "grep", "blame", "status", "branch", "op", "tag", "ls-files", "cat-file",
          "help", "describe", "version", "sparse", "locks", "--help", "--version"}

PREAMBLE_COMMON = (
    "You answer questions about a code repository by running read-only commands. The commands run inside the repository "
    "(current directory is the repo root). Use as few commands as you reasonably can; output is truncated at {max} bytes. "
    "Only the one tool is available (no cat, head, pipes or shell). When you know the answer, reply with one final line `ANSWER: <json>` where <json> is a JSON string, an integer, or a JSON list of strings "
    "(for set answers; order does not matter). Do not answer before you have evidence from the repository.\n"
    "A `zone` is a named component of the repo that owns a path prefix. ")
PREAMBLE = {
    "git": PREAMBLE_COMMON + "This is a git repository (use the `git` tool commands only). Git has no zone concept, so for this repo the zones are: each "
           "directory directly under `crates/`, plus `tools/gen` (zone `tools`) and `docs` (zone `docs`). Files outside those directories belong to zone `root`. "
           "A zone's dependencies are the other zones named in `path = \"...\"` lines of its Cargo.toml.",
    "vcs": PREAMBLE_COMMON + "This is a vcs repository (use the `vcs` tool commands only). vcs knows zones natively. Useful commands: "
           "`vcs zone graph --json` (zones, paths, dependency closure), `vcs zone describe [ZONE] [--history N] [--transactions N] [--path P] [--fields purpose,owners,history,...] --json` "
           "(purpose, owners, dependencies, subscriptions, recent commits with changed files, operations), `vcs log --zone Z [-S text] [--format %s]`, "
           "`vcs diff`, `vcs --help`. Use `vcs zone describe --help` for options; `--fields` keeps only the named keys per zone, which shrinks output. File paths inside zone commit listings are zone-relative.",
}


V2_VCS_HINT = (" Per-zone history with changed files: `vcs zone describe --fields path,history --history 20 --json` lists every zone's recent commits "
               "(subject, timestamp, changed files relative to the zone `path`); a commit made in several zones appears once in each. "
               "`vcs zone describe --transactions N` lists recent operations and the zones each touched. `vcs log` is per zone (`--zone Z`); there is no cross-zone log.")


class Executor:
    def __init__(self, repo_dirs, vcs_bin, ssh=None):
        self.dirs, self.vcs_bin, self.ssh = repo_dirs, vcs_bin, ssh

    def run(self, tool, argv):
        if not argv or argv[0] != tool:
            return "error: command must start with `%s`" % tool, 2
        sub = next((a for a in argv[1:] if not a.startswith("-") or a in ("--help", "--version")), None)
        if sub not in (GIT_OK if tool == "git" else VCS_OK):
            return f"error: `{tool} {sub}` is not allowed in this benchmark (read-only commands only)", 2
        if tool == "git" and any(a in ("--output", "-o") or a.startswith("--output=") for a in argv):
            return "error: output redirection is not allowed", 2
        real = [self.vcs_bin if tool == "vcs" else "git"] + argv[1:]
        env_note = {"GIT_PAGER": "cat", "PAGER": "cat", "GIT_CONFIG_NOSYSTEM": "1"}
        if self.ssh:
            cmd = f"cd {shlex.quote(self.dirs[tool])} && " + " ".join(f"{k}={v}" for k, v in env_note.items()) + " " + shlex.join(real)
            full = ["ssh", "-o", "BatchMode=yes", "-o", "ControlMaster=auto", "-o", "ControlPath=/tmp/ab-ssh-%C",
                    "-o", "ControlPersist=120", *self.ssh, cmd]
            r = subprocess.run(full, capture_output=True, text=True, errors="replace")
        else:
            r = subprocess.run(real, cwd=self.dirs[tool], capture_output=True, text=True, errors="replace",
                               env={**os.environ, **env_note})
        return (r.stdout + r.stderr), r.returncode


class Episode:
    """One task attempt with one tool: records every call."""

    def __init__(self, ex, tool):
        self.ex, self.tool, self.calls, self.t_tool = ex, tool, [], 0.0

    def __call__(self, argv):
        t0 = time.time()
        out, rc = self.ex.run(self.tool, argv)
        dt = time.time() - t0
        raw = len(out.encode())
        trunc = raw > MAX_OUT
        if trunc:
            out = out.encode()[:MAX_OUT].decode(errors="ignore") + "\n[output truncated]"
        self.t_tool += dt
        self.calls.append({"cmd": shlex.join(argv), "rc": rc, "out_bytes": min(raw, MAX_OUT), "truncated": trunc,
                           "secs": round(dt, 4), "output": out})
        return out


def run_oracle(ex, task, tool):
    ep = Episode(ex, tool)
    t0 = time.time()
    try:
        ans = (task.git if tool == "git" else task.vcs)(ep)
        err = None
    except Exception as e:  # a failed strategy is a wrong answer, recorded
        ans, err = None, f"{type(e).__name__}: {e}"
    return finish(task, tool, ep, ans, err, time.time() - t0, {"mode": "offline"})


def finish(task, tool, ep, ans, err, wall, extra):
    if isinstance(ans, (set, frozenset)):
        ans = sorted(ans)
    return {"task": task.id, "category": task.cat, "tool": tool, "answer": ans, "expected": task.answer,
            "correct": bool(err is None and ans is not None and TK.grade(task, ans)), "error": err,
            "tool_calls": len(ep.calls), "out_bytes": sum(c["out_bytes"] for c in ep.calls),
            "tool_secs": round(ep.t_tool, 4), "wall_secs": round(wall, 4), "calls": ep.calls, **extra}


# ---------------------------------------------------------------- live

def chat(key, messages):
    body = {"model": MODEL, "messages": messages, "temperature": 0.7, "max_tokens": 600,
            "tools": [{"type": "function", "function": {
                "name": "run", "description": "Run one read-only command (git or vcs, as stated) and return its output.",
                "parameters": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}}}]}
    req = urllib.request.Request("https://api.openai.com/v1/chat/completions", json.dumps(body).encode(),
                                 {"Content-Type": "application/json", "Authorization": "Bearer " + key})
    with urllib.request.urlopen(req, timeout=120) as r:  # only api.openai.com is ever contacted
        return json.loads(r.read())


def parse_answer(text):
    for l in reversed((text or "").splitlines()):
        if l.strip().startswith("ANSWER:"):
            s = l.split("ANSWER:", 1)[1].strip()
            try:
                return json.loads(s)
            except Exception:
                return s.strip("`\" ")
    return None


def run_live(ex, task, tool, key, prompt="v1"):
    ep = Episode(ex, tool)
    sysmsg = PREAMBLE[tool].format(max=MAX_OUT) + (V2_VCS_HINT if tool == "vcs" and prompt == "v2" else "")
    msgs = [{"role": "system", "content": sysmsg}, {"role": "user", "content": task.question}]
    tin = tout = 0
    t0 = time.time()
    ans, err, llm_secs = None, None, 0.0
    try:
        for step in range(MAX_CALLS + 3):
            t1 = time.time()
            r = chat(key, msgs)
            llm_secs += time.time() - t1
            tin += r["usage"]["prompt_tokens"]
            tout += r["usage"]["completion_tokens"]
            m = r["choices"][0]["message"]
            msgs.append({k: v for k, v in m.items() if v is not None and k in ("role", "content", "tool_calls")})
            if not m.get("tool_calls"):
                ans = parse_answer(m.get("content"))
                if ans is None:
                    err = "no ANSWER line"
                break
            for tc in m["tool_calls"]:
                try:
                    cmd = json.loads(tc["function"]["arguments"])["command"]
                    argv = shlex.split(cmd)
                except Exception as e:
                    out = f"error: bad command ({e})"
                else:
                    out = "error: call limit reached; reply with your final `ANSWER: <json>` now" if len(ep.calls) >= MAX_CALLS else ep(argv)
                msgs.append({"role": "tool", "tool_call_id": tc["id"], "content": out})
    except Exception as e:
        err = f"{type(e).__name__}: {str(e)[:200]}"
    cost = tin * PRICE_IN + tout * PRICE_OUT
    return finish(task, tool, ep, ans, err, time.time() - t0,
                  {"mode": "live", "model": MODEL, "prompt": prompt, "llm_secs": round(llm_secs, 3), "tokens_in": tin, "tokens_out": tout, "cost_usd": round(cost, 5)})


# ---------------------------------------------------------------- reporting

def summarize(rows):
    out = {}
    for tool in ("git", "vcs"):
        r = [x for x in rows if x["tool"] == tool]
        if not r:
            continue
        n = len(r)
        out[tool] = {"episodes": n, "correct": sum(x["correct"] for x in r), "accuracy": round(sum(x["correct"] for x in r) / n, 3),
                     "mean_calls": round(sum(x["tool_calls"] for x in r) / n, 2), "mean_out_bytes": round(sum(x["out_bytes"] for x in r) / n),
                     "mean_wall_secs": round(sum(x["wall_secs"] for x in r) / n, 3), "mean_tool_secs": round(sum(x["tool_secs"] for x in r) / n, 4)}
        if "tokens_in" in r[0]:
            out[tool].update(mean_tokens_in=round(sum(x["tokens_in"] for x in r) / n), cost_usd=round(sum(x["cost_usd"] for x in r), 4))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mode", choices=["offline", "live"], default="offline")
    ap.add_argument("--vcs-bin", default=os.environ.get("VCS_BIN", "vcs"))
    ap.add_argument("--fixture", help="existing fixture dir with git/ and vcs/ (default: build a fresh one in a temp dir)")
    ap.add_argument("--build-fixture", metavar="DIR", help="build the fixture into DIR and exit")
    ap.add_argument("--ssh-target", help="run tools over ssh on this user@host (the repos live there; --fixture is a path there)")
    ap.add_argument("--ssh-key", help="ssh identity file")
    ap.add_argument("--prompt", choices=["v1", "v2"], default="v1", help="v2 adds a vcs hint about per-zone history (written after seeing v1 results)")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--tasks", help="comma-separated task ids (default all)")
    ap.add_argument("--budget-usd", type=float, default=0.9)
    ap.add_argument("--out", help="write raw JSONL here")
    ap.add_argument("--list", action="store_true")
    a = ap.parse_args()
    all_tasks = TK.make_tasks()
    if a.list:
        for t in all_tasks:
            print(f"{t.id:20} {t.cat:13} {t.question}")
        return
    if a.build_fixture:
        fixture.build(a.build_fixture, a.vcs_bin)
        return
    fx = a.fixture
    if not fx:
        import tempfile
        fx = tempfile.mkdtemp(prefix="agentbench-")
        fixture.build(fx, a.vcs_bin)
    ssh = (["-i", a.ssh_key] if a.ssh_key else []) + [a.ssh_target] if a.ssh_target else None
    ex = Executor({"git": os.path.join(fx, "git"), "vcs": os.path.join(fx, "vcs")}, a.vcs_bin, ssh)
    sel = [t for t in all_tasks if not a.tasks or t.id in a.tasks.split(",")]
    key = None
    if a.mode == "live":
        key = os.environ.get("OPENAI_API_KEY")
        if not key:
            sys.exit("live mode needs OPENAI_API_KEY in the environment")
    rows, spent = [], 0.0
    reps = 1 if a.mode == "offline" else a.repeats
    outf = open(a.out, "w") if a.out else None
    for rep in range(reps):
        for t in sel:
            for tool in ("git", "vcs"):
                if a.mode == "live" and spent >= a.budget_usd:
                    print(f"budget reached (${spent:.3f}); stopping", file=sys.stderr)
                    reps = rep + 1 if rep else 0
                    break
                row = run_oracle(ex, t, tool) if a.mode == "offline" else run_live(ex, t, tool, key, a.prompt)
                row["repeat"] = rep
                spent += row.get("cost_usd", 0)
                rows.append(row)
                if outf:
                    outf.write(json.dumps(row) + "\n")
                    outf.flush()
                print(f"[{rep}] {t.id:20} {tool:3} {'ok ' if row['correct'] else 'BAD'} calls={row['tool_calls']:2} bytes={row['out_bytes']:6}"
                      f" wall={row['wall_secs']:.2f}s" + (f" cost=${spent:.3f}" if a.mode == "live" else ""), flush=True)
            else:
                continue
            break
        else:
            continue
        break
    print(json.dumps(summarize(rows), indent=1))


if __name__ == "__main__":
    main()
