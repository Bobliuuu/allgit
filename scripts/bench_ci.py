#!/usr/bin/env python3
"""CI-style benchmark: cold runner, warm runner, repeated CI cycles, parallel runners.  vcs vs git vs walgit.

usage (on a Linux box, after `cargo build --release -p vcs-cli -p vcs-server -p vcs-bench`):
  AWS_ACCESS_KEY_ID=.. AWS_SECRET_ACCESS_KEY=.. [AWS_SESSION_TOKEN=..] ALLGIT_BENCH_BUCKET=<private bucket> \
  scripts/bench_ci.py run <flat|deep|zones70> <n_files> <runs> <out.json> [--cycles 20] [--par 8] [--no-walgit] [--no-s3]
  scripts/bench_ci.py report <out.json>...        # markdown tables on stdout

Servers (all on the same box, loopback; the box is the "runner" and the "server" at once):
  git      bare repo + `git daemon` (git://), repacked once with bitmaps. git never touches S3.
  walgit   walgit-server (stock git over HTTP) with its object store in real S3 (skipped if the binary is missing).
  vcs@s3   vcs-server --backend s3://BUCKET/PREFIX (real S3, same region as the box).
  vcs@disk vcs-server --root <dir> (local disk, no object store: isolates S3 latency from the rest).
Object-store credentials are read from the environment, handed only to the vcs-server / walgit processes, never
written to files or printed. All S3 keys live under one run prefix that is deleted at the end.

Every timed command is run with `wait4`, so client CPU is the rusage of the whole child tree. Server CPU is the
utime+stime(+reaped children) delta of the server process. Order is interleaved (rotated every rep). Page caches are
warm (no sudo: caches are not dropped). Bytes/requests: vcs from `--stats` (client to vcs-server); git over
git:// and walgit over HTTP from a counting forwarder in a separate single-run pass.
"""
import json, os, platform, re, shutil, socket, statistics, subprocess, sys, time, urllib.request

HOME = os.path.expanduser("~")
REL = os.environ.get("CI_REL", f"{HOME}/rt/cibench/target/release")
BIN = os.environ.get("CI_BIN", f"{HOME}/rt/cibench-bin")
WB = os.environ.get("CI_WORK", f"{HOME}/rt/cibench-work")
HERE = os.path.dirname(os.path.abspath(__file__))
TCPCOUNT = f"{HERE}/ci_tcpcount.py"
HTTPCOUNT = f"{HERE}/walgit_bench/s3count_proxy.py"
WALGIT = f"{BIN}/walgit-server"
S3EP = "https://s3.us-east-1.amazonaws.com"
PB = int(os.environ.get("CI_PORT_BASE", "23400"))  # the shared box has many agents: pick a free range
P_WG, P_VS3, P_VDISK, P_GD = PB + 1, PB + 2, PB + 3, PB + 20
P_GDC, P_WGC = PB + 22, PB + 11  # counting forwarders in front of git daemon / walgit

SECRETS = [k for k in os.environ if k.startswith("AWS_")]
CLEAN = {k: v for k, v in os.environ.items() if not k.startswith("AWS_") and k != "ALLGIT_BENCH_BUCKET"}
GITENV = dict(CLEAN, GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b",
              GIT_COMMITTER_EMAIL="b@x.invalid", GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
              GIT_TERMINAL_PROMPT="0", GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_0="gc.auto", GIT_CONFIG_VALUE_0="0",
              GIT_CONFIG_KEY_1="maintenance.auto", GIT_CONFIG_VALUE_1="false", VCS_TOKEN="tok1", VCS_PRINCIPAL="bench",
              VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", PATH=f"{REL}:{CLEAN['PATH']}")


def sh(cmd, cwd=None, check=True, env=None):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=env or GITENV, capture_output=True, text=True)
    if check and r.returncode != 0:
        err = (r.stdout[-600:] + r.stderr[-1200:])
        for s in SECRETS:
            if os.environ.get(s):
                err = err.replace(os.environ[s], "<redacted>")
        raise RuntimeError(f"failed ({r.returncode}): {cmd[:200]}\n{err}")
    return r


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return sum(int(x) for x in f[11:15]) / os.sysconf("SC_CLK_TCK")
    except Exception:
        return 0.0


