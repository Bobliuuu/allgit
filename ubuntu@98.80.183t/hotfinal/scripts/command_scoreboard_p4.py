#!/usr/bin/env python3
"""Perforce half of the per-command scoreboard: p4 (client + p4d CPU) vs the vcs spelling from docs/p4-to-vcs.md.

  python3 -I scripts/command_scoreboard_p4.py --work DIR --vcs BIN --p4 P4 --p4d P4D --datagen DG [--n 100000]
         [--runs 3] --out FILE.json

Needs the DIR from `command_scoreboard.py setup` (canonical vcs repo in DIR/vcs/repo, files identical to
`datagen flat N`). Starts a p4d on loopback (journal off, no log, free tier, fresh root), mirrors the
bench_p4.sh setup, builds its own history (c1 = all files, c2 = 1% edited, then 48 one-file changes), and times
p4 and vcs commands interleaved. Reported CPU for p4 = p4 client user+sys (wait4) + the p4d process CPU delta.
Run under `flock ~/bench.lock`. Prep and post steps are untimed.
"""
import argparse, json, os, random, shlex, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from command_scoreboard import BASE_ENV, files, mod_files, timed, sh, load1  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--work", required=True)
ap.add_argument("--vcs", required=True)
ap.add_argument("--p4", required=True)
ap.add_argument("--p4d", required=True)
ap.add_argument("--datagen", required=True)
ap.add_argument("--n", type=int, default=100000)
ap.add_argument("--runs", type=int, default=3)
ap.add_argument("--port", default="11777")
ap.add_argument("--out")
ap.add_argument("--only")
a = ap.parse_args()

W = os.path.abspath(a.work)
P = os.path.join(W, "p4")
SRV, WS = os.path.join(P, "server"), os.path.join(P, "ws")
V = os.path.abspath(a.vcs)
P4 = os.path.abspath(a.p4)
P4D = os.path.abspath(a.p4d)
VR = os.path.join(W, "vcs", "repo")
state = json.load(open(os.path.join(W, "state.json")))
F, pct = state["F"], state["pct"]
ENV = dict(BASE_ENV, P4PORT="127.0.0.1:" + a.port, P4USER="bench", P4CLIENT="ws", P4CONFIG="", P4ENVIRO="/dev/null",
           P4TICKETS=os.path.join(P, "tickets"))
rng = random.Random(1)
srv = {"proc": None}


def p4(args, cwd=WS, inp=None):
    return subprocess.run([P4] + shlex.split(args), cwd=cwd, env=dict(ENV, PWD=cwd), input=inp, capture_output=True, text=True)


def stop():
    if srv["proc"]:
        p4("admin stop")
        try:
            srv["proc"].wait(timeout=15)
        except subprocess.TimeoutExpired:
            srv["proc"].kill()
        srv["proc"] = None


