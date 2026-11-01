#!/usr/bin/env python3
"""Full remote-workflow matrix: plain git vs walgit vs vcs (default settings) vs vcs (best documented settings).

usage: walgit_full.py <flat|deep|binary> <n_files> <reps> <out.json> [--init-reps K] [--count] [--no-maint] [--arms a,b]

All four arms run at the same time against ONE RustFS on loopback; every operation is run in INTERLEAVED order:
for each rep the arm order rotates (git, walgit, vcs_def, vcs_best -> walgit, vcs_def, vcs_best, git -> ...), so box
load drifts hit every arm alike. Timing pass talks to RustFS directly; the counting pass (--count, 1 rep, same
scenario) routes walgit-server and the vcs servers through a counting proxy and records requests and payload bytes
at the object store per operation.

Per operation (median of reps, raw values kept): wall ms, client-tree CPU (rusage of the command and every
descendant), front-server CPU (walgit-server / vcs-server utime+stime incl. reaped children), RustFS CPU, and peak
RSS (VmHWM, reset via clear_refs before the op) of the front server and RustFS.

Phases: init_push, inc_push, clone_full/clone_partial (warm|cold), fetch_1 (warm|cold), fetch_noop (warm|cold),
then maintenance on the remote and on a client copy, then the read phases again against the maintained remote
("post_*"). cold = server restarted (walgit: cache dir wiped, serve-only role; vcs-server: restarted) AND
`echo 3 > drop_caches` before the command.
"""
import argparse, json, os, platform, shutil, statistics, subprocess, sys, time, resource, urllib.request

HOME = os.path.expanduser("~")
WB = os.environ.get("WB_DIR", f"{HOME}/wb2")
RT = os.environ.get("WB_RT", f"{HOME}/rt/walgitfull/target/release")
WALGIT_SRC = os.environ.get("WB_WALGIT_SRC", f"{HOME}/ref/walgit")
WALGIT = f"{WALGIT_SRC}/target/release/walgit-server"
RUSTFS = os.environ.get("WB_RUSTFS", f"{HOME}/s3local/rustfs/rustfs")
PROXY_PY = os.path.join(os.path.dirname(os.path.abspath(__file__)), "s3count_proxy.py")
P_RF, P_WG, P_VD, P_VB, P_VF, P_PROXY = 19200, 19201, 19202, 19203, 19204, 19210
ENV = dict(os.environ, AWS_ACCESS_KEY_ID="benchak", AWS_SECRET_ACCESS_KEY="benchsecret1", AWS_DEFAULT_REGION="us-east-1",
           GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_TERMINAL_PROMPT="0",
           GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_0="gc.auto", GIT_CONFIG_VALUE_0="0", GIT_CONFIG_KEY_1="maintenance.auto", GIT_CONFIG_VALUE_1="false",
           VCS_TOKEN="tok1", VCS_PRINCIPAL="bench", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", PATH=f"{RT}:{os.environ['PATH']}")
ENV.pop("VCS_PACK_FORMAT", None)
ARMS_ALL = ["git", "walgit", "vcs_def", "vcs_best", "vcs_fast"]
COUNT = False  # counting pass: servers go through the proxy


def sh(cmd, cwd=None, check=True, env=None):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=env or ENV, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"failed ({r.returncode}): {cmd}\n{r.stdout[-800:]}\n{r.stderr[-1500:]}")
    return r


def ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return sum(int(x) for x in f[11:15]) / os.sysconf("SC_CLK_TCK")
    except Exception:
        return 0.0


def rss_reset(pid):
    try:
        open(f"/proc/{pid}/clear_refs", "w").write("5")
    except Exception:
        pass