def ru_cpu(ru):
    return ru.ru_utime + ru.ru_stime


def med(xs):
    return statistics.median(xs) if xs else None


def p95(xs):
    if not xs:
        return None
    s = sorted(xs)
    return s[min(len(s) - 1, int(round(0.95 * (len(s) - 1))))]


def port_open(p):
    with socket.socket() as s:
        s.settimeout(0.3)
        return s.connect_ex(("127.0.0.1", p)) == 0


def wait_port(p, tries=150):
    for _ in range(tries):
        if port_open(p):
            return
        time.sleep(0.2)
    raise RuntimeError(f"port {p} not up")


class Procs:
    def __init__(self):
        self.p = {}

    def start(self, name, argv, port, env=None, log=None):
        if port_open(port):
            raise RuntimeError(f"port {port} busy; refusing to start {name}")
        lg = open(log or f"{WB}/{name}.log", "ab")
        self.p[name] = subprocess.Popen(argv, env=env or GITENV, stdout=lg, stderr=lg, start_new_session=True)
        wait_port(port)

    def stop(self, name):
        pr = self.p.pop(name, None)
        if pr:
            pr.terminate()
            try:
                pr.wait(20)
            except subprocess.TimeoutExpired:
                pr.kill(); pr.wait()

    def stop_all(self):
        for n in list(self.p):
            self.stop(n)

    def pid(self, name):
        return self.p[name].pid if name in self.p else 0


P = Procs()


def timed(cmd, cwd, srv=()):
    """Run cmd once. Returns wall_ms, client tree cpu, per-server cpu delta, stdout."""
    out = open(f"{WB}/.out", "w+")
    c0 = {n: ticks(P.pid(n)) for n in srv}
    l1 = load1()
    t = time.perf_counter()
    pr = subprocess.Popen(cmd, shell=True, cwd=cwd, env=GITENV, stdout=out, stderr=subprocess.STDOUT)
    _, status, ru = os.wait4(pr.pid, 0)
    wall = (time.perf_counter() - t) * 1000
    pr.returncode = os.waitstatus_to_exitcode(status)
    out.seek(0); text = out.read(); out.close()
    if pr.returncode != 0:
        raise RuntimeError(f"failed ({pr.returncode}): {cmd[:200]}\n{text[-800:]}")
    return {"wall_ms": wall, "cpu_s": ru_cpu(ru), "srv_cpu_s": sum(ticks(P.pid(n)) - c0[n] for n in srv), "load1": l1, "out": text}


def timed_par(cmds, srv=()):
    """Start all cmds at once (list of (cmd, cwd)); wall of the slowest, summed client cpu."""
    c0 = {n: ticks(P.pid(n)) for n in srv}
    l1 = load1()
    t = time.perf_counter()
    prs = [subprocess.Popen(c, shell=True, cwd=d, env=GITENV, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE) for c, d in cmds]
    cpu, ends, bad = 0.0, [], []
    for pr in prs:
        _, status, ru = os.wait4(pr.pid, 0)
        ends.append((time.perf_counter() - t) * 1000)
        cpu += ru_cpu(ru)
        if os.waitstatus_to_exitcode(status) != 0:
            bad.append(pr.stderr.read().decode()[-300:])
    if bad:
        raise RuntimeError("parallel clone failed: " + bad[0])
    return {"wall_ms": max(ends), "first_ms": min(ends), "cpu_s": cpu, "srv_cpu_s": sum(ticks(P.pid(n)) - c0[n] for n in srv), "load1": l1}


def du_kib(path):
    return int(sh(f"du -sk '{path}' | cut -f1").stdout)


def count_files(path):
    return int(sh(f"find '{path}' -type f -not -path '*/.git/*' -not -path '*/.vcs/*' | wc -l").stdout)


# ---------------------------------------------------------------- datasets
def regroup(raw, dst, groups):
    leaves = []
    for d, _, fs in os.walk(raw):
        if fs:
            leaves.append((os.path.relpath(d, raw), fs))
    leaves.sort()
    for i, (l, fs) in enumerate(leaves):
        g = i * groups // len(leaves)
        t = os.path.join(dst, f"g{g:02d}", l)
        os.makedirs(t, exist_ok=True)
        for f in fs:
            shutil.move(os.path.join(raw, l, f), os.path.join(t, f))


