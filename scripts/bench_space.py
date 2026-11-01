#!/usr/bin/env python3
"""Space-cost sanity check: how many bytes do git and vcs store for the same history?

MEASUREMENT ONLY. vcs optimizes for speed, not storage; this just flags cells where vcs uses a crazy amount
of extra space. No timing is measured. Nothing here changes product code.

usage: bench_space.py <flat|deep|binary> <n_files> <out.json> [--work DIR] [--keep]
       bench_space.py report <out.json>...            # print the markdown table(s)

Dataset: `datagen <layout> <n> <dir> 42`, one initial commit of everything, then 50 commits that each append one
line to one file (file index (i*7919+42) % n), exactly like scripts/walgit_bench (git keeps its default gc.auto; only gc.autoDetach=false so a
background auto-gc cannot race the measurement). Needs `datagen`, `vcs`,
`vcs-server`, `git` on PATH (cargo build --release -p vcs-cli -p vcs-server -p vcs-bench).

Sizes: "apparent" = sum of st_size of regular files (like `du --apparent-size`, directories not counted);
"alloc" = sum of st_blocks*512 over files and directories (like `du`). "files" = number of regular files.

Cells per layout/size (client = the repo directory's .git / .vcs; remote = the remote's whole store):
  wt                          working-tree bytes (no .git/.vcs): the baseline
  git.client                  after 1+50 commits, git's default config (gc.auto at default)
  git.remote_push             bare repo after `git push` (file://)
  git.clone                   .git of a fresh `git clone`
  git.client_gc, git.remote_gc   after `git gc` on the client / on the bare repo
  vcs.<cfg>.client            .vcs after 1+50 commits
  vcs.<cfg>.remote_dir        directory remote after `vcs publish`
  vcs.<cfg>.remote_server     `vcs-server --root DIR` store after `vcs publish` over http
  vcs.<cfg>.clone             .vcs of a fresh `vcs clone <directory remote>`
  vcs.<cfg>.client_gc         after `vcs repack` + `vcs gc` (default pack format)
  vcs.<cfg>.client_repack_<f> after `vcs repack --format <f>` on a copy (f = v1 v2 v3 v4)
  vcs.<cfg>.remote_gc         directory remote after `vcs gc --remote --yes`
  vcs.<cfg>.remote_compact    ... then `vcs gc --remote --compact --yes` (default compaction format)
  vcs.<cfg>.remote_compact_v2 on a copy of the remote: `vcs gc --remote --compact --yes --pack-format v2`
Configs <cfg>:
  default   no configuration; auto-repack at gc.autoPackLimit (6700 loose objects) packs old loose objects
  loose     `vcs config set gc.autoPackLimit 0` right after init: never auto-pack, every object stays a loose file
            (the lowest-CPU write path; the clone/remote are default-config)
"""
import json, os, shutil, subprocess, sys, time, platform, socket

ENV = dict(os.environ, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b",
           GIT_COMMITTER_EMAIL="b@x.invalid", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
           GIT_TERMINAL_PROMPT="0", GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="gc.autoDetach", GIT_CONFIG_VALUE_0="false", VCS_TOKEN="tok1", VCS_PRINCIPAL="bench", VCS_NO_HOOKS="1")
NHIST = 50


def sh(cmd, cwd=None, check=True):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=ENV, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"failed ({r.returncode}): {cmd}\n{r.stdout[-800:]}\n{r.stderr[-1500:]}")
    return r


def size(path):
    """-> {apparent, alloc, files}"""
    app = alloc = nf = 0
    for d, ds, fs in os.walk(path):
        alloc += os.lstat(d).st_blocks * 512
        for f in fs:
            st = os.lstat(os.path.join(d, f))
            app += st.st_size
            alloc += st.st_blocks * 512
            nf += 1
    return {"apparent": app, "alloc": alloc, "files": nf}


def tree_size(root):
    """working tree without .git / .vcs"""
    app = alloc = nf = 0
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs")]
        alloc += os.lstat(d).st_blocks * 512
        for f in fs:
            st = os.lstat(os.path.join(d, f))
            app += st.st_size
            alloc += st.st_blocks * 512
            nf += 1
    return {"apparent": app, "alloc": alloc, "files": nf}


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def edit(d, files, i):
    p = files[(i * 7919 + 42) % len(files)]
    with open(f"{d}/{p}", "a") as f:
        f.write(f"edit {i}\n")
    return p


def list_files(root):
    out = []
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs")]
        for f in fs:
            out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out)


def build(tool, src, files, cfg):
    if tool == "git":
        sh("git init -q -b main && git add -A && git commit -q -m initial", cwd=src)
    else:
        sh("vcs init", cwd=src)
        if cfg == "loose":
            sh("vcs config set gc.autoPackLimit 0", cwd=src)
        sh("vcs add . && vcs commit -m initial", cwd=src)
    for i in range(1, NHIST + 1):
        p = edit(src, files, i)
        if tool == "git":
            sh(f"git add '{p}' && git commit -q -m e{i}", cwd=src)
        else:
            sh(f"vcs add '{p}' && vcs commit -m e{i}", cwd=src)


