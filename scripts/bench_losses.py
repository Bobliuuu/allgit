#!/usr/bin/env python3
"""Interleaved wall/CPU timing of git vs vcs for the scoreboard's remaining-loss commands.

  bench_losses.py --work DIR --vcs BIN [--runs N] [--only a,b] [--out F.json]

DIR is a `scripts/command_scoreboard.py setup` tree. Each command runs RUNS times per tool, alternating tools;
reports the median and min of wall and CPU (user+sys via wait4) in ms. macOS-safe (no /proc).
"""
import argparse, json, os, shlex, statistics, subprocess, sys, time

ENV = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", VCS_NO_HOOKS="1")


def timed(argv, cwd):
    t = time.perf_counter()
    p = subprocess.Popen(argv, cwd=cwd, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, st, ru = os.wait4(p.pid, 0)
    return (time.perf_counter() - t) * 1000, (ru.ru_utime + ru.ru_stime) * 1000, os.waitstatus_to_exitcode(st)


def sh(cmd, cwd):
    subprocess.run(["bash", "-c", cmd], cwd=cwd, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--vcs", required=True)
    ap.add_argument("--runs", type=int, default=9)
    ap.add_argument("--only", default="")
    ap.add_argument("--out")
    a = ap.parse_args()
    st = json.load(open(os.path.join(a.work, "state.json")))
    V = os.path.abspath(a.vcs)
    F, FD, N = st["F"], os.path.dirname(st["F"]) or ".", st["needle"]
    # id, clean?, git cmd, vcs cmd
    S = [
        ("log_stat", "clean", "git log --stat --oneline -n 5", f"{V} git log --stat --oneline -n 5"),
        ("log_stat_all", "clean", "git log --stat --oneline", f"{V} git log --stat --oneline"),
        ("bundle_create", "clean", "git bundle create ../out/f.bundle main", f"{V} bundle create ../out/f.bundle main"),
        ("lsfiles", "clean", "git ls-files", f"{V} ls-files"),
        ("grep_path", "clean", f"git grep -F {N} -- {FD}", f"{V} grep -F {N} -- {FD}"),
        ("gc", "clean", "git gc -q", f"{V} gc"),
        ("diff_nameonly", "dirty", "git diff --name-only", f"{V} diff --name-only"),
        ("diff_dirty", "dirty", "git diff", f"{V} diff"),
    ]
    only = [x for x in a.only.split(",") if x]
    res = {}
    for sid, kind, g, v in S:
        if only and sid not in only:
            continue
        data = {"git": [], "vcs": []}
        for i in range(a.runs):
            for tool, cmd in (("git", g), ("vcs", v)):
                cwd = os.path.join(a.work, tool, "repo")
                sh("mkdir -p ../out; rm -f ../out/f.bundle", cwd)
                if kind == "dirty":
                    sh(f"python3 - <<'PY'\nimport os\nr=os.getcwd()\nfs={json.dumps(st['pct'])}\nfor i,p in enumerate(fs):\n open(os.path.join(r,p),'wb').write(('modified x %d\\n'%i).encode()*8)\nPY", cwd) if i == 0 else None
                w, c, rc = timed(shlex.split(cmd), cwd)
                data[tool].append((w, c, rc))
        r = {}
        for tool, xs in data.items():
            r[tool] = dict(wall_med=statistics.median(x[0] for x in xs), wall_min=min(x[0] for x in xs),
                           cpu_med=statistics.median(x[1] for x in xs), cpu_min=min(x[1] for x in xs), rc=sorted({x[2] for x in xs}))
        res[sid] = r
        g_, v_ = r["git"], r["vcs"]
        print(f"{sid:14} git wall {g_['wall_med']:7.1f}/{g_['wall_min']:7.1f} cpu {g_['cpu_med']:7.1f}/{g_['cpu_min']:7.1f} | "
              f"vcs wall {v_['wall_med']:7.1f}/{v_['wall_min']:7.1f} cpu {v_['cpu_med']:7.1f}/{v_['cpu_min']:7.1f} | "
              f"ratio wall {v_['wall_med']/g_['wall_med']:.2f} cpu {v_['cpu_med']/g_['cpu_med']:.2f} rc={g_['rc']}/{v_['rc']}", flush=True)
    if a.out:
        json.dump(res, open(a.out, "w"), indent=1)


main()
