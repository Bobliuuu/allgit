#!/usr/bin/env python3
"""Fair `status` pair (docs/research/benchmark-audit.md #2). Linux box only.
usage: bench_status_fair.py <workdir> <out.json> [runs=9] [files=100000]
needs $CARGO_TARGET_DIR/release/{vcs,vcs-watchd,datagen} (default <repo>/target); python3 for scripts/fair_fsmon.py.
NOTE builtin fsmonitor--daemon is macOS/Windows only (git 2.43 on Linux: "not supported on this platform") and watchman is not installed,
so the fsmonitor arm uses scripts/fair_fsmon.py: an inotify daemon + hook protocol v2, with core.untrackedCache. Its CPU is counted.

Two identical trees (a .git next to a .vcs in ONE tree would make git list .vcs/ as untracked): t/g (git) and t/v (vcs).
Block A (no vcs daemon): git default | git untrackedCache | git fsmonitor hook+untrackedCache | vcs with VCS_NO_AUTOWATCH+VCS_NO_FSMONITOR.
Block B (vcs-watchd running): git default | git untrackedCache | git fsmonitor hook+untrackedCache | vcs using the journal. Arms interleave per round.
Phases: clean tree, then 1% of the files modified. Per run: wall + client CPU (rusage). Daemon CPU (/proc/<pid>/stat) over the
measured window is divided over the runs and added to the summary's total CPU; 60 s idle CPU of each daemon is recorded too.
Caches: warm page + dentry cache (the tree was just written). Every measured run takes the shared box lock."""
import json, os, shutil, signal, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fair_lib import *