def rss_peak_mib(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmHWM"):
                return int(l.split()[1]) / 1024
    except Exception:
        pass
    return None


def drop_caches():
    sh("sync; sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'")


def wait_http(url, tries=150):
    for _ in range(tries):
        try:
            urllib.request.urlopen(url, timeout=2).read()
            return
        except Exception as e:
            if hasattr(e, "code") and e.code in (200, 403, 404):
                return
            time.sleep(0.2)
    raise RuntimeError("not up: " + url)


class Svc:
    def __init__(self):
        self.p = {}

    def start(self, name, argv, url, env=None):
        port = int(url.split(":")[2].split("/")[0])
        if sh(f"ss -ltn | grep -q ':{port} '", check=False).returncode == 0:
            raise RuntimeError(f"port {port} already in use; refusing to start {name}")
        log = open(f"{WB}/{name}.log", "ab")
        self.p[name] = subprocess.Popen(argv, env=env or ENV, stdout=log, stderr=log, start_new_session=True)
        wait_http(url)

    def stop(self, name):
        pr = self.p.pop(name, None)
        if pr:
            pr.terminate()
            try:
                pr.wait(30)
            except subprocess.TimeoutExpired:
                pr.kill(); pr.wait()

    def stop_all(self):
        for n in list(self.p):
            self.stop(n)

    def pid(self, name):
        return self.p[name].pid if name in self.p else 0


S = Svc()


def start_rustfs():
    S.stop("rustfs"); time.sleep(1)
    sh(f"rm -rf {WB}/rfdata && mkdir -p {WB}/rfdata")
    e = dict(ENV, RUSTFS_ACCESS_KEY="benchak", RUSTFS_SECRET_KEY="benchsecret1")
    S.start("rustfs", [RUSTFS, "server", "--address", f"127.0.0.1:{P_RF}", f"{WB}/rfdata"], f"http://127.0.0.1:{P_RF}/", e)
    for b in ("walgit-b", "vcs"):
        for _ in range(30):
            if sh((f"aws --endpoint-url http://127.0.0.1:{P_RF} s3 mb s3://{b}" if not os.environ.get("WB_NO_AWS") else f"curl -sf -X PUT --aws-sigv4 aws:amz:us-east-1:s3 --user benchak:benchsecret1 http://127.0.0.1:{P_RF}/{b}"), check=False).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError("cannot create bucket " + b)


def proxy(op):
    if op == "reset":
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{P_PROXY}/__proxy/reset", method="POST", data=b""))
    else:
        return json.load(urllib.request.urlopen(f"http://127.0.0.1:{P_PROXY}/__proxy/stats"))


def endpoint():
    return f"http://127.0.0.1:{P_PROXY if COUNT else P_RF}"


def start_walgit(wipe_cache=False, serve_only=False):
    S.stop("walgit")
    cache = f"{WB}/walgit-cache"
    if wipe_cache:
        sh(f"rm -rf {cache}")
    cfg = f"""[server]
listen = "127.0.0.1:{P_WG}"
auto_create_on_push = true
roles = {'["serve"]' if serve_only else '[]'}
[server.tls]
mode = "off"
[server.auth]
mode = "none"
[store]
backend = "s3"
bucket = "walgit-b"
prefix = "p/"
[store.s3]
endpoint = "{endpoint()}"
region = "us-east-1"
force_path_style = true
[cache]
dir = "{cache}"
"""
    open(f"{WB}/cfg/walgit.toml", "w").write(cfg)
    S.start("walgit", [WALGIT, "--config", f"{WB}/cfg/walgit.toml"], f"http://127.0.0.1:{P_WG}/healthz")
    time.sleep(1)


def hash_token():
    tok = sh("vcs-server hash-token tok1").stdout.strip()
    open(f"{WB}/cfg/tokens", "w").write(f"bench write * {tok}\n")


def pack_env(name):
    f = {"vcs_best": "v3", "vcs_fast": "v1"}.get(name)
    return dict(ENV, VCS_PACK_FORMAT=f) if f else ENV


def start_vcs(name, port, prefix, best=None):
    S.stop(name)
    env = pack_env(name)
    extra = ["--max-body", os.environ["WB_VCS_MAX_BODY"]] if os.environ.get("WB_VCS_MAX_BODY") else []
    S.start(name, ["vcs-server", "--backend", f"s3://vcs/{prefix}?endpoint={endpoint()}", "--tokens", f"{WB}/cfg/tokens",
                   "--listen", f"127.0.0.1:{port}"] + extra, f"http://127.0.0.1:{port}/healthz", env)


def med(xs):
    return statistics.median(xs) if xs else None


def cell(vals):
    return {"runs": vals, "median": med(vals), "min": min(vals) if vals else None, "max": max(vals) if vals else None}


class Arm:
    def __init__(self, name, work):
        self.name, self.work = name, work
        self.kind = "git" if name == "git" else "walgit" if name == "walgit" else "vcs"
        self.svc = None if name == "git" else name
        self.port = {"walgit": P_WG, "vcs_def": P_VD, "vcs_best": P_VB, "vcs_fast": P_VF}.get(name)
        self.env = pack_env(name)
        self.main_url = self.main_dir = self.main_prefix = None
        self.reader = {}
        self.files = None
        self.ctr = 0
        sh(f"mkdir -p {work}/remotes")

    def pid(self):
        return S.pid(self.svc) if self.svc else 0

    def run(self, cmd, cwd=None, check=True):
        return sh(cmd, cwd=cwd, check=check, env=self.env)

    # ---- remotes
    def new_remote(self, rname):
        if self.kind == "git":
            p = f"{self.work}/remotes/{rname}.git"
            self.run(f"git init -q --bare -b main {p} && git -C {p} config uploadpack.allowFilter true && "
                     f"git -C {p} config uploadpack.allowAnySHA1InWant true")
            return f"file://{p}"
        if self.kind == "walgit":
            return f"http://127.0.0.1:{P_WG}/o/{rname}.git"
        prefix = f"{self.name}-{rname}"
        start_vcs(self.svc, self.port, prefix)
        self.cur_prefix = prefix
        return f"http://127.0.0.1:{self.port}"

    def restart_cold(self):
        if self.kind == "walgit":
            start_walgit(wipe_cache=True, serve_only=True)
        elif self.kind == "vcs":
            start_vcs(self.svc, self.port, self.main_prefix)

    def restart_warm_main(self):
        if self.kind == "vcs":
            start_vcs(self.svc, self.port, self.main_prefix)
        elif self.kind == "walgit":
            start_walgit()

    # ---- commands
    def edit(self, d, i):
        p = self.files[(i * 7919 + 42) % len(self.files)]
        with open(f"{d}/{p}", "a") as f:
            f.write(f"edit {i}\n")
        return p

    def commit_edit(self, d, i):
        p = self.edit(d, i)
        if self.kind == "vcs":
            self.run(f"vcs add '{p}' && vcs commit -m e{i}", cwd=d)
        else:
            self.run(f"git add '{p}' && git commit -q -m e{i}", cwd=d)

    def push_cmd(self, url):
        return "vcs publish" if self.kind == "vcs" else f"git push -q '{url}' main"

    def clone_cmd(self, url, dst, partial):
        if self.kind == "vcs":
            return f"vcs clone '{url}' '{dst}'" + (" --zone g0" if partial else "")
        if partial:
            return f"git clone -q --filter=blob:none --sparse '{url}' '{dst}' && git -C '{dst}' sparse-checkout set g0"
        return f"git clone -q '{url}' '{dst}'"

    def fetch_cmd(self):
        return "vcs fetch" if self.kind == "vcs" else "git fetch -q origin"

    def client_dir(self, d):
        return f"{d}/.{'vcs' if self.kind == 'vcs' else 'git'}"

    def timed(self, cmd, cwd):
        pid, rf = self.pid(), S.pid("rustfs")
        c0, r0 = ticks(pid), ticks(rf)
        for p in (pid, rf):
            if p:
                rss_reset(p)
        ru0 = resource.getrusage(resource.RUSAGE_CHILDREN)
        if COUNT:
            proxy("reset")
        t = time.perf_counter()
        r = self.run(cmd, cwd=cwd)
        wall = (time.perf_counter() - t) * 1000
        ru1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        o = {"wall_ms": wall, "client_cpu_s": (ru1.ru_utime + ru1.ru_stime) - (ru0.ru_utime + ru0.ru_stime),
             "srv_cpu_s": (ticks(pid) - c0) if pid else None, "rf_cpu_s": ticks(rf) - r0,
             "srv_rss_mib": rss_peak_mib(pid) if pid else None, "rf_rss_mib": rss_peak_mib(rf), "stdout": r.stdout[-300:]}
        if COUNT:
            o["proxy"] = proxy("stats")
        return o

    # ---- sizes
    def store_dir(self):
        if self.kind == "git":
            return f"{self.work}/remotes/init0.git"
        if self.kind == "walgit":
            return f"{WB}/rfdata/walgit-b/p/repos/o/init0"
        return f"{WB}/rfdata/vcs/{self.main_prefix}"

    def store_kib(self):
        d = self.store_dir()
        if not os.path.isdir(d):
            # walgit layout discovery fallback
            return None
        a = int(sh(f"du -sk '{d}' | cut -f1").stdout)
        b = int(sh(f"du -sk --apparent-size '{d}' | cut -f1").stdout)
        n = int(sh(f"find '{d}' -type f | wc -l").stdout)
        return {"du_kib": a, "apparent_kib": b, "files": n, "dir": d}


def opcell(rows):
    o = {}
    for k in ("wall_ms", "client_cpu_s", "srv_cpu_s", "rf_cpu_s", "srv_rss_mib", "rf_rss_mib"):
        v = [r[k] for r in rows if r.get(k) is not None]
        o[k] = cell(v) if v else None
    return o


def rotated(arms, k):
    k %= len(arms)
    return arms[k:] + arms[:k]


def prep_cold(a):
    a.restart_cold()
    drop_caches()


def prep_warm(a):
    # walgit's serve-only cold restart left a serve-only server: bring the normal (all roles) one back
    pass


def run_matrix(arms, srcs, reps, init_reps, res, maint):
    W = {a.name: a.work for a in arms}
    # ---------------- init push
    rows = {a.name: [] for a in arms}
    for k in range(init_reps):
        for a in rotated(arms, k):
            url = a.new_remote(f"init{k}")
            d = f"{a.work}/rep{k}"
            sh(f"cp -a '{srcs[a.name]}' '{d}'")
            if a.kind == "vcs":
                a.run(f"vcs remote set '{url}'", cwd=d)
            else:
                a.run(f"git remote add origin '{url}'", cwd=d)
            if a.kind != "git":
                time.sleep(0.5)
            rows[a.name].append(a.timed(a.push_cmd(url), d))
            if k == 0:
                a.main_url, a.main_dir = url, d
                a.main_prefix = getattr(a, "cur_prefix", None)
                time.sleep(2 if a.kind != "git" else 0)
                res[a.name]["store_after_init"] = a.store_kib()
            else:
                sh(f"rm -rf '{d}'")
    for a in arms:
        res[a.name]["init_push"] = opcell(rows[a.name])
        res[a.name]["init_push_stdout"] = rows[a.name][0]["stdout"]
        a.restart_warm_main()
        if a.kind == "walgit":
            pass
        if COUNT:
            res[a.name]["init_push_proxy"] = rows[a.name][0]["proxy"]
    # walgit idle chatter (counting pass only, one minute)
    if COUNT and "walgit" in W:
        proxy("reset"); time.sleep(65)
        res["walgit"]["idle_65s"] = proxy("stats")
    # ---------------- 1-file push
    rows = {a.name: [] for a in arms}
    for j in range(reps):
        for a in rotated(arms, j):
            a.commit_edit(a.main_dir, 1000 + j)
            rows[a.name].append(a.timed(a.push_cmd(a.main_url), a.main_dir))
            if a.kind == "walgit":
                time.sleep(1)
    for a in arms:
        res[a.name]["inc_push"] = opcell(rows[a.name])
        if COUNT:
            res[a.name]["inc_push_proxy"] = rows[a.name][0]["proxy"]
    read_phase(arms, reps, res, "")
    if maint:
        maintenance(arms, res)
        read_phase(arms, reps, res, "post_")


def read_phase(arms, reps, res, pre):
    # ---------------- clones
    for label, partial in (("clone_full", False), ("clone_partial", True)):
        for mode in ("warm", "cold"):
            rows = {a.name: [] for a in arms}
            for j in range(reps):
                for a in rotated(arms, j):
                    dst = f"{a.work}/{pre}{label}_{mode}_{j}"
                    sh(f"rm -rf '{dst}'")
                    if mode == "cold":
                        prep_cold(a)
                    elif a.kind == "walgit" and a.serve_only_now:
                        start_walgit(); a.serve_only_now = False
                    r = a.timed(a.clone_cmd(a.main_url, dst, partial), a.work)
                    rows[a.name].append(r)
                    if j == 0 and mode == "warm":
                        out = res[a.name].setdefault(pre + label + "_facts", {})
                        out["checked_out_files"] = int(sh(f"find '{dst}' -type f -not -path '*/.git/*' -not -path '*/.vcs/*' | wc -l").stdout)
                        out["client_store_kib"] = int(sh(f"du -sk '{a.client_dir(dst)}' | cut -f1").stdout)
                        out["client_store_apparent_kib"] = int(sh(f"du -sk --apparent-size '{a.client_dir(dst)}' | cut -f1").stdout)
                    if mode == "cold" and a.kind != "git":
                        a.serve_only_now = a.kind == "walgit"
                    if label == "clone_full" and mode == "warm" and j == 0:
                        a.reader[pre] = dst
                    else:
                        sh(f"rm -rf '{dst}'")
            for a in arms:
                res[a.name][f"{pre}{label}_{mode}"] = opcell(rows[a.name])
                if COUNT:
                    res[a.name][f"{pre}{label}_{mode}_proxy"] = rows[a.name][0]["proxy"]
    # ---------------- fetch 1 commit, then no-op fetch
    for what in ("fetch_1", "fetch_noop"):
        for mode in ("warm", "cold"):
            rows = {a.name: [] for a in arms}
            for j in range(reps):
                for a in rotated(arms, j):
                    if what == "fetch_1":
                        a.ctr += 1
                        a.commit_edit(a.main_dir, 2000 + a.ctr + (500 if pre else 0))
                        if a.kind == "walgit" and a.serve_only_now:
                            start_walgit(); a.serve_only_now = False
                        a.run(a.push_cmd(a.main_url), cwd=a.main_dir)
                        if a.kind == "walgit":
                            time.sleep(1)
                    if mode == "cold":
                        prep_cold(a)
                        a.serve_only_now = a.kind == "walgit"
                    elif a.kind == "walgit" and a.serve_only_now:
                        start_walgit(); a.serve_only_now = False
                    rows[a.name].append(a.timed(a.fetch_cmd(), a.reader[pre]))
            for a in arms:
                res[a.name][f"{pre}{what}_{mode}"] = opcell(rows[a.name])
                if COUNT:
                    res[a.name][f"{pre}{what}_{mode}_proxy"] = rows[a.name][0]["proxy"]
    for a in arms:
        if a.kind == "walgit" and a.serve_only_now:
            start_walgit(); a.serve_only_now = False


def dir_signature(d):
    return int(sh(f"du -sk '{d}' | cut -f1").stdout), int(sh(f"find '{d}' -type f | wc -l").stdout)


def maintenance(arms, res):
    """Maintain the main remote of every arm; record time, CPU, request counts (vcs) and sizes before/after."""
    for a in arms:
        m = res[a.name].setdefault("maint", {})
        m["remote_before"] = a.store_kib()
        if a.kind == "git":
            bare = a.store_dir()[0:]
            bare = bare.replace("file://", "")
            m["gc_default"] = a.timed(f"git -C '{bare}' gc -q", a.work)
            m["remote_after"] = a.store_kib()
            cp = f"{a.work}/remotes/aggr.git"
            sh(f"cp -a '{bare}' '{cp}'")
            m["gc_aggressive"] = a.timed(f"git -C '{cp}' gc -q --aggressive --prune=now", a.work)
            m["gc_aggressive_size_kib"] = int(sh(f"du -sk '{cp}' | cut -f1").stdout)
            sh(f"rm -rf '{cp}'")
        elif a.kind == "walgit":
            # (1) let the maintainer loop (default config, all roles, 60 s interval) settle; (2) force a compaction with
            # base rebuild through its ops endpoint
            t0 = time.time(); last = None; stable = 0; samples = []
            while time.time() - t0 < 420 and stable < 2:
                time.sleep(25)
                sig = dir_signature(f"{WB}/rfdata/walgit-b/p/repos/o/init0")
                samples.append((round(time.time() - t0), sig))
                stable = stable + 1 if sig == last else 0
                last = sig
            m["settle_samples"] = samples
            m["remote_after_settle"] = a.store_kib()
            ops = None
            try:
                r = a.timed(f"curl -sS -m 900 -X POST 'http://127.0.0.1:{P_WG}/o/init0/api/ops/compact?force=1&base=1'", a.work)
                ops = r
            except Exception as e:
                ops = {"error": str(e)[:300]}
            m["ops_compact_force_base"] = ops
            time.sleep(5)
            m["remote_after"] = a.store_kib()
        else:
            hold = a.main_dir
            # compaction goes straight to the store (the HTTP server has no maintenance endpoint)
            ep = f"s3://vcs/{a.main_prefix}?endpoint={endpoint()}"
            a.run(f"vcs remote set '{ep}'", cwd=hold)
            fmt = {"vcs_best": " --pack-format v3", "vcs_fast": " --pack-format v1"}.get(a.name, "")
            m["dry_run"] = a.run(f"vcs gc --remote --compact --dry-run{fmt}", cwd=hold, check=False).stdout[-600:]
            m["compact"] = a.timed(f"vcs gc --remote --compact --yes{fmt}", hold)
            a.run(f"vcs remote set '{a.main_url}'", cwd=hold)
            m["remote_after"] = a.store_kib()
            a.restart_warm_main()
    # client side maintenance on a copy of the (pre-maintenance) full-clone reader
    for a in arms:
        rd = a.reader[""]
        m = res[a.name]["maint"]
        m["client_before_kib"] = int(sh(f"du -sk '{a.client_dir(rd)}' | cut -f1").stdout)
        variants = {"git": [("gc", "git gc -q"), ("gc_aggressive", "git gc -q --aggressive --prune=now")]}.get(a.kind) or \
                   [("repack_default", "vcs repack"), ("repack_v2", "vcs repack --format v2"), ("repack_v3", "vcs repack --format v3"),
                    ("repack_v4", "vcs repack --format v4")]
        if a.name == "vcs_best":
            variants = [("repack_v3", "vcs repack --format v3"), ("repack_v4", "vcs repack --format v4")]
        if a.name == "vcs_fast":
            variants = [("repack_v1", "vcs repack --format v1")]
        if a.kind == "walgit":
            variants = [("gc", "git gc -q"), ("gc_aggressive", "git gc -q --aggressive --prune=now")]
        m["client"] = {}
        for name, cmd in variants:
            cp = f"{a.work}/maintcopy"
            sh(f"rm -rf '{cp}'; cp -a '{rd}' '{cp}'")
            try:
                r = a.timed(cmd, cp)
                r["size_kib"] = int(sh(f"du -sk '{a.client_dir(cp)}' | cut -f1").stdout)
                if a.kind == "vcs" and os.environ.get("WB_FSCK", "1") == "1":
                    f = a.run("vcs fsck", cwd=cp, check=False)
                    r["fsck_ok"] = f.returncode == 0
                m["client"][name] = r
            except Exception as e:
                m["client"][name] = {"error": str(e)[:300]}
            sh(f"rm -rf '{cp}'")


def list_files(root):
    out = []
    for d, ds, fs in os.walk(root):
        for f in fs:
            out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out)