def gen_zones70(root, n):
    """Port of the generator in scripts/bench_zones_autodetect.sh: 50 crates, 20 npm packages, docs."""
    import random
    random.seed(42)
    os.makedirs(root)
    words = ["alpha", "beta", "gamma", "delta", "omega", "sigma", "kappa", "theta", "lambda", "zeta", "ledger", "cursor",
             "packet", "socket", "buffer", "vector", "matrix", "kernel"]

    def w(p, c):
        p = os.path.join(root, p); os.makedirs(os.path.dirname(p), exist_ok=True)
        open(p, "w").write(c)

    def body():
        return "\n".join(" ".join(random.choice(words) + str(random.randrange(10**6)) for _ in range(8)) for _ in range(30)) + "\n"

    npk, nweb, ndocs = 50, 20, n // 20
    per = (n - ndocs) // (npk + nweb)
    w("Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n')
    for i in range(npk):
        name = f"p{i:02d}"
        deps = (f'p{i-1:02d} = {{ path = "../p{i-1:02d}" }}\n' if i % 4 else "")
        w(f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n[dependencies]\n{deps}')
        for j in range(per - 1):
            w(f"crates/{name}/src/m{j//100}/f{j}.rs", body())
    for i in range(nweb):
        name = f"w{i:02d}"
        w(f"web/{name}/package.json", '{"name":"%s","version":"1.0.0"}\n' % name)
        for j in range(per - 1):
            w(f"web/{name}/src/c{j//100}/f{j}.js", body())
    for j in range(ndocs):
        w(f"docs/d{j//100}/p{j}.md", body())
    w("CODEOWNERS", "/web/w03/ @alice\n/crates/p10/ @bob\n")
    w("README", "monorepo\n")


def list_files(root):
    out = []
    for d, _, fs in os.walk(root):
        if os.sep + ".git" in d or os.sep + ".vcs" in d or d.endswith(".git") or d.endswith(".vcs"):
            continue
        for f in fs:
            out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out)


# ---------------------------------------------------------------- the benchmark
class Bench:
    def __init__(self, layout, n, runs, cycles, par, use_walgit, use_s3):
        self.layout, self.n, self.runs, self.cycles, self.par = layout, n, runs, cycles, par
        self.use_walgit, self.use_s3 = use_walgit, use_s3
        self.bucket = os.environ.get("ALLGIT_BENCH_BUCKET", "")
        self.runid = f"cibench/{time.strftime('%Y%m%d-%H%M%S')}-{layout}-{n}"
        self.w = f"{WB}/{layout}-{n}"
        self.res = {}
        self.ctr = 0

    # --- fixtures
    def build(self):
        w = self.w
        sh(f"rm -rf '{w}' && mkdir -p '{w}/clones'")
        ds = f"{w}/ds"
        if self.layout in ("flat", "deep"):
            sh(f"datagen {self.layout} {self.n} '{w}/raw' 42")
            os.makedirs(ds)
            regroup(f"{w}/raw", ds, 50)
            shutil.rmtree(f"{w}/raw")
            self.sel_for, self.git_dirs, self.task = "g07", ["g07"], None
        else:
            gen_zones70(ds, self.n)
            self.sel_for, self.git_dirs = "crates/p10/src/m0/f1.rs", ["crates/p08", "crates/p09", "crates/p10"]
            self.task = "fix a bug in p10"
        files = list_files(ds)
        self.files = files
        self.nbytes = sum(os.path.getsize(f"{ds}/{f}") for f in files)
        pref = tuple(d + "/" for d in self.git_dirs)
        self.in_slice = [f for f in files if f.startswith(pref)]
        self.out_slice = [f for f in files if not f.startswith(pref)]
        self.ds = ds

    def edit_files(self, i):
        return [self.in_slice[(i * 7919 + 42) % len(self.in_slice)], self.out_slice[(i * 104729 + 7) % len(self.out_slice)]]

    def commit_edits(self, repo, i, kind):
        fs = self.edit_files(i)
        for f in fs:
            with open(f"{repo}/{f}", "a") as fh:
                fh.write(f"edit {i}\n")
        q = " ".join(f"'{f}'" for f in fs)
        if kind == "git":
            sh(f"git add {q} && git commit -q -m c{i}", cwd=repo)
        else:
            sh(f"vcs add {q} && vcs commit -m c{i}", cwd=repo)

    def start_servers(self):
        w = self.w
        sh(f"mkdir -p {WB}/cfg")
        tok = sh("vcs-server hash-token tok1").stdout.strip()
        open(f"{WB}/cfg/tokens", "w").write(f"bench write * {tok}\n")
        self.bare = f"{w}/gitrepo/main.git"
        sh(f"mkdir -p {w}/gitrepo && git init -q --bare -b main '{self.bare}' && git -C '{self.bare}' config uploadpack.allowFilter true "
           f"&& git -C '{self.bare}' config uploadpack.allowAnySHA1InWant true")
        P.start("gitd", ["git", "daemon", "--reuseaddr", "--listen=127.0.0.1", f"--port={P_GD}", f"--base-path={w}/gitrepo", "--export-all",
                         "--max-connections=64", f"{w}/gitrepo"], P_GD)
        P.start("gitd-count", [sys.executable, TCPCOUNT, str(P_GDC), "127.0.0.1", str(P_GD)], P_GDC)
        self.urls = {"git": f"git://127.0.0.1:{P_GD}/main.git", "git-count": f"git://127.0.0.1:{P_GDC}/main.git"}
        senv = dict(os.environ, PATH=GITENV["PATH"])  # the only processes that see the S3 credentials
        self.senv = senv
        if self.use_s3:
            P.start("vcs-s3", ["vcs-server", "--backend", f"s3://{self.bucket}/{self.runid}/vcs?endpoint={S3EP}", "--tokens", f"{WB}/cfg/tokens",
                               "--listen", f"127.0.0.1:{P_VS3}"], P_VS3, env=senv)
            self.urls["vcs@s3"] = f"http://127.0.0.1:{P_VS3}"
        os.makedirs(f"{w}/vcsroot", exist_ok=True)
        P.start("vcs-disk", ["vcs-server", "--root", f"{w}/vcsroot", "--tokens", f"{WB}/cfg/tokens", "--listen", f"127.0.0.1:{P_VDISK}"],
                P_VDISK, env=GITENV)
        self.urls["vcs@disk"] = f"http://127.0.0.1:{P_VDISK}"
        if self.use_walgit:
            cfg = f"""[server]
listen = "127.0.0.1:{P_WG}"
auto_create_on_push = true
roles = []
[server.tls]
mode = "off"
[server.auth]
mode = "none"
[store]
backend = "s3"
bucket = "{self.bucket}"
prefix = "{self.runid}/walgit/"
[store.s3]
endpoint = "{S3EP}"
region = "us-east-1"
force_path_style = true
[cache]
dir = "{w}/walgit-cache"
"""
            open(f"{WB}/cfg/walgit.toml", "w").write(cfg)
            P.start("walgit", [WALGIT, "--config", f"{WB}/cfg/walgit.toml"], P_WG, env=senv)
            P.start("walgit-count", [sys.executable, HTTPCOUNT, str(P_WGC), "127.0.0.1", str(P_WG)], P_WGC)
            self.urls["walgit"] = f"http://127.0.0.1:{P_WG}/o/main.git"
            self.urls["walgit-count"] = f"http://127.0.0.1:{P_WGC}/o/main.git"

    def publish_sources(self):
        w = self.w
        ds = self.ds
        # git source (also the walgit source)
        g = f"{w}/dev_git"
        sh(f"cp -a '{ds}' '{g}'")
        sh("git init -q -b main && git add -A && git commit -q -m initial", cwd=g)
        for i in range(1, 51):
            self.commit_edits(g, i, "git")
        self.ncommits = 51
        sh(f"git push -q '{self.bare}' main", cwd=g)
        sh(f"git -C '{self.bare}' -c gc.auto=0 repack -adbq")
        if self.use_walgit:
            sh(f"git remote add walgit_o '{self.urls['walgit']}'", cwd=g)
            sh("git push -q walgit_o main", cwd=g)
            time.sleep(2)
        # vcs source, one workspace per vcs server
        v = f"{w}/dev_vcs0"
        sh(f"cp -a '{ds}' '{v}'")
        sh("vcs init", cwd=v)
        if self.layout in ("flat", "deep"):
            for gi in range(50):
                sh(f"vcs zone create g{gi:02d} --path g{gi:02d}", cwd=v)
            sh("vcs add . && vcs commit -m initial", cwd=v)
            self.nzones = 50
        else:
            sh("vcs add . && vcs commit -m initial", cwd=v)
            sug = json.loads(sh("vcs --json zone suggest", cwd=v).stdout)["zones"]
            for z in sug:
                sh(f"vcs zone apply '{z['path']}' --yes", cwd=v)
            self.nzones = len(sug)
        for i in range(1, 51):
            self.commit_edits(v, i, "vcs")
        self.dev_vcs = {}
        for name in [k for k in ("vcs@s3", "vcs@disk") if k in self.urls]:
            d = f"{w}/dev_{name.replace('@', '_')}"
            sh(f"cp -a '{v}' '{d}'")
            sh(f"vcs remote set '{self.urls[name]}' && vcs publish", cwd=d)
            self.dev_vcs[name] = d
        shutil.rmtree(v)
        self.dev_git = g

    # --- cells
    def cells(self):
        d = " ".join(self.git_dirs)
        sp = f"git -C {{dst}} sparse-checkout set {d}"
        C = []

        def vcs(name, srv, extra, label):
            if srv in self.urls:
                C.append(dict(name=name, srv=srv, kind="vcs", clone=f"vcs clone '{self.urls[srv]}' '{{dst}}' --stats {extra}",
                              svp=["vcs-s3" if srv == "vcs@s3" else "vcs-disk"]))

        for srv in ("vcs@s3", "vcs@disk"):
            vcs(f"vcs --for", srv, f"--for '{self.sel_for}'", "")
            if self.task:
                vcs(f"vcs --task", srv, f"--task '{self.task}'", "")
            vcs("vcs full", srv, "", "")
        for k in C:
            k["name"] = f"{k['name']} [{k['srv']}]"

        def gitc(name, srv, flags, fetch):
            if srv in self.urls:
                C.append(dict(name=f"{name} [{srv}]", srv=srv, kind="git", svp=["gitd"] if srv == "git" else ["walgit"],
                              clone=f"git clone -q {flags} '{{url}}' '{{dst}}'" + (f" && {sp}" if "--sparse" in flags else ""), fetch=fetch))

        for srv in ("git", "walgit"):
            gitc("git partial (blob:none+sparse)", srv, "--filter=blob:none --sparse", "git fetch -q origin main && git checkout -q FETCH_HEAD")
            gitc("git full", srv, "", "git fetch -q origin main && git checkout -q FETCH_HEAD")
        gitc("git shallow (depth1+sparse)", "git", "--depth 1 --sparse", "git fetch -q --depth 1 origin main && git checkout -q FETCH_HEAD")
        gitc("git shallow+blobless", "git", "--depth 1 --filter=blob:none --sparse", "git fetch -q --depth 1 origin main && git checkout -q FETCH_HEAD")
        for c in C:
            c["url"] = self.urls[c["srv"]]
            c["cnt_url"] = self.urls.get(c["srv"] + "-count")
        return C

    def cmd_clone(self, c, dst, counting=False):
        url = c["cnt_url"] if counting else c["url"]
        return c["clone"].replace("{dst}", dst).replace("{url}", url)

    @staticmethod
    def parse_stats(text):
        m = re.search(r"requests:\s*(\d+).*?downloaded:\s*(\d+)\s*bytes", text)
        return (int(m.group(1)), int(m.group(2))) if m else (None, None)

    # --- scenario 1 / 4
    def s1(self):
        cells, R = self.cells(), self.runs
        res = {c["name"]: {"wall_ms": [], "cpu_s": [], "srv_cpu_s": [], "disk_kib": [], "load1": [], "requests": [], "bytes": []} for c in cells}
        for rep in range(R):
            order = cells[rep % len(cells):] + cells[:rep % len(cells)]
            for c in order:
                dst = f"{self.w}/clones/s1"
                sh(f"rm -rf '{dst}'")
                r = timed(self.cmd_clone(c, dst), self.w, c["svp"])
                x = res[c["name"]]
                for k in ("wall_ms", "cpu_s", "srv_cpu_s", "load1"):
                    x[k].append(r[k])
                x["disk_kib"].append(du_kib(dst))
                if c["kind"] == "vcs":
                    q, b = self.parse_stats(r["out"])
                    x["requests"].append(q); x["bytes"].append(b)
                if rep == 0:
                    x["files_on_disk"] = count_files(dst)
                sh(f"rm -rf '{dst}'")
                print(f"  s1 {rep+1}/{R} {c['name']}: {r['wall_ms']:.0f} ms", flush=True)
        # counting pass for git-transport cells
        for c in cells:
            if c["kind"] == "git" and c.get("cnt_url"):
                self.count_reset(c)
                dst = f"{self.w}/clones/s1c"
                sh(f"rm -rf '{dst}'")
                sh(self.cmd_clone(c, dst, True), cwd=self.w)
                res[c["name"]]["count"] = self.count_get(c)
                sh(f"rm -rf '{dst}'")
        self.res["s1"] = res

    def count_reset(self, c):
        if c["srv"] == "git":
            with socket.create_connection(("127.0.0.1", P_GDC + 1)) as s:
                s.sendall(b"RESET\n"); s.recv(4096)
        else:
            urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{P_WGC}/__proxy/reset", method="POST", data=b"")).read()

    def count_get(self, c):
        if c["srv"] == "git":
            with socket.create_connection(("127.0.0.1", P_GDC + 1)) as s:
                s.sendall(b"STATS\n"); d = json.loads(s.recv(4096))
            return {"requests": d["connections"], "requests_are": "tcp connections", "bytes": d["bytes_down"], "bytes_up": d["bytes_up"]}
        d = json.load(urllib.request.urlopen(f"http://127.0.0.1:{P_WGC}/__proxy/stats"))
        return {"requests": d["requests"], "requests_are": "http requests", "bytes": d["bytes_down"], "bytes_up": d["bytes_up"]}

    def s4(self):
        cells = [c for c in self.cells() if c["name"].split(" [")[0] in
                 ("vcs --for", "git partial (blob:none+sparse)", "git shallow+blobless", "git full") or c["name"] == "vcs full [vcs@disk]"]
        K, R = self.par, max(3, self.runs if self.n <= 20000 else 3)
        res = {c["name"]: {"wall_ms": [], "first_ms": [], "cpu_s": [], "srv_cpu_s": [], "load1": []} for c in cells}
        for rep in range(R):
            order = cells[rep % len(cells):] + cells[:rep % len(cells)]
            for c in order:
                dsts = [f"{self.w}/clones/p{i}" for i in range(K)]
                for d in dsts:
                    sh(f"rm -rf '{d}'")
                r = timed_par([(self.cmd_clone(c, d), self.w) for d in dsts], c["svp"])
                for k in res[c["name"]]:
                    res[c["name"]][k].append(r[k])
                for d in dsts:
                    sh(f"rm -rf '{d}'")
                print(f"  s4 {rep+1}/{R} {c['name']}: slowest {r['wall_ms']:.0f} ms", flush=True)
        self.res["s4"] = res

    # --- scenario 2 / 3
    def make_runners(self):
        self.runners = {}
        for c in self.cells():
            if "full" in c["name"].split(" [")[0] and c["kind"] == "vcs":
                continue  # a warm vcs full clone is not a CI shape; the zone clone is
            if c["name"].startswith("vcs --task"):
                continue
            dst = f"{self.w}/clones/warm_{len(self.runners)}"
            sh(f"rm -rf '{dst}'")
            sh(self.cmd_clone(c, dst), cwd=self.w)
            self.runners[c["name"]] = (c, dst)

    def dev_cycle(self):
        self.ctr += 1
        i = 5000 + self.ctr
        self.commit_edits(self.dev_git, i, "git")
        sh(f"git push -q '{self.bare}' main", cwd=self.dev_git)
        if self.use_walgit:
            sh("git push -q walgit_o main", cwd=self.dev_git)
            time.sleep(1)
        for name, d in self.dev_vcs.items():
            self.commit_edits(d, i, "vcs")
            sh("vcs publish", cwd=d)

    def warm_cmds(self, c, counting=False):
        if c["kind"] == "vcs":
            return ("vcs sync", "vcs status")
        f = c["fetch"]
        if counting:
            f = f.replace(" origin ", f" '{c['cnt_url']}' ")
        return (f, "git status -s")

    def cycles_run(self, n, label):
        res = {k: {"fetch_ms": [], "cpu_s": [], "srv_cpu_s": [], "status_ms": [], "load1": []} for k in self.runners}
        names = list(self.runners)
        for cyc in range(n):
            self.dev_cycle()
            order = names[cyc % len(names):] + names[:cyc % len(names)]
            for k in order:
                c, dst = self.runners[k]
                a, b = self.warm_cmds(c)
                r = timed(a, dst, c["svp"])
                s = timed(b, dst)
                x = res[k]
                x["fetch_ms"].append(r["wall_ms"]); x["cpu_s"].append(r["cpu_s"]); x["srv_cpu_s"].append(r["srv_cpu_s"])
                x["status_ms"].append(s["wall_ms"]); x["load1"].append(r["load1"])
            print(f"  {label} cycle {cyc+1}/{n}", flush=True)
        self.res[label] = res

    def counting_cycle(self):
        self.dev_cycle()
        cnt = {}
        for k, (c, dst) in self.runners.items():
            if c["kind"] == "vcs":
                r = timed("vcs fetch --stats", dst, c["svp"])
                q, b = self.parse_stats(r["out"])
                cnt[k] = {"requests": q, "requests_are": "vcs-server requests", "bytes": b}
                sh("vcs sync", cwd=dst)
            else:
                self.count_reset(c)
                a, _ = self.warm_cmds(c, True)
                sh(a, cwd=dst)
                cnt[k] = self.count_get(c)
        self.res["warm_counts"] = cnt

    def sizes(self):
        z = {"git_bare_kib": du_kib(self.bare), "vcs_disk_root_kib": du_kib(f"{self.w}/vcsroot")}
        if self.use_s3 and self.bucket:
            for tag, pre in (("vcs_s3", "vcs"), ("walgit_s3", "walgit")):
                r = sh(f"aws s3 ls 's3://{self.bucket}/{self.runid}/{pre}/' --recursive --summarize | tail -2", env=dict(os.environ), check=False).stdout
                m = re.search(r"Total Objects:\s*(\d+)\s*Total Size:\s*(\d+)", r.replace("\n", " "))
                if m:
                    z[tag + "_objects"], z[tag + "_kib"] = int(m.group(1)), int(m.group(2)) // 1024
        self.res["server_sizes"] = z

    def cleanup_s3(self):
        if self.use_s3 and self.bucket:
            sh(f"aws s3 rm 's3://{self.bucket}/{self.runid}/' --recursive --quiet", env=dict(os.environ), check=False)


def run(argv):
    layout, n, runs, out = argv[0], int(argv[1]), int(argv[2]), argv[3]
    opt = lambda k, d: int(argv[argv.index(k) + 1]) if k in argv else d
    cycles, par = opt("--cycles", 20), opt("--par", 8)
    use_s3 = "--no-s3" not in argv and bool(os.environ.get("AWS_ACCESS_KEY_ID") and os.environ.get("ALLGIT_BENCH_BUCKET"))
    use_walgit = "--no-walgit" not in argv and use_s3 and os.path.exists(WALGIT)
    os.makedirs(WB, exist_ok=True)
    b = Bench(layout, n, runs, cycles, par, use_walgit, use_s3)
    meta = {"layout": layout, "n_requested": n, "runs": runs, "cycles": cycles, "parallel": par, "date": time.strftime("%FT%TZ", time.gmtime()),
            "kernel": platform.release(), "cores": os.cpu_count(), "load_start": open("/proc/loadavg").read().split()[:3],
            "git": sh("git --version").stdout.strip(), "vcs": sh("vcs --version", check=False).stdout.strip(),
            "walgit": use_walgit, "s3": use_s3, "cpu": [l.split(":")[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")][0],
            "s3_endpoint": S3EP if use_s3 else None, "dropped_caches": False}
    out_d = {"meta": meta}
    try:
        b.build()
        meta.update(files=len(b.files), bytes=b.nbytes, slice_files=len(b.in_slice))
        b.start_servers()
        b.publish_sources()
        meta.update(commits=b.ncommits, zones=b.nzones, sel_for=b.sel_for, git_dirs=b.git_dirs, task=b.task)
        print("fixtures ready", flush=True)
        b.sizes()
        b.s1()
        b.make_runners()
        b.cycles_run(max(3, min(runs, 5)), "s2")
        b.cycles_run(cycles, "s3")
        b.counting_cycle()
        b.s4()
        meta["load_end"] = open("/proc/loadavg").read().split()[:3]
        out_d.update(b.res)
        json.dump(out_d, open(out, "w"), indent=1)
        print("wrote", out)
    finally:
        P.stop_all()
        b.cleanup_s3()
        shutil.rmtree(b.w, ignore_errors=True)


# ---------------------------------------------------------------- report
def fmt(xs, d=0):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "n/a"
    return f"{med(xs):.{d}f} ({min(xs):.{d}f}-{max(xs):.{d}f})"


def mib(k):
    return "n/a" if not k else f"{k/1024:.1f}"


def report(files):
    for f in files:
        d = json.load(open(f))
        m = d["meta"]
        print(f"\n### {m['layout']} {m['files']} files ({m['bytes']/1048576:.0f} MiB), {m['zones']} vcs zones, {m['commits']} commits, job slice {m['slice_files']} files ({100*m['slice_files']/m['files']:.1f}%)\n")
        print(f"box cores {m['cores']}, load1 start/5/15 {'/'.join(m['load_start'])}, end {'/'.join(m['load_end'])}; {m['git']}; {m['vcs']}; runs {m['runs']}; date {m['date']}\n")
        print("**Scenario 1: cold runner clone** (wall ms median (min-max); client CPU s; server CPU s; runner disk MiB; files checked out; requests; payload MiB)\n")
        print("| tool [server] | wall ms | client CPU s | server CPU s | runner MiB | files | requests | MiB down |\n|---|---|---|---|---|---|---|---|")
        for k, x in d["s1"].items():
            rq = x["requests"] if x["requests"] else None
            if rq and rq[0] is not None:
                rqs, by = f"{med(rq):.0f} (vcs-server)", mib(med(x["bytes"]) / 1024)
            elif x.get("count"):
                rqs, by = f"{x['count']['requests']} ({x['count']['requests_are']})", mib(x["count"]["bytes"] / 1024)
            else:
                rqs, by = "n/a", "n/a"
            print(f"| {k} | {fmt(x['wall_ms'])} | {fmt(x['cpu_s'],2)} | {fmt(x['srv_cpu_s'],2)} | {mib(med(x['disk_kib']))} | {x.get('files_on_disk')} | {rqs} | {by} |")
        for sc, title in (("s2", "Scenario 2: warm runner, fetch 1 new commit + checkout"), ("s3", f"Scenario 3: {m['cycles']} sequential CI cycles (fetch+checkout), per-cycle")):
            if sc not in d:
                continue
            print(f"\n**{title}** (ms; then no-op build step = status)\n")
            print("| tool [server] | median | p95 | min-max | client CPU s (med) | server CPU s (med) | status ms (med) |\n|---|---|---|---|---|---|---|")
            for k, x in d[sc].items():
                f = x["fetch_ms"]
                print(f"| {k} | {med(f):.0f} | {p95(f):.0f} | {min(f):.0f}-{max(f):.0f} | {med(x['cpu_s']):.2f} | {med(x['srv_cpu_s']):.2f} | {med(x['status_ms']):.0f} |")
        if "warm_counts" in d:
            print("\n**Warm fetch, one commit: requests and payload**\n\n| tool | requests | bytes down |\n|---|---|---|")
            for k, x in d["warm_counts"].items():
                print(f"| {k} | {x['requests']} ({x['requests_are']}) | {x['bytes']} |")
        if "s4" in d:
            print(f"\n**Scenario 4: {m['parallel']} simultaneous cold clones from one server** (wall of slowest ms; summed client CPU s; server CPU s)\n")
            print("| tool [server] | slowest ms | client CPU s | server CPU s |\n|---|---|---|---|")
            for k, x in d["s4"].items():
                print(f"| {k} | {fmt(x['wall_ms'])} | {fmt(x['cpu_s'],1)} | {fmt(x['srv_cpu_s'],1)} |")
        print(f"\nserver stores: {json.dumps(d['server_sizes'])}")


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in ("run", "report"):
        sys.exit(__doc__)
    run(sys.argv[2:]) if sys.argv[1] == "run" else report(sys.argv[2:])
