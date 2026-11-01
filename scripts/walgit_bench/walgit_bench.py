#!/usr/bin/env python3
"""Remote-workflow benchmark: plain git (bare repo on local disk) vs walgit vs vcs-server, the latter two backed
by one local RustFS S3 server. Runs on the Linux build box; everything is loopback.

usage: walgit_bench.py <flat|deep|binary> <n_files> <reps> <out.json> [--init-reps K] [--tools git,walgit,vcs]

Phases per tool (all timing is client wall clock of the command, median of `reps`, raw values kept):
  init_push        push/publish of the full tree plus 50 history commits to a fresh remote (K reps, fresh remote each)
  inc_push         edit 1 file, commit (untimed), push/publish (reps)
  clone_full       full clone, warm / cold-os (drop_caches) / cold-server (walgit: cache dir wiped + restart + drop)
  clone_partial    git/walgit: --filter=blob:none --sparse + `sparse-checkout set g0`; vcs: clone --zone g0 (10% of files)
  fetch_1          1 new commit pushed from elsewhere, then the reader fetches (timed)
Then a counting pass (1 rep per op) with a counting proxy between the servers and RustFS: requests and payload
bytes at the object store per op. Timed runs go direct to RustFS (no proxy).
Server CPU = utime+stime(+reaped children) delta of the walgit-server / vcs-server process; RustFS CPU likewise.
"""
import argparse, json, os, platform, shutil, signal, statistics, subprocess, sys, time, resource, urllib.request

HOME = os.path.expanduser("~")
WB = f"{HOME}/wb"
RT = f"{HOME}/rt/walgit-bench/target/release"
WALGIT = f"{HOME}/ref/walgit/target/release/walgit-server"
RUSTFS = f"{HOME}/s3local/rustfs/rustfs"
PROXY_PY = f"{HOME}/rt/walgit-bench/scripts/walgit_bench/s3count_proxy.py"
P_RF, P_WG, P_VCS, P_PROXY = 19100, 19101, 19102, 19110
ENV = dict(os.environ, AWS_ACCESS_KEY_ID="benchak", AWS_SECRET_ACCESS_KEY="benchsecret1", AWS_DEFAULT_REGION="us-east-1",
           GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_TERMINAL_PROMPT="0",
           GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_0="gc.auto", GIT_CONFIG_VALUE_0="0", GIT_CONFIG_KEY_1="maintenance.auto", GIT_CONFIG_VALUE_1="false",
           VCS_TOKEN="tok1", VCS_PRINCIPAL="bench", PATH=f"{RT}:{os.environ['PATH']}")


def sh(cmd, cwd=None, check=True, quiet=True):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=ENV, capture_output=quiet, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"failed ({r.returncode}): {cmd}\n{r.stdout[-800:]}\n{r.stderr[-1500:]}")
    return r


def ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return sum(int(x) for x in f[11:15]) / os.sysconf("SC_CLK_TCK")  # utime stime cutime cstime
    except Exception:
        return 0.0


def drop_caches():
    sh("sync; sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'")


def wait_http(url, tries=100):
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
    """Process manager for rustfs / proxy / walgit / vcs-server."""

    def __init__(self):
        self.p = {}

    def start(self, name, argv, url, env=None):
        port = int(url.split(":")[2].split("/")[0])
        if sh(f"ss -ltn | grep -q ':{port} '", check=False).returncode == 0:
            raise RuntimeError(f"port {port} already in use; refusing to start {name}")
        log = open(f"{WB}/{name}.log", "ab")
        self.p[name] = subprocess.Popen(argv, env=env or ENV, stdout=log, stderr=log, start_new_session=True)
        wait_http(url)
        return self.p[name].pid

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


S = Svc()


def start_rustfs(fresh=True):
    S.stop("rustfs")
    time.sleep(1)
    if fresh:
        sh(f"rm -rf {WB}/rfdata && mkdir -p {WB}/rfdata")
    e = dict(ENV, RUSTFS_ACCESS_KEY="benchak", RUSTFS_SECRET_KEY="benchsecret1")
    S.start("rustfs", [RUSTFS, "server", "--address", f"127.0.0.1:{P_RF}", f"{WB}/rfdata"], f"http://127.0.0.1:{P_RF}/", e)
    if fresh:
        for b in ("walgit-b", "vcs"):
            for attempt in range(30):
                if sh(f"aws --endpoint-url http://127.0.0.1:{P_RF} s3 mb s3://{b}", check=False).returncode == 0:
                    break
                time.sleep(1)
            else:
                raise RuntimeError("cannot create bucket " + b)


def start_proxy():
    S.start("proxy", [sys.executable, PROXY_PY, str(P_PROXY), "127.0.0.1", str(P_RF)], f"http://127.0.0.1:{P_PROXY}/__proxy/stats")