def run_vcs_cfg(cfg, ds, files, work, out):
    pre = f"vcs.{cfg}."
    cl = f"{work}/vcs-{cfg}"
    sh(f"cp -a '{ds}' '{cl}'")
    build("vcs", cl, files, cfg)
    out[pre + "client"] = size(f"{cl}/.vcs")
    # server remote on a copy (so the main client keeps one remote)
    c2 = f"{work}/vcs-{cfg}-srv"
    sh(f"cp -a '{cl}' '{c2}'")
    sdir = f"{work}/vcs-{cfg}-srvroot"
    os.makedirs(sdir)
    tok = sh("vcs-server hash-token tok1").stdout.strip()
    open(f"{work}/tokens", "w").write(f"bench write * {tok}\n")
    port = free_port()
    log = open(f"{work}/server.log", "ab")
    srv = subprocess.Popen(["vcs-server", "--root", sdir, "--tokens", f"{work}/tokens", "--listen", f"127.0.0.1:{port}"],
                           env=ENV, stdout=log, stderr=log)
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), 1).close()
                break
            except OSError:
                time.sleep(0.2)
        sh(f"vcs remote set http://127.0.0.1:{port} && vcs publish", cwd=c2)
        out[pre + "remote_server"] = size(sdir)
    except Exception as e:
        out[pre + "remote_server"] = {"error": str(e)[:300]}
    finally:
        srv.terminate()
        srv.wait()
    shutil.rmtree(c2)
    # directory remote
    rem = f"{work}/vcs-{cfg}-rem"
    sh(f"vcs remote set '{rem}' && vcs publish", cwd=cl)
    out[pre + "remote_dir"] = size(rem)
    cn = f"{work}/vcs-{cfg}-clone"
    sh(f"vcs clone '{rem}' '{cn}'")
    out[pre + "clone"] = size(f"{cn}/.vcs")
    shutil.rmtree(cn)
    # local repack formats on copies
    for f in ("v1", "v2", "v3", "v4"):
        c3 = f"{work}/vcs-{cfg}-rp"
        sh(f"cp -a '{cl}' '{c3}'")
        r = sh(f"vcs repack --format {f}", cwd=c3, check=False)
        out[pre + "client_repack_" + f] = size(f"{c3}/.vcs") if r.returncode == 0 else {"error": (r.stderr or r.stdout)[-200:]}
        shutil.rmtree(c3)
    # local maintenance on the main client
    sh("vcs repack && vcs gc", cwd=cl)
    out[pre + "client_gc"] = size(f"{cl}/.vcs")
    # remote maintenance: v2 variant on a copy first
    rem2 = f"{work}/vcs-{cfg}-rem2"
    sh(f"cp -a '{rem}' '{rem2}'")
    sh(f"vcs remote set '{rem2}'", cwd=cl)
    sh("vcs gc --remote --compact --yes --pack-format v2", cwd=cl)
    out[pre + "remote_compact_v2"] = size(rem2)
    shutil.rmtree(rem2)
    sh(f"vcs remote set '{rem}'", cwd=cl)
    sh("vcs gc --remote --yes", cwd=cl)
    out[pre + "remote_gc"] = size(rem)
    sh("vcs gc --remote --compact --yes", cwd=cl)
    out[pre + "remote_compact"] = size(rem)
    shutil.rmtree(cl)
    shutil.rmtree(rem)
    shutil.rmtree(sdir)


def run_git(ds, files, work, out):
    cl = f"{work}/git"
    sh(f"cp -a '{ds}' '{cl}'")
    build("git", cl, files, None)
    out["git.client"] = size(f"{cl}/.git")
    bare = f"{work}/git-bare.git"
    sh(f"git init -q --bare -b main '{bare}'")
    sh(f"git push -q 'file://{bare}' main", cwd=cl)
    out["git.remote_push"] = size(bare)
    cn = f"{work}/git-clone"
    sh(f"git clone -q 'file://{bare}' '{cn}'")
    out["git.clone"] = size(f"{cn}/.git")
    shutil.rmtree(cn)
    sh("git gc -q", cwd=cl)
    out["git.client_gc"] = size(f"{cl}/.git")
    sh("git gc -q", cwd=bare)
    out["git.remote_gc"] = size(bare)
    shutil.rmtree(cl)
    shutil.rmtree(bare)


