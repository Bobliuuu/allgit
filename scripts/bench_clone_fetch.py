#!/usr/bin/env python3
"""Interleaved clone/fetch benchmark: git vs vcs (one or more builds), file remote and vcs-server loopback.

usage: bench_clone_fetch.py LAYOUT N RUNS WORKDIR DATAGEN VCS_SERVER VCS_BIN [VCS_BIN...]
Cells: clone (file remote: git vs vcs; http: vcs-server vs git file), fetch of one new commit (1 changed file).
Per run every tool executes once, in a shuffled order. Reports wall and CPU (user+sys of children) medians.
"""
import os, sys, subprocess, resource, time, random, shutil, statistics as st, socket

layout, n, runs, work, datagen, server = sys.argv[1], sys.argv[2], int(sys.argv[3]), os.path.abspath(sys.argv[4]), sys.argv[5], sys.argv[6]
bins = sys.argv[7:]
env = dict(os.environ, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", VCS_TOKEN="tok-bench")
DEV = subprocess.DEVNULL


def sh(cmd, cwd=None, check=True):
    return subprocess.run(cmd, cwd=cwd, env=env, stdout=DEV, stderr=DEV, check=check)


def timed(cmd, cwd=None):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t = time.perf_counter()
    subprocess.run(cmd, cwd=cwd, env=env, stdout=DEV, stderr=DEV, check=True)
    w = time.perf_counter() - t
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    return w * 1000, ((r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)) * 1000


shutil.rmtree(work, ignore_errors=True)
os.makedirs(work)
src = f"{work}/src"
sh([datagen, layout, n, src])
tok = subprocess.run([server, "hash-token", "tok-bench"], capture_output=True, text=True, check=True).stdout.strip()
open(f"{work}/tokens", "w").write(f"bench write * {tok}\n")
srvs = []


def start_server(name):
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    root = f"{work}/srvroot-{name}"; os.makedirs(root)
    srvs.append(subprocess.Popen([server, "--root", root, "--tokens", f"{work}/tokens", "--listen", f"127.0.0.1:{port}"], env=env, stdout=DEV, stderr=DEV))
    time.sleep(0.7)
    return f"http://127.0.0.1:{port}"

# tools: (name, kind, bin, remote kind)
tools = [("git-file", "git", None, "file")]
for b in bins:
    tag = os.path.basename(os.path.dirname(os.path.abspath(b))) or os.path.basename(b)
    tools += [(f"{tag}-file", "vcs", b, "file"), (f"{tag}-http", "vcs", b, "http")]

# One source repo per tool (same tree), each publishing to its own remote.
state = {}
for name, kind, b, rk in tools:
    d = f"{work}/s-{name}"
    shutil.copytree(src, d, symlinks=True)
    if kind == "git":
        sh(["git", "init", "-q", "-b", "main"], d); sh(["git", "add", "-A"], d); sh(["git", "commit", "-q", "-m", "init"], d)
        rem = f"{work}/git-remote.git"
        sh(["git", "clone", "-q", "--bare", d, rem]); sh(["git", "remote", "add", "origin", rem], d)
        remote = f"file://{rem}"
    else:
        sh([b, "init"], d)
        for k, v in (("user.name", "b"), ("user.email", "b@x.invalid")):
            sh([b, "config", "set", k, v], d)
        sh([b, "add", "."], d); sh([b, "commit", "-m", "init"], d)
        if rk == "file":
            remote = f"{work}/vcs-remote-{name}"; os.makedirs(remote)
        else:
            remote = start_server(name)
        sh([b, "remote", "set", remote], d)
        sh([b, "publish"], d)
    state[name] = dict(src=d, remote=remote, clone=f"{work}/c-{name}")
    if kind == "git":
        sh(["git", "clone", "-q", remote, state[name]["clone"]])
    else:
        sh([b, "clone", remote, state[name]["clone"]])
    # sanity: clone is complete
    # file touched by every fetch round
files = sorted(os.listdir(src)) if False else None
target = None
for root, ds, fs in os.walk(src):
    ds[:] = sorted(d for d in ds if d not in (".git", ".vcs"))
    if fs:
        target = os.path.relpath(os.path.join(root, sorted(fs)[0]), src); break

res = {}
for i in range(runs):
    order = tools[:]; random.shuffle(order)
    for name, kind, b, rk in order:
        st_ = state[name]
        shutil.rmtree(st_["clone"] + "-n", ignore_errors=True)
        sh(["sync"])
        cmd = ["git", "clone", "-q", st_["remote"], st_["clone"] + "-n"] if kind == "git" else [b, "clone", st_["remote"], st_["clone"] + "-n"]
        res.setdefault(("clone", name), []).append(timed(cmd))
        shutil.rmtree(st_["clone"] + "-n", ignore_errors=True)
    # one-file commit in every source, publish/push, then fetch in the clones
    for name, kind, b, rk in tools:
        st_ = state[name]
        with open(os.path.join(st_["src"], target), "ab") as f:
            f.write(b"change %d\n" % i)
        if kind == "git":
            sh(["git", "commit", "-q", "-a", "-m", f"c{i}"], st_["src"]); sh(["git", "push", "-q", "origin", "main"], st_["src"])
        else:
            sh([b, "add", target], st_["src"]); sh([b, "commit", "-m", f"c{i}"], st_["src"]); sh([b, "publish"], st_["src"])
    order = tools[:]; random.shuffle(order)
    for name, kind, b, rk in order:
        st_ = state[name]
        cmd = ["git", "fetch", "-q"] if kind == "git" else [b, "fetch"]
        res.setdefault(("fetch", name), []).append(timed(cmd, st_["clone"]))

[p.terminate() for p in srvs]
print(f"## {layout} {n} runs={runs} load1={os.getloadavg()[0]:.2f} git={subprocess.run(['git','--version'],capture_output=True,text=True).stdout.strip()}")
for (cell, name), v in sorted(res.items()):
    print(f"{cell:6} {name:28} wall_med {st.median(x[0] for x in v):9.1f} ms  cpu_med {st.median(x[1] for x in v):9.1f} ms")
shutil.rmtree(work, ignore_errors=True)
