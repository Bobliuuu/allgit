#!/usr/bin/env python3
"""Synthetic multi-zone monorepo benchmark for `sync --auto` / `clone --auto-profile` (docs/research/zone-profile-bench.md).

  bench_zoneprofile.py --phase gen --work DIR --vcs BIN --server BIN      # build repos (not timed)
  flock ~/bench.lock bench_zoneprofile.py --phase run --work DIR ...      # timed runs

Everything is local: vcs-server on 127.0.0.1 (no network latency), git over file://. Writes DIR/result.json.
"""
import argparse, json, os, random, resource, shutil, statistics, subprocess, sys, time, re, signal

ap = argparse.ArgumentParser()
ap.add_argument("--phase", choices=["gen", "run"], required=True)
ap.add_argument("--work", required=True)
ap.add_argument("--vcs", required=True)
ap.add_argument("--server", required=True)
ap.add_argument("--zones", type=int, default=50)
ap.add_argument("--files", type=int, default=2000, help="files per zone")
ap.add_argument("--runs", type=int, default=3)
a = ap.parse_args()
W = os.path.abspath(a.work)
ENV = dict(os.environ, VCS_AUTHOR_NAME="Bench", VCS_AUTHOR_EMAIL="b@example.com", VCS_PRINCIPAL="alice",
           VCS_TOKEN="tok-alice", VCS_NO_CACHE="1", VCS_PROFILE_NOW_MS=str(100 * 86400000), GIT_AUTHOR_NAME="b",
           GIT_AUTHOR_EMAIL="b@e", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@e")
Z = [f"z{i:02}" for i in range(a.zones)]
rng = random.Random(7)
DEPS = {z: sorted({Z[rng.randrange(i)] for _ in range(rng.randrange(4))}) if i else [] for i, z in enumerate(Z)}


def run(cmd, cwd=None, env=None, check=True):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, env=env or ENV, capture_output=True, text=True)
    wall = time.perf_counter() - t
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if check and p.returncode:
        sys.exit(f"FAILED {cmd}: {p.stderr[-800:]}{p.stdout[-400:]}")
    return p, wall, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)


def vcs(args, cwd=None, **kw):
    return run([a.vcs] + args, cwd=cwd, **kw)


def du(path, skip=()):
    n = 0
    for d, dirs, fs in os.walk(path):
        for f in fs:
            try:
                n += os.lstat(os.path.join(d, f)).st_size
            except OSError:
                pass
    return n


WORDS = None


def pool():
    global WORDS
    if WORDS is None:
        r = random.Random(1)
        voc = ["".join(r.choice("abcdefghijklmnopqrstuvwxyz") for _ in range(r.randrange(3, 10))) for _ in range(4000)]
        WORDS = " ".join(r.choice(voc) + (";\n" if r.random() < 0.08 else "") for _ in range(900000)).encode()
    return WORDS


def gen_tree(root):
    p = pool()
    r = random.Random(3)
    for i, z in enumerate(Z):
        base = os.path.join(root, "crates", z)
        os.makedirs(base, exist_ok=True)
        toml = f'[package]\nname = "{z}"\nversion = "0.1.0"\n\n[dependencies]\n' + "".join(
            f'{d} = {{ path = "../{d}" }}\n' for d in DEPS[z])
        open(os.path.join(base, "Cargo.toml"), "w").write(toml)
        per_dir = 100
        for k in range(a.files):
            d = os.path.join(base, "src", f"m{k // per_dir:02}")
            if k % per_dir == 0:
                os.makedirs(d, exist_ok=True)
            off = r.randrange(len(p) - 6000)
            ln = r.randrange(1000, 5000)
            open(os.path.join(d, f"f{k % per_dir:03}.rs"), "wb").write(p[off:off + ln] + f"\n// {z} {k}\n".encode())
    open(os.path.join(root, "README"), "w").write("root\n")


def start_server():
    h = subprocess.run([a.server, "hash-token", "tok-alice"], capture_output=True, text=True).stdout.strip()
    tok = os.path.join(W, "tokens")
    open(tok, "w").write(f"# principal role zones token-hash\nalice write * {h}\n")
    p = subprocess.Popen([a.server, "--root", os.path.join(W, "data"), "--tokens", tok, "--listen", "127.0.0.1:0"],
                         stderr=open(os.path.join(W, "server.log"), "w"), stdout=subprocess.DEVNULL, text=True)
    for _ in range(100):  # the server prints its address as its first stderr line
        time.sleep(0.1)
        line = open(os.path.join(W, "server.log")).readline()
        if "http://" in line:
            break
    url = [w for w in line.split() if w.startswith("http://")][0]
    return p, url


