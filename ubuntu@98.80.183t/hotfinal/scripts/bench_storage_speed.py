#!/usr/bin/env python3
"""Interleaved speed benchmark of storage-engine settings: git vs one or more vcs configurations.
usage: bench_storage_speed.py <flat|deep|binary> <n> [--runs 5] [--out f.json]
         --vcs name=/path/to/vcs[,ENV=VAL,...]   (repeatable; same binary with different env is fine)
Cells: add, commit, status, blame, checkout, clone, fetch, plus store sizes (side effect only).
Every round runs each tool once in random order; wall and CPU (wait4 rusage) are recorded."""
import argparse, json, os, random, shutil, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from bench_compare import ENV, files, median, modify_1pct, load1  # noqa: E402


def run(argv, cwd, env):
    t = time.perf_counter()
    p = subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, st, ru = os.wait4(p.pid, 0)
    return (time.perf_counter() - t) * 1000, (ru.ru_utime + ru.ru_stime) * 1000, os.waitstatus_to_exitcode(st)


def sh(cmd, cwd, env):
    r = subprocess.run(["bash", "-c", cmd], cwd=cwd, env=env, capture_output=True, text=True)
    if r.returncode:
        print("FAIL", cmd, r.stderr[-300:], file=sys.stderr)
    return r.stdout


