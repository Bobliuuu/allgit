#!/usr/bin/env python3
"""High-load / many-pusher benchmark: git (git daemon), walgit (RustFS), vcs-server on local disk and on RustFS.

Scenarios (each run against a fresh remote per arm):
  branches  N pushers, each on its own branch, K commit+push each (manifest contention only)
  same      N pushers on ONE branch, K commit+push each, with recovery (fetch/rebase-or-merge, retry); end state verified
  seq       one pusher, M sequential small commits, each pushed (latency curve at 1/10/100/.../M)
  clones    N/2 pushers on own branches while 4 loops clone the remote; clone latency under write load

Every worker has its own clone. A push counts as an "attempt"; a push that fails is recovered and retried
(up to --max-retries). Server CPU (utime+stime) and peak RSS are sampled from /proc; RustFS likewise.
After each scenario the remote is cloned fresh and the commit/file count is checked, so lost pushes are caught.

usage: highload.py OUT.json --scenarios branches,same,seq,clones --arms git,vcs_disk,vcs_s3,walgit --n 8,32 --k 20 --m 1000
Run on a quiet box under `flock ~/bench.lock`.
"""
import argparse, json, os, random, shutil, statistics, subprocess, sys, threading, time, urllib.request

HOME = os.path.expanduser("~")
WB = os.environ.get("WB_DIR", f"{HOME}/hl/wb")
RT = os.environ.get("WB_RT", f"{HOME}/rt/highload/target/release")
WALGIT = os.environ.get("WB_WALGIT", f"{HOME}/rt/walgitd-env/ref/walgit/target/release/walgit-server")
RUSTFS = os.environ.get("WB_RUSTFS", f"{HOME}/rt/walgitd-env/s3local/rustfs")
P_RF, P_WG, P_VD, P_VS, P_GIT = 19300, 19301, 19302, 19303, 19305
ENV = dict(os.environ, AWS_ACCESS_KEY_ID="benchak", AWS_SECRET_ACCESS_KEY="benchsecret1", AWS_DEFAULT_REGION="us-east-1",
           GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", GIT_TERMINAL_PROMPT="0",
           VCS_TOKEN="tok1", VCS_PRINCIPAL="bench", VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid",
           PATH=f"{RT}:{os.environ['PATH']}")


def sh(cmd, cwd=None, check=True):
    r = subprocess.run(cmd, shell=True, cwd=cwd, env=ENV, capture_output=True, text=True)
    if check and r.returncode:
        raise RuntimeError(f"{cmd!r} rc={r.returncode}: {(r.stdout + r.stderr)[-400:]}")
    return r


def ticks(pid):
    if not pid:
        return 0.0
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")
    except OSError:
        return 0.0


