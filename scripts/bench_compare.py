#!/usr/bin/env python3
"""Interleaved git vs vcs(base) vs vcs(new) benchmark. See scripts/bench_compare.sh and
docs/research/interleaved-comparison.md. Every run round executes each tool once in random
order, so all tools see the same machine load; wall AND CPU (user+sys via wait4 rusage) are kept."""
import argparse, json, os, random, shutil, subprocess, sys, tempfile, time

ENV = dict(os.environ, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid",
           GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")


def load1():
    try:
        return float(open("/proc/loadavg").read().split()[0])
    except OSError:
        return os.getloadavg()[0]


def cpu_stat():
    try:
        v = [int(x) for x in open("/proc/stat").readline().split()[1:]]
        return sum(v), v[4], v[3] + v[4]  # total, iowait, idle+iowait
    except (OSError, ValueError, IndexError):
        return None


def iowait_pct(a, b):
    if not a or not b or b[0] == a[0]:
        return None
    return 100.0 * (b[1] - a[1]) / (b[0] - a[0])


def free_gb(path):
    s = os.statvfs(path)
    return s.f_bavail * s.f_frsize / 2**30


def timed(argv, cwd, prefix):
    """Run argv; return (wall_ms, cpu_ms, rc) using the child's rusage."""
    t = time.perf_counter()
    p = subprocess.Popen(prefix + argv, cwd=cwd, env=ENV, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL)
    _, status, ru = os.wait4(p.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    return wall, (ru.ru_utime + ru.ru_stime) * 1000, os.waitstatus_to_exitcode(status)


def sh(cmd, cwd):
    subprocess.run(["bash", "-c", cmd], cwd=cwd, env=ENV, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)


def files(root):
    fs = []
    for d, ds, names in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs")]
        fs += [os.path.join(d, f) for f in names]
    fs.sort()
    return fs


def modify_1pct(root, tag):
    for i, p in enumerate(files(root)[::100]):
        with open(p, "wb") as fh:
            fh.write(f"modified {tag} {i}\n".encode() * 8)


def touch_one(root, k):
    fs = files(root)
    with open(fs[k * 997 % len(fs)], "wb") as fh:
        fh.write(f"history {k}\n".encode())


def median(a):
    s = sorted(a)
    n = len(s)
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2


def boot_ci(ratios, rng, iters=10000):
    n = len(ratios)
    ms = sorted(median([ratios[rng.randrange(n)] for _ in range(n)]) for _ in range(iters))
    return ms[int(0.025 * iters)], ms[int(0.975 * iters) - 1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout", choices=["flat", "deep", "binary"])
    ap.add_argument("n", type=int)
    ap.add_argument("--runs", type=int, default=7)
    ap.add_argument("--out", help="write JSON results here")
    ap.add_argument("--taskset", help="CPU list for taskset -c, e.g. 2-5")
    ap.add_argument("--nice", type=int, help="nice level")
    ap.add_argument("--noisy-spread", type=float, default=2.0)
    ap.add_argument("--min-free-gb", type=float, default=4.0)
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()

    tgt = os.environ.get("CARGO_TARGET_DIR", "target")
    datagen = os.environ.get("DATAGEN", os.path.join(tgt, "release", "datagen"))
    tools = {"git": None}
    for name, var in (("base", "VCS_BIN_BASE"), ("new", "VCS_BIN_NEW")):
        if os.environ.get(var):
            tools[name] = os.environ[var]
    if os.environ.get("VCS_BIN") and "new" not in tools:
        tools["new"] = os.environ["VCS_BIN"]
    # "neww": a build with `--features watcher` (VCS_BIN_WATCH) with `vcs watch` running; status cells only.
    if os.environ.get("VCS_BIN_WATCH"):
        tools["neww"] = os.environ["VCS_BIN_WATCH"]
    status_only = {"neww"}
    daemons = {}
    prefix = []
    if a.nice is not None:
        prefix += ["nice", "-n", str(a.nice)]
    if a.taskset:
        prefix += ["taskset", "-c", a.taskset]
    rng = random.Random(a.seed)
    work = tempfile.mkdtemp(prefix="vcs-cmp.", dir=os.environ.get("BENCH_TMP"))
    dirs = {t: os.path.join(work, t) for t in tools}

    def guard():
        f = free_gb(work)
        if f < a.min_free_gb:
            sys.exit(f"ABORT: free space {f:.2f} GiB < {a.min_free_gb} GiB")

    def quiet(maxwait=90):
        """Wait until the machine is idle (<8% busy over 1s) or give up; return (busy_pct, waited_s)."""
        t0 = time.time(); b = 100.0
        while True:
            s0 = cpu_stat(); time.sleep(1); s1 = cpu_stat()
            if not s0 or not s1:
                return None, 0
            b = 100.0 - 100.0 * (s1[2] - s0[2]) / max(s1[0] - s0[0], 1)
            if b < 8 or time.time() - t0 > maxwait:
                return b, time.time() - t0

    def cmds(t):
        b = tools[t]
        if t == "neww":
            t = "new"
        if t == "git":
            return dict(init="git init -q -b main", add=["git", "add", "."],
                        commit=["git", "commit", "-q", "-m", "c"], status=["git", "status", "--porcelain"],
                        log=["git", "log", "--oneline", "-n", "50"], branch="git branch side",
                        side="git checkout -q side", main=["git", "checkout", "-q", "main"],
                        reset="rm -rf .git")
        q = lambda s: f"'{b}' {s}"
        return dict(init=q("init") + f"; {q('config set user.name b')} || true; {q('config set user.email b@x.invalid')} || true", add=[b, "add", "."], commit=[b, "commit", "-m", "c"],
                    status=[b, "status"], log=[b, "log", "-n", "50"], branch=q("branch side"),
                    side=q("switch side"), main=[b, "switch", "main"], reset="rm -rf .vcs")

    C = {t: cmds(t) for t in tools}
    sh_cmd = lambda c: c if isinstance(c, str) else " ".join(f"'{x}'" for x in c)
    results = {"meta": {"layout": a.layout, "n": a.n, "runs": a.runs, "tools": tools,
                        "prefix": prefix, "start": time.strftime("%FT%TZ", time.gmtime()),
                        "load1_start": load1(), "nproc": os.cpu_count()}, "cells": {}}

    def cell(name, prep, cmd, status=False):
        """prep(tool) untimed; cmd(tool) -> argv. Interleaved, random order per round."""
        active = [t for t in tools if status or t not in status_only]
        rows = {t: [] for t in active}
        rounds = []
        for t in active:
            if t in status_only:
                dm = subprocess.Popen([tools[t], "watch"], cwd=dirs[t], env=ENV, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL)
                daemons[t] = dm
                time.sleep(1)
                timed(cmd(t), dirs[t], prefix)  # untimed: saves the monitor state
        for _ in range(a.runs):
            guard()
            order = list(active)
            rng.shuffle(order)
            busy, waited = quiet()
            l0, s0 = load1(), cpu_stat()
            for t in order:
                prep(t)
                w, c, rc = timed(cmd(t), dirs[t], prefix)
                rows[t].append((w, c, rc))
            rounds.append({"order": order, "idle_busy_pct": busy, "waited_s": waited, "load_before": l0, "load_after": load1(),
                           "iowait_pct": iowait_pct(s0, cpu_stat())})
        for t, dm in list(daemons.items()):
            dm.terminate(); dm.wait(); daemons.pop(t)
        results["cells"][name] = {"rows": rows, "rounds": rounds}
        print(f"cell {name}: " + " ".join(f"{t}={median([r[1] for r in rows[t]]):.0f}msCPU" for t in rows),
              file=sys.stderr, flush=True)

    try:
        for t in tools:
            os.makedirs(dirs[t])
            guard()
            subprocess.run([datagen, a.layout, str(a.n), dirs[t]], stdout=subprocess.DEVNULL, check=True)
        cell("add (fresh index)", lambda t: sh(f"{C[t]['reset']}; {C[t]['init']}", dirs[t]), lambda t: C[t]["add"])
        cell("commit (all staged)",
             lambda t: sh(f"{C[t]['reset']}; {C[t]['init']}; {sh_cmd(C[t]['add'])}", dirs[t]),
             lambda t: C[t]["commit"])
        for t in tools:
            sh(f"{C[t]['reset']}; {C[t]['init']}; {sh_cmd(C[t]['add'])}; {sh_cmd(C[t]['commit'])}", dirs[t])
        time.sleep(3)
        nop = lambda t: None
        cell("status (clean)", nop, lambda t: C[t]["status"], status=True)
        for t in tools:
            sh(C[t]["branch"], dirs[t])
            modify_1pct(dirs[t], "a")
        cell("status (1% modified)", nop, lambda t: C[t]["status"], status=True)
        for t in tools:
            sh(f"{sh_cmd(C[t]['add'])}; {sh_cmd(C[t]['commit'])}", dirs[t])
            for k in range(1, 49):
                touch_one(dirs[t], k)
                sh(f"{sh_cmd(C[t]['add'])}; {sh_cmd(C[t]['commit'])}", dirs[t])
        cell("log (50 commits)", nop, lambda t: C[t]["log"])
        cell("checkout (1% differ)", lambda t: sh(C[t]["side"], dirs[t]), lambda t: C[t]["main"])
    finally:
        shutil.rmtree(work, ignore_errors=True)

    results["meta"]["load1_end"] = load1()
    report(results, a, rng)
    if a.out:
        json.dump(results, open(a.out, "w"), indent=1)


def report(res, a, rng):
    tools = list(res["meta"]["tools"])
    m = res["meta"]
    print(f"\n### layout={m['layout']} files={m['n']} runs={m['runs']} prefix={' '.join(m['prefix']) or '-'} "
          f"load1 start/end {m['load1_start']:.2f}/{m['load1_end']:.2f}\n")
    print("| cell | tool | wall med | wall min | CPU med | CPU min | CPU max/min | ratio vs git (CPU) | ratio vs git (wall) |")
    print("|---|---|---|---|---|---|---|---|---|")
    for name, c in res["cells"].items():
        rows = c["rows"]
        for t in rows:
            w = [r[0] for r in rows[t]]; cp = [r[1] for r in rows[t]]
            spread = max(cp) / max(min(cp), 0.001)
            noisy = spread > a.noisy_spread
            fail = any(r[2] != 0 for r in rows[t])
            rs = []
            if t != "git":
                gnoisy = max(r[1] for r in rows["git"]) / max(min(r[1] for r in rows["git"]), 0.001) > a.noisy_spread
                for key in (1, 0):
                    if noisy or gnoisy:
                        rs.append("noisy")
                    else:
                        rat = [x[key] / max(g[key], 0.001) for x, g in zip(rows[t], rows["git"])]
                        lo, hi = boot_ci(rat, rng)
                        rs.append(f"{median(rat):.2f} [{lo:.2f}, {hi:.2f}]")
            else:
                rs = ["1", "1"]
            print(f"| {name} | {t}{' (rc!=0)' if fail else ''} | {median(w):.0f} | {min(w):.0f} | {median(cp):.0f} | "
                  f"{min(cp):.0f} | {spread:.2f}{' NOISY' if noisy else ''} | {rs[0]} | {rs[1]} |")
    print("\nload1 before/after each round, iowait% (per cell, rounds in order):")
    for name, c in res["cells"].items():
        print(f"- {name}: " + "; ".join(
            f"{r['load_before']:.1f}->{r['load_after']:.1f}" + (f" io{r['iowait_pct']:.1f}" if r['iowait_pct'] is not None else "")
            for r in c["rounds"]))


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "--report":  # re-render saved JSON: --report a.json b.json
        for f in sys.argv[2:]:
            r = json.load(open(f))
            report(r, argparse.Namespace(noisy_spread=2.0), random.Random(1))
    else:
        main()
