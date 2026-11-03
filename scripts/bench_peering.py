#!/usr/bin/env python3
"""Peer vs origin load test (docs/peering.md): N concurrent `vcs clone`s straight from an origin
vcs-server, through a cold peer, and through a warm peer; origin/peer request counts and bytes
from /metrics, per-clone latency p50/p99.  Box-local servers, loopback, no cloud.

usage: bench_peering.py <vcs-bin-dir> <workdir> [files=400] [file_kb=25] [N,N,..=64,128,256] [repeats=1]
"""
import os, re, shutil, statistics, subprocess, sys, time, urllib.request
from concurrent.futures import ThreadPoolExecutor

bins, work = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
files = int(sys.argv[3]) if len(sys.argv) > 3 else 400
kb = int(sys.argv[4]) if len(sys.argv) > 4 else 25
ns = [int(x) for x in (sys.argv[5] if len(sys.argv) > 5 else "64,128,256").split(",")]
reps = int(sys.argv[6]) if len(sys.argv) > 6 else 1
VCS, SRV = f"{bins}/vcs", f"{bins}/vcs-server"
MT = "metrics-token-0123456789"
shutil.rmtree(work, ignore_errors=True)
os.makedirs(work)
tok = "tok-alice"
h = subprocess.check_output([SRV, "hash-token", tok]).decode().strip()
open(f"{work}/tokens", "w").write(f"alice write * {h}\n")
env = dict(os.environ, VCS_TOKEN=tok, VCS_AUTHOR_NAME="alice", VCS_AUTHOR_EMAIL="a@e.com", VCS_PRINCIPAL="alice")
procs = []

def start(port, args):
    p = subprocess.Popen([SRV, "--tokens", f"{work}/tokens", "--listen", f"127.0.0.1:{port}", "--max-connections", "4096",
                          "--metrics-token", MT, "--log-level", "error"] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    procs.append(p)
    for _ in range(200):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1)
            return p
        except Exception:
            time.sleep(0.05)
    raise SystemExit("server did not start")

def stop(p):
    p.terminate(); p.wait(); procs.remove(p)

def metrics(port):
    rq = urllib.request.Request(f"http://127.0.0.1:{port}/metrics", headers={"Authorization": f"Bearer {MT}"})
    t = urllib.request.urlopen(rq).read().decode()
    reqs = sum(float(m.group(1)) for m in re.finditer(r'^vcs_server_requests_total\{.*\} (\S+)$', t, re.M))
    byt = float(re.search(r'^vcs_server_response_bytes_total (\S+)$', t, re.M).group(1))
    return reqs, byt

origin_url, peer_url = "http://127.0.0.1:18480", "http://127.0.0.1:18481"
origin = start(18480, ["--root", f"{work}/odata"])
seed = f"{work}/seed"
subprocess.check_call([VCS, "init", seed], env=env, stdout=subprocess.DEVNULL)
for i in range(files):
    d = f"{seed}/d{i % 20}"
    os.makedirs(d, exist_ok=True)
    open(f"{d}/f{i}.bin", "wb").write(os.urandom(kb * 1024))
for a in (["add", "."], ["commit", "-m", "seed"], ["remote", "set", origin_url], ["publish"]):
    subprocess.check_call([VCS] + a, cwd=seed, env=env, stdout=subprocess.DEVNULL)
print(f"repo: {files} files x {kb} KiB random = {files*kb/1024:.1f} MiB; box load1={os.getloadavg()[0]:.1f}, cpus={os.cpu_count()}")

def one(args):
    url, dest = args
    t = time.time()
    r = subprocess.run([VCS, "clone", url, dest], env=env, capture_output=True)
    dt = time.time() - t
    shutil.rmtree(dest, ignore_errors=True)
    return dt, r.returncode

def run(label, url, n, srv_ports):
    before = {p: metrics(p) for p in srv_ports}
    t0 = time.time()
    dests = [(url, f"{work}/c/{label}-{n}-{i}") for i in range(n)]
    with ThreadPoolExecutor(n) as ex:
        res = list(ex.map(one, dests))
    wall = time.time() - t0
    lat = sorted(x[0] for x in res)
    bad = sum(1 for x in res if x[1])
    q = lambda p: lat[min(len(lat) - 1, int(p * len(lat)))]
    out = f"{label:12s} N={n:3d} fail={bad} wall={wall:6.2f}s p50={statistics.median(lat):6.2f}s p99={q(0.99):6.2f}s max={lat[-1]:6.2f}s"
    for p in srv_ports:
        a, b = metrics(p)
        out += f" | :{p} reqs={a-before[p][0]:.0f} MiB_out={(b-before[p][1])/2**20:.0f}"
    print(out, flush=True)

os.makedirs(f"{work}/c")
try:
    for rep in range(reps):
        for n in ns:
            run("origin", origin_url, n, [18480])
            peer = start(18481, ["--upstream", origin_url, "--upstream-token", tok, "--cache-dir", f"{work}/pc-{n}-{rep}"])
            run("peer-cold", peer_url, n, [18480, 18481])
            run("peer-warm", peer_url, n, [18480, 18481])
            stop(peer)
            # same warm cache, but the manifest/listing may be served 30 s without asking origin
            peer = start(18481, ["--upstream", origin_url, "--upstream-token", tok, "--cache-dir", f"{work}/pc-{n}-{rep}",
                                 "--upstream-manifest-ttl", "30"])
            run("peer-ttl30", peer_url, n, [18480, 18481])
            stop(peer)
finally:
    for p in procs:
        p.terminate()