def mutate(i, zones, repo_dirs):
    """Append a line to 100 files in each of `zones` (same change in every repo)."""
    for rd in repo_dirs:
        for z in zones:
            for k in range(100):
                f = os.path.join(rd, "crates", z, "src", f"m{(k * 7 + i) % (a.files // 100):02}", f"f{k:03}.rs")
                with open(f, "ab") as fh:
                    fh.write(f"// change {i}\n".encode())


STAT = re.compile(r"requests:? (\d+), downloaded:? (\d+) bytes")


def stat(p):
    m = STAT.findall(p.stdout + p.stderr)
    return (int(m[-1][0]), int(m[-1][1])) if m else (None, None)


if a.phase == "gen":
    shutil.rmtree(W, ignore_errors=True)
    os.makedirs(W)
    t0 = time.time()
    gen_tree(os.path.join(W, "pub"))
    print("generated", sum(len(f) for _, _, f in os.walk(os.path.join(W, "pub"))), "files", round(time.time() - t0), "s")
    shutil.copytree(os.path.join(W, "pub"), os.path.join(W, "gitsrc"))
    srv, url = start_server()
    pub = os.path.join(W, "pub")
    vcs(["init", "."], pub)
    for z in Z:
        vcs(["zone", "create", z, "--path", f"crates/{z}"], pub)
    vcs(["add", "."], pub)
    vcs(["commit", "-m", "init"], pub)
    vcs(["remote", "set", url], pub)
    p, w, _ = vcs(["publish"], pub)
    print("vcs publish", round(w, 1), "s")
    srv.terminate()
    g = os.path.join(W, "gitsrc")
    run(["git", "init", "-q", "-b", "main"], g)
    run(["git", "add", "."], g)
    run(["git", "commit", "-q", "-m", "init"], g)
    run(["git", "clone", "-q", "--bare", g, os.path.join(W, "bare.git")])
    for k, v in [("uploadpack.allowFilter", "true"), ("uploadpack.allowAnySHA1InWant", "true")]:
        run(["git", "config", k, v], os.path.join(W, "bare.git"))
    run(["git", "remote", "add", "origin", os.path.join(W, "bare.git")], g)
    run(["git", "fetch", "-q", "origin"], g)
    run(["git", "branch", "-q", "--set-upstream-to=origin/main", "main"], g)
    print("gen done; vcs remote bytes", du(os.path.join(W, "data")), "git bare bytes", du(os.path.join(W, "bare.git")))
    sys.exit(0)

# ------------------------------------------------------------------------------------------ run
res = {"zones": a.zones, "files_per_zone": a.files, "runs": a.runs, "load1_start": os.getloadavg()[0],
       "git_version": run(["git", "--version"])[0].stdout.strip(), "vcs_version": vcs(["--version"])[0].stdout.strip(),
       "cores": os.cpu_count()}
pub, gsrc, bare = (os.path.join(W, x) for x in ("pub", "gitsrc", "bare.git"))
srv, url = start_server()
vcs(["remote", "set", url], pub)  # the server restarted on a new port
out = os.path.join(W, "runs")
shutil.rmtree(out, ignore_errors=True)
os.makedirs(out)

# the persona: edits z07, reads z08 and z21 in the same sessions, builds z07
prof = os.path.join(W, "persona.profile")
with open(prof, "w") as f:
    f.write("vcs-profile 1\n")
    for d in (88, 91, 94, 97, 99):
        t = d * 86400000
        f.write(f"{t}\te\tz07\tsrc\n{t + 60000}\tr\tz08\tsrc\n{t + 120000}\tr\tz21\tsrc\n{t + 180000}\tb\tz07\t\n")


def med(v):
    v = [x for x in v if x is not None]
    return statistics.median(v) if v else None


def rec(name, rows):
    res[name] = {"wall_s": [r[0] for r in rows], "cpu_s": [r[1] for r in rows], "requests": [r[2] for r in rows],
                 "bytes_down": [r[3] for r in rows], "disk_bytes": [r[4] for r in rows]}


