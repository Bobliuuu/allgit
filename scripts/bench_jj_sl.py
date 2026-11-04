#!/usr/bin/env python3
"""vcs vs Jujutsu (jj) vs Sapling (sl, Git-backed store), per command, interleaved, one dataset.

usage: scripts/bench_jj_sl.py <flat|deep|binary> [n=100000] [runs=5]
env:   VCS_BIN, DATAGEN, JJ_BIN, SL_BIN, BENCH_TMP (scratch parent), TOOLS (default "vcs jj sl"),
       OUT (JSON result path), CELLS (comma list of cell-name prefixes to time; setup still runs)
Method: every cell runs RUNS times; inside each run the tools are visited in a rotating order (interleaved),
each run preceded by untimed prep. Wall ms and CPU ms (user+sys of the child tree via wait4), load1 before/after.
Slow cells are cut: a first run (prep+timed) over SLOW_S total keeps 3 runs, over VERYSLOW_S keeps 2; the
number of runs actually taken (n) is stored per cell. Commands that a tool does not have are absent (N/A).
Mapping: jj/sl have no staging area; jj's `add` is the first snapshot (`jj status`), sl `sl add .`.
"""
import json, os, shutil, statistics, subprocess, sys, tempfile, time

layout = sys.argv[1] if len(sys.argv) > 1 else "flat"
N = int(sys.argv[2]) if len(sys.argv) > 2 else 100000
RUNS = int(sys.argv[3]) if len(sys.argv) > 3 else 5
VCS = os.path.abspath(os.environ.get("VCS_BIN", "vcs"))
DATAGEN = os.path.abspath(os.environ.get("DATAGEN", "datagen"))
JJ = os.path.abspath(os.environ.get("JJ_BIN", "jj"))
SL = os.path.abspath(os.environ.get("SL_BIN", "sl"))
TOOLS = os.environ.get("TOOLS", "vcs jj sl").split()
PARENT = os.environ.get("BENCH_TMP", tempfile.gettempdir())
OUT = os.environ.get("OUT", "")
ONLY = os.environ.get("CELLS", "").split(",") if os.environ.get("CELLS") else None
SLOW_S, VERYSLOW_S, CMD_TIMEOUT = 150, 600, 3000
WORK = tempfile.mkdtemp(prefix="jjsl.", dir=PARENT)