def proxy(op):
    if op == "reset":
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{P_PROXY}/__proxy/reset", method="POST", data=b""))
    else:
        return json.load(urllib.request.urlopen(f"http://127.0.0.1:{P_PROXY}/__proxy/stats"))


def endpoint(use_proxy):
    return f"http://127.0.0.1:{P_PROXY if use_proxy else P_RF}"


def start_walgit(prefix, use_proxy, wipe_cache=False, serve_only=False):
    S.stop("walgit")
    cache = f"{WB}/walgit-cache-{prefix}"
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
prefix = "{prefix}/"
[store.s3]
endpoint = "{endpoint(use_proxy)}"
region = "us-east-1"
force_path_style = true
[cache]
dir = "{cache}"
"""
    open(f"{WB}/cfg/walgit-{prefix}.toml", "w").write(cfg)
    S.start("walgit", [WALGIT, "--config", f"{WB}/cfg/walgit-{prefix}.toml"], f"http://127.0.0.1:{P_WG}/healthz")
    time.sleep(1)


def start_vcs(prefix, use_proxy):
    S.stop("vcs")
    tok = sh("vcs-server hash-token tok1").stdout.strip()
    open(f"{WB}/cfg/tokens", "w").write(f"bench write * {tok}\n")
    S.start("vcs", ["vcs-server", "--backend", f"s3://vcs/{prefix}?endpoint={endpoint(use_proxy)}", "--tokens", f"{WB}/cfg/tokens",
                    "--listen", f"127.0.0.1:{P_VCS}"] + (["--max-body", os.environ["WB_VCS_MAX_BODY"]] if os.environ.get("WB_VCS_MAX_BODY") else []), f"http://127.0.0.1:{P_VCS}/healthz")


def med(xs):
    return statistics.median(xs) if xs else None


def cell(vals):
    return {"runs": vals, "median": med(vals), "min": min(vals) if vals else None, "max": max(vals) if vals else None}


class Tool:
    def __init__(self, name, work, use_proxy=False, tag="t"):
        self.name, self.work, self.use_proxy, self.tag = name, work, use_proxy, tag
        self.files = None
        self.res = {}
        self.ctr = 0

    # ---- server side -----
    def srv_names(self):
        return {"git": [], "walgit": ["walgit"], "vcs": ["vcs"]}[self.name]

    def remote_url(self, rname):
        if self.name == "git":
            return f"file://{self.work}/remotes/{rname}.git"
        if self.name == "walgit":
            return f"http://127.0.0.1:{P_WG}/o/{rname}.git"
        return f"http://127.0.0.1:{P_VCS}"

    def new_remote(self, rname):
        """Fresh empty remote; returns its URL."""
        if self.name == "git":
            p = f"{self.work}/remotes/{rname}.git"
            sh(f"git init -q --bare -b main {p} && git -C {p} config uploadpack.allowFilter true && "
               f"git -C {p} config uploadpack.allowAnySHA1InWant true")
        elif self.name == "vcs":
            start_vcs(f"{self.tag}-{rname}", self.use_proxy)
        return self.remote_url(rname)

    # ---- client commands ----
    def edit(self, d, i):
        p = self.files[(i * 7919 + 42) % len(self.files)]
        with open(f"{d}/{p}", "a") as f:
            f.write(f"edit {i}\n")
        return p

    def commit_edit(self, d, i):
        p = self.edit(d, i)
        if self.name == "vcs":
            sh(f"vcs add '{p}' && vcs commit -m e{i}", cwd=d)
        else:
            sh(f"git add '{p}' && git commit -q -m e{i}", cwd=d)

    def push_cmd(self, url):
        return f"git push -q '{url}' main" if self.name != "vcs" else "vcs publish"

    def clone_cmd(self, url, dst, partial):
        if self.name == "vcs":
            return f"vcs clone '{url}' '{dst}'" + (" --zone g0" if partial else "")
        if partial:
            return f"git clone -q --filter=blob:none --sparse '{url}' '{dst}' && git -C '{dst}' sparse-checkout set g0"
        return f"git clone -q '{url}' '{dst}'"

    def fetch_cmd(self):
        return "vcs fetch" if self.name == "vcs" else "git fetch -q origin"

    def build_source(self, ds_dir, files_sorted, nhist=50):
        src = f"{self.work}/src"
        sh(f"cp -a '{ds_dir}' '{src}'")
        self.files = files_sorted
        if self.name == "vcs":
            sh("vcs init", cwd=src)
            for g in range(10):
                sh(f"vcs zone create g{g} --path g{g}", cwd=src)
            sh("vcs add . && vcs commit -m initial", cwd=src)
        else:
            sh("git init -q -b main && git add -A && git commit -q -m initial", cwd=src)
        for i in range(1, nhist + 1):
            self.commit_edit(src, i)
        return src

    def timed(self, cmd, cwd, svc_pids=None):
        """Run cmd; return dict(wall_ms, srv_cpu_s, rustfs_cpu_s, client_cpu_s)."""
        names = self.srv_names()
        c0 = {n: ticks(S.pid(n)) for n in names}
        r0 = ticks(S.pid("rustfs"))
        ru0 = resource.getrusage(resource.RUSAGE_CHILDREN)
        t = time.perf_counter()
        sh(cmd, cwd=cwd)
        wall = (time.perf_counter() - t) * 1000
        ru1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        return {"wall_ms": wall, "srv_cpu_s": sum(ticks(S.pid(n)) - c0[n] for n in names) if names else None,
                "rustfs_cpu_s": ticks(S.pid("rustfs")) - r0,
                "client_tree_cpu_s": (ru1.ru_utime + ru1.ru_stime) - (ru0.ru_utime + ru0.ru_stime)}

    def opcell(self, rows):
        o = {"wall_ms": cell([r["wall_ms"] for r in rows])}
        for k in ("srv_cpu_s", "rustfs_cpu_s", "client_tree_cpu_s"):
            v = [r[k] for r in rows if r[k] is not None]
            o[k] = cell(v) if v else None
        return o

    def counted(self, cmd, cwd):
        proxy("reset")
        self.timed(cmd, cwd)
        return proxy("stats")

    # ---- scenario -----
    def run(self, src, reps, init_reps, counting):
        W = self.work
        res = {}
        # restart services for this tool with the right endpoint
        if self.name == "walgit":
            start_walgit(self.tag, counting, wipe_cache=True)
        rows = []
        main_url = None
        cnt = {}
        for k in range(init_reps):
            url = self.new_remote(f"init{k}")
            d = f"{W}/rep{k}"
            sh(f"cp -a '{src}' '{d}'")
            if self.name != "vcs":
                sh(f"git remote add origin '{url}'", cwd=d)
            else:
                sh(f"vcs remote set '{url}'", cwd=d)
            if self.name == "vcs" or self.name == "walgit":
                time.sleep(0.5)
            if counting:
                cnt["init_push"] = self.counted(self.push_cmd(url), d)
            else:
                rows.append(self.timed(self.push_cmd(url), d))
            if k == 0:
                main_url, main_dir = url, d
                if self.name == "vcs":
                    main_prefix = f"{self.tag}-init0"
            else:
                sh(f"rm -rf '{d}'")
        if self.name == "vcs":
            start_vcs(f"{self.tag}-init0", counting)
        if not counting:
            res["init_push"] = self.opcell(rows)
            res["init_push"]["fresh_remotes"] = init_reps
        res["store_after_init_kib"] = self.store_kib()
        if counting and self.name != "git":
            time.sleep(3)
            proxy("reset"); time.sleep(65)
            cnt["idle_65s_after_push"] = proxy("stats")
        # incremental push on the main remote
        rows = []
        for j in range(reps if not counting else 1):
            self.commit_edit(main_dir, 1000 + j)
            if counting:
                cnt["inc_push"] = self.counted(self.push_cmd(main_url), main_dir)
            else:
                rows.append(self.timed(self.push_cmd(main_url), main_dir))
        if not counting:
            res["inc_push"] = self.opcell(rows)
        if self.name == "walgit":
            time.sleep(2)

        def clone_variants(label, partial):
            out = {}
            modes = ["warm", "cold_os"] + (["cold_server"] if self.name == "walgit" else [])
            for mode in modes:
                rows = []
                for j in range(reps if not counting else 1):
                    dst = f"{W}/cl_{label}_{mode}_{j}"
                    sh(f"rm -rf '{dst}'")
                    if mode == "cold_server":
                        start_walgit(self.tag, counting, wipe_cache=True, serve_only=True)
                    if mode != "warm":
                        drop_caches()
                    if counting:
                        cnt[f"clone_{label}_{mode}"] = self.counted(self.clone_cmd(main_url, dst, partial), W)
                    else:
                        rows.append(self.timed(self.clone_cmd(main_url, dst, partial), W))
                    if j == 0 and not counting and mode == "warm":
                        out["checked_out_files"] = int(sh(f"find '{dst}' -type f -not -path '*/.git/*' -not -path '*/.vcs/*' | wc -l").stdout)
                        out["client_store_kib"] = int(sh(f"du -sk '{dst}/.{'vcs' if self.name == 'vcs' else 'git'}' | cut -f1").stdout)
                    if label == "full" and mode == "warm" and j == 0:
                        self.reader = dst
                    else:
                        sh(f"rm -rf '{dst}'")
                if not counting:
                    out[mode] = self.opcell(rows)
            return out

        res["clone_full"] = clone_variants("full", False)
        res["clone_partial"] = clone_variants("partial", True)
        if not counting:
            # make sure reader exists: first warm full clone kept in self.reader
            pass
        # fetch of 1 new commit
        fres = {}
        for mode in ["warm", "cold_os"] + (["cold_server"] if self.name == "walgit" else []):
            rows = []
            for j in range(reps if not counting else 1):
                self.ctr += 1
                self.commit_edit(main_dir, 2000 + self.ctr)
                sh(self.push_cmd(main_url), cwd=main_dir)
                if self.name == "walgit":
                    time.sleep(1)
                if mode == "cold_server":
                    start_walgit(self.tag, counting, wipe_cache=True, serve_only=True)
                if mode != "warm":
                    drop_caches()
                if counting:
                    cnt[f"fetch_1_{mode}"] = self.counted(self.fetch_cmd(), self.reader)
                else:
                    rows.append(self.timed(self.fetch_cmd(), self.reader))
            if not counting:
                fres[mode] = self.opcell(rows)
        res["fetch_1"] = fres
        if counting:
            return cnt
        return res

    def store_kib(self):
        if self.name == "git":
            return int(sh(f"du -sk {self.work}/remotes/init0.git | cut -f1").stdout)
        b = "walgit-b" if self.name == "walgit" else "vcs"
        return int(sh(f"du -sk {WB}/rfdata/{b} | cut -f1").stdout)


def list_files(root):
    out = []
    for d, ds, fs in os.walk(root):
        for f in fs:
            out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out)


def regroup(ds, dst):
    """Move every directory that directly holds files into one of 10 contiguous groups g0..g9 (same bytes, one extra
    path level), so a zone / sparse cone of one group is about 10% of the files."""
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


def idle_counts(tool_name, secs=30):
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("layout"); ap.add_argument("n", type=int); ap.add_argument("reps", type=int); ap.add_argument("out")
    ap.add_argument("--init-reps", type=int, default=None)
    ap.add_argument("--tools", default="git,walgit,vcs")
    ap.add_argument("--passes", default="time,count")
    a = ap.parse_args()
    init_reps = a.init_reps or a.reps
    os.makedirs(f"{WB}/cfg", exist_ok=True)
    work = f"{WB}/work-{a.layout}-{a.n}"
    sh(f"rm -rf {work} && mkdir -p {work}")
    raw = f"{work}/raw"
    sh(f"datagen {a.layout} {a.n} {raw} 42")
    ds = f"{work}/ds"
    os.makedirs(ds)
    regroup(raw, ds)
    shutil.rmtree(raw)
    files = list_files(ds)
    total = sum(os.path.getsize(f"{ds}/{f}") for f in files)
    g0 = [f for f in files if f.startswith("g0/")]
    meta = {"layout": a.layout, "n_files": len(files), "bytes": total, "g0_files": len(g0), "reps": a.reps, "init_reps": init_reps,
            "date": time.strftime("%FT%TZ", time.gmtime()), "kernel": platform.release(), "git": sh("git --version").stdout.strip(),
            "cpu": [l.split(":")[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")][0], "cores": os.cpu_count(),
            "load_start": open("/proc/loadavg").read().split()[:3], "rustfs": sh(f"{RUSTFS} --version").stdout.strip(),
            "walgit_rev": sh(f"git -C {HOME}/ref/walgit rev-parse HEAD").stdout.strip(),
            "vcs_rev": sh("vcs --version", check=False).stdout.strip()}
    out = {"meta": meta, "timing": {}, "counting": {}}
    try:
        for tool in a.tools.split(","):
            for counting in [c for c in ((False,) if tool == "git" else (False, True)) if ("count" if c else "time") in a.passes]:
                start_rustfs(fresh=True)
                if counting:
                    start_proxy()
                t = Tool(tool, f"{work}/{tool}-{'c' if counting else 't'}", counting, "c" if counting else "t")
                sh(f"mkdir -p {t.work}/remotes")
                src = t.build_source(ds, files)
                r = t.run(src, a.reps, init_reps, counting)
                (out["counting"] if counting else out["timing"])[tool] = r
                if counting and tool == "walgit":
                    pass
                S.stop("walgit"); S.stop("vcs"); S.stop("proxy")
                sh(f"rm -rf {t.work}")
                json.dump(out, open(a.out, "w"), indent=1)
        meta["load_end"] = open("/proc/loadavg").read().split()[:3]
        json.dump(out, open(a.out, "w"), indent=1)
    finally:
        S.stop_all()
        sh(f"rm -rf {work} {WB}/rfdata/* {WB}/walgit-cache-*", check=False)


main()