full_rows, auto_rows, gfull_rows, gpart_rows = [], [], [], []
hot = []
for i in range(a.runs):
    d = os.path.join(out, f"full{i}")
    p, w, c = vcs(["clone", url, d, "--stats"])
    r, b = stat(p)
    full_rows.append((w, c, r, b, du(d)))
    if i < a.runs - 1:
        shutil.rmtree(d)
    d = os.path.join(out, f"auto{i}")
    p, w, c = vcs(["clone", url, d, "--auto-profile", prof, "--stats"])
    r, b = stat(p)
    auto_rows.append((w, c, r, b, du(d)))
    if i == 0:
        res["auto_clone_output"] = p.stdout
        hot = [l.split()[0] for l in p.stdout.splitlines() if l.startswith("  ") and "added" not in l][: 0]
        blk = []
        on = False
        for l in p.stdout.splitlines():
            if l.startswith("hot zones"):
                on = True
            elif l.startswith("skipped zones"):
                on = False
            elif on and l.startswith("  ") and not l.strip().startswith("-"):
                blk.append(l.split()[0])
        hot = blk
    if i < a.runs - 1:
        shutil.rmtree(d)
    d = os.path.join(out, f"gfull{i}")
    p, w, c = run(["git", "clone", "-q", bare, d])
    gfull_rows.append((w, c, None, None, du(os.path.join(d, ".git"))))
    if i < a.runs - 1:
        shutil.rmtree(d)
    d = os.path.join(out, f"gpart{i}")
    t = time.perf_counter()
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    run(["git", "clone", "-q", "--filter=blob:none", "--no-checkout", "--sparse", "file://" + bare, d])
    run(["git", "sparse-checkout", "set", "--cone"] + [f"crates/{z}" for z in hot if z != "root"], d)
    run(["git", "checkout", "-q", "main"], d)
    w = time.perf_counter() - t
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    gpart_rows.append((w, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime), None, None, du(os.path.join(d, ".git"))))
    if i < a.runs - 1:
        shutil.rmtree(d)
res["hot_zones"] = hot
rec("clone_full_vcs", full_rows)
rec("clone_auto_vcs", auto_rows)
rec("clone_full_git", gfull_rows)
rec("clone_partial_sparse_git", gpart_rows)

# incremental sync: same upstream change applied to vcs and git
lastf, lasta = os.path.join(out, f"full{a.runs - 1}"), os.path.join(out, f"auto{a.runs - 1}")
lastg, lastp = os.path.join(out, f"gfull{a.runs - 1}"), os.path.join(out, f"gpart{a.runs - 1}")
sf, sa, sg, sp = [], [], [], []
hotz = [z for z in hot if z.startswith("z")]
for i in range(a.runs):
    others = rng.sample([z for z in Z if z not in hotz], 7)
    zs = hotz[:3] + others
    mutate(i + 1, zs, [pub, gsrc])
    vcs(["add", "."], pub)
    vcs(["commit", "-m", f"change {i}"], pub)
    vcs(["publish"], pub)
    run(["git", "add", "."], gsrc)
    run(["git", "commit", "-q", "-m", f"change {i}"], gsrc)
    run(["git", "push", "-q", "origin", "main"], gsrc)
    bf = du(os.path.join(lastg, ".git"))
    bp = du(os.path.join(lastp, ".git"))
    p, w, c = vcs(["sync", "--stats"], lastf)
    r, b = stat(p)
    sf.append((w, c, r, b, None))
    p, w, c = vcs(["sync", "--auto", "--stats"], lasta)
    r, b = stat(p)
    sa.append((w, c, r, b, None))
    p, w, c = run(["git", "pull", "-q", "--ff-only"], lastg)
    sg.append((w, c, None, du(os.path.join(lastg, ".git")) - bf, None))
    p, w, c = run(["git", "pull", "-q", "--ff-only"], lastp)
    sp.append((w, c, None, du(os.path.join(lastp, ".git")) - bp, None))
rec("sync_full_vcs", sf)
rec("sync_auto_vcs", sa)
rec("sync_full_git", sg)
rec("sync_partial_sparse_git", sp)

# on-demand fetch of one skipped zone (50 zones: pick one that is not hot)
cold = next(z for z in Z if z not in hotz and z != "z00")
p, w, c = vcs(["profile", "fetch", cold, "--stats"], lasta)
r, b = stat(p)
res["on_demand_vcs"] = {"zone": cold, "wall_s": w, "cpu_s": c, "requests": r, "bytes_down": b}
bp = du(os.path.join(lastp, ".git"))
p, w, c = run(["git", "sparse-checkout", "add", f"crates/{cold}"], lastp)
res["on_demand_git"] = {"zone": cold, "wall_s": w, "cpu_s": c, "bytes_down": du(os.path.join(lastp, ".git")) - bp}

# recording overhead on a cheap command (interleaved)
on, off = [], []
env_off = dict(ENV, VCS_PROFILE="off")
for _ in range(40):
    on.append(vcs(["status"], lastf)[1])
    off.append(vcs(["status"], lastf, env=env_off)[1])
res["status_wall_ms"] = {"recording_on_median": 1000 * statistics.median(on), "recording_off_median": 1000 * statistics.median(off), "n": 40}
res["load1_end"] = os.getloadavg()[0]
srv.terminate()
json.dump(res, open(os.path.join(W, "result.json"), "w"), indent=1)
print(json.dumps({k: v for k, v in res.items() if k != "auto_clone_output"}, indent=1))
