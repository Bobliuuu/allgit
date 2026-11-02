#!/usr/bin/env python3
"""Interleaved git vs vcs benchmark for cherry-pick, revert, cherry, blame, op undo, tag/branch, am -3, apply -3,
rebase -i, repack/gc. Sibling of bench_ops.py (same CPU = user+sys from wait4, same shuffled interleaving).
usage: bench_ops2.py LAYOUT N [--runs R] [--cells a,b,..] [--out f.json]
env: VCS_BIN_NEW (vcs binary), DATAGEN."""
import argparse, json, os, random, shutil, subprocess, sys, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench_compare as bc

ENV = dict(bc.ENV, GIT_EDITOR="true", GIT_SEQUENCE_EDITOR="true", VCS_SEQUENCE_EDITOR="true")
bc.ENV = ENV
median = bc.median
ALL = "cherrypick,revert,cherry,blame,opundo,tagmk,tagls,branchmk,branchls,am3,apply3,rebasei,repack,gc"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout"); ap.add_argument("n", type=int)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cells", default=ALL)
    ap.add_argument("--out")
    a = ap.parse_args()
    cells = a.cells.split(",")
    tools = {"git": None, "new": os.environ["VCS_BIN_NEW"]}
    work = tempfile.mkdtemp(prefix="vcs-ops2.", dir=os.environ.get("BENCH_TMP"))
    dirs = {t: os.path.join(work, t) for t in tools}
    rng = random.Random(1)
    datagen = os.environ.get("DATAGEN", os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "release", "datagen"))
    sh = lambda cmd, t: bc.sh(cmd, dirs[t])
    G = lambda t: t == "git"
    def V(t, args): return f"'{tools[t]}' {args}"
    def init(t):
        if G(t): return "git init -q -b main"
        return V(t, "init") + f"; {V(t,'config set user.name b')}; {V(t,'config set user.email b@x.invalid')}"
    def rm(t): return "rm -rf .git" if G(t) else "rm -rf .vcs"
    def ADD(t, p="."): return f"git add {p}" if G(t) else V(t, f"add {p}")
    def COMMIT(t): return "git commit -q -m c" if G(t) else V(t, "commit -m c")
    def BR(t, n, s): return f"git branch -f {n} {s}" if G(t) else V(t, f"branch {n} {s}")
    def SW(t, n): return f"git checkout -q -f {n}" if G(t) else V(t, f"switch {n}")
    def RESET(t, n): return f"git reset -q --hard {n}" if G(t) else V(t, f"reset --hard {n}")
    res = {"meta": {"layout": a.layout, "n": a.n, "runs": a.runs, "load1_start": bc.load1(), "nproc": os.cpu_count()},
           "cells": {}}

    def cell(name, prep, cmd):
        if name not in cells: return
        rows = {t: [] for t in tools}
        for _ in range(a.runs):
            order = list(tools); rng.shuffle(order)
            for t in order:
                prep(t)
                w, c, rc = bc.timed(["bash", "-c", cmd(t)], dirs[t], [])
                rows[t].append((w, c, rc))
                if os.environ.get("DBG"): print("RC", name, t, rc, file=sys.stderr)
        res["cells"][name] = {"rows": rows}
        print(f"cell {name}: " + " ".join(f"{t}={median([r[1] for r in rows[t]]):.0f}msCPU/{median([r[0] for r in rows[t]]):.0f}msWall" for t in rows)
              + " rc=" + ",".join(f"{t}:{sorted(set(r[2] for r in rows[t]))}" for t in rows), file=sys.stderr, flush=True)

    try:
        base = os.path.join(work, "_src")
        subprocess.run([datagen, a.layout, str(a.n), base], stdout=subprocess.DEVNULL, check=True)
        for t in tools:
            subprocess.run(["cp", "-a", base, dirs[t]], check=True)
        shutil.rmtree(base)
        cnt = [0]
        def mod(t, off=0, tag="a"):
            cnt[0] += 1
            for i, p in enumerate(bc.files(dirs[t])[off::100]):
                with open(p, "wb") as fh:
                    fh.write(f"modified {tag}{cnt[0]} {i}\n".encode() * 8)
        for t in tools:
            sh(f"{rm(t)}; {init(t)}; {ADD(t)}; {COMMIT(t)}", t)
            sh(BR(t, "b0", "main"), t)
            # history: 6 commits on main touching 1% each; blame target file touched by 3 of them
            bf = bc.files(dirs[t])[200]
            for k in range(6):
                mod(t, 10 + k, "h"); open(bf, "ab").write(f"line {k}\n".encode()) if k % 2 == 0 else None
                sh(f"{ADD(t)}; {COMMIT(t)}", t)
            sh(BR(t, "m0", "main"), t)
            sh(BR(t, "sideA", "b0") + "; " + SW(t, "sideA"), t)
            mod(t, 40, "s"); sh(f"{ADD(t)}; {COMMIT(t)}", t)
            sh(BR(t, "sA0", "sideA") + "; " + SW(t, "main"), t)
            if G(t): sh(f"git format-patch b0..sideA --stdout > ../{t}.mbox; git diff b0 sideA > ../{t}.diff", t)
            else: sh(V(t, "format-patch b0..sideA --stdout") + f" > ../{t}.mbox; " + V(t, "show sideA") + f" > ../{t}.diff", t)
            if G(t): pass
            else: sh(V(t, "diff") + " >/dev/null", t)
            open(os.path.join(work, f"{t}.blamefile"), "w").write(os.path.relpath(bf, dirs[t]))
        bfile = lambda t: open(os.path.join(work, f"{t}.blamefile")).read()
        abort = lambda t: ("git cherry-pick --abort; git rebase --abort; git revert --abort; git am --abort" if G(t) else V(t, "rebase --abort") + "; " + V(t, "am --abort")) + " 2>/dev/null; true"
        home = lambda t: abort(t) + "; " + SW(t, "main") + "; " + RESET(t, "m0")
        pcp = lambda t: sh(home(t), t)
        cell("cherrypick", pcp, lambda t: "git cherry-pick sideA" if G(t) else V(t, "cherry-pick sideA"))
        cell("revert", pcp, lambda t: "git revert --no-edit HEAD" if G(t) else V(t, "revert HEAD"))
        cell("cherry", pcp, lambda t: "git cherry main sideA" if G(t) else V(t, "cherry main sideA"))
        cell("blame", pcp, lambda t: f"git blame {bfile(t)}" if G(t) else V(t, f"blame {bfile(t)}"))
        def pundo(t):
            sh(home(t), t); sh("echo x >> " + bfile(t) + "; " + (ADD(t, bfile(t)) + "; " + COMMIT(t)), t)
        cell("opundo", pundo, lambda t: "git reset -q --hard HEAD~1" if G(t) else V(t, "op undo"))
        def ptag(t): sh(home(t) + "; " + ("git tag -d v1" if G(t) else V(t, "tag -d v1")), t)
        cell("tagmk", ptag, lambda t: "git tag v1" if G(t) else V(t, "tag v1"))
        for t in tools: sh("git tag v1 || true" if G(t) else V(t, "tag v1"), t)
        cell("tagls", pcp, lambda t: "git tag" if G(t) else V(t, "tag"))
        def pbr(t): sh(home(t) + "; " + ("git branch -D nb" if G(t) else V(t, "branch -d --force nb")), t)
        cell("branchmk", pbr, lambda t: "git branch nb" if G(t) else V(t, "branch nb"))
        cell("branchls", pcp, lambda t: "git branch" if G(t) else V(t, "branch"))
        def pam(t): sh("git am --abort || true" if G(t) else V(t, "am --abort") + " || true", t); sh(SW(t, "b0") + "; " + RESET(t, "b0"), t)
        cell("am3", pam, lambda t: f"git am -3 ../{t}.mbox" if G(t) else V(t, f"am -3 ../{t}.mbox"))
        def pap(t): sh(SW(t, "b0") + "; " + RESET(t, "b0"), t)
        cell("apply3", pap, lambda t: f"git apply -3 ../{t}.diff" if G(t) else V(t, f"apply -3 ../{t}.diff"))
        for t in tools: sh(SW(t, "main"), t)
        cell("rebasei", pcp, lambda t: "git rebase -i HEAD~4" if G(t) else V(t, "rebase -i HEAD~4"))
        def prep(t): sh(home(t) + "; " + ("git gc --prune=now -q" if G(t) else ""), t)
        # repack/gc: re-create loose objects first
        def ploose(t):
            sh(f"{rm(t)}; {init(t)}; {ADD(t)}; {COMMIT(t)}", t)
        cell("repack", ploose, lambda t: "git repack -adq" if G(t) else V(t, "repack"))
        cell("gc", ploose, lambda t: "git gc -q" if G(t) else V(t, "gc --grace-secs 1300000"))
    finally:
        if not os.environ.get("KEEP"): shutil.rmtree(work, ignore_errors=True)
        else: print("KEPT", work, file=sys.stderr)
    if a.out: json.dump(res, open(a.out, "w"), indent=1)

if __name__ == "__main__":
    main()