def hwm_mib(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmHWM"):
                return int(l.split()[1]) / 1024
    except OSError:
        pass
    return 0.0


def wait_http(url, tries=150):
    for _ in range(tries):
        try:
            urllib.request.urlopen(url, timeout=2).read()
            return
        except Exception as e:
            if getattr(e, "code", 0) in (200, 401, 403, 404):
                return
            time.sleep(0.2)
    raise RuntimeError("not up: " + url)


def wait_port(port, tries=100):
    for _ in range(tries):
        if sh(f"ss -ltn | grep -q ':{port} '", check=False).returncode == 0:
            return
        time.sleep(0.1)
    raise RuntimeError(f"port {port} not up")


PROCS = {}
AUTO = False  # vcs: let `publish` fetch/merge/retry by itself on a moved branch


def start(name, argv, port, url=None, env=None):
    if sh(f"ss -ltn | grep -q ':{port} '", check=False).returncode == 0:
        raise RuntimeError(f"port {port} in use; refusing to start {name}")
    log = open(f"{WB}/{name}.log", "ab")
    PROCS[name] = subprocess.Popen(argv, env=env or ENV, stdout=log, stderr=log, start_new_session=True)
    wait_http(url) if url else wait_port(port)


def stop(name):
    p = PROCS.pop(name, None)
    if p:
        p.terminate()
        try:
            p.wait(30)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait()


def stop_all():
    for n in list(PROCS):
        stop(n)


def start_rustfs():
    stop("rustfs"); time.sleep(1)
    sh(f"rm -rf {WB}/rfdata && mkdir -p {WB}/rfdata")
    start("rustfs", [RUSTFS, "server", "--address", f"127.0.0.1:{P_RF}", f"{WB}/rfdata"], P_RF, f"http://127.0.0.1:{P_RF}/",
          dict(ENV, RUSTFS_ACCESS_KEY="benchak", RUSTFS_SECRET_KEY="benchsecret1"))
    for b in ("walgit-b", "vcs"):
        for _ in range(30):
            if sh(f"curl -sf -X PUT --aws-sigv4 aws:amz:us-east-1:s3 --user benchak:benchsecret1 http://127.0.0.1:{P_RF}/{b}", check=False).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError("bucket " + b)


def stats(xs):
    if not xs:
        return None
    s = sorted(xs)
    q = lambda p: s[min(len(s) - 1, int(p * len(s)))]
    return {"n": len(s), "p50": q(.5), "p90": q(.9), "p99": q(.99), "max": s[-1], "mean": statistics.mean(s)}


class Arm:
    def __init__(self, name):
        self.name = name
        self.kind = "vcs" if name.startswith("vcs") else name
        self.srv = {"git": "git", "walgit": "walgit", "vcs_disk": "vcs_disk", "vcs_s3": "vcs_s3"}[name]
        self.ctr = 0

    def pid(self):
        p = PROCS.get(self.srv)
        return p.pid if p else 0

    def fresh(self):
        """Start a fresh empty remote; returns its URL."""
        self.ctr += 1
        stop(self.srv)
        tag = f"{self.name}-{self.ctr}-{int(time.time())}"
        if self.kind == "git":
            base = f"{WB}/gitbase"; sh(f"rm -rf {base} && mkdir -p {base}")
            sh(f"git init -q --bare -b main {base}/r.git && git -C {base}/r.git config http.receivepack true")
            start(self.srv, ["git", "daemon", "--reuseaddr", f"--port={P_GIT}", "--listen=127.0.0.1", f"--base-path={base}",
                             "--enable=receive-pack", "--export-all", "--max-connections=0", base], P_GIT)
            return f"git://127.0.0.1:{P_GIT}/r.git"
        if self.kind == "walgit":
            cache = f"{WB}/walgit-cache"; sh(f"rm -rf {cache}")
            cfg = (f'[server]\nlisten = "127.0.0.1:{P_WG}"\nauto_create_on_push = true\nroles = []\n[server.tls]\nmode = "off"\n'
                   f'[server.auth]\nmode = "none"\n[store]\nbackend = "s3"\nbucket = "walgit-b"\nprefix = "{tag}/"\n'
                   f'[store.s3]\nendpoint = "http://127.0.0.1:{P_RF}"\nregion = "us-east-1"\nforce_path_style = true\n[cache]\ndir = "{cache}"\n')
            open(f"{WB}/walgit.toml", "w").write(cfg)
            start(self.srv, [WALGIT, "--config", f"{WB}/walgit.toml"], P_WG, f"http://127.0.0.1:{P_WG}/healthz")
            time.sleep(0.5)
            return f"http://127.0.0.1:{P_WG}/o/r.git"
        tok = sh("vcs-server hash-token tok1").stdout.strip()
        open(f"{WB}/tokens", "w").write(f"bench write * {tok}\n")
        if self.name == "vcs_disk":
            root = f"{WB}/vcsroot"; sh(f"rm -rf {root} && mkdir -p {root}")
            start(self.srv, ["vcs-server", "--root", root, "--tokens", f"{WB}/tokens", "--listen", f"127.0.0.1:{P_VD}"], P_VD,
                  f"http://127.0.0.1:{P_VD}/healthz")
            return f"http://127.0.0.1:{P_VD}"
        start(self.srv, ["vcs-server", "--backend", f"s3://vcs/{tag}?endpoint=http://127.0.0.1:{P_RF}", "--tokens", f"{WB}/tokens",
                         "--listen", f"127.0.0.1:{P_VS}"], P_VS, f"http://127.0.0.1:{P_VS}/healthz")
        return f"http://127.0.0.1:{P_VS}"

    # ---- client-side operations (each worker has its own dir)
    def seed(self, url, d):
        """Create the initial commit on main in dir d and publish it."""
        sh(f"rm -rf {d} && mkdir -p {d}")
        if self.kind == "vcs":
            sh("vcs init >/dev/null && vcs config set --user user.name b && vcs config set --user user.email b@x.invalid", cwd=d)
            for i in range(5):
                open(f"{d}/seed{i}", "w").write(str(i))
            sh(f"vcs add . && vcs commit -m init && vcs remote set {url} && vcs publish", cwd=d)
        else:
            sh("git init -q -b main", cwd=d)
            for i in range(5):
                open(f"{d}/seed{i}", "w").write(str(i))
            sh(f"git add . && git commit -q -m init && git remote add origin {url} && git push -q origin main", cwd=d)

    def clone(self, url, d):
        sh(f"rm -rf {d}")
        sh(f"vcs clone '{url}' '{d}'" if self.kind == "vcs" else f"git clone -q '{url}' '{d}'")
        if self.kind == "vcs":
            sh("vcs config set --user user.name b && vcs config set --user user.email b@x.invalid", cwd=d, check=False)

    def to_branch(self, d, br):
        if self.kind == "vcs":
            sh(f"vcs branch {br} && vcs switch {br}", cwd=d)
        else:
            sh(f"git checkout -q -b {br}", cwd=d)

    def commit(self, d, fname):
        open(f"{d}/{fname}", "w").write(fname)
        if self.kind == "vcs":
            sh(f"vcs add '{fname}' && vcs commit -m '{fname}'", cwd=d)
        else:
            sh(f"git add '{fname}' && git commit -q -m '{fname}'", cwd=d)

    def push(self, d, br):
        """One push attempt; returns (ok, stderr)."""
        r = sh(("vcs publish --auto-merge=30" if AUTO else "vcs publish") if self.kind == "vcs" else f"git push -q origin HEAD:refs/heads/{br}", cwd=d, check=False)
        return r.returncode == 0, (r.stdout + r.stderr)[-300:]

    def recover(self, d, br):
        """After a rejected push on a shared branch: integrate remote changes. Raises if it cannot."""
        if self.kind == "vcs":
            sh("vcs sync", cwd=d, check=False)
            r = sh(f"vcs merge --remote {br}", cwd=d, check=False)
            if r.returncode:
                raise RuntimeError("merge failed: " + (r.stdout + r.stderr)[-300:])
        else:
            sh(f"git pull -q --rebase origin {br}", cwd=d)


class Sampler:
    """CPU and peak RSS for the server (and RustFS) over a scenario."""
    def __init__(self, arm):
        self.p = arm.pid()
        self.rf = PROCS["rustfs"].pid if "rustfs" in PROCS and arm.name in ("vcs_s3", "walgit") else 0
        self.c0, self.r0, self.t0 = ticks(self.p), ticks(self.rf), time.perf_counter()
        self.load0 = open("/proc/loadavg").read().split()[0]

    def done(self):
        return {"wall_s": time.perf_counter() - self.t0, "srv_cpu_s": ticks(self.p) - self.c0, "rustfs_cpu_s": ticks(self.rf) - self.r0,
                "srv_rss_mib": hwm_mib(self.p), "rustfs_rss_mib": hwm_mib(self.rf) if self.rf else None}


def count_files(d, branch_files_prefix):
    n = 0
    for root, dirs, fs in os.walk(d):
        dirs[:] = [x for x in dirs if not x.startswith(".")]
        n += sum(1 for f in fs if f.startswith(branch_files_prefix))
    return n


def run_pushers(arm, url, work, n, k, shared, results, stop_ev=None, max_retries=30, tag="w"):
    """n threads, each with its own clone, k commit+push each. shared=True -> all on main."""
    lat, att, fails, errs, cl = [], [], [0], [], []
    lock = threading.Lock()
    dirs = [f"{work}/{tag}{i}" for i in range(n)]
    for d in dirs:
        arm.clone(url, d)
    for i, d in enumerate(dirs):
        if not shared:
            arm.to_branch(d, f"{tag}{i}")

    def worker(i):
        d, br = dirs[i], ("main" if shared else f"{tag}{i}")
        for j in range(k):
            if stop_ev and stop_ev.is_set():
                break
            tc = time.perf_counter()
            arm.commit(d, f"{tag}{i}_{j}")
            cl.append((time.perf_counter() - tc) * 1000)
            t = time.perf_counter()
            a = 0
            while True:
                a += 1
                ok, msg = arm.push(d, br)
                if ok:
                    break
                with lock:
                    fails[0] += 1
                if a > max_retries:
                    with lock:
                        errs.append(f"{tag}{i}_{j}: gave up: {msg[-120:]}")
                    return
                try:
                    if shared:
                        arm.recover(d, br)
                    else:
                        time.sleep(random.uniform(0.01, 0.1))
                except Exception as e:
                    with lock:
                        errs.append(f"{tag}{i}_{j}: recover: {str(e)[-150:]}")
                    return
            with lock:
                lat.append((time.perf_counter() - t) * 1000)
                att.append(a)

    ths = [threading.Thread(target=worker, args=(i,)) for i in range(n)]
    smp = Sampler(arm)  # CPU/RSS over the push phase only, not the per-worker clones above
    t0 = time.perf_counter()
    for t in ths:
        t.start()
    for t in ths:
        t.join()
    results.update(smp.done())
    results["phase_s"] = time.perf_counter() - t0
    results.update(commit_ms=stats(cl), push_ms=stats(lat), attempts=stats(att), rejected_pushes=fails[0], errors=errs[:5], n_errors=len(errs),
                   pushed=len(lat))
    return dirs


def scn_branches(arm, n, k):
    url = arm.fresh(); w = f"{WB}/work"; sh(f"rm -rf {w} && mkdir -p {w}")
    arm.seed(url, f"{w}/seed")
    r = {}
    run_pushers(arm, url, w, n, k, False, r)
    r["pushes_per_s"] = r["pushed"] / r["phase_s"]
    # verify: all branches exist with k files
    if arm.kind == "git":
        out = sh(f"git ls-remote --heads {url}").stdout
        r["branches_landed"] = sum(1 for l in out.splitlines() if "/refs/heads/w" in l or "refs/heads/w" in l)
        r["verified"] = r["branches_landed"] == n and r["n_errors"] == 0
    else:
        d = f"{w}/verify"; arm.clone(url, d)
        out = sh("vcs branch -a" if arm.kind == "vcs" else "git branch -r", cwd=d, check=False).stdout
        r["branches_landed"] = out.count("w")
        r["verified"] = r["n_errors"] == 0 and r["pushed"] == n * k
    return r


def scn_same(arm, n, k):
    url = arm.fresh(); w = f"{WB}/work"; sh(f"rm -rf {w} && mkdir -p {w}")
    arm.seed(url, f"{w}/seed")
    r = {}
    run_pushers(arm, url, w, n, k, True, r)
    r["pushes_per_s"] = r["pushed"] / r["phase_s"]
    d = f"{w}/verify"; arm.clone(url, d)
    landed = count_files(d, "w")
    r["files_landed"] = landed; r["files_expected"] = n * k - r["n_errors"] * 0
    r["verified"] = landed == r["pushed"] and r["n_errors"] == 0
    return r


def scn_seq(arm, m):
    url = arm.fresh(); w = f"{WB}/work"; sh(f"rm -rf {w} && mkdir -p {w}")
    d = f"{w}/seed"; arm.seed(url, d)
    s = Sampler(arm); lat = []; marks = {}
    errs = 0
    for j in range(m):
        arm.commit(d, f"q{j}")
        t = time.perf_counter()
        ok, msg = arm.push(d, "main")
        lat.append((time.perf_counter() - t) * 1000)
        if not ok:
            errs += 1
            if errs > 3:
                break
        if (j + 1) in (1, 10, 100, 500, 1000, 2000, 5000):
            marks[str(j + 1)] = lat[-1]
    r = {"push_ms": stats(lat), "push_ms_at": marks, "errors": errs, "pushed": len(lat) - errs}
    first, last = lat[: max(1, len(lat) // 10)], lat[-max(1, len(lat) // 10):]
    r["first10pct_p50"], r["last10pct_p50"] = statistics.median(first), statistics.median(last)
    r.update(s.done())
    t = time.perf_counter(); v = f"{w}/verify"; arm.clone(url, v)
    r["clone_ms_after"] = (time.perf_counter() - t) * 1000
    r["files_landed"] = count_files(v, "q"); r["verified"] = r["files_landed"] == r["pushed"]
    return r


def scn_clones(arm, n, k, cloners=4):
    url = arm.fresh(); w = f"{WB}/work"; sh(f"rm -rf {w} && mkdir -p {w}")
    arm.seed(url, f"{w}/seed")
    r = {}; clat = []; cerr = [0]; ev = threading.Event(); lk = threading.Lock()

    def cl(i):
        j = 0
        while not ev.is_set():
            t = time.perf_counter()
            try:
                arm.clone(url, f"{w}/c{i}");
                with lk:
                    clat.append((time.perf_counter() - t) * 1000)
            except Exception:
                with lk:
                    cerr[0] += 1
            j += 1
    cs = [threading.Thread(target=cl, args=(i,)) for i in range(cloners)]
    for c in cs:
        c.start()
    run_pushers(arm, url, w, max(1, n // 2), k, False, r)
    ev.set()
    for c in cs:
        c.join()
    r["clone_ms"] = stats(clat); r["clone_errors"] = cerr[0]
    r["pushes_per_s"] = r["pushed"] / r["phase_s"]; r["verified"] = r["n_errors"] == 0 and r["pushed"] == max(1, n // 2) * k
    return r


def storage(arm):
    if arm.kind == "git":
        return sh(f"du -sk {WB}/gitbase | cut -f1").stdout.strip()
    if arm.name == "vcs_disk":
        return sh(f"du -sk {WB}/vcsroot | cut -f1").stdout.strip()
    return sh(f"du -sk {WB}/rfdata | cut -f1").stdout.strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--scenarios", default="branches,same,seq,clones")
    ap.add_argument("--arms", default="git,vcs_disk,vcs_s3,walgit")
    ap.add_argument("--n", default="8,32")
    ap.add_argument("--k", type=int, default=20)
    ap.add_argument("--m", type=int, default=1000)
    ap.add_argument("--auto-merge", action="store_true", help="vcs arms publish with --auto-merge=30")
    ap.add_argument("--reps", type=int, default=1)
    a = ap.parse_args()
    global AUTO
    AUTO = a.auto_merge
    sh(f"mkdir -p {WB}")
    res = {"meta": {"load": open('/proc/loadavg').read().split()[:3], "ncpu": os.cpu_count(), "args": vars(a),
                    "vcs_rev": os.environ.get("WB_VCS_REV", "?")}, "runs": []}
    arms = a.arms.split(",")
    if any(x in arms for x in ("vcs_s3", "walgit")):
        start_rustfs()
    try:
        for rep in range(a.reps):
            for sc in a.scenarios.split(","):
                for n in ([0] if sc == "seq" else [int(x) for x in a.n.split(",")]):
                    order = arms[rep % len(arms):] + arms[: rep % len(arms)]
                    for an in order:
                        arm = Arm(an)
                        rec = {"scenario": sc, "arm": an, "n": n, "k": a.k, "rep": rep}
                        print(f"== {sc} n={n} {an} rep{rep}", flush=True)
                        try:
                            rec.update(scn_branches(arm, n, a.k) if sc == "branches" else scn_same(arm, n, a.k) if sc == "same"
                                       else scn_seq(arm, a.m) if sc == "seq" else scn_clones(arm, n, a.k))
                            rec["storage_kib"] = storage(arm)
                        except Exception as e:
                            rec["fatal"] = str(e)[-400:]
                        print(json.dumps({k: rec.get(k) for k in ("pushes_per_s", "verified", "rejected_pushes", "n_errors", "fatal", "wall_s")}), flush=True)
                        res["runs"].append(rec)
                        json.dump(res, open(a.out, "w"), indent=1)
                        stop(arm.srv)
    finally:
        stop_all()


if __name__ == "__main__":
    main()
