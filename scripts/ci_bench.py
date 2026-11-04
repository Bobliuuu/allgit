#!/usr/bin/env python3
"""CI-workload benchmark: vcs vs git (git:// daemon and file://), 100k flat/deep, plus parallel-runner fan-out.

usage (Linux box with `cargo build --release -p vcs-cli -p vcs-server -p vcs-bench` done):
  scripts/ci_bench.py run <flat|deep> <n_files> <rounds> <out.json> [--fan 8,32,64] [--fan-rounds 3]
  scripts/ci_bench.py report <out.json>...      # markdown tables with WIN/TIE/LOSS verdicts

Scenarios (every cell is run once per round, order rotated each round = interleaved):
  s1 cold clone of the job slice (one directory g07, 2% of files): vcs --for / vcs full / git depth-1 (+sparse, +blobless) / blobless / full
  s2 warm runner: fetch one specific new commit and check it out (vcs fetch + switch --detach vs git fetch <sha> + checkout)
  s3 PR changed files: fetch the PR head, list changed files vs base (git diff --name-only, vcs diff-tree), and status with 20 modified files
  s4 `clean -fdx` + checkout of the other commit between jobs (3000 untracked build files, 20 modified tracked files)
  s5 cache-warm re-clone: new working copy from a local cache of the repo (git --reference --dissociate / local mirror; vcs clone of a local directory remote and of a bundle file)
  s6 fan-out: K simultaneous cold clones from one server (vcs-server vs git daemon vs git file://)
Metrics: wall ms, client CPU (wait4 rusage of the child tree), server CPU (utime+stime+reaped children of the server process),
requests and bytes (vcs --stats; git:// through a counting TCP forwarder in a separate single pass; file:// has none).
Verdict rule (owner): TIE if ratio within 10%, or min-max ranges overlap, or absolute gap < 2 ms; LOSS only if vcs is slower by
more than 10% AND the ranges do not overlap; WIN is the mirror image.
"""
import json, os, platform, re, shutil, socket, statistics, subprocess, sys, time

HOME = os.path.expanduser("~")
REL = os.environ.get("CI_REL", f"{HOME}/rt/cibench2/target/release")
WB = os.environ.get("CI_WORK", f"{HOME}/rt/cibench2-work")
HERE = os.path.dirname(os.path.abspath(__file__))
PB = int(os.environ.get("CI_PORT_BASE", "24400"))
P_VS, P_GD, P_GDC = PB + 3, PB + 20, PB + 22
GITENV = dict(os.environ, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
              GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_TERMINAL_PROMPT="0", GIT_CONFIG_COUNT="2",
              GIT_CONFIG_KEY_0="gc.auto", GIT_CONFIG_VALUE_0="0", GIT_CONFIG_KEY_1="maintenance.auto", GIT_CONFIG_VALUE_1="false",
              VCS_TOKEN="tok1", VCS_PRINCIPAL="bench", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", PATH=f"{REL}:{os.environ['PATH']}")


def sh(cmd, cwd=None, check=True):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=GITENV, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"failed ({r.returncode}): {cmd[:200]}\n{r.stdout[-600:]}{r.stderr[-1200:]}")
    return r


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return sum(int(x) for x in f[11:15]) / os.sysconf("SC_CLK_TCK")
    except Exception:
        return 0.0


def med(xs):
    return statistics.median(xs) if xs else None


def port_open(p):
    with socket.socket() as s:
        s.settimeout(0.3)
        return s.connect_ex(("127.0.0.1", p)) == 0


class Procs:
    def __init__(self):
        self.p = {}

    def start(self, name, argv, port):
        if port_open(port):
            raise RuntimeError(f"port {port} busy")
        lg = open(f"{WB}/{name}.log", "ab")
        self.p[name] = subprocess.Popen(argv, env=GITENV, stdout=lg, stderr=lg, start_new_session=True)
        for _ in range(150):
            if port_open(port):
                return
            time.sleep(0.2)
        raise RuntimeError(f"{name} not up")

    def pid(self, n):
        return self.p[n].pid if n in self.p else 0

    def stop_all(self):
        for pr in self.p.values():
            pr.terminate()
            try:
                pr.wait(20)
            except subprocess.TimeoutExpired:
                pr.kill()
        self.p = {}


