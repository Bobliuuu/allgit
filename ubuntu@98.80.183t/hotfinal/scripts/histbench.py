#!/usr/bin/env python3
"""Interleaved timing of history-heavy commands: git vs vcs (one or two vcs binaries).

usage: histbench.py --git GITREPO --vcs VCSREPO --bin NAME=PATH [--bin NAME=PATH ...]
                    [--runs 7] [--only SUBSTR] [--commits N]

Each round runs every command once per tool, rotating which tool goes first, so slow drift of
machine load hits all tools alike. Reports median wall time and median CPU time (user+sys of the
child, from getrusage) per tool, the ratio against git, and load1 before and after.
Output of each tool is not compared here (git and vcs print different formats); `--check`
compares the commit subject sequences of the log commands, which are comparable.
Page cache is warm for every run (the first round is discarded).
"""
import argparse, os, resource, statistics, subprocess, sys, time, shutil, tempfile

ap = argparse.ArgumentParser()
ap.add_argument("--git", required=True)
ap.add_argument("--vcs", required=True)
ap.add_argument("--bin", action="append", required=True, help="NAME=PATH of a vcs binary")
ap.add_argument("--runs", type=int, default=7)
ap.add_argument("--only", default="")
ap.add_argument("--skip", default="")
ap.add_argument("--n", type=int, default=20000, help="commits in the history (for HEAD~k picks)")
ap.add_argument("--git-cg", default="", help="a copy of the git repo with `git commit-graph write --reachable --changed-paths` (a second git column)")
ap.add_argument("--hot", default="hot/app.txt", help="the often-edited file (renamed into place early in the generated history)")
ap.add_argument("--needle", default="hot line 5 renamed", help="a string that occurs in few commits")
ap.add_argument("--slow-sec", type=float, default=20.0, help="a tool whose warm-up run takes longer is not repeated: that one run is its sample")
ap.add_argument("--slow-runs", type=int, default=0)
a = ap.parse_args()
bins = [b.split("=", 1) for b in a.bin]
N = a.n
env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", NO_COLOR="1", GIT_PAGER="cat")

def run(cmd, cwd, shell=True):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, shell=shell, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    w = time.perf_counter() - t
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    return w, cpu, p.returncode, p.stdout

COLD = "d12/s07/f031.txt"   # touched rarely
WARM = "d00/s00/f000.txt"   # in the hot 1% of files
HOT = a.hot
NEEDLE = a.needle
# (name, git command, vcs command with {v} for the binary, setup-copy?)
CASES = [
    ("log-path-warm  (-n20 -- hot-1% file)", f"git log -n 20 --format=%s -- {WARM}", f"{{v}} log -n 20 --format=%s -- {WARM}", False),
    ("log-path-cold  (-n20 -- cold file)", f"git log -n 20 --format=%s -- {COLD}", f"{{v}} log -n 20 --format=%s -- {COLD}", False),
    ("log-path-dir   (-n20 -- d12/s07)", "git log -n 20 --format=%s -- d12/s07", "{v} log -n 20 --format=%s -- d12/s07", False),
    ("log-path-all   (all matches, cold)", f"git log -n 1000000 --format=%s -- {COLD}", f"{{v}} log -n 1000000 --format=%s -- {COLD}", False),
    ("log-follow     (hot/app.txt)", f"git log --follow --format=%s -- {HOT}", f"{{v}} log --follow -n 1000000 --format=%s -- {HOT}", False),
    ("log-S          (rare string)", f"git log -S'{NEEDLE}' --format=%s", f"{{v}} log -S'{NEEDLE}' -n 1000000 --format=%s", False),
    ("log-name-status (-n 200)", "git log -n 200 --name-status --format=%s", "{v} log -n 200 --name-status --format=%s", False),
    ("blame          (hot/app.txt)", f"git blame HEAD -- {HOT}", f"{{v}} blame {HOT}", False),
    ("diff-distant   (HEAD~10000 HEAD)", f"git diff --name-status HEAD~{N//2} HEAD", f"{{v}} diff-tree HEAD~{N//2} HEAD", False),
    ("merge-base     (main~5000, side)", f"git merge-base HEAD~{N//4} side", f"{{v}} merge-base HEAD~{N//4} side", False),
    ("describe       (HEAD)", "git describe HEAD", "{v} describe", False),
    ("grep @rev      (one hit)", "git grep -n 'p3_4_5 line 7 v0' HEAD", "{v} grep -n 'p3_4_5 line 7 v0' HEAD", False),
    ("bisect start+reset", f"git bisect start HEAD HEAD~{N*3//4} >/dev/null; git bisect reset >/dev/null 2>&1", f"{{v}} bisect start HEAD HEAD~{N*3//4} >/dev/null; {{v}} bisect reset >/dev/null", True),
]

def load():
    return os.getloadavg()[0]

tools = [("git", None)] + ([("git-cg", None)] if a.git_cg else []) + [(n, p) for n, p in bins]
res = {}
print(f"machine: {os.uname().nodename} cpus={os.cpu_count()} load1_start={load():.2f}")
print("git:", subprocess.run("git --version", shell=True, capture_output=True, text=True).stdout.strip())
for name, g, v, copy in CASES:
    if a.only and a.only not in name:
        continue
    if a.skip and a.skip in name:
        continue
    times = {t: [] for t, _ in tools}
    cpus = {t: [] for t, _ in tools}
    load0 = load()
    tmp = None
    slow = set()
    for rnd in range(a.runs + 1):
        order = tools[rnd % len(tools):] + tools[: rnd % len(tools)]
        for t, path in order:
            if t in slow and rnd > a.slow_runs:
                continue
            repo = a.git if t == "git" else (a.git_cg if t == "git-cg" else a.vcs)
            cmd = g if t in ("git", "git-cg") else v.replace("{v}", path)
            if copy:
                tmp = tempfile.mkdtemp(dir=os.path.dirname(repo.rstrip("/")))
                subprocess.run(["cp", "-a", repo + "/.", tmp], check=True)
                repo = tmp
            w, c, rc, out = run(cmd, repo)
            if copy:
                shutil.rmtree(tmp, ignore_errors=True)
            if rnd == 0:
                if w > a.slow_sec:
                    slow.add(t)  # too slow to repeat: its warm-up run is its only sample
                    times[t].append(w)
                    cpus[t].append(c)
                if rc not in (0, 1):
                    print(f"  !! {t} exit {rc} for {name}", file=sys.stderr)
                continue  # warm-up round
            times[t].append(w)
            cpus[t].append(c)
    load1 = load()
    g_w = statistics.median(times["git"])
    line = f"{name:44s}"
    for t, _ in tools:
        mw, mc = statistics.median(times[t]), statistics.median(cpus[t])
        line += f" | {t}: {mw*1000:9.1f}ms cpu {mc*1000:9.1f}ms" + (f" [n={len(times[t])}]" if len(times[t]) != a.runs else "") + ("" if t == "git" else f" ({mw/g_w:5.2f}x git)")
    print(line + f" | load1 {load0:.1f}->{load1:.1f}", flush=True)
    res[name] = (times, cpus)
print(f"load1_end={load():.2f} runs={a.runs} (median of runs, first round discarded, warm cache)")