def du_kib(p):
    return int(subprocess.run(["du", "-sk", p], capture_output=True, text=True).stdout.split()[0])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout"); ap.add_argument("n", type=int)
    ap.add_argument("--runs", type=int, default=5); ap.add_argument("--out")
    ap.add_argument("--vcs", action="append", default=[])
    ap.add_argument("--datagen", default=os.environ.get("DATAGEN", "target/release/datagen"))
    a = ap.parse_args()
    base_env = dict(ENV, VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid")
    tools = {"git": ("git", None, base_env)}
    for spec in a.vcs:
        name, rest = spec.split("=", 1)
        parts = rest.split(",")
        env = dict(base_env)
        for kv in parts[1:]:
            k, v = kv.split("=", 1); env[k] = v
        tools[name] = ("vcs", parts[0], env)
    work = tempfile.mkdtemp(prefix="vcs-spd.", dir=os.environ.get("BENCH_TMP"))
    D = {t: os.path.join(work, t) for t in tools}
    R = {t: os.path.join(work, t + ".remote") for t in tools}
    CL = {t: os.path.join(work, t + ".clone") for t in tools}
    e = lambda t: tools[t][2]
    isgit = lambda t: tools[t][0] == "git"
    B = lambda t: tools[t][1]

    def initc(t):
        return "git init -q -b main" if isgit(t) else f"'{B(t)}' init; '{B(t)}' config set user.name b; '{B(t)}' config set user.email b@x.invalid"
    resetc = lambda t: "rm -rf .git" if isgit(t) else "rm -rf .vcs"
    addc = lambda t: ["git", "add", "."] if isgit(t) else [B(t), "add", "."]
    commitc = lambda t: ["git", "commit", "-q", "-m", "c"] if isgit(t) else [B(t), "commit", "-m", "c"]
    shq = lambda l: " ".join(f"'{x}'" for x in l)

    res = {"meta": dict(layout=a.layout, n=a.n, runs=a.runs, load1=load1(), nproc=os.cpu_count(),
                        git=subprocess.run(["git", "--version"], capture_output=True, text=True).stdout.strip(),
                        tools={t: [v[0], v[1], {k: x for k, x in v[2].items() if k.startswith("VCS_")}] for t, v in tools.items()}),
           "cells": {}, "sizes_kib": {}}
    rng = random.Random(1)
    cwdm = {}

    def cell(name, prep, argv, cwd=None):
        rows = {t: [] for t in tools}
        for _ in range(a.runs):
            order = list(tools); rng.shuffle(order)
            for t in order:
                prep(t)
                rows[t].append(run(argv(t), (cwd or D)[t], e(t)))
        res["cells"][name] = rows
        print(f"{name}: " + " ".join(f"{t}={median([r[0] for r in rows[t]]):.0f}ms/{median([r[1] for r in rows[t]]):.0f}cpu" for t in rows), file=sys.stderr, flush=True)

    try:
        for t in tools:
            os.makedirs(D[t])
            subprocess.run([a.datagen, a.layout, str(a.n), D[t]], stdout=subprocess.DEVNULL, check=True)
        nop = lambda t: None
        cell("add", lambda t: sh(f"{resetc(t)}; {initc(t)}", D[t], e(t)), addc)
        cell("commit", lambda t: sh(f"{resetc(t)}; {initc(t)}; {shq(addc(t))}", D[t], e(t)), commitc)
        for t in tools:
            sh(f"{resetc(t)}; {initc(t)}; {shq(addc(t))}; {shq(commitc(t))}", D[t], e(t))
        time.sleep(2)
        cell("status_clean", nop, lambda t: ["git", "status", "--porcelain"] if isgit(t) else [B(t), "status"])
        victim = {t: os.path.relpath(files(D[t])[len(files(D[t])) // 2], D[t]) for t in tools}
        for t in tools:
            sh("git branch side" if isgit(t) else f"'{B(t)}' branch side", D[t], e(t))
            for i in range(1, 31):
                open(os.path.join(D[t], victim[t]), "a").write(f"history {i}\n")
                sh("git add -A && git commit -q -m h" if isgit(t) else f"'{B(t)}' add . && '{B(t)}' commit -m h", D[t], e(t))
        def c1_prep(t):
            modify_1pct(D[t], str(rng.random()))
            sh(shq(addc(t)), D[t], e(t))
        cell("commit_1pct_modified", c1_prep, commitc)
        cell("blame_30_commits", nop, lambda t: ["git", "blame", victim[t]] if isgit(t) else [B(t), "blame", victim[t]])
        for t in tools:
            modify_1pct(D[t], "a")
            sh("git add -A && git commit -q -m x" if isgit(t) else f"'{B(t)}' add . && '{B(t)}' commit -m x", D[t], e(t))
        side = lambda t: sh("git checkout -q side" if isgit(t) else f"'{B(t)}' switch side", D[t], e(t))
        cell("checkout_1pct", side, lambda t: ["git", "checkout", "-q", "main"] if isgit(t) else [B(t), "switch", "main"])
        for t in tools:
            sh("git checkout -q main" if isgit(t) else f"'{B(t)}' switch main", D[t], e(t))
            if isgit(t):
                sh(f"git init -q --bare -b main '{R[t]}' && git push -q '{R[t]}' main", D[t], e(t))
            else:
                os.makedirs(R[t]); sh(f"'{B(t)}' remote set '{R[t]}' && '{B(t)}' publish", D[t], e(t))
        W = {t: work for t in tools}
        cell("clone", lambda t: shutil.rmtree(CL[t], ignore_errors=True),
             lambda t: ["git", "clone", "-q", R[t], CL[t]] if isgit(t) else [B(t), "clone", R[t], CL[t]], cwd=W)
        for t in tools:
            res["sizes_kib"][t] = {"client_store": du_kib(os.path.join(CL[t], ".git" if isgit(t) else ".vcs")), "remote": du_kib(R[t]),
                                   "source_store": du_kib(os.path.join(D[t], ".git" if isgit(t) else ".vcs"))}
        cnt = [0]

        def fetch_prep(t):
            cnt[0] += 1
            open(os.path.join(D[t], victim[t]), "a").write(f"fetch {cnt[0]}\n")
            sh(f"git add -A && git commit -q -m f && git push -q '{R[t]}' main" if isgit(t) else f"'{B(t)}' add . && '{B(t)}' commit -m f && '{B(t)}' publish", D[t], e(t))
        cell("fetch_1_commit", fetch_prep, lambda t: ["git", "fetch", "-q", "origin"] if isgit(t) else [B(t), "fetch"], cwd=CL)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    res["meta"]["load1_end"] = load1()
    print("\n| cell | tool | wall med ms | CPU med ms | wall vs git | CPU vs git |\n|---|---|---|---|---|---|")
    for name, rows in res["cells"].items():
        g = [median([r[i] for r in rows["git"]]) for i in (0, 1)]
        for t in rows:
            w, c = [median([r[i] for r in rows[t]]) for i in (0, 1)]
            bad = " (rc!=0)" if any(r[2] for r in rows[t]) else ""
            print(f"| {name} | {t}{bad} | {w:.0f} | {c:.0f} | {w/g[0]:.2f} | {c/g[1]:.2f} |")
    print("\nsizes KiB (side effect):", json.dumps(res["sizes_kib"]))
    if a.out:
        json.dump(res, open(a.out, "w"), indent=1)


main()
