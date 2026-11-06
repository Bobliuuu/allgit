#!/usr/bin/env python3
"""Real end-to-end scaling points: vcs against git on IDENTICAL data (one `git fast-export`-style stream feeds both).

usage: e2e.py KIND N WORKDIR OUT.jsonl [--no-git] [--timeout SECS]
  KIND = deep  N linear commits, each touching one of 1,000 files in 100 directories (+ N/10 tags)
         wide  one commit with N files in N/100 directories (the "files" axis)
Every row: {tool, kind, n, step, wall_s, user_s, sys_s, maxrss_kb, rc}. One run per cell, binaries from $VCS_BIN
(default `vcs` on PATH) and `git`. Wall time is wall clock around the child; RSS is the child's peak (wait4 rusage).
The stream generator is scripts/scalability/gen.py. All data lives under WORKDIR and is deleted at the end.
"""
import json, os, resource, shutil, subprocess, sys, time

kind, n, work, out = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
use_git = "--no-git" not in sys.argv
timeout = int(sys.argv[sys.argv.index("--timeout") + 1]) if "--timeout" in sys.argv else 3600
here = os.path.dirname(os.path.abspath(__file__))
gen = os.path.join(here, "..", "scalability", "gen.py")
vcs = os.environ.get("VCS_BIN", "vcs")
env = dict(os.environ, VCS_AUTHOR_NAME="B", VCS_AUTHOR_EMAIL="b@x", GIT_AUTHOR_NAME="B", GIT_AUTHOR_EMAIL="b@x",
           GIT_COMMITTER_NAME="B", GIT_COMMITTER_EMAIL="b@x", XDG_CACHE_HOME=os.path.join(work, "cache"))
shutil.rmtree(work, ignore_errors=True)
os.makedirs(work)
sink = open(out, "a")


def run(tool, step, argv, cwd=None, stdin=None, stdout=subprocess.DEVNULL):
    """Run argv under /usr/bin/time (its %M is the child's own peak RSS; wait4 on a forked python child is floored
    at python's RSS). Wall time is measured around the call."""
    tf = os.path.join(work, "time.out")
    t0 = time.time()
    p = subprocess.Popen(["/usr/bin/time", "-f", "%e %U %S %M", "-o", tf, *argv], cwd=cwd, env=env, stdin=stdin,
                         stdout=stdout, stderr=subprocess.DEVNULL)
    try:
        rc = p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        subprocess.run(["pkill", "-9", "-P", str(p.pid)])
        p.kill()
        p.wait()
        rc = "timeout"
    wall = time.time() - t0
    try:
        el, u, sy, m = open(tf).read().split()[-4:]
        u, sy, m = float(u), float(sy), int(m)
    except Exception:
        u = sy = m = None
    row = dict(tool=tool, kind=kind, n=n, step=step, wall_s=round(wall, 3), user_s=u, sys_s=sy, maxrss_kb=m, rc=rc)
    print(json.dumps(row), flush=True)
    sink.write(json.dumps(row) + "\n")
    sink.flush()
    return rc


def du(path):
    r = subprocess.run(["du", "-sk", path], capture_output=True, text=True)
    return int(r.stdout.split()[0])


def size_row(tool, step, path):
    row = dict(tool=tool, kind=kind, n=n, step=step, disk_kb=du(path))
    print(json.dumps(row), flush=True)
    sink.write(json.dumps(row) + "\n")


stream = os.path.join(work, "s.stream")
t = time.time()
with open(stream, "wb") as f:
    subprocess.run([sys.executable, gen, kind, str(n)], stdout=f, check=True)
print(f"stream {os.path.getsize(stream)/1e6:.0f} MB in {time.time()-t:.0f}s", flush=True)
touch = "d1/f1.txt" if kind == "deep" else "a0/b0/f0.txt"


def edit(repo, tag):
    with open(os.path.join(repo, touch), "a") as f:
        f.write(f"edit {tag}\n")


# ---------------------------------------------------------------- vcs
repo, remote, clone = (os.path.join(work, x) for x in ("v", "vremote", "vclone"))
run("vcs", "init", [vcs, "init", repo])
with open(stream, "rb") as s:
    run("vcs", "import", [vcs, "import-git", "-"], cwd=repo, stdin=s)
