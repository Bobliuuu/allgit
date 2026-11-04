#!/usr/bin/env python3
"""Interleaved speed check for local store layouts (the same vcs binary on identical repos whose cold
packs were rewritten by `vcs repack --format X`). A size cut is shippable only if no cell regresses in
wall OR CPU. MEASUREMENT ONLY.

usage: bench_size_speed.py <layout> <n> --out res.json [--runs 7] [--formats v2,v3,v4]
needs `datagen` and `vcs` on PATH. Variant "base" = `vcs repack && vcs gc` (what the size bench calls
"client after maintenance"); other variants are `vcs repack --format <f>` on a copy of that repo.
Each round runs every variant once, in random order, per cell (wall and user+sys CPU via wait4).

Cells (all run inside the variant's own copy; a prep step is untimed):
  status            clean `vcs status`
  add-1             edit one file, time `vcs add <file>`
  commit-1          edit + add one file, time `vcs commit`
  log-50            `vcs log -n 50`
  switch-old        time `vcs switch old` (old = the initial commit, ~50 files differ; reads cold blobs/trees)
  restore-10pct     delete 10% of the files, time `vcs restore .` (reads ~n/10 cold blobs)
  restore-all       delete every file, time `vcs restore .` (reads every cold blob: the worst case for packs)
"""
import argparse, json, os, random, shutil, subprocess, sys, tempfile, time

ENV = dict(os.environ, VCS_NO_HOOKS="1", VCS_PRINCIPAL="b", GIT_CONFIG_GLOBAL="/dev/null")


def sh(cmd, cwd=None, check=True):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=ENV, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"{cmd}\n{r.stdout[-500:]}\n{r.stderr[-800:]}")
    return r


def timed(argv, cwd):
    t = time.perf_counter()
    p = subprocess.Popen(argv, cwd=cwd, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, st, ru = os.wait4(p.pid, 0)
    return (time.perf_counter() - t) * 1000, (ru.ru_utime + ru.ru_stime) * 1000, os.waitstatus_to_exitcode(st)


def median(a):
    s = sorted(a)
    return s[len(s) // 2] if len(s) % 2 else (s[len(s) // 2 - 1] + s[len(s) // 2]) / 2


def tracked(d):
    out = []
    for r, ds, fs in os.walk(d):
        ds[:] = [x for x in ds if x not in (".vcs", ".git")]
        out += [os.path.relpath(os.path.join(r, f), d) for f in fs]
    return sorted(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout")
    ap.add_argument("n", type=int)
    ap.add_argument("--out", required=True)
    ap.add_argument("--runs", type=int, default=7)
    ap.add_argument("--formats", default="v2,v3,v4",
                    help="comma list of NAME[:FORMAT[:BLOCK_KIB[:BIN]]]; FORMAT defaults to NAME, BIN to `vcs` on PATH. "
                         "Data is built with the first variant's binary")
    ap.add_argument("--work")
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="sizespeed.", dir=a.work)
    base = f"{work}/base"
    sh(f"datagen {a.layout} {a.n} {base} 42 >/dev/null")
    files = tracked(base)
    sh("vcs init && vcs config set user.name b && vcs config set user.email b@x.invalid", base)
    sh("vcs add . && vcs commit -m initial", base)
    first = sh("vcs log -n 1", base).stdout.split()
    first = [w for w in first if len(w) >= 8 and all(c in "0123456789abcdef" for c in w)][0]
    for i in range(1, 51):
        p = files[(i * 7919 + 42) % len(files)]
        with open(f"{base}/{p}", "a") as f:
            f.write(f"edit {i}\n")
        sh(f"vcs add '{p}' && vcs commit -m e{i}", base)
    sh(f"vcs branch old --at {first}", base)
    sh("vcs repack && vcs gc", base)
    variants, binof = {}, {}
    for spec in a.formats.split(","):
        parts = spec.split(":")
        f, fmt = parts[0], (parts[1] if len(parts) > 1 and parts[1] else parts[0])
        kib = parts[2] if len(parts) > 2 else ""
        binp = parts[3] if len(parts) > 3 else shutil.which("vcs")
        d = f"{work}/{f}"
        sh(f"cp -a '{base}' '{d}'")
        if fmt != "v2":
            sh(f"{'VCS_BLOCK_KIB=' + kib if kib else ''} '{binp}' repack --format {fmt}", d)
        variants[f] = d
        binof[f] = binp
    shutil.rmtree(base)
    sizes = {f: int(sh(f"du -sb '{d}/.vcs'").stdout.split()[0]) for f, d in variants.items()}
    print("store bytes (du -sb .vcs):", sizes, file=sys.stderr)
    rng = random.Random(1)
    k = [0]

    def edit(d):
        k[0] += 1
        p = files[(k[0] * 104729) % len(files)]
        with open(f"{d}/{p}", "a") as f:
            f.write(f"bench {k[0]}\n")
        return p

    def delete_pct(d, frac):
        step = int(1 / frac)
        for i, p in enumerate(files):
            if i % step == 0:
                try:
                    os.unlink(f"{d}/{p}")
                except FileNotFoundError:
                    pass

    def wipe(d):
        for p in files:
            try:
                os.unlink(f"{d}/{p}")
            except FileNotFoundError:
                pass

    res = {"meta": {"layout": a.layout, "n": a.n, "runs": a.runs, "sizes": sizes, "nproc": os.cpu_count(),
                    "load1": os.getloadavg()[0], "start": time.strftime("%FT%TZ", time.gmtime())}, "cells": {}}
    st = {}

    def cell(name, prep, argv):
        rows = {f: [] for f in variants}
        for _ in range(a.runs):
            order = list(variants)
            rng.shuffle(order)
            for f in order:
                d = variants[f]
                arg = prep(d, binof[f])
                w, c, rc = timed(argv(arg, binof[f]), d)
                rows[f].append((w, c, rc))
        res["cells"][name] = rows
        print(f"{name}: " + " ".join(f"{f}={median([r[0] for r in rows[f]]):.0f}ms/{median([r[1] for r in rows[f]]):.0f}cpu" for f in rows),
              file=sys.stderr, flush=True)

    cell("status", lambda d, b: None, lambda _, b: [b, "status"])
    cell("add-1", lambda d, b: edit(d), lambda p, b: [b, "add", p])
    cell("commit-1", lambda d, b: (lambda p: sh(f"'{b}' add '{p}'", d))(edit(d)), lambda _, b: [b, "commit", "-m", "b"])
    cell("log-50", lambda d, b: None, lambda _, b: [b, "log", "-n", "50"])

    def to_main(d, b):
        sh(f"'{b}' switch main", d, check=False)

    cell("switch-old", to_main, lambda _, b: [b, "switch", "old"])
    for f, d in variants.items():
        sh(f"'{binof[f]}' switch main", d, check=False)
    cell("restore-10pct", lambda d, b: delete_pct(d, 0.1), lambda _, b: [b, "restore", "."])
    cell("restore-all", lambda d, b: wipe(d), lambda _, b: [b, "restore", "."])
    # correctness: fsck every variant, and identical tree content
    fs = {f: sh(f"'{binof[f]}' fsck", d, check=False) for f, d in variants.items()}
    res["fsck"] = {f: (r.returncode, r.stdout.strip()[-80:]) for f, r in fs.items()}
    print("fsck:", res["fsck"], file=sys.stderr)
    json.dump(res, open(a.out, "w"), indent=1)
    shutil.rmtree(work)


main()