def start():
    stop()
    shutil.rmtree(SRV, ignore_errors=True)
    os.makedirs(SRV)
    os.makedirs(WS, exist_ok=True)
    srv["proc"] = subprocess.Popen([P4D, "-r", SRV, "-J", "off", "-L", "/dev/null", "-p", ENV["P4PORT"]], cwd=SRV,
                                   env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(1800):  # db creation can take a while on a busy disk
        if p4("info").returncode == 0:
            break
        time.sleep(0.1)
    else:
        sys.exit("p4d did not start")
    p4("passwd", inp="Benchpass1!\nBenchpass1!\n")
    p4("login", inp="Benchpass1!\n")
    u = p4("user -o").stdout
    p4("user -i", inp=u)
    mkclient("ws", WS)


def mkclient(name, root):
    cl = subprocess.run([P4, "client", "-o"], env=dict(ENV, P4CLIENT=name), capture_output=True, text=True).stdout
    import re
    cl = re.sub(r"(?m)^Root:.*$", "Root:\t" + root, cl)
    subprocess.run([P4, "client", "-i"], env=dict(ENV, P4CLIENT=name), input=cl, capture_output=True, text=True)


def chmodw(paths):
    for p in paths:
        try:
            os.chmod(os.path.join(WS, p), 0o644)
        except OSError:
            pass


def wsfresh():
    shutil.rmtree(WS, ignore_errors=True)
    os.makedirs(WS)
    subprocess.run([a.datagen, "flat", str(a.n), WS], check=True, stdout=subprocess.DEVNULL)


def vsh(cmd, cwd=VR):
    return sh(cmd.replace("{B}", shlex.quote(V)).replace("{F}", F), cwd, BASE_ENV)


cells = {}
meta = {"p4": subprocess.run([P4, "-V"], capture_output=True, text=True).stdout.strip().splitlines()[-1],
        "p4d": [l for l in subprocess.run([P4D, "-V"], capture_output=True, text=True).stdout.splitlines() if "Rev." in l][0],
        "vcs": subprocess.run([V, "--version"], capture_output=True, text=True).stdout.strip(), "n": a.n,
        "host": os.uname().nodename, "cores": os.cpu_count(), "load1_start": load1(), "runs": a.runs,
        "start": time.strftime("%FT%TZ", time.gmtime())}


def cell(cid, pcmd, vcmd, pprep=None, vprep=None, ppost=None, vpost=None, reps=None, vcwd=VR):
    """pprep/ppost: callables run before/after the timed p4 command; vprep/vpost: bash snippets for vcs."""
    if a.only and cid not in a.only.split(","):
        return
    rows = {"p4": [], "vcs": []}
    loads = []
    for _ in range(reps or a.runs):
        order = ["p4", "vcs"]
        rng.shuffle(order)
        l0 = load1()
        for t in order:
            if t == "p4":
                if pprep:
                    pprep()
                w, c, rc = timed([P4] + shlex.split(pcmd), WS, ENV, srv_pid=srv["proc"].pid)
                if rc and cid not in cells:
                    q = subprocess.run([P4] + shlex.split(pcmd), cwd=WS, env=dict(ENV, PWD=WS), capture_output=True, text=True)
                    print("  DEBUG", cid, rc, (q.stdout + q.stderr)[:300].replace("\n", " | "), file=sys.stderr)
                if ppost:
                    ppost()
            else:
                if vprep:
                    vsh(vprep, vcwd)
                w, c, rc = timed([V] + shlex.split(vcmd.replace("{F}", F)), vcwd, BASE_ENV)
                if vpost:
                    vsh(vpost, vcwd)
            rows[t].append((w, c, rc))
        loads.append((l0, load1()))
    cells[cid] = {"rows": rows, "load": loads, "cmds": {"p4": "p4 " + pcmd, "vcs": "vcs " + vcmd}}
    med = lambda t: sorted(r[1] for r in rows[t])[len(rows[t]) // 2]
    print(f"{cid}: p4 {med('p4'):.0f}ms vcs {med('vcs'):.0f}ms cpu rc={[r[2] for r in rows['p4']]}/{[r[2] for r in rows['vcs']]}",
          file=sys.stderr, flush=True)
    if a.out:
        json.dump({"meta": meta, "cells": cells}, open(a.out, "w"), indent=1)


def edit1(rel=F, tag="e"):
    p4(f"-q edit {rel}")
    chmodw([rel])
    mod_files(WS, [rel], tag + str(time.time_ns()))


# ---- 1. add all + initial submit (fresh server per rep). vcs: fresh dir, `vcs add .`, then `vcs p4 submit`
VF = os.path.join(W, "vcs", "fresh")


def vcs_prep_fresh():
    os.makedirs(VF, exist_ok=True)
    if not os.path.exists(os.path.join(VF, "d0000")):
        subprocess.run(["bash", "-c", f"cp -a '{VR}'/d0* ."], cwd=VF, check=True)
    rem = os.path.join(W, "vcs", "p4remote")
    vsh(f"rm -rf .vcs {rem}; {{B}} init; {{B}} config set user.name b; {{B}} config set user.email b@x.invalid; "
        f"{{B}} remote set {rem}", VF)


rows_a = {"p4": [], "vcs": []}
rows_s = {"p4": [], "vcs": []}
loads = []
for _ in range(min(a.runs, 3)):
    order = ["p4", "vcs"]
    rng.shuffle(order)
    l0 = load1()
    for t in order:
        if t == "p4":
            start()
            wsfresh()
            rows_a[t].append(timed([P4, "-q", "reconcile", "-a", "//..."], WS, ENV, srv_pid=srv["proc"].pid))
            rows_s[t].append(timed([P4, "submit", "-d", "c1"], WS, ENV, srv_pid=srv["proc"].pid))
        else:
            vcs_prep_fresh()
            rows_a[t].append(timed([V, "add", "."], VF, BASE_ENV))
            rows_s[t].append(timed([V, "p4", "submit", "-d", "c1"], VF, BASE_ENV))
    loads.append((l0, load1()))
cells["add_all"] = {"rows": rows_a, "load": loads, "cmds": {"p4": "p4 -q reconcile -a //...", "vcs": "vcs add ."}}
cells["submit_all"] = {"rows": rows_s, "load": loads, "cmds": {"p4": "p4 submit -d c1 (100k files opened for add)", "vcs": "vcs p4 submit -d c1 (after vcs add .)"}}
for k in ("add_all", "submit_all"):
    print(k, [r[2] for r in cells[k]["rows"]["p4"]], [r[2] for r in cells[k]["rows"]["vcs"]], file=sys.stderr, flush=True)

# ---- 2. history: c2 = 1% edited, c3.. one file each (48)
if srv["proc"] is None:
    start()
    wsfresh()
    p4("-q reconcile -a //...")
    p4("submit -d c1")
if len(p4("changes").stdout.splitlines()) < 2:
    chmodw(pct)
    for chunk in range(0, len(pct), 500):
        p4("-x - -q edit", inp="\n".join(pct[chunk:chunk + 500]) + "\n")
    chmodw(pct)
    mod_files(WS, pct, "c2")
    p4("submit -d c2")
    fs = files(WS)
    for k in range(1, 49):
        r = fs[k * 997 % len(fs)]
        edit1(r, f"h{k}")
        p4(f"submit -d h{k}")
print("p4 changes:", len(p4("changes").stdout.splitlines()), file=sys.stderr, flush=True)

# ---- 3. cells
vsh("{B} reset --hard base; {B} switch main")
_rem = os.path.join(W, "vcs", "remote")  # fresh vcs remote so publish (submit_1, clone) starts from base
shutil.rmtree(_rem, ignore_errors=True)
vsh("{B} merge --abort; {B} clean -f -d; {B} remote set " + shlex.quote(_rem) + "; {B} publish")
nd = lambda: None


def p4_revert():
    p4("-q revert -k //...")


cell("status_clean", "reconcile -n //...", "status")
# 1% modified in both
chmodw(pct)
mod_files(WS, pct, "pct")
mod_files(VR, pct, "pct")
cell("status_1pct", "reconcile -n //...", "status")
vsh("{B} restore .")
subprocess.run([P4, "-q", "revert", "//..."], cwd=WS, env=dict(ENV, PWD=WS), capture_output=True)
chmodw(pct)
p4("-q sync -f //...@2")
cell("changes", "changes -m 50", "log -n 50")
cell("sync_1pct", "-q sync //...@2", "switch main", pprep=lambda: p4("-q sync //...@1"), vprep="{B} switch side", reps=3)
p4("-q sync //...")
cell("sync_n", "sync -n //...", "fetch")
cell("files", "files //...", "ls-files --rev HEAD", reps=3)
cell("have", "have", "ls-files", reps=3)
cell("describe", "describe -s 5", "show --name-status HEAD")
cell("filelog", f"filelog {F}", f"log -n 1000000000 --follow --name-status -- {F}")
cell("annotate", f"annotate {F}", f"blame {F}")
cell("fstat", f"fstat {F}", f"log -n 1 --oneline -- {F}")
cell("print", f"print {F}", f"cat-file -p HEAD:{F}")
cell("where", f"where {F}", f"p4 where {F}")
cell("info", "info", "p4 info")
cell("users", "users", "shortlog -s -e")
cell("depots", "depots", "zone list")
cell("clients", "clients", "worktree list")
cell("labels", "labels", "tag")
cell("branches", "branches", "branch")
cell("grep", f"grep -e {state['needle']} //...", f"grep {state['needle']}", reps=3)
cell("verify", "verify -q //...", "fsck", reps=3)
cell("edit_file", f"-q edit {F}", f"p4 edit {F}", ppost=lambda: p4(f"-q revert {F}"), vpost=None)
cell("opened", "opened", "status", pprep=lambda: p4(f"-q edit {F}"), ppost=lambda: p4(f"-q revert {F}"), vprep="printf 'x' >> {F}", vpost="{B} restore {F}")
cell("diff_file", f"diff {F}", "diff", pprep=lambda: edit1(), ppost=lambda: (p4(f"-q revert {F}"), chmodw([F])), vprep="printf 'x' >> {F}", vpost="{B} restore {F}")
cell("delete_file", f"-q delete {F}", f"rm {F}", pprep=lambda: os.chmod(os.path.join(WS, F), 0o444), ppost=lambda: (p4(f"-q revert {F}"), chmodw([F])), vpost=f"{{B}} restore --staged {{F}}; {{B}} restore {{F}}")
cell("revert_file", f"-q revert {F}", f"restore {F}", pprep=lambda: edit1(), vprep="printf 'x' >> {F}")
cell("add_file", "-q add new1.txt", "add new1.txt",
     pprep=lambda: open(os.path.join(WS, "new1.txt"), "w").write("n\n"), ppost=lambda: (p4("-q revert -k new1.txt"), os.remove(os.path.join(WS, "new1.txt"))),
     vprep="printf n > new1.txt", vpost="{B} restore --staged new1.txt; rm -f new1.txt")
cell("submit_1", "submit -d one", "p4 submit -d one", pprep=lambda: edit1(), vprep="printf 'x%s' $RANDOM >> {F}", reps=3)
cell("clone_sync", "-c ws2 -q sync //...", f"clone {os.path.join(W, 'vcs', 'remote')} {os.path.join(W, 'dstp4')}",
     pprep=lambda: (shutil.rmtree(os.path.join(P, "ws2"), ignore_errors=True), os.makedirs(os.path.join(P, "ws2")), mkclient("ws2", os.path.join(P, "ws2"))),
     vprep=f"rm -rf {os.path.join(W, 'dstp4')}", reps=2)

stop()
shutil.rmtree(P, ignore_errors=True)
meta["load1_end"] = load1()
json.dump({"meta": meta, "cells": cells}, open(a.out, "w"), indent=1)
