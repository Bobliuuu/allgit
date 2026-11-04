#!/usr/bin/env python3
"""git vs vcs vs Jujutsu (jj, git backend) on one generated dataset, interleaved runs.

usage: scripts/bench_jj.py <flat|deep|binary> [n=100000] [runs=5]
env:   VCS_BIN, DATAGEN, JJ_BIN, BENCH_TMP (scratch parent), TOOLS (default "git vcs jj")
Every cell runs RUNS times; inside each run the tools are visited in a rotating order (interleaved),
each with untimed prep. Prints wall ms and CPU ms (user+sys of the child process tree, via wait4) per run, medians,
and load1 before/after each cell. Same dataset (datagen, seed 42) in one work dir per tool.
Mapping: add -> git/vcs `add .`, jj first snapshot (`jj status`, jj has no staging area); commit -> `commit -m c`
(jj `commit -m c`); checkout -> git checkout / vcs switch / jj edit; undo -> git `reset --hard HEAD~1`
(not an op-log undo, git has none), vcs `op undo`, jj `undo`.
"""
import os, shutil, subprocess, sys, tempfile, statistics, time

layout = sys.argv[1] if len(sys.argv) > 1 else "flat"
N = int(sys.argv[2]) if len(sys.argv) > 2 else 100000
RUNS = int(sys.argv[3]) if len(sys.argv) > 3 else 5
VCS = os.environ.get("VCS_BIN", "vcs")
DATAGEN = os.environ.get("DATAGEN", "datagen")
JJ = os.environ.get("JJ_BIN", "jj")
TOOLS = os.environ.get("TOOLS", "git vcs jj").split()
PARENT = os.environ.get("BENCH_TMP", tempfile.gettempdir())
WORK = tempfile.mkdtemp(prefix="vcs-jj.", dir=PARENT)