def regroup(ds, dst):
    leaves = []
    for d, ds_, fs in os.walk(ds):
        if fs:
            leaves.append((os.path.relpath(d, ds), fs))
    leaves.sort()
    for i, (l, fs) in enumerate(leaves):
        g = i * 10 // len(leaves)
        t = os.path.join(dst, f"g{g}", l)
        os.makedirs(t, exist_ok=True)
        for f in fs:
            shutil.move(os.path.join(ds, l, f), os.path.join(t, f))


def build_sources(arms, ds, files, nhist=50):
    """One plain-git source and one vcs source (vcs_def and vcs_best clone the same one)."""
    srcs = {}
    todo = {}
    for a in arms:
        key = "vcs" if a.kind == "vcs" else a.name if a.kind == "git" else "git"
        todo.setdefault(key, []).append(a)
    for key, group in todo.items():
        a = group[0]
        a.files = files
        src = f"{a.work}/src"
        sh(f"cp -a '{ds}' '{src}'")
        if key == "vcs":
            a.run("vcs init", cwd=src)
            for g in range(10):
                a.run(f"vcs zone create g{g} --path g{g}", cwd=src)
            a.run("vcs add . && vcs commit -m initial", cwd=src)
        else:
            a.run("git init -q -b main && git add -A && git commit -q -m initial", cwd=src)
        for i in range(1, nhist + 1):
            a.commit_edit(src, i)
        for b in group:
            b.files = files
            if b is a:
                srcs[b.name] = src
            else:
                s2 = f"{b.work}/src"
                sh(f"cp -a '{src}' '{s2}'")
                srcs[b.name] = s2
    if "walgit" in [a.name for a in arms] and "walgit" not in srcs:
        pass
    return srcs