P = Procs()


def wait_quiet(limit=600):
    t0 = time.time()
    while load1() >= float(os.environ.get("BENCH_MAX_LOAD1", "1.0")) and time.time() - t0 < limit:
        time.sleep(2)


def timed(cmd, cwd, srv=()):
    out = open(f"{WB}/.out", "w+")
    wait_quiet()
    c0 = {n: ticks(P.pid(n)) for n in srv}
    l1 = load1()
    t = time.perf_counter()
    pr = subprocess.Popen(cmd, shell=True, cwd=cwd, env=GITENV, stdout=out, stderr=subprocess.STDOUT)
    _, status, ru = os.wait4(pr.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    out.seek(0); text = out.read(); out.close()
    if os.waitstatus_to_exitcode(status):
        raise RuntimeError(f"failed: {cmd[:200]}\n{text[-800:]}")
    return {"wall_ms": wall, "cpu_s": ru.ru_utime + ru.ru_stime, "srv_cpu_s": sum(ticks(P.pid(n)) - c0[n] for n in srv), "load1": l1, "out": text}


def timed_par(cmds, srv=()):
    wait_quiet()
    c0 = {n: ticks(P.pid(n)) for n in srv}
    l1 = load1()
    t = time.perf_counter()
    prs = [subprocess.Popen(c, shell=True, cwd=d, env=GITENV, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE) for c, d in cmds]
    cpu, ends, bad = 0.0, [], []
    for pr in prs:
        _, status, ru = os.wait4(pr.pid, 0)
        ends.append((time.perf_counter() - t) * 1000)
        cpu += ru.ru_utime + ru.ru_stime
        if os.waitstatus_to_exitcode(status):
            bad.append(pr.stderr.read().decode()[-300:])
    if bad:
        raise RuntimeError("parallel failed: " + bad[0])
    return {"wall_ms": max(ends), "cpu_s": cpu, "srv_cpu_s": sum(ticks(P.pid(n)) - c0[n] for n in srv), "load1": l1}


def rm_many(paths):
    """delete many trees in parallel (a 100k-file tree takes seconds on EBS; serial rm of 32 of them took minutes)"""
    subprocess.run("xargs -P 16 -n 1 rm -rf", shell=True, input="\n".join(paths).encode(), check=False)


def du_kib(p):
    return int(sh(f"du -sk '{p}' | cut -f1").stdout)


def regroup(raw, dst, groups):
    leaves = sorted((os.path.relpath(d, raw), fs) for d, _, fs in os.walk(raw) if fs)
    for i, (l, fs) in enumerate(leaves):
        t = os.path.join(dst, f"g{i * groups // len(leaves):02d}", l)
        os.makedirs(t, exist_ok=True)
        for f in fs:
            shutil.move(os.path.join(raw, l, f), os.path.join(t, f))


def stats(text):
    m = re.search(r"requests:\s*(\d+).*?downloaded:\s*(\d+)\s*bytes", text)
    return (int(m.group(1)), int(m.group(2))) if m else (None, None)


class Bench:
    SLICE = "g07"

    def __init__(self, layout, n, rounds, fan, fan_rounds):
        self.layout, self.n, self.R, self.fan, self.FR = layout, n, rounds, fan, fan_rounds
        self.w = f"{WB}/{layout}-{n}"
        self.res = {}
        self.ck = None
        self.ctr = 0

    # ---------------- fixtures
    def build(self):
        w = self.w
        sh(f"rm -rf '{w}' && mkdir -p '{w}/clones' '{w}/gitrepo' '{w}/vcsroot' '{w}/cfg'")
        sh(f"datagen {self.layout} {self.n} '{w}/raw' 42")
        os.makedirs(f"{w}/ds")
        regroup(f"{w}/raw", f"{w}/ds", 50)
        shutil.rmtree(f"{w}/raw")
        self.files = sh(f"cd '{w}/ds' && find . -type f | sort | sed 's|^./||'").stdout.split("\n")[:-1]
        self.in_slice = [f for f in self.files if f.startswith(self.SLICE + "/")]
        tok = sh("vcs-server hash-token tok1").stdout.strip()
        open(f"{w}/cfg/tokens", "w").write(f"bench write * {tok}\n")
        self.bare = f"{w}/gitrepo/main.git"
        sh(f"git init -q --bare -b main '{self.bare}' && git -C '{self.bare}' config uploadpack.allowFilter true && git -C '{self.bare}' config uploadpack.allowAnySHA1InWant true")
        P.start("gitd", ["git", "daemon", "--reuseaddr", "--listen=127.0.0.1", f"--port={P_GD}", f"--base-path={w}/gitrepo", "--export-all", "--max-connections=128", f"{w}/gitrepo"], P_GD)
        P.start("gitd-count", [sys.executable, f"{HERE}/ci_tcpcount.py", str(P_GDC), "127.0.0.1", str(P_GD)], P_GDC)
        P.start("vcs", ["vcs-server", "--root", f"{w}/vcsroot", "--tokens", f"{w}/cfg/tokens", "--listen", f"127.0.0.1:{P_VS}"], P_VS)
        self.u = {"git": f"git://127.0.0.1:{P_GD}/main.git", "gitc": f"git://127.0.0.1:{P_GDC}/main.git", "file": f"file://{self.bare}", "vcs": f"http://127.0.0.1:{P_VS}"}
        g = f"{w}/dev_git"
        sh(f"cp -a '{w}/ds' '{g}'")
        sh("git init -q -b main && git add -A && git commit -q -m initial", cwd=g)
        v = f"{w}/dev_vcs"
        sh(f"cp -a '{w}/ds' '{v}'")
        sh("vcs init", cwd=v)
        for gi in range(50):
            sh(f"vcs zone create g{gi:02d} --path g{gi:02d}", cwd=v)
        sh("vcs add . && vcs commit -m initial", cwd=v)
        sh(f"vcs remote set '{self.u['vcs']}'", cwd=v)
        self.dev_git, self.dev_vcs = g, v
        for i in range(1, 6):
            self.commit(i)
        sh("vcs publish", cwd=v)
        sh(f"git push -q '{self.bare}' main", cwd=g)
        sh(f"git -C '{self.bare}' -c gc.auto=0 repack -adbq")
        self.base_git, self.base_vcs = self.heads()

    def edit(self, i, k=3):
        fs = [self.in_slice[(i * 7919 + j * 104729) % len(self.in_slice)] for j in range(k)]
        for d in (self.dev_git, self.dev_vcs):
            for f in fs:
                open(f"{d}/{f}", "a").write(f"edit {i}\n")
        return fs

    def commit(self, i, k=3):
        fs = self.edit(i, k)
        q = " ".join(f"'{f}'" for f in fs)
        sh(f"git add {q} && git commit -q -m c{i}", cwd=self.dev_git)
        sh(f"vcs add {q} && vcs commit -m c{i}", cwd=self.dev_vcs)

    def heads(self):
        return (sh("git rev-parse HEAD", cwd=self.dev_git).stdout.strip(),
                sh(f"vcs log --zone {self.SLICE} --oneline -n1", cwd=self.dev_vcs).stdout.split()[0])

    def new_commit(self, k=3):
        """untimed: a dev pushes one new commit (k files in the job slice) to both servers; returns (git sha, vcs id)."""
        self.ctr += 1
        self.commit(100 + self.ctr, k)
        sh(f"git push -q '{self.bare}' main", cwd=self.dev_git)
        sh("vcs publish", cwd=self.dev_vcs)
        return self.heads()

    # ---------------- cells
    def clone_cells(self):
        s = self.SLICE
        sp = f" && git -C {{dst}} sparse-checkout set {s}"
        G = lambda name, url, flags, post="": dict(name=name, kind="git", url=url, cmd=f"git clone -q {flags} '{{url}}' '{{dst}}'{post}", svp=["gitd"] if url == "git" else [])
        return [
            dict(name="vcs --for (slice) [vcs-server]", kind="vcs", url="vcs", cmd=f"vcs clone '{{url}}' '{{dst}}' --stats --for {s}", svp=["vcs"], maxk=64),
            dict(name="vcs full [vcs-server]", kind="vcs", url="vcs", cmd="vcs clone '{url}' '{dst}' --stats", svp=["vcs"], maxk=8),
            dict(G("git depth1 (whole tree) [git://]", "git", "--depth 1"), maxk=8),
            dict(G("git depth1 sparse [git://]", "git", "--depth 1 --sparse", sp), maxk=32),
            dict(G("git depth1 blobless sparse [git://]", "git", "--depth 1 --filter=blob:none --sparse", sp), maxk=64),
            dict(G("git blobless sparse [git://]", "git", "--filter=blob:none --sparse", sp), maxk=64),
            dict(G("git full [git://]", "git", ""), maxk=8),
            dict(G("git depth1 blobless sparse [file://]", "file", "--depth 1 --filter=blob:none --sparse", sp), maxk=64),
            dict(G("git depth1 (whole tree) [file://]", "file", "--depth 1"), maxk=0),
            dict(G("git full [file://]", "file", ""), maxk=8),
        ]

    def cmd(self, c, dst, counting=False):
        url = self.u["gitc"] if counting and c["url"] == "git" else self.u[c["url"]]
        return c["cmd"].replace("{dst}", dst).replace("{url}", url)

    def counts(self, c, dst):
        if c["url"] != "git":
            return None
        with socket.create_connection(("127.0.0.1", P_GDC + 1)) as s:
            s.sendall(b"RESET\n"); s.recv(4096)
        sh(self.cmd(c, dst, True), cwd=self.w)
        with socket.create_connection(("127.0.0.1", P_GDC + 1)) as s:
            s.sendall(b"STATS\n"); d = json.loads(s.recv(4096))
        sh(f"rm -rf '{dst}'")
        return {"requests": d["connections"], "bytes": d["bytes_down"]}

    def rotate(self, items, rep):
        k = rep % len(items)
        return items[k:] + items[:k]

    def s1(self):
        cells = self.clone_cells()
        res = {c["name"]: dict(wall_ms=[], cpu_s=[], srv_cpu_s=[], disk_kib=[], load1=[], requests=[], bytes=[]) for c in cells}
        dst = f"{self.w}/clones/s1"
        for rep in range(self.R):
            for c in self.rotate(cells, rep):
                sh(f"rm -rf '{dst}'")
                r = timed(self.cmd(c, dst), self.w, c["svp"])
                x = res[c["name"]]
                for k in ("wall_ms", "cpu_s", "srv_cpu_s", "load1"):
                    x[k].append(r[k])
                x["disk_kib"].append(du_kib(dst))
                if c["kind"] == "vcs":
                    q, b = stats(r["out"]); x["requests"].append(q); x["bytes"].append(b)
                sh(f"rm -rf '{dst}'")
                print(f"  s1 {rep+1}/{self.R} {c['name']}: {r['wall_ms']:.0f} ms", flush=True)
        for c in cells:
            res[c["name"]]["count"] = self.counts(c, dst)
        self.res["s1"] = res

    def runners(self, kinds):
        """warm runners: dict name -> (cell, dir), cloned at the base commit."""
        out = {}
        for c in self.clone_cells():
            if c["name"] in kinds:
                d = f"{self.w}/clones/w{len(out)}"
                sh(f"rm -rf '{d}'")
                sh(self.cmd(c, d), cwd=self.w)
                if c["kind"] == "git":
                    sh("git checkout -q " + self.base_git, cwd=d, check=False)
                out[c["name"]] = (c, d)
        return out

    WARM = ["vcs --for (slice) [vcs-server]", "git depth1 sparse [git://]", "git depth1 blobless sparse [git://]",
            "git blobless sparse [git://]", "git full [git://]", "git depth1 blobless sparse [file://]"]

    def fetch_cmd(self, c, ids):
        g, v = ids
        if c["kind"] == "vcs":
            return f"vcs fetch --stats && vcs switch --detach {v} --zone {self.SLICE}"
        d = "--depth 1 " if "depth1" in c["name"] else ""
        return f"git fetch -q {d}origin {g} && git checkout -q FETCH_HEAD"

    def s2_s3(self):
        run = self.runners(self.WARM)
        for name, (c, d) in run.items():
            if c["kind"] == "git":
                sh(f"git remote set-url origin '{self.u[c['url']]}'", cwd=d)
        names = list(run)
        s2 = {n: dict(wall_ms=[], cpu_s=[], srv_cpu_s=[], requests=[], bytes=[]) for n in names}
        s3 = {n: dict(diff_ms=[], diff_cpu_s=[], status_ms=[], status_cpu_s=[]) for n in names}
        for rep in range(self.R):
            ids = self.new_commit(3)
            # s2 timed fetch+checkout of that exact commit (each runner starts from the previous commit)
            for n in self.rotate(names, rep):
                c, d = run[n]
                r = timed(self.fetch_cmd(c, ids), d, c["svp"])
                x = s2[n]
                for k in ("wall_ms", "cpu_s", "srv_cpu_s"):
                    x[k].append(r[k])
                if c["kind"] == "vcs":
                    q, b = stats(r["out"]); x["requests"].append(q); x["bytes"].append(b)
                print(f"  s2 {rep+1}/{self.R} {n}: {r['wall_ms']:.0f} ms", flush=True)
            # s3: PR = next commit with 20 changed files; runner at previous commit fetches it and lists changed files vs base
            prev = ids
            pr_ids = self.new_commit(20)
            for n in self.rotate(names, rep):
                c, d = run[n]
                if c["kind"] == "vcs":
                    sh("vcs fetch", cwd=d)
                    a = f"vcs diff-tree {prev[1]} {pr_ids[1]} --zone {self.SLICE}"
                else:
                    sh(f"git fetch -q {'--depth 1 ' if 'depth1' in n else ''}origin {pr_ids[0]}", cwd=d)
                    a = f"git diff --name-only {prev[0]} {pr_ids[0]}" if "depth1" not in n else f"git diff --name-only {prev[0]} {pr_ids[0]}"
                    if "depth1" in n:  # shallow clone: base is not in history, CI would diff against the fetched head's parent by fetching base too
                        sh(f"git fetch -q --depth 1 origin {prev[0]}", cwd=d)
                r = timed(a, d, c["svp"])
                s3[n]["diff_ms"].append(r["wall_ms"]); s3[n]["diff_cpu_s"].append(r["cpu_s"])
                # check out the PR head, modify 20 tracked files, time status
                if c["kind"] == "vcs":
                    sh(f"vcs switch --detach {pr_ids[1]} --zone {self.SLICE}", cwd=d)
                else:
                    sh(f"git checkout -q {pr_ids[0]}", cwd=d)
                for f in self.in_slice[:20]:
                    open(f"{d}/{f}", "a").write("x\n")
                r = timed("vcs status" if c["kind"] == "vcs" else "git status -s", d)
                s3[n]["status_ms"].append(r["wall_ms"]); s3[n]["status_cpu_s"].append(r["cpu_s"])
                sh(("vcs restore ." if c["kind"] == "vcs" else "git checkout -q -- ."), cwd=d)
                print(f"  s3 {rep+1}/{self.R} {n}: diff {s3[n]['diff_ms'][-1]:.0f} status {r['wall_ms']:.0f} ms", flush=True)
        self.res["s2"], self.res["s3"] = s2, s3
        self.runners_state = run

    def s4(self):
        """clean -fdx + checkout of the other commit between jobs; alternates between two commits."""
        run = self.runners_state
        a_ids = self.heads()
        res = {n: dict(wall_ms=[], cpu_s=[]) for n in run}
        for rep in range(self.R):
            other = self.new_commit(3)
            for n in self.rotate(list(run), rep):
                c, d = run[n]
                if c["kind"] == "git":
                    sh(f"git fetch -q {'--depth 1 ' if 'depth1' in n else ''}origin {other[0]}", cwd=d)
                else:
                    sh("vcs fetch", cwd=d)
                # job leftovers: untracked build tree (3000 files) and 20 modified tracked files
                for i in range(30):
                    os.makedirs(f"{d}/build/b{i}", exist_ok=True)
                    for j in range(100):
                        open(f"{d}/build/b{i}/o{j}.o", "w").write("x")
                for f in self.in_slice[:20]:
                    open(f"{d}/{f}", "a").write("dirty\n")
                if c["kind"] == "git":
                    cmd = f"git clean -fdxq && git checkout -q -f {other[0]}"
                else:
                    cmd = f"vcs clean -f -d >/dev/null && vcs restore . && vcs switch --detach {other[1]} --zone {self.SLICE}"
                r = timed(cmd, d, c["svp"])
                res[n]["wall_ms"].append(r["wall_ms"]); res[n]["cpu_s"].append(r["cpu_s"])
                sh("rm -rf build", cwd=d)
                print(f"  s4 {rep+1}/{self.R} {n}: {r['wall_ms']:.0f} ms", flush=True)
        self.res["s4"] = res

    def s5(self):
        """cache-warm re-clone: the runner keeps a local cache of the repo; a job makes a fresh working copy from it."""
        w = self.w
        sh(f"rm -rf '{w}/cache_git' '{w}/cache.bundle'")
        sh(f"git clone -q --mirror '{self.u['git']}' '{w}/cache_git'")
        sh(f"vcs bundle create '{w}/cache.bundle'", cwd=self.dev_vcs, check=False)
        cells = [
            ("git clone --reference cache --dissociate (full)", f"git clone -q --reference '{w}/cache_git' --dissociate '{self.u['git']}' '{{dst}}'"),
            ("git clone cache (local path, hardlinks)", f"git clone -q '{w}/cache_git' '{{dst}}'"),
            ("git clone --reference cache (depth-free, borrowed objects)", f"git clone -q --reference '{w}/cache_git' '{self.u['git']}' '{{dst}}'"),
        ]
        cells.append(("vcs clone cache dir (full)", f"vcs clone '{w}/vcsroot' '{{dst}}'"))
        cells.append(("vcs clone cache dir --for (slice)", f"vcs clone '{w}/vcsroot' '{{dst}}' --for {self.SLICE}"))
        if os.path.exists(f"{w}/cache.bundle"):
            cells.append(("vcs clone cache bundle (full)", f"vcs clone '{w}/cache.bundle' '{{dst}}'"))
            cells.append(("vcs clone cache bundle --for (slice)", f"vcs clone '{w}/cache.bundle' '{{dst}}' --for {self.SLICE}"))
        res = {n: dict(wall_ms=[], cpu_s=[]) for n, _ in cells}
        dst = f"{w}/clones/s5"
        for rep in range(self.R):
            for n, cmd in self.rotate(cells, rep):
                sh(f"rm -rf '{dst}'")
                try:
                    r = timed(cmd.replace("{dst}", dst), w, ["gitd", "vcs"])
                except RuntimeError as e:
                    res[n]["error"] = str(e)[:300]; continue
                res[n]["wall_ms"].append(r["wall_ms"]); res[n]["cpu_s"].append(r["cpu_s"])
                print(f"  s5 {rep+1}/{self.R} {n}: {r['wall_ms']:.0f} ms", flush=True)
        sh(f"rm -rf '{dst}'")
        self.res["s5"] = {n: x for n, x in res.items() if x["wall_ms"] or x.get("error")}

    def s6(self):
        out = {}
        for K in self.fan:
            cells = [c for c in self.clone_cells() if c["maxk"] >= K]
            res = {c["name"]: dict(wall_ms=[], cpu_s=[], srv_cpu_s=[], load1=[]) for c in cells}
            for rep in range(self.FR):
                for c in self.rotate(cells, rep):
                    ds = [f"{self.w}/clones/p{i}" for i in range(K)]
                    rm_many(ds)
                    try:
                        r = timed_par([(self.cmd(c, d), self.w) for d in ds], c["svp"])
                    except RuntimeError as e:
                        res[c["name"]].setdefault("failed", []).append(str(e)[:200])
                        print(f"  s6 K={K} {c['name']}: FAILED {str(e)[:100]}", flush=True)
                        rm_many(ds)
                        continue
                    for k in ("wall_ms", "cpu_s", "srv_cpu_s", "load1"):
                        res[c["name"]][k].append(r[k])
                    rm_many(ds)
                    print(f"  s6 K={K} {rep+1}/{self.FR} {c['name']}: slowest {r['wall_ms']:.0f} ms", flush=True)
            out[str(K)] = res
            self.res["s6"] = out
            if self.ck:
                self.ck()


def run(argv):
    layout, n, rounds, out = argv[0], int(argv[1]), int(argv[2]), argv[3]
    opt = lambda k, d: argv[argv.index(k) + 1] if k in argv else d
    fan = [int(x) for x in opt("--fan", "8,32,64").split(",") if x]
    os.makedirs(WB, exist_ok=True)
    only = opt("--only", "s1,s2,s4,s5,s6").split(",")
    b = Bench(layout, n, rounds, fan, int(opt("--fan-rounds", 3)))
    meta = dict(layout=layout, n_requested=n, rounds=rounds, fan=fan, fan_rounds=b.FR, date=time.strftime("%FT%TZ", time.gmtime()),
                kernel=platform.release(), cores=os.cpu_count(), load_start=open("/proc/loadavg").read().split()[:3],
                git=sh("git --version").stdout.strip(), vcs=sh("vcs --version").stdout.strip(), build_sha=os.environ.get("BUILD_SHA"), quiet_gate=float(os.environ.get("BENCH_MAX_LOAD1", "1.0")),
                cpu=[l.split(":")[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")][0], dropped_caches=False)
    try:
        b.build()
        meta.update(files=len(b.files), slice_files=len(b.in_slice), bytes=sum(os.path.getsize(f"{b.w}/ds/{f}") for f in b.files))
        print("fixtures ready", flush=True)
        meta["server_kib"] = dict(git_bare=du_kib(b.bare), vcs_root=du_kib(f"{b.w}/vcsroot"))
        def ck():
            meta["load_end"] = open("/proc/loadavg").read().split()[:3]
            json.dump(dict(meta=meta, **b.res), open(out, "w"), indent=1)
        b.ck = ck
        if "s1" in only: b.s1(); ck()
        if "s2" in only or "s4" in only: b.s2_s3(); ck()
        if "s4" in only: b.s4(); ck()
        if "s5" in only: b.s5(); ck()
        if "s6" in only: b.s6(); ck()
        print("wrote", out)
    finally:
        P.stop_all()
        shutil.rmtree(b.w, ignore_errors=True)


# ---------------- report
def verdict(v, g):
    """v, g = lists of samples (lower is better). Owner rule."""
    if not v or not g:
        return "n/a"
    mv, mg = med(v), med(g)
    overlap = not (max(v) < min(g) or max(g) < min(v))
    if overlap or abs(mv - mg) < 2 or max(mv, mg) <= 1.10 * min(mv, mg):
        return "TIE"
    return "WIN" if mv < mg else "LOSS"


def fmt(xs, d=0):
    return "n/a" if not xs else f"{med(xs):.{d}f} ({min(xs):.{d}f}-{max(xs):.{d}f})"


def table(sec, metric, title, ref_prefix="vcs", unit="ms", d=0):
    print(f"\n{title}\n\n| cell | {metric} median (min-max) | server CPU s |\n|---|---|---|")
    for k, x in sec.items():
        print(f"| {k} | {fmt(x.get('wall_ms') or x.get(metric), d)} | {fmt(x.get('srv_cpu_s'), 2)} |")


def verdicts(sec, key, label):
    vk = [k for k in sec if k.startswith("vcs")]
    gk = [k for k in sec if k.startswith("git") or k.startswith("git ")]
    print(f"\n**Verdicts, {label}** (vcs row vs git column; WIN/TIE/LOSS by the owner rule)\n")
    print("| vcs cell | " + " | ".join(gk) + " |\n|---|" + "---|" * len(gk))
    for a in vk:
        print(f"| {a} | " + " | ".join(verdict(sec[a].get(key), sec[b].get(key)) for b in gk) + " |")


def report(files):
    for f in files:
        d = json.load(open(f)); m = d["meta"]
        print(f"\n## {m['layout']} {m['files']} files ({m['bytes']/1048576:.0f} MiB), slice {m['slice_files']} files\n")
        print(f"box cores {m['cores']} ({m['cpu']}), load1 start/5/15 {'/'.join(m['load_start'])}, end {'/'.join(m.get('load_end', []))}; {m['git']}; {m['vcs']}; rounds {m['rounds']}, fan-out rounds {m['fan_rounds']}; {m['date']}")
        if "s1" in d:
            print("\n**S1 cold clone of the slice** (wall ms; client CPU s; server CPU s; runner MiB; requests; MiB down)\n")
            print("| cell | wall ms | client CPU s | server CPU s | runner MiB | requests | MiB down |\n|---|---|---|---|---|---|---|")
            for k, x in d["s1"].items():
                if x["requests"] and x["requests"][0] is not None:
                    rq, by = f"{med(x['requests']):.0f}", f"{med(x['bytes'])/1048576:.1f}"
                elif x.get("count"):
                    rq, by = f"{x['count']['requests']} conn", f"{x['count']['bytes']/1048576:.1f}"
                else:
                    rq, by = "n/a", "n/a"
                print(f"| {k} | {fmt(x['wall_ms'])} | {fmt(x['cpu_s'],2)} | {fmt(x['srv_cpu_s'],2)} | {med(x['disk_kib'])/1024:.1f} | {rq} | {by} |")
            verdicts(d["s1"], "wall_ms", "S1 wall"); verdicts(d["s1"], "cpu_s", "S1 client CPU")
        for sec, title, keys in (("s2", "S2 warm: fetch specific commit + checkout", [("wall_ms", "wall ms"), ("cpu_s", "client CPU s")]),
                                 ("s4", "S4 clean -fdx + checkout", [("wall_ms", "wall ms"), ("cpu_s", "client CPU s")]),
                                 ("s5", "S5 cache-warm re-clone", [("wall_ms", "wall ms"), ("cpu_s", "client CPU s")])):
            if sec not in d: continue
            print(f"\n**{title}**\n\n| cell | " + " | ".join(l for _, l in keys) + " |\n|---|" + "---|" * len(keys))
            for k, x in d[sec].items():
                print(f"| {k} | " + " | ".join(fmt(x.get(a), 2 if "cpu" in a else 0) if x.get(a) else x.get("error", "n/a")[:60] for a, _ in keys) + " |")
            verdicts(d[sec], "wall_ms", title + " wall")
        if "s3" in d:
            print("\n**S3 PR: changed files vs base, and status with 20 modified files** (ms)\n\n| cell | diff --name-only | status |\n|---|---|---|")
            for k, x in d["s3"].items():
                print(f"| {k} | {fmt(x['diff_ms'])} | {fmt(x['status_ms'])} |")
            verdicts(d["s3"], "diff_ms", "S3 changed-files wall"); verdicts(d["s3"], "status_ms", "S3 status wall")
        for K, sec in d.get("s6", {}).items():
            print(f"\n**S6 fan-out: {K} simultaneous cold clones, one server** (slowest ms; summed client CPU s; server CPU s)\n\n| cell | slowest ms | client CPU s | server CPU s |\n|---|---|---|---|")
            for k, x in sec.items():
                nf = f" ({len(x['failed'])} of {len(x['failed']) + len(x['wall_ms'])} rounds FAILED: {x['failed'][0][:70]})" if x.get("failed") else ""
                print(f"| {k} | {fmt(x['wall_ms'])}{nf} | {fmt(x['cpu_s'],1)} | {fmt(x['srv_cpu_s'],1)} |")
            verdicts(sec, "wall_ms", f"S6 K={K} slowest")
        print(f"\nserver stores KiB: {json.dumps(m.get('server_kib'))}")


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in ("run", "report"):
        sys.exit(__doc__)
    run(sys.argv[2:]) if sys.argv[1] == "run" else report(sys.argv[2:])
