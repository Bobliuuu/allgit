#!/usr/bin/env python3
"""Audit harness: run the real `vcs` CLI on generated trees; record wall time (untraced, median of N)
and syscall counts (strace -c -f, deterministic so one traced run) per operation.
usage: audit_cli.py <vcs> <datagen> <workdir> <layout> <files> [trials]"""
import json, os, re, shutil, subprocess, sys, time, statistics
vcs, datagen, work, layout, n = sys.argv[1:6]
trials = int(sys.argv[6]) if len(sys.argv) > 6 else 5
env = dict(os.environ, VCS_AUTHOR_NAME="B", VCS_AUTHOR_EMAIL="b@example.invalid", VCS_CONFIG_HOME=work + "/cfg")
os.makedirs(work + "/cfg", exist_ok=True)

def run(args, cwd, trace=None):
    cmd = [vcs] + args
    if trace:
        cmd = ["strace", "-f", "-c", "-o", trace] + cmd
    t = time.perf_counter()
    subprocess.run(cmd, cwd=cwd, env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return (time.perf_counter() - t) * 1e3

def parse(path):
    out = {}
    for l in open(path):
        m = re.match(r"\s*[\d.]+\s+[\d.]+\s+\d+\s+(\d+)\s+(\d+)?\s*(\w+)$", l) or re.match(r"\s*[\d.]+\s+[\d.]+\s+\d+\s+(\d+)\s+(\w+)$", l)
        if m:
            g = m.groups()
            out[g[-1]] = int(g[0])
        elif l.strip().startswith("100.00"):
            out["_total"] = int(l.split()[3])
    return out

def pipeline(root, trace_dir=None):
    ops = {}
    def op(name, args):
        tr = f"{trace_dir}/{name}.txt" if trace_dir else None
        ms = run(args, root, tr)
        ops[name] = parse(tr) if tr else ms
    shutil.rmtree(root, ignore_errors=True)
    subprocess.run([datagen, layout, n, root], check=True, stdout=subprocess.DEVNULL)
    if os.environ.get("SETTLE") == "1":
        time.sleep(2.5)  # let every file mtime fall outside the 2 s racy window before `add` stamps the index
    run(["init"], root)
    op("add_all", ["add", "."])
    op("commit_all", ["commit", "-m", "first"])
    op("status_clean_1", ["status"])
    op("status_clean_2", ["status"])
    # pick a file deterministically via the layout's path function (file index n//2)
    i = int(n) // 2
    rel = (f"d{i//100:04d}/f{i:06d}.txt" if layout == "flat" else f"big/f{i:06d}.txt" if layout == "bigdir" else
           "".join(f"l{(i >> (2*k)) % 4}/" for k in range(8)) + f"f{i:06d}.txt")
    open(f"{root}/{rel}", "w").write("edited once\n")
    op("add_one", ["add", rel])
    op("commit_one", ["commit", "-m", "second"])
    op("status_after_commit", ["status"])
    return ops

res = {"layout": layout, "files": int(n), "vcs": vcs, "settled_before_add": os.environ.get("SETTLE") == "1"}
timed = [pipeline(work + "/t") for _ in range(trials)]
res["wall_ms"] = {k: {"median": statistics.median(t[k] for t in timed), "min": min(t[k] for t in timed), "max": max(t[k] for t in timed)} for k in timed[0]}
td = work + "/trace"; shutil.rmtree(td, ignore_errors=True); os.makedirs(td)
res["syscalls"] = pipeline(work + "/t", td)
print(json.dumps(res))
