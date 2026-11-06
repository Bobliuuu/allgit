#!/usr/bin/env python3
"""Refs at scale: import N tags, then time the commands that used to read and rewrite the whole refs file.
usage: refs_bench.py VCS_BINARY N WORKDIR [legacy]   (legacy sets VCS_REFS_DELTA=0: the old single-file layout)
Wall seconds and peak RSS (MB) per command, one run each. Local numbers; label them as such."""
import os, subprocess, sys, time, shutil, resource
vcs, n, work = sys.argv[1], int(sys.argv[2]), sys.argv[3]
legacy = len(sys.argv) > 4 and sys.argv[4] == "legacy"
env = dict(os.environ, VCS_AUTHOR_NAME="B", VCS_AUTHOR_EMAIL="b@x")
if legacy:
    env["VCS_REFS_DELTA"] = "0"
shutil.rmtree(work, ignore_errors=True); os.makedirs(work)
stream = os.path.join(work, "s.stream")
with open(stream, "w") as f:
    f.write("blob\nmark :1\ndata 2\nx\n\ncommit refs/heads/main\nmark :2\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\ndata 1\nc\nM 100644 :1 f.txt\n\n")
    for t in range(n):
        f.write(f"reset refs/tags/t{t}\nfrom :2\n\n")
def run(label, *args, cwd=None, stdin=None):
    t = time.time()
    p = subprocess.Popen([vcs, *args], cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, stdin=stdin)
    _, st, ru = os.wait4(p.pid, 0)
    dt = time.time() - t
    rc = os.waitstatus_to_exitcode(st)
    print(f"{label:28s} {dt:8.3f} s {ru.ru_maxrss/(1024 if sys.platform == 'linux' else 1048576):8.0f} MB" + ("" if rc == 0 else f"  (exit {rc})"), flush=True)
    return rc
repo = os.path.join(work, "r")
run("init", "init", repo)
run("import-git (%d tags)" % n, "import-git", stream, cwd=repo)
vd = os.path.join(repo, ".vcs")
sizes = {k: os.path.getsize(os.path.join(vd, k)) for k in os.listdir(vd) if k.startswith("refs") or k.startswith("reftags")}
print("state files:", sizes)
for label, a in [("tag create (new)", ["tag", "nt1", "HEAD"]), ("tag create again", ["tag", "nt2", "HEAD"]),
                 ("tag delete", ["tag", "-d", "nt1"]), ("branch create", ["branch", "bq"]),
                 ("log --oneline -n 1", ["log", "--oneline", "-n", "1"]), ("status", ["status"]),
                 ("tag (list)", ["tag"]), ("gc", ["gc"])]:
    run(label, *a, cwd=repo)
print("state files after:", {k: os.path.getsize(os.path.join(vd, k)) for k in os.listdir(vd) if k.startswith("refs") or k.startswith("reftags")})
