#!/usr/bin/env python3
"""Interleaved git vs vcs benchmark for add/commit/amend/stash/merge/rebase --continue.
usage: bench_ops.py LAYOUT N [--runs R] [--cells a,b,..] [--out f.json]
env: VCS_BIN_BASE, VCS_BIN_NEW (like bench_compare.py), DATAGEN. CPU = user+sys from wait4 rusage."""
import argparse, json, os, random, shutil, subprocess, sys, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench_compare as bc

ENV = dict(bc.ENV, GIT_EDITOR="true")
bc.ENV = ENV
median = bc.median


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout"); ap.add_argument("n", type=int)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cells", default="add,commit,add1,commit1,amend,stash,merge,rebase")
    ap.add_argument("--out")
    a = ap.parse_args()
    cells = a.cells.split(",")
    tools = {"git": None}
    for name, var in (("base", "VCS_BIN_BASE"), ("new", "VCS_BIN_NEW")):
        if os.environ.get(var):
            tools[name] = os.environ[var]
    work = tempfile.mkdtemp(prefix="vcs-ops.", dir=os.environ.get("BENCH_TMP"))
    dirs = {t: os.path.join(work, t) for t in tools}
    rng = random.Random(1)
    tgt = os.environ.get("CARGO_TARGET_DIR", "target")
    datagen = os.environ.get("DATAGEN", os.path.join(tgt, "release", "datagen"))
    sh = lambda cmd, t: bc.sh(cmd, dirs[t])

    def V(t, args):  # shell snippet
        return f"'{tools[t]}' {args}"
    def init(t):
        if t == "git": return "git init -q -b main"
        return V(t, "init") + f"; {V(t,'config set user.name b')}; {V(t,'config set user.email b@x.invalid')}"
    def rm(t): return "rm -rf .git" if t == "git" else "rm -rf .vcs"
    def ADD(t, p="."): return f"git add {p}" if t == "git" else V(t, f"add {p}")
    def COMMIT(t): return "git commit -q -m c" if t == "git" else V(t, "commit -m c")
    def argv(t, s):  # command string -> argv through bash
        return ["bash", "-c", s]

    res = {"meta": {"layout": a.layout, "n": a.n, "runs": a.runs, "tools": tools, "load1_start": bc.load1(),
                    "nproc": os.cpu_count()}, "cells": {}}

    def cell(name, prep, cmd):
        rows = {t: [] for t in tools}
        for _ in range(a.runs):
            order = list(tools); rng.shuffle(order)
            bc.cpu_stat()
            for t in order:
                prep(t)
                w, c, rc = bc.timed(argv(t, cmd(t)), dirs[t], [])
                rows[t].append((w, c, rc))
        res["cells"][name] = {"rows": rows}
        print(f"cell {name}: " + " ".join(f"{t}={median([r[1] for r in rows[t]]):.0f}msCPU" for t in rows) +
              " rc=" + ",".join(f"{t}:{[r[2] for r in rows[t]]}" for t in rows), file=sys.stderr, flush=True)

    try:
        base = os.path.join(work, "_src")
        subprocess.run([datagen, a.layout, str(a.n), base], stdout=subprocess.DEVNULL, check=True)
        for t in tools:
            subprocess.run(["cp", "-a", base, dirs[t]], check=True)
        shutil.rmtree(base)
        fresh = lambda t: sh(f"{rm(t)}; {init(t)}", t)
        if "add" in cells:
            cell("add (fresh index)", fresh, lambda t: ADD(t))
        if "commit" in cells:
            cell("commit (first, all staged)", lambda t: sh(f"{rm(t)}; {init(t)}; {ADD(t)}", t), COMMIT)
        for t in tools:
            sh(f"{rm(t)}; {init(t)}; {ADD(t)}; {COMMIT(t)}", t)
        cnt = [0]
        def mod(t, off=0, tag="a"):
            cnt[0] += 1
            fs = bc.files(dirs[t])[off::100]
            for i, p in enumerate(fs):
                with open(p, "wb") as fh:
                    fh.write(f"modified {tag}{cnt[0]} {i}\n".encode() * 8)
        if "add1" in cells:
            cell("add (1% modified)", lambda t: mod(t), lambda t: ADD(t))
        if "commit1" in cells:
            def p(t): mod(t); sh(ADD(t), t)
            cell("commit (1% modified)", p, COMMIT)
        if "amend" in cells:
            def p(t): mod(t); sh(ADD(t), t)
            cell("amend (1% staged)", p, lambda t: "git commit -q --amend --no-edit" if t == "git"
                 else V(t, "reset --soft HEAD^") + " && " + V(t, "commit -m c"))
        if "stash" in cells:
            def p(t):
                sh("git stash drop -q" if t == "git" else V(t, "stash drop"), t); mod(t)
            cell("stash (1% modified)", p, lambda t: "git stash -q" if t == "git" else V(t, "stash push"))
        if "merge" in cells or "rebase" in cells:
            def BR(t, n, start): return f"git branch -f {n} {start}" if t == "git" else V(t, f"branch {n} {start}")
            def SW(t, n): return f"git checkout -q -f {n}" if t == "git" else V(t, f"switch {n}")
            cfs = {}
            for t in tools:
                sh("git stash drop -q || true" if t == "git" else V(t, "stash drop") + " || true", t)
                sh("git reset -q --hard HEAD" if t == "git" else V(t, "reset --hard"), t)
                cf = bc.files(dirs[t])[20]; cfs[t] = cf
                sh(BR(t, "b0", "main"), t)
                sh(BR(t, "sideA", "b0") + "; " + SW(t, "sideA"), t)
                mod(t, 10, "s"); sh(f"{ADD(t)}; {COMMIT(t)}", t)
                sh(BR(t, "sideX", "b0") + "; " + SW(t, "sideX"), t)
                mod(t, 10, "s"); open(cf, "wb").write(b"side version\n" * 4); sh(f"{ADD(t)}; {COMMIT(t)}", t)
                mod(t, 15, "t"); sh(f"{ADD(t)}; {COMMIT(t)}", t)
                sh(BR(t, "sx0", "sideX") + "; " + SW(t, "main"), t)
                mod(t, 50, "m"); open(cf, "wb").write(b"main version\n" * 4); sh(f"{ADD(t)}; {COMMIT(t)}", t)
                sh(BR(t, "m0", "main"), t)
            def reset_to(t, n): return f"git reset -q --hard {n}" if t == "git" else V(t, f"reset --hard {n}")
            if "merge" in cells:
                cell("merge (two 1% branches)", lambda t: sh(SW(t, "main") + "; " + reset_to(t, "m0"), t),
                     lambda t: "git merge -q --no-ff -m c sideA" if t == "git" else V(t, "merge --no-ff -m c sideA"))
            if "rebase" in cells:
                def pr(t):
                    sh("git rebase --abort" if t == "git" else V(t, "rebase --abort"), t)
                    sh(SW(t, "main") + "; " + reset_to(t, "m0"), t)
                    sh(SW(t, "sideX") + "; " + reset_to(t, "sx0"), t)
                    sh("git rebase main" if t == "git" else V(t, "rebase main"), t)
                    if os.environ.get("DBG"): subprocess.run(["bash","-c","git status 2>&1 | head -5" if t=="git" else V(t,"status")+" | head -5"],cwd=dirs[t])
                    open(cfs[t], "wb").write(b"resolved\n" * 4)
                    sh(ADD(t, cfs[t]), t)
                cell("rebase --continue", pr, lambda t: "git rebase --continue" if t == "git" else V(t, "rebase --continue"))
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if a.out: json.dump(res, open(a.out, "w"), indent=1)

if __name__ == "__main__":
    main()