env = dict(os.environ)
env.update(GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", JJ_USER="b", JJ_EMAIL="b@x.invalid",
           JJ_CONFIG="/dev/null", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid")
env["PATH"] = os.path.dirname(os.path.abspath(JJ)) + ":" + os.path.dirname(os.path.abspath(VCS)) + ":" + env["PATH"]

def load1():
    return open("/proc/loadavg").read().split()[0]

def sh(cmd, cwd):
    return subprocess.run(["bash", "-c", cmd], cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

def prep(cmd, cwd):
    r = sh(cmd, cwd)
    if r.returncode != 0:
        print(f"PREP FAILED ({cmd!r} in {cwd}): {r.stderr.decode()[-300:]}", file=sys.stderr)

def timed(cmd, cwd):
    t = time.perf_counter()
    p = subprocess.Popen(["bash", "-c", cmd], cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    _, status, ru = os.wait4(p.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    err = p.stderr.read().decode()
    p.stderr.close()
    rc = os.waitstatus_to_exitcode(status)
    if rc != 0:
        print(f"TIMED CMD FAILED rc={rc} ({cmd!r}): {err[-300:]}", file=sys.stderr)
    return wall, (ru.ru_utime + ru.ru_stime) * 1000, rc

def files(root):
    out = []
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs", ".jj")]
        out += [os.path.join(d, f) for f in fs]
    out.sort()
    return out

def modify_1pct(root, tag):
    for i, p in enumerate(files(root)[::100]):
        with open(p, "wb") as fh:
            fh.write((f"modified {tag} {i}\n").encode() * 8)

def touch_one(root, k):
    fs = files(root)
    with open(fs[k * 997 % len(fs)], "wb") as fh:
        fh.write(f"history {k}\n".encode())

CMD = {
 "git": dict(init="git init -q -b main", add="git add .", commit="git commit -q -m c", addall="git add -A", status="git status --porcelain",
             log="git log --oneline -n 50", opl="git reflog -n 20", store=".git", reset="rm -rf .git"),
 "vcs": dict(init="vcs init", add="vcs add .", commit="vcs commit -m c", addall="vcs add .", status="vcs status",
             log="vcs log -n 50", opl="vcs op log -n 20", store=".vcs", reset="rm -rf .vcs"),
 "jj":  dict(init="jj git init --no-colocate", add="jj status", commit="jj commit -m c", addall="true", status="jj status",
             log="jj log --no-graph -n 50", opl="jj op log --no-graph -n 20", store=".jj", reset="rm -rf .jj"),
}
def commit_all(t):  # one commit of the current working copy
    c = CMD[t]; return f"{c['addall']}; {c['commit']}"

wd = {t: os.path.join(WORK, t) for t in TOOLS}
results = []  # (cell, tool, [wall], [cpu], load0, load1)

ONLY = os.environ.get("CELLS", "").split(",") if os.environ.get("CELLS") else None  # e.g. CELLS=clone: time only cells whose name starts with one of these

def cell(name, prepf, cmdf, tools=None):
    if ONLY and not any(name.startswith(o) for o in ONLY):
        return
    tools = tools or TOOLS
    w = {t: [] for t in tools}; c = {t: [] for t in tools}; fails = {t: 0 for t in tools}
    l0 = load1()
    for i in range(RUNS):
        order = tools[i % len(tools):] + tools[:i % len(tools)]   # rotate: interleaved, no fixed first tool
        for t in order:
            prepf(t, i)
            wm, cm, rc = timed(cmdf(t, i), wd[t])
            w[t].append(wm); c[t].append(cm); fails[t] += rc != 0
    l1 = load1()
    for t in tools:
        results.append((name, t, w[t], c[t], l0, l1, fails[t]))
        print(f"| {name} | {t} | {' '.join('%.0f' % x for x in w[t])} | {statistics.median(w[t]):.0f} | {statistics.median(c[t]):.0f} | {l0} -> {l1} |"
              + (f" FAILED x{fails[t]}" if fails[t] else ""), flush=True)

def du(t):
    return int(subprocess.run(["du", "-sk", os.path.join(wd[t], CMD[t]["store"])], capture_output=True, text=True).stdout.split()[0])

def ver(cmd):
    return subprocess.run(cmd, shell=True, env=env, capture_output=True, text=True).stdout.strip().splitlines()[0]

try:
    print("## Environment")
    print("date:", time.strftime("%FT%TZ", time.gmtime()), "uname:", os.uname().release, os.uname().machine)
    cpu = [l for l in open("/proc/cpuinfo") if l.startswith("model name")][0].split(":")[1].strip()
    print(f"cpu: {cpu} cores={os.cpu_count()}; fs: {subprocess.run(['df','-T',WORK],capture_output=True,text=True).stdout.splitlines()[1].split()[1]}")
    print("git:", ver("git --version"), "| jj:", ver(JJ + " --version"), "| vcs:", ver(VCS + " --version"))
    print(f"dataset: layout={layout} files={N} seed=42 runs={RUNS} warm cache; load1 at start {load1()}")
    print("\n| cell | tool | wall ms per run | median wall ms | median cpu ms | load1 |")
    print("|---|---|---|---|---|---|")
    for t in TOOLS:
        os.makedirs(wd[t]); subprocess.run([DATAGEN, layout, str(N), wd[t]], stdout=subprocess.DEVNULL, check=True, env=env)

    cell("add (git/vcs add ., jj first snapshot)", lambda t, i: prep(f"{CMD[t]['reset']}; {CMD[t]['init']}", wd[t]), lambda t, i: CMD[t]["add"])
    cell("commit (after add/snapshot)", lambda t, i: prep(f"{CMD[t]['reset']}; {CMD[t]['init']}; {CMD[t]['add']}", wd[t]), lambda t, i: CMD[t]["commit"])
    for t in TOOLS:
        prep(f"{CMD[t]['reset']}; {CMD[t]['init']}; {CMD[t]['add']}; {CMD[t]['commit']}", wd[t])
    # base commit exists; mark it `side`
    for t in TOOLS:
        prep({"git": "git branch side", "vcs": "vcs branch side", "jj": "jj bookmark create side -r @-"}[t], wd[t])
    sz1 = {t: du(t) for t in TOOLS}
    time.sleep(3)
    cell("status (clean)", lambda t, i: None, lambda t, i: CMD[t]["status"])
    cell("status (1% modified)", lambda t, i: modify_1pct(wd[t], f"s{i}") if True else None, lambda t, i: CMD[t]["status"])
    for t in TOOLS:
        modify_1pct(wd[t], "a"); prep(commit_all(t), wd[t])
        for k in range(1, 49):
            touch_one(wd[t], k); prep(commit_all(t), wd[t])
        if t == "jj": prep("jj bookmark create main -r @-", wd[t])
    sz50 = {t: du(t) for t in TOOLS}
    cell("log (50 commits)", lambda t, i: None, lambda t, i: CMD[t]["log"])
    SW = {"git": ("git checkout -q side", "git checkout -q main"), "vcs": ("vcs switch side", "vcs switch main"),
          "jj": ("jj edit side --quiet", "jj edit main --quiet")}
    cell("checkout (1% differ)", lambda t, i: prep(SW[t][0], wd[t]), lambda t, i: SW[t][1])
    # undo: prep makes a one-file commit; timed = undo it
    UNDO = {"git": "git reset -q --hard HEAD~1", "vcs": "vcs op undo", "jj": "jj undo --quiet"}
    def undo_prep(t, i):
        touch_one(wd[t], 100 + i); prep(commit_all(t), wd[t])
    cell("undo last commit (git: reset --hard)", undo_prep, lambda t, i: UNDO[t])
    cell("op log / reflog, 20 entries", lambda t, i: None, lambda t, i: CMD[t]["opl"])
    # clone (vcs clones from a published remote directory; the publish itself is untimed and not part of the cell)
    REMOTE = os.path.join(WORK, "remote-vcs")
    if "vcs" in TOOLS:
        prep(f"vcs remote set {REMOTE}; vcs publish", wd["vcs"])
    if "jj" in TOOLS:
        prep("jj git export; git --git-dir=.jj/repo/store/git symbolic-ref HEAD refs/heads/main", wd["jj"])  # non-colocated jj repos do not write git refs until exported
    CL = {"git": "git clone -q --no-local {s} {d}", "vcs": "vcs clone " + REMOTE + " {d}", "jj": "jj git clone --quiet --no-colocate {s}/.jj/repo/store/git {d}"}
    def clone_cmd(t, i): return CL[t].format(s=wd[t], d=os.path.join(WORK, "clone-" + t))
    def clone_prep(t, i): shutil.rmtree(os.path.join(WORK, "clone-" + t), ignore_errors=True)
    cell("clone (local path)", clone_prep, clone_cmd)
    print("\nfiles in last clone (working files, excluding store):", {t: len(files(os.path.join(WORK, "clone-" + t))) for t in TOOLS})
    print("clone store sizes KiB:", {t: int(subprocess.run(["du", "-sk", os.path.join(WORK, "clone-" + t, CMD[t]["store"])], capture_output=True, text=True).stdout.split()[0]) for t in TOOLS})
    print("store size KiB after first commit:", sz1)
    print("store size KiB after 50 commits (+ a few undo-free runs):", sz50)
    print("store size KiB at end (incl. undo/op history):", {t: du(t) for t in TOOLS})
    print("load1 at end:", load1())
finally:
    shutil.rmtree(WORK, ignore_errors=True)