size_row("vcs", "disk_after_import", os.path.join(repo, ".vcs"))
run("vcs", "status_first", [vcs, "status"], cwd=repo)
run("vcs", "status_second", [vcs, "status"], cwd=repo)
run("vcs", "log_n20", [vcs, "log", "--oneline", "-n", "20"], cwd=repo)
run("vcs", "rev_list_head", [vcs, "rev-list", "HEAD"], cwd=repo)
run("vcs", "tag_create", [vcs, "tag", "scaletag", "HEAD"], cwd=repo)
edit(repo, 1)
run("vcs", "add_one_file", [vcs, "add", touch], cwd=repo)
run("vcs", "commit_one_file", [vcs, "commit", "-m", "scale edit"], cwd=repo)
os.makedirs(remote)
run("vcs", "remote_set", [vcs, "remote", "set", remote], cwd=repo)
run("vcs", "publish_first", [vcs, "publish"], cwd=repo)
edit(repo, 2)
run("vcs", "add_one_file2", [vcs, "add", touch], cwd=repo)
run("vcs", "commit_one_file2", [vcs, "commit", "-m", "scale edit 2"], cwd=repo)
run("vcs", "publish_incremental", [vcs, "publish"], cwd=repo)
size_row("vcs", "disk_remote", remote)
run("vcs", "clone_full", [vcs, "clone", remote, clone])
run("vcs", "gc_first", [vcs, "gc"], cwd=repo)
run("vcs", "gc_second", [vcs, "gc"], cwd=repo)
run("vcs", "fsck", [vcs, "fsck"], cwd=repo)
size_row("vcs", "disk_after_gc", os.path.join(repo, ".vcs"))
shutil.rmtree(clone, ignore_errors=True)
shutil.rmtree(remote, ignore_errors=True)
shutil.rmtree(repo, ignore_errors=True)

# ---------------------------------------------------------------- git (same stream)
if use_git:
    g, gremote, gclone = (os.path.join(work, x) for x in ("g", "gremote.git", "gclone"))
    os.makedirs(g)
    run("git", "init", ["git", "init", "-q", g])
    with open(stream, "rb") as s:
        run("git", "import", ["git", "fast-import", "--quiet"], cwd=g, stdin=s)
    run("git", "checkout", ["git", "checkout", "-q", "-f", "main"], cwd=g)
    size_row("git", "disk_after_import", os.path.join(g, ".git"))
    run("git", "status_first", ["git", "status", "--porcelain"], cwd=g)
    run("git", "status_second", ["git", "status", "--porcelain"], cwd=g)
    run("git", "log_n20", ["git", "log", "--oneline", "-n", "20"], cwd=g)
    run("git", "rev_list_head", ["git", "rev-list", "HEAD"], cwd=g)
    run("git", "tag_create", ["git", "tag", "scaletag", "HEAD"], cwd=g)
    edit(g, 1)
    run("git", "add_one_file", ["git", "add", touch], cwd=g)
    run("git", "commit_one_file", ["git", "commit", "-q", "-m", "scale edit"], cwd=g)
    run("git", "remote_init", ["git", "init", "-q", "--bare", gremote])
    run("git", "publish_first", ["git", "push", "-q", gremote, "main"], cwd=g)
    edit(g, 2)
    run("git", "add_one_file2", ["git", "add", touch], cwd=g)
    run("git", "commit_one_file2", ["git", "commit", "-q", "-m", "scale edit 2"], cwd=g)
    run("git", "publish_incremental", ["git", "push", "-q", gremote, "main"], cwd=g)
    size_row("git", "disk_remote", gremote)
    run("git", "clone_full", ["git", "clone", "-q", "--no-local", gremote, gclone])
    run("git", "gc_first", ["git", "gc", "-q"], cwd=g)
    run("git", "gc_second", ["git", "gc", "-q"], cwd=g)
    run("git", "commit_graph_write", ["git", "commit-graph", "write", "--reachable"], cwd=g)
    run("git", "fsck", ["git", "fsck", "--no-progress"], cwd=g)
    size_row("git", "disk_after_gc", os.path.join(g, ".git"))
shutil.rmtree(work, ignore_errors=True)