def main():
    a = sys.argv[1:]
    layout, n, outp = a[0], int(a[1]), a[2]
    work = os.path.expanduser("~/sb/work")
    if "--work" in a:
        work = a[a.index("--work") + 1]
    work = f"{work}/{layout}-{n}"
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    ds = f"{work}/ds"
    sh(f"datagen {layout} {n} '{ds}' 42")
    files = list_files(ds)
    out = {"wt": tree_size(ds)}
    meta = {"layout": layout, "n_files": len(files), "history_commits": NHIST + 1, "date": time.strftime("%FT%TZ", time.gmtime()),
            "machine": platform.node(), "kernel": platform.release(), "cores": os.cpu_count(),
            "cpu": next((l.split(":")[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), "?"),
            "fs": sh(f"df -T '{work}' | tail -1").stdout.split()[1], "git": sh("git --version").stdout.strip(),
            "vcs": sh("vcs --version").stdout.strip(), "vcs_commit": os.environ.get("VCS_COMMIT", "?")}
    res = {"meta": meta, "cells": out}
    for fn, label in ((lambda: run_git(ds, files, work, out), "git"),
                      (lambda: run_vcs_cfg("default", ds, files, work, out), "vcs default"),
                      (lambda: run_vcs_cfg("loose", ds, files, work, out), "vcs loose")):
        try:
            fn()
        except Exception as e:
            out[f"error.{label}"] = str(e)[:1500]
        json.dump(res, open(outp, "w"), indent=1)
        print("done", label, file=sys.stderr, flush=True)
    if "--keep" not in a:
        shutil.rmtree(work, ignore_errors=True)


def mib(b):
    return f"{b / 1048576:.1f}"


# (row label, vcs cell suffix, git cell it is compared with)
ROWS = [("client after 51 commits", "client", "git.client"),
        ("client after maintenance", "client_gc", "git.client_gc"),
        ("client after `repack --format v2`", "client_repack_v2", "git.client_gc"),
        ("client after `repack --format v3`", "client_repack_v3", "git.client_gc"),
        ("client after `repack --format v4`", "client_repack_v4", "git.client_gc"),
        ("client after `repack --format v1`", "client_repack_v1", "git.client_gc"),
        ("remote store after push (directory)", "remote_dir", "git.remote_push"),
        ("remote store after push (vcs-server)", "remote_server", "git.remote_push"),
        ("remote after `gc --remote`", "remote_gc", "git.remote_push"),
        ("remote after `gc --remote --compact`", "remote_compact", "git.remote_gc"),
        ("remote after compact `--pack-format v2`", "remote_compact_v2", "git.remote_gc"),
        ("fresh clone", "clone", "git.clone")]


def flag(r):
    return "RED" if r > 10 else "FLAG" if r > 3 else ""


def report(paths):
    for p in paths:
        d = json.load(open(p))
        m, c = d["meta"], d["cells"]
        wt = c["wt"]
        print(f"### {m['layout']}, {m['n_files']} files (working tree {mib(wt['apparent'])} MiB apparent, "
              f"{mib(wt['alloc'])} MiB allocated)\n")
        print("Bytes are MiB apparent / MiB allocated (files). Ratio = vcs / git on apparent bytes, git column shown "
              "in the row (git maintenance column paired with vcs maintenance).\n")
        print("| cell | git | vcs default | ratio | vcs loose | ratio |")
        print("|---|---|---|---|---|---|")
        flagged = []
        for label, suf, gk in ROWS:
            g = c.get(gk)
            if not g or "apparent" not in g:
                continue
            cells = [f"{mib(g['apparent'])} / {mib(g['alloc'])} ({g['files']})"]
            for cfg in ("default", "loose"):
                v = c.get(f"vcs.{cfg}.{suf}")
                if not v or "apparent" not in v:
                    cells += ["n/a", ""]
                    continue
                ra, rl = v["apparent"] / max(g["apparent"], 1), v["alloc"] / max(g["alloc"], 1)
                fl = flag(max(ra, rl))
                if fl:
                    flagged.append((cfg, label, fl, ra, rl))
                cells += [f"{mib(v['apparent'])} / {mib(v['alloc'])} ({v['files']})",
                          f"{ra:.2f}x / {rl:.2f}x {fl}".strip()]
            print(f"| {label} | " + " | ".join(cells) + " |")
        print("\nvs working tree (apparent): git client "
              f"{c['git.client']['apparent'] / wt['apparent']:.2f}x, vcs default client {c.get('vcs.default.client', {}).get('apparent', 0) / wt['apparent']:.2f}x, "
              f"vcs loose client {c.get('vcs.loose.client', {}).get('apparent', 0) / wt['apparent']:.2f}x\n")
        if flagged:
            print("Flagged: " + "; ".join(f"{cfg}/{l}: {fl} ({ra:.1f}x apparent, {rl:.1f}x alloc)" for cfg, l, fl, ra, rl in flagged) + "\n")
        for k, v in c.items():
            if k.startswith("error") or (isinstance(v, dict) and "error" in v):
                print(f"ERROR {k}: {v}\n")


if __name__ == "__main__":
    if sys.argv[1] == "report":
        report(sys.argv[2:])
    else:
        main()