W = os.path.abspath(sys.argv[1])
OUT = sys.argv[2]
N = int(sys.argv[3]) if len(sys.argv) > 3 else 9
FILES = int(sys.argv[4]) if len(sys.argv) > 4 else 100000
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
REL = os.environ.get("CARGO_TARGET_DIR", ROOT + "/target") + "/release"
VCS, WD, DG = REL + "/vcs", REL + "/vcs-watchd", REL + "/datagen"
os.environ.update(VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid",
                  GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid")
G, V = f"{W}/t/g", f"{W}/t/v"
res = dict(meta=meta(ROOT, dict(vcs=VCS, vcs_watchd=WD)), files=FILES, runs=N,
           cache_state="warm page+dentry cache (tree just written/committed; no eviction)", runs_raw=[], daemon=[])


def setup():
    shutil.rmtree(f"{W}/t", ignore_errors=True)
    os.makedirs(f"{W}/t")
    sh(f"{DG} flat {FILES} {G}")
    sh(f"cp -a {G} {V}")
    sh("git init -q . && git add -A && git commit -qm one", cwd=G)
    sh(f"{VCS} init && {VCS} add . && {VCS} commit -m one", cwd=V, env=dict(os.environ, VCS_NO_AUTOWATCH="1"))


def modify(pct=1):
    files = sorted(os.path.join(dp, x) for dp, _, fn in os.walk(G) if ".git" not in dp for x in fn)
    step = 100 // pct
    sel = files[::step]
    for f in sel:
        for base in (G, V):
            with open(f.replace(G, base, 1), "ab") as fh:
                fh.write(b"edit\n")
    return len(sel)


HOOK = f"{W}/fsmon-hook.sh"
SOCK = f"{W}/fsmon.sock"


def git_mode(m):
    cfg = {"default": ("false", "false"), "uc": ("false", "true"), "hook": (HOOK, "true")}[m]
    sh(f"git config core.fsmonitor {cfg[0]}", cwd=G)
    sh(f"git config core.untrackedCache {cfg[1]}", cwd=G)


def start_git_daemon():
    here = os.path.dirname(os.path.abspath(__file__))
    open(HOOK, "w").write(f'#!/bin/sh\nexec python3 {here}/fair_fsmon.py hook {SOCK} "$@"\n')
    os.chmod(HOOK, 0o755)
    subprocess.Popen(["python3", f"{here}/fair_fsmon.py", "serve", G, SOCK], cwd=G, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True)
    time.sleep(2)


def start_vcs_daemon():
    subprocess.Popen([WD, "watch"], cwd=V, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)
    time.sleep(5)
    for _ in range(3):  # the first scans fully, later ones use the journal
        run_timed([VCS, "status"], cwd=V, env={"VCS_NO_AUTOWATCH": "1"})


def stop_all():
    for pid in pids_under(V, "vcs-watchd"):
        os.kill(pid, signal.SIGTERM)
    for pid in pids_under(G, "fair_fsmon.py"):
        os.kill(pid, signal.SIGTERM)
    time.sleep(1)


def block(name, phase):
    stop_all()
    if name == "B":
        start_vcs_daemon()
    start_git_daemon()
    git_mode("hook")
    for _ in range(3):  # prime git's fsmonitor token + untracked cache outside the timed rounds
        run_timed(["git", "status", "--porcelain"], cwd=G)
    git_mode("uc")
    for _ in range(3):
        run_timed(["git", "status", "--porcelain"], cwd=G)
    gp, vp = pids_under(G, "fair_fsmon.py"), pids_under(V, "vcs-watchd")
    c0 = {p: proc_cpu_ms(p) for p in gp + vp}
    vcs_env = {"VCS_NO_AUTOWATCH": "1"} if name == "B" else {"VCS_NO_AUTOWATCH": "1", "VCS_NO_FSMONITOR": "1"}
    arms = [("git_default", ["git", "status", "--porcelain"], G, {}, "default"),
            ("git_untracked_cache", ["git", "status", "--porcelain"], G, {}, "uc"),
            ("git_fsmonitor_hook_uc", ["git", "status", "--porcelain"], G, {}, "hook"),
            ("vcs_nowatch" if name == "A" else "vcs_watch", [VCS, "status"], V, vcs_env, None)]
    with box_lock():  # one lock hold per block: the whole N-round interleaved block is a few seconds
        c0 = {p: proc_cpu_ms(p) for p in gp + vp}
        for r in range(N):
            for arm, argv, cwd, env, fsm in arms:
                if fsm is not None:
                    git_mode(fsm)
                    if fsm != "default":
                        run_timed(["git", "status", "--porcelain"], cwd=G)  # re-prime after toggling (index/untracked cache/token)
                x = run_timed(argv, env=env, cwd=cwd)
                x.update(arm=arm, block=name, phase=phase, round=r)
                res["runs_raw"].append(x)
                print(json.dumps(x), flush=True)
    c1 = {p: proc_cpu_ms(p) for p in gp + vp}
    d = dict(block=name, phase=phase, rounds=N, git_fsmonitor_daemon_cpu_ms=sum(c1[p] - c0[p] for p in gp),
             vcs_daemon_cpu_ms=sum(c1[p] - c0[p] for p in vp), git_daemon_pids=gp, vcs_daemon_pids=vp)
    i0 = {p: proc_cpu_ms(p) for p in gp + vp}
    time.sleep(60)
    d["idle_60s_git_daemon_cpu_ms"] = sum(proc_cpu_ms(p) - i0[p] for p in gp)
    d["idle_60s_vcs_daemon_cpu_ms"] = sum(proc_cpu_ms(p) - i0[p] for p in vp)
    res["daemon"].append(d)
    print(json.dumps(d), flush=True)


setup()
for phase in ("clean", "dirty"):
    if phase == "dirty":
        stop_all()
        res["modified_files"] = modify(1)
    for b in ("A", "B"):
        block(b, phase)
stop_all()
summ = {}
for arm, blk, ph in sorted({(x["arm"], x["block"], x["phase"]) for x in res["runs_raw"]}):
    v = [x for x in res["runs_raw"] if (x["arm"], x["block"], x["phase"]) == (arm, blk, ph)]
    s = dict(n=len(v), wall_med_ms=median([x["wall_ms"] for x in v]), wall_min_ms=min(x["wall_ms"] for x in v),
             wall_max_ms=max(x["wall_ms"] for x in v), client_cpu_med_ms=median([x["user_ms"] + x["sys_ms"] for x in v]),
             load1_max=max(x["load1"] for x in v), nonzero_rc=sum(1 for x in v if x["rc"]), out_lines_med=median([x["out_lines"] for x in v]))
    d = [d for d in res["daemon"] if d["block"] == blk and d["phase"] == ph][0]
    extra = d["vcs_daemon_cpu_ms"] if arm == "vcs_watch" else d["git_fsmonitor_daemon_cpu_ms"] if arm == "git_fsmonitor_hook_uc" else 0
    s["daemon_cpu_per_run_ms"] = round(extra / len(v), 1)
    s["total_cpu_med_ms"] = round(s["client_cpu_med_ms"] + s["daemon_cpu_per_run_ms"], 1)
    summ[f"{ph}/{blk}/{arm}"] = s
res["summary"] = summ
json.dump(res, open(OUT, "w"), indent=1)
print(json.dumps(summ, indent=1))
