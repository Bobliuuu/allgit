#!/usr/bin/env python3
"""Group-commit pair at high writer counts (docs/research/benchmark-audit.md #4). Linux box only.
usage: bench_groupcommit_fair.py <workdir> <out.json> [writers=512] [pushes=2] [runs=3] [seed_zones=512]
Same vcs-server binary, same flags, same disk backend (local NVMe, fsync) and same driver (writerbench --mode zones: every writer
its own zone, i.e. all contend on the ONE manifest); only `--no-group-commit` differs. Arms alternate per run (off, on, off, on...) so
drift hits both. Each run: fresh backend dir, server start, seed `seed_zones` zones, then the measured run under the shared box lock.
Raw writerbench output (every latency) is saved next to <out.json> as <out>.runs/*.json. Records build commit, binary hashes, load."""
import json, os, shutil, signal, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fair_lib import *

W = os.path.abspath(sys.argv[1]); OUT = sys.argv[2]
NW = int(sys.argv[3]) if len(sys.argv) > 3 else 512
K = int(sys.argv[4]) if len(sys.argv) > 4 else 2
N = int(sys.argv[5]) if len(sys.argv) > 5 else 3
SEED = int(sys.argv[6]) if len(sys.argv) > 6 else 512
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
REL = os.environ.get("CARGO_TARGET_DIR", ROOT + "/target") + "/release"
SRV, WB = REL + "/vcs-server", REL + "/writerbench"
TOK = "bench-token"
res = dict(meta=meta(ROOT, dict(vcs_server=SRV, writerbench=WB)), writers=NW, pushes_per_writer=K, seed_zones=SEED, backend="local disk (NVMe, fsync)", runs=[])
raw = OUT + ".runs"
os.makedirs(raw, exist_ok=True)
os.makedirs(W, exist_ok=True)
hashed = sh(f"{SRV} hash-token {TOK}")


def one(label, flags, i):
    d = f"{W}/run"
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    open(f"{d}/tokens", "w").write(f"bench write * {hashed}\n")
    port = 20000 + (os.getpid() + i * 7) % 20000
    cmd = [SRV, "--backend", f"{d}/repo", "--tokens", f"{d}/tokens", "--listen", f"127.0.0.1:{port}", "--max-connections", "4096"] + flags
    sp = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=open(f"{d}/server.log", "wb"), preexec_fn=lambda: os.setrlimit(os.RLIMIT_NOFILE, (65536, 65536)))
    time.sleep(1.5)
    url = f"http://127.0.0.1:{port}"
    try:
        sh(f"ulimit -n 65536; {WB} {url} {TOK} --mode zones --clients {SEED} --pushes 1 --first-client 100000 --tag seed > /dev/null")
        mf = sum(os.path.getsize(os.path.join(dp, f)) for dp, _, fn in os.walk(f"{d}/repo") for f in fn if "manifest" in f)
        c0 = proc_cpu_ms(sp.pid)
        with box_lock():
            l0 = load1()
            t0 = time.time()
            out = sh(f"ulimit -n 65536; {WB} {url} {TOK} --mode zones --clients {NW} --pushes {K} --tag run")
            wall = time.time() - t0
        srv_cpu = proc_cpu_ms(sp.pid) - c0
        open(f"{raw}/{label}-{i}.json", "w").write(out)
        j = json.loads(out.strip().splitlines()[-1])
        lat = sorted(j["lat_ms"])
        p = lambda q: lat[min(len(lat) - 1, int(q * len(lat)))] if lat else None
        r = dict(arm=label, run=i, acked=j["acked"], of=NW * K, wall_s=j["wall_s"], pps=round(j["acked"] / j["wall_s"], 1), p50_ms=p(.5), p95_ms=p(.95),
                 p99_ms=p(.99), conflicts=j["conflicts"], errors=len(j["errors"]), first_error=(j["errors"][:1] or [None])[0], load1_start=l0,
                 manifest_bytes=mf, server_cpu_s=round(srv_cpu / 1000, 1))
        res["runs"].append(r)
        print(json.dumps(r), flush=True)
    finally:
        sp.send_signal(signal.SIGTERM)
        sp.wait()


for i in range(N):
    one("group_commit_off", ["--no-group-commit"], i)
    one("group_commit_on", [], i)
summ = {}
for a in ("group_commit_off", "group_commit_on"):
    v = [r for r in res["runs"] if r["arm"] == a]
    summ[a] = dict(n=len(v), pps_med=median([r["pps"] for r in v]), p95_ms_med=median([r["p95_ms"] for r in v]), p99_ms_med=median([r["p99_ms"] for r in v]),
                   acked_min=min(r["acked"] for r in v), conflicts_med=median([r["conflicts"] for r in v]), load1_max=max(r["load1_start"] for r in v))
res["summary"] = summ
json.dump(res, open(OUT, "w"), indent=1)
print(json.dumps(summ, indent=1))