BIN = os.path.join(WORK, "bin"); os.makedirs(BIN)
open(os.path.join(BIN, "sl"), "w").write(f'#!/bin/sh\nexec {SL} --config "ui.username=b <b@x.invalid>" "$@"\n')
os.chmod(os.path.join(BIN, "sl"), 0o755)
env = dict(os.environ)
env.update(GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", JJ_USER="b", JJ_EMAIL="b@x.invalid",
           JJ_CONFIG="/dev/null", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", HGPLAIN="1")
env["PATH"] = ":".join([BIN, os.path.dirname(JJ), os.path.dirname(VCS), env["PATH"]])

def load1(): return open("/proc/loadavg").read().split()[0]

def sh(cmd, cwd, show=False):
    r = subprocess.run(["bash", "-c", cmd], cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=CMD_TIMEOUT)
    if r.returncode != 0 and show:
        print(f"PREP FAILED rc={r.returncode} ({cmd!r} in {cwd}): {r.stderr.decode()[-300:]}", file=sys.stderr)
    return r.returncode

def prep(cmd, cwd): return sh(cmd, cwd, True)

def timed(cmd, cwd):
    t = time.perf_counter()
    p = subprocess.Popen(["bash", "-c", cmd], cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    _, status, ru = os.wait4(p.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    err = p.stderr.read().decode(); p.stderr.close()
    rc = os.waitstatus_to_exitcode(status)
    if rc != 0:
        print(f"TIMED CMD FAILED rc={rc} ({cmd!r}): {err[-300:]}", file=sys.stderr)
    return wall, (ru.ru_utime + ru.ru_stime) * 1000, rc

_files = {}
def files(root):
    if root not in _files:
        out = []
        for d, ds, fs in os.walk(root):
            ds[:] = [x for x in ds if x not in (".git", ".vcs", ".jj", ".sl")]
            out += [os.path.join(d, f) for f in fs]
        out.sort(); _files[root] = out
    return _files[root]

def modify_1pct(root, tag):
    for i, p in enumerate(files(root)[::100]):
        with open(p, "wb") as fh: fh.write((f"modified {tag} {i}\n").encode() * 8)

def write_at(root, idx, tag):
    fs = files(root)
    with open(fs[idx % len(fs)], "wb") as fh: fh.write(f"{tag}\n".encode() * 4)

def touch_one(root, k): write_at(root, k * 997, f"history {k}")

C = {
 "vcs": dict(init="vcs init", add="vcs add .", commit="vcs commit -m c", status="vcs status", diff="vcs diff",
             log="vcs log -n 50", blame="vcs blame {f}", branch="vcs branch {b}", side="vcs branch side",
             sw_side="vcs switch side", sw_main="vcs switch main", undo="vcs op undo", gc="vcs gc",
             store=".vcs", reset="rm -rf .vcs"),
 "jj":  dict(init="jj git init --no-colocate", add="jj status", commit="jj commit -m c", status="jj status", diff="jj diff --git",
             log="jj log --no-graph -n 50", blame="jj file annotate {f}", branch="jj bookmark create {b} -r @-",
             side="jj bookmark create side -r @-", sw_side="jj edit side --quiet", sw_main="jj edit main --quiet",
             undo="jj undo --quiet", gc="jj util gc", describe="jj describe -m x{i} --quiet", store=".jj", reset="rm -rf .jj"),
 "sl":  dict(init="sl init --git", add="sl add .", commit="sl commit -m c", status="sl status", diff="sl diff --git",
             log="sl log -l 50 --template '{node|short} {desc|firstline}\\n'", blame="sl annotate {f}", branch="sl bookmark {b}",
             side="sl bookmark side", sw_side="sl goto -q side", sw_main="sl goto -q main", undo="sl undo -f",
             describe="sl metaedit -m x{i}", store=".sl", reset="rm -rf .sl"),
}
def commit_all(t):  # one commit of the current working copy
    c = C[t]; return (c["add"] + "; " if t == "vcs" else "") + c["commit"]

wd = {t: os.path.join(WORK, t) for t in TOOLS}
results = []

def cell(name, prepf, cmdf, tools=None):
    if ONLY and not any(name.startswith(o) for o in ONLY): return
    tools = [t for t in (tools or TOOLS) if t in TOOLS]
    if not tools: return
    w = {t: [] for t in tools}; c = {t: [] for t in tools}; fails = {t: 0 for t in tools}
    t_q = time.time()
    while float(load1()) >= float(os.environ.get("BENCH_MAX_LOAD1", "1.0")) and time.time() - t_q < 600: time.sleep(2)
    l0 = load1(); runs = RUNS; i = 0; worst_first = 0
    while i < runs:
        order = tools[i % len(tools):] + tools[:i % len(tools)]
        for t in order:
            t0 = time.perf_counter()
            prepf(t, i)
            wm, cm, rc = timed(cmdf(t, i), wd[t])
            w[t].append(wm); c[t].append(cm); fails[t] += rc != 0
            if i == 0: worst_first = max(worst_first, time.perf_counter() - t0)
        if i == 0:
            if worst_first > VERYSLOW_S: runs = min(runs, 2)
            elif worst_first > SLOW_S: runs = min(runs, 3)
        i += 1
    l1 = load1()
    for t in tools:
        results.append(dict(cell=name, tool=t, wall=w[t], cpu=c[t], load0=l0, load1=l1, fails=fails[t], n=len(w[t])))
        print(f"| {name} | {t} | {' '.join('%.0f' % x for x in w[t])} | {statistics.median(w[t]):.0f} | {statistics.median(c[t]):.0f} | {l0} -> {l1} | n={len(w[t])}"
              + (f" FAILED x{fails[t]}" if fails[t] else ""), flush=True)

def ver(cmd):
    out = subprocess.run(cmd, shell=True, env=env, capture_output=True, text=True).stdout.strip().splitlines()
    return out[0] if out else "not installed"

meta = {}
try:
    print("## Environment")
    cpu = [l for l in open("/proc/cpuinfo") if l.startswith("model name")][0].split(":")[1].strip()
    vers = {"git": ver("git --version"), "vcs": ver(VCS + " --version"), "jj": ver(JJ + " --version"), "sl": ver(SL + " --version")}
    meta = dict(date=time.strftime("%FT%TZ", time.gmtime()), uname=os.uname().release, cpu=cpu, cores=os.cpu_count(),
                versions=vers, build_sha=os.environ.get("BUILD_SHA"), layout=layout, files=N, runs=RUNS, load_start=load1(),
                fs=subprocess.run(["df", "-T", WORK], capture_output=True, text=True).stdout.splitlines()[1].split()[1])
    print(json.dumps(meta))
    print("\n| cell | tool | wall ms per run | median wall ms | median cpu ms | load1 | runs |")
    print("|---|---|---|---|---|---|---|")
    for t in TOOLS:
        os.makedirs(wd[t]); subprocess.run([DATAGEN, layout, str(N), wd[t]], stdout=subprocess.DEVNULL, check=True, env=env)

    rs = lambda t: f"{C[t]['reset']}; {C[t]['init']}"
    cell("init (empty dir)", lambda t, i: prep(C[t]["reset"], wd[t]), lambda t, i: C[t]["init"])
    cell("add/track (all files)", lambda t, i: prep(rs(t), wd[t]), lambda t, i: C[t]["add"])
    cell("commit (all files)", lambda t, i: prep(f"{rs(t)}; {C[t]['add']}", wd[t]), lambda t, i: C[t]["commit"])
    for t in TOOLS:
        prep(f"{rs(t)}; {C[t]['add']}; {C[t]['commit']}", wd[t])
        prep(C[t]["side"], wd[t])
    time.sleep(3)
    cell("status (clean)", lambda t, i: None, lambda t, i: C[t]["status"])
    cell("status (1% modified)", lambda t, i: modify_1pct(wd[t], f"s{i}"), lambda t, i: C[t]["status"])
    cell("diff (1% modified)", lambda t, i: modify_1pct(wd[t], f"d{i}"), lambda t, i: C[t]["diff"])
    for t in TOOLS:   # 1% commit + 48 single-file commits => 50-commit history on main
        modify_1pct(wd[t], "a"); prep(commit_all(t), wd[t])
        for k in range(1, 49):
            touch_one(wd[t], k); prep(commit_all(t), wd[t])
        if t == "jj": prep("jj bookmark create main -r @-", wd[t])
        if t == "sl": prep("sl bookmark main", wd[t])
    cell("log (50 commits)", lambda t, i: None, lambda t, i: C[t]["log"])
    BF = lambda t: C[t]["blame"].format(f=os.path.relpath(files(wd[t])[0], wd[t]))
    cell("blame/annotate (1 file)", lambda t, i: None, lambda t, i: BF(t))
    cell("describe/metaedit (message)", lambda t, i: None, lambda t, i: C[t]["describe"].format(i=i), tools=["jj", "sl"])
    cell("branch/bookmark (create)", lambda t, i: None, lambda t, i: C[t]["branch"].format(b=f"bm{i}"))
    cell("checkout/switch (1% differ)", lambda t, i: prep(C[t]["sw_side"], wd[t]), lambda t, i: C[t]["sw_main"])
    cell("amend (1 file)", lambda t, i: (touch_one(wd[t], 200 + i), prep("jj status" if t == "jj" else "true", wd[t])),
         lambda t, i: "jj squash -u --quiet" if t == "jj" else "sl amend", tools=["jj", "sl"])
    cell("undo (last commit)", lambda t, i: (touch_one(wd[t], 100 + i), prep(commit_all(t), wd[t])), lambda t, i: C[t]["undo"])

    # merge / rebase: per run a feature commit on top of `side` editing a file the main line never touched
    def feat(t, i, kind, then_main):
        c = C[t]; r = f"{kind}{i}"; idx = (50 if kind == "f" else 60) + 100 * i
        if t == "vcs":
            prep(f"{c['sw_side']}", wd[t]); write_at(wd[t], idx, r)
            prep(f"vcs add .; vcs commit -m {r}; vcs branch {r}" + (f"; {c['sw_main']}" if then_main else ""), wd[t])
        elif t == "jj":
            prep(f"jj new side -m {r} --quiet", wd[t]); write_at(wd[t], idx, r)
            prep(f"jj bookmark create {r} -r @; jj status", wd[t])
        else:
            prep(f"{c['sw_side']}", wd[t]); write_at(wd[t], idx, r)
            prep(f"sl commit -m {r}; sl bookmark {r}" + (f"; {c['sw_main']}" if then_main else ""), wd[t])
    def merge_cmd(t, i):
        r = f"f{i}"
        return {"vcs": f"vcs merge {r}", "jj": f"jj new main {r} -m merge --quiet", "sl": f"sl merge -q {r} && sl commit -m merge"}[t]
    cell("merge (disjoint files)", lambda t, i: feat(t, i, "f", True), merge_cmd)
    def rebase_cmd(t, i):
        r = f"r{i}"
        return {"vcs": "vcs rebase main", "jj": f"jj rebase -b {r} -d main --quiet", "sl": f"sl rebase -q -s {r} -d main"}[t]
    cell("rebase (1 commit onto main)", lambda t, i: feat(t, i, "r", False), rebase_cmd)
    for t in TOOLS: prep(C[t]["sw_main"], wd[t])   # back on main (jj: its tip may be a rebased/merge child; main bookmark is what we push)
    for t in TOOLS:
        if t == "jj": prep("jj bookmark set main -r @- --allow-backwards --quiet", wd[t])

    # remotes: vcs directory remote; jj and sl push to a bare Git repo
    SRC = {"vcs": os.path.join(WORK, "remote-vcs"), "jj": os.path.join(WORK, "src-jj"), "sl": os.path.join(WORK, "src-sl")}
    for t in TOOLS:
        if t == "vcs": prep(f"vcs remote set {SRC[t]}", wd[t])
        else:
            prep(f"git init -q --bare -b main {SRC[t]}", WORK)
            prep(f"jj git remote add origin {SRC[t]}" if t == "jj" else f"sl paths --add origin {SRC[t]}", wd[t])
    PUSH = {"vcs": "vcs publish", "jj": "jj git push --remote origin -b main", "sl": "sl push --to main -r main origin"}
    def bump(t, k):   # new commit on main
        touch_one(wd[t], 300 + k)
        prep(commit_all(t), wd[t])
        if t == "jj": prep("jj bookmark set main -r @- --allow-backwards --quiet", wd[t])
        if t == "sl": prep("sl bookmark -f -r . main", wd[t])
    for t in TOOLS:   # untimed initial full push
        prep(PUSH[t], wd[t])
    cell("push (1 new commit)", lambda t, i: bump(t, i), lambda t, i: PUSH[t])
    for t in TOOLS:
        if t != "vcs": prep(f"git -C {SRC[t]} repack -adq", WORK)
    CL = {"vcs": f"vcs clone {SRC['vcs']} {{d}}", "jj": f"jj git clone --quiet --no-colocate {SRC['jj']} {{d}}", "sl": f"sl clone --git -q {SRC['sl']} {{d}}"}
    cd = lambda t: os.path.join(WORK, "clone-" + t)
    cell("clone (local remote, full history)", lambda t, i: shutil.rmtree(cd(t), ignore_errors=True), lambda t, i: CL[t].format(d=cd(t)))
    # fetch: a clone per tool; untimed prep pushes one new commit from the work dir
    for t in TOOLS:
        shutil.rmtree(cd(t), ignore_errors=True); prep(CL[t].format(d=cd(t)), WORK)
    FETCH = {"vcs": "vcs fetch", "jj": "jj git fetch", "sl": "sl pull -q"}
    def fetch_prep(t, i):
        bump(t, 100 + i); prep(PUSH[t], wd[t])
    wd_save = dict(wd)
    cell("fetch/pull (1 new commit)", fetch_prep, lambda t, i: "cd " + cd(t) + " && " + FETCH[t])
    cell("gc (local)", lambda t, i: None, lambda t, i: C[t]["gc"], tools=["vcs", "jj"])  # sl: `sl gc` is "no longer supported"
    print("load1 at end:", load1())
finally:
    if OUT:
        json.dump(dict(meta=meta, cells=results), open(OUT, "w"), indent=1)
    shutil.rmtree(WORK, ignore_errors=True)
