#!/usr/bin/env python3
"""Fair clone pair for the 1M-file claim (docs/research/benchmark-audit.md #1). Linux box only (needs ~30 GB).
usage: bench_clone_fair.py <srctree|-> <workdir> <out.json> [runs=3] [files=1000000]
  srctree = existing tree (output of scripts/huge_gen_tree.py <dest> nested 1000000), or '-' to generate <workdir>/src with 8 slices.
Arms: git loose (clone --no-hardlinks BEFORE gc: what huge_s1.sh timed, cold only) then, after `git gc`:
  git_packed_file_url (file://), git_packed_nohardlinks (local path), git_packed_hardlinks (local default: favours git, informational),
  vcs_clone (published local-path remote). Each arm: N cold runs then N warm runs, arms interleaved per round.
COLD = clean pages of the source repo and the vcs remote evicted with posix_fadvise(DONTNEED) after sync (drop_caches would hurt the
other users of the shared box); dentry/inode caches stay warm. WARM = right after a previous clone of the same source.
Each timed run takes the shared box lock. Output: JSON with meta, raw runs, file counts, sizes, medians."""
import json, os, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fair_lib import *

SRC, W, OUT = sys.argv[1], os.path.abspath(sys.argv[2]), sys.argv[3]
N = int(sys.argv[4]) if len(sys.argv) > 4 else 3
FILES = int(sys.argv[5]) if len(sys.argv) > 5 else 1000000
ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
REL = os.environ.get("CARGO_TARGET_DIR", ROOT + "/target") + "/release"
VCS = REL + "/vcs"
os.environ.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid",
                  GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid", VCS_NO_AUTOWATCH="1",
                  VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid")
G, V, R, C = f"{W}/g", f"{W}/v", f"{W}/r", f"{W}/c"
res = dict(meta=meta(ROOT, dict(vcs=VCS)), files=FILES, runs=N, setup={}, runs_raw=[], counts={}, sizes_kb={})
os.makedirs(W, exist_ok=True)


def timed_setup(label, cmd, **kw):
    t0 = time.time()
    sh(cmd, **kw)
    res["setup"][label] = round(time.time() - t0, 1)
    print("setup", label, res["setup"][label], flush=True)


if SRC == "-":
    SRC = f"{W}/src"
    if not os.path.isdir(SRC):
        gen = os.path.join(ROOT, "scripts/huge_gen_tree.py")
        ps = [subprocess.Popen(["nice", "-n", "10", "python3", gen, SRC, "nested", str(FILES), str(i * FILES // 8), str((i + 1) * FILES // 8)])
              for i in range(8)]
        assert all(p.wait() == 0 for p in ps)
for d in (G, V, R, C):
    shutil.rmtree(d, ignore_errors=True)
timed_setup("copy_src_git", f"cp -a {SRC} {G}")
timed_setup("copy_src_vcs", f"cp -a {SRC} {V}")
timed_setup("git_add_commit", "git init -q && git add -A && git commit -qm one", cwd=G)
os.makedirs(R)
timed_setup("vcs_add_commit_publish", f"{VCS} init && {VCS} add . && {VCS} commit -m one && {VCS} remote set {R} && {VCS} publish", cwd=V)


def count_files(d):
    return sum(len([f for f in fn]) for dp, dn, fn in os.walk(d) if ".git" not in dp.split(os.sep) and ".vcs" not in dp.split(os.sep))


res["counts"]["source"] = count_files(G)


def one(arm, argv, mode, r, seen=set()):
    shutil.rmtree(C, ignore_errors=True)
    if mode == "cold":
        evict(f"{G}/.git")
        evict(R)
    with box_lock():
        x = run_timed(argv)
    x.update(arm=arm, mode=mode, round=r)
    if arm not in seen and x["rc"] == 0:
        seen.add(arm)
        res["counts"][arm] = count_files(C)
        res["sizes_kb"][arm] = int(sh(f"du -sk {C}/.git {C}/.vcs 2>/dev/null | awk '{{s+=$1}} END {{print s}}'", check=False) or 0)
    res_x = x
    res["runs_raw"].append(res_x)
    print(json.dumps(res_x), flush=True)


for r in range(N):
    one("git_loose_nohardlinks", ["git", "clone", "-q", "--no-hardlinks", G, C], "cold", r)
t0 = time.time()
sh("git gc -q", cwd=G)
res["setup"]["git_gc"] = round(time.time() - t0, 1)
res["sizes_kb"]["git_source_after_gc"] = int(sh(f"du -sk {G}/.git | cut -f1"))
res["sizes_kb"]["vcs_remote"] = int(sh(f"du -sk {R} | cut -f1"))
arms = [("git_packed_file_url", ["git", "clone", "-q", f"file://{G}", C]),
        ("git_packed_nohardlinks", ["git", "clone", "-q", "--no-hardlinks", G, C]),
        ("git_packed_hardlinks", ["git", "clone", "-q", G, C]),
        ("vcs_clone", [VCS, "clone", R, C])]
for mode in ("cold", "warm"):
    for r in range(N):
        for arm, argv in arms:
            one(arm, argv, mode, r)
shutil.rmtree(C, ignore_errors=True)
summ = {}
for arm, mode in sorted({(x["arm"], x["mode"]) for x in res["runs_raw"]}):
    v = [x for x in res["runs_raw"] if (x["arm"], x["mode"]) == (arm, mode)]
    summ[f"{arm}/{mode}"] = dict(n=len(v), wall_med_s=round(median([x["wall_ms"] for x in v]) / 1000, 2), wall_min_s=round(min(x["wall_ms"] for x in v) / 1000, 2),
                                wall_max_s=round(max(x["wall_ms"] for x in v) / 1000, 2), cpu_med_s=round(median([x["user_ms"] + x["sys_ms"] for x in v]) / 1000, 2),
                                load1_max=max(x["load1"] for x in v), failures=sum(1 for x in v if x["rc"]))
res["summary"] = summ
json.dump(res, open(OUT, "w"), indent=1)
print(json.dumps(summ, indent=1))