def main():
    global COUNT
    ap = argparse.ArgumentParser()
    ap.add_argument("layout"); ap.add_argument("n", type=int); ap.add_argument("reps", type=int); ap.add_argument("out")
    ap.add_argument("--init-reps", type=int, default=None)
    ap.add_argument("--count", action="store_true")
    ap.add_argument("--no-maint", action="store_true")
    ap.add_argument("--arms", default=",".join(ARMS_ALL))
    a_ = ap.parse_args()
    COUNT = a_.count
    reps = 1 if COUNT else a_.reps
    init_reps = 1 if COUNT else (a_.init_reps or a_.reps)
    os.makedirs(f"{WB}/cfg", exist_ok=True)
    work = f"{WB}/work-{a_.layout}-{a_.n}{'-c' if COUNT else ''}"
    sh(f"rm -rf {work} && mkdir -p {work}")
    raw = f"{work}/raw"
    sh(f"datagen {a_.layout} {a_.n} {raw} 42")
    ds = f"{work}/ds"
    os.makedirs(ds)
    regroup(raw, ds)
    shutil.rmtree(raw)
    files = list_files(ds)
    total = sum(os.path.getsize(f"{ds}/{f}") for f in files)
    vcs_rev = sh("vcs --version", check=False).stdout.strip()
    meta = {"layout": a_.layout, "n_files": len(files), "bytes": total, "g0_files": len([f for f in files if f.startswith("g0/")]),
            "reps": reps, "init_reps": init_reps, "count_pass": COUNT, "date": time.strftime("%FT%TZ", time.gmtime()),
            "kernel": platform.release(), "git": sh("git --version").stdout.strip(),
            "cpu": [l.split(":")[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")][0], "cores": os.cpu_count(),
            "load_start": open("/proc/loadavg").read().split()[:3], "rustfs": sh(f"{RUSTFS} --version").stdout.strip(),
            "walgit_rev": sh(f"git -C {WALGIT_SRC} rev-parse HEAD").stdout.strip(), "vcs_version": vcs_rev,
            "vcs_rev": os.environ.get("WB_VCS_REV", "?")}
    out = {"meta": meta, "res": {}}
    try:
        start_rustfs()
        hash_token()
        if COUNT:
            S.start("proxy", [sys.executable, PROXY_PY, str(P_PROXY), "127.0.0.1", str(P_RF)], f"http://127.0.0.1:{P_PROXY}/__proxy/stats")
        names = a_.arms.split(",")
        arms = [Arm(n, f"{work}/{n}") for n in names]
        for a in arms:
            a.serve_only_now = False
            out["res"][a.name] = {}
        if "walgit" in names:
            start_walgit()
        srcs = build_sources(arms, ds, files)
        run_matrix(arms, srcs, reps, init_reps, out["res"], not a_.no_maint)
        meta["load_end"] = open("/proc/loadavg").read().split()[:3]
    finally:
        json.dump(out, open(a_.out, "w"), indent=1)
        S.stop_all()
        sh(f"rm -rf {work} {WB}/rfdata {WB}/walgit-cache", check=False)


main()
