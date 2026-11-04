#!/usr/bin/env python3
"""Interleaved A/B CPU-time harness for vcs builds (macOS and Linux, no root, no network).

  ab.py setup --work DIR --datagen BIN --vcs BIN [--n 100000]
  ab.py run   --work DIR --variant NAME=BINDIR [--variant ...] [--scen a,b,c] [--runs 15] [--out FILE.json]

BINDIR[@VAR=VAL,...] holds `vcs` and (for the server scenario) `vcs-server`. Every round runs each variant once per scenario, in
shuffled order, so all variants see the same machine load. The metric is CPU time (user+sys from wait4 rusage;
for the server scenario the server's own CPU is added and reported separately); wall is recorded but is inflated on a
loaded host. Prints the median per variant, the min, and the relative spread (MAD/median) as the noise estimate.
Scenarios: floor status status_dirty add_noop diff log clone gc server (`ab.py list`).
"""
import argparse, json, os, random, shutil, signal, socket, statistics, subprocess, sys, time

ENV = dict(os.environ, VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", VCS_NO_HOOKS="1", VCS_NO_AUTOWATCH="1", VCS_NO_FSMONITOR="1")
SCEN = ["floor", "status", "status_dirty", "add_noop", "diff", "log", "clone", "gc", "server"]


def timed(argv, cwd, env=ENV):
    t = time.perf_counter()
    p = subprocess.Popen(argv, cwd=cwd, env=dict(env, PWD=cwd), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, st, ru = os.wait4(p.pid, 0)
    return (time.perf_counter() - t) * 1000, (ru.ru_utime + ru.ru_stime) * 1000, os.waitstatus_to_exitcode(st)


def sh(cmd, cwd, env=ENV):
    return subprocess.run(["bash", "-c", cmd], cwd=cwd, env=dict(env, PWD=cwd), stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode


def files(root):
    out = []
    for d, ds, names in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".vcs")]
        out += [os.path.join(d, f) for f in names]
    return sorted(out)


def setup(a):
    work = os.path.abspath(a.work)
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    r = os.path.join(work, "repo")
    subprocess.run([a.datagen, "flat", str(a.n), r], check=True, stdout=subprocess.DEVNULL)
    V = a.vcs
    for c in (f"{V} init", f"{V} config set user.name b", f"{V} config set user.email b@x.invalid", f"{V} add .",
              f"{V} commit -m c0", f"{V} remote set {work}/remote", f"{V} publish"):
        if sh(c, r) != 0:
            sys.exit(f"setup step failed: {c}")
    for k in range(1, 6):  # a little history
        fs = files(r)
        open(fs[k * 997 % len(fs)], "w").write(f"history {k}\n")
        sh(f"{V} add . && {V} commit -m h{k}", r)
    json.dump({"n": a.n}, open(os.path.join(work, "state.json"), "w"))
    print("setup done", file=sys.stderr)


def dirty(repo, tag):
    fs = files(repo)[::100]
    for i, p in enumerate(fs):
        open(p, "w").write(f"modified {tag} {i}\n" * 8)
    return fs


class Ctx:
    def __init__(self, a):
        self.work = os.path.abspath(a.work)
        self.repo = os.path.join(self.work, "repo")
        self.dirty_done = False

    def restore(self):
        sh(f"{self.vcs} restore .", self.repo)


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def scenario(sid, ctx, vcs, bindir):
    """Return {metric: (wall, cpu)} for one timed execution of scenario `sid`."""
    repo, work = ctx.repo, ctx.work
    if sid == "floor":
        w, c, rc = timed([vcs, "--version"], repo)
    elif sid == "status":
        w, c, rc = timed([vcs, "status"], repo)
    elif sid == "status_dirty":
        w, c, rc = timed([vcs, "status"], repo)
    elif sid == "add_noop":
        w, c, rc = timed([vcs, "add", "."], repo)
    elif sid == "diff":
        w, c, rc = timed([vcs, "diff"], repo)
    elif sid == "log":
        w, c, rc = timed([vcs, "log", "-n", "1000000", "--oneline"], repo)
    elif sid == "clone":
        d = os.path.join(work, "dst")
        shutil.rmtree(d, ignore_errors=True)
        w, c, rc = timed([vcs, "clone", os.path.join(work, "remote"), d], work)
        shutil.rmtree(d, ignore_errors=True)
    elif sid == "gc":
        d = os.path.join(work, "gcrepo")
        shutil.rmtree(d, ignore_errors=True)
        sh(f"{vcs} clone {work}/remote {d}", work)
        w, c, rc = timed([vcs, "gc"], d)
        shutil.rmtree(d, ignore_errors=True)
    elif sid == "server":
        root = os.path.join(work, "srvroot")
        shutil.rmtree(root, ignore_errors=True)
        os.makedirs(root)
        tok = "tok-ab"
        h = subprocess.run([os.path.join(bindir, "vcs-server"), "hash-token", tok], capture_output=True, text=True).stdout.strip()
        tf = os.path.join(work, "tokens")
        open(tf, "w").write(f"ab admin * {h}\n")
        port = free_port()
        sp = subprocess.Popen([os.path.join(bindir, "vcs-server"), "--root", root, "--tokens", tf, "--listen", f"127.0.0.1:{port}"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), 0.1).close()
                break
            except OSError:
                time.sleep(0.05)
        env = dict(ENV, VCS_TOKEN=tok, VCS_PRINCIPAL="ab")
        url = f"http://127.0.0.1:{port}"
        # publish an unpublished copy of the 100k-file repo to the server (the copy and the publish are untimed:
        # server CPU is what is measured), then clone it back twice
        pristine, src = os.path.join(work, "srvpristine"), os.path.join(work, "srvsrc")
        if not os.path.isdir(pristine):
            sh(f"cp -c -R {repo} {pristine}; rm -rf {pristine}/.vcs/remote-refs {pristine}/.vcs/remote", work)
        shutil.rmtree(src, ignore_errors=True)
        sh(f"cp -c -R {pristine} {src}", work)
        if sh(f"{vcs} remote set {url} && {vcs} publish", src, env) != 0:
            print("WARN server publish failed", file=sys.stderr)
        shutil.rmtree(src, ignore_errors=True)
        d = os.path.join(work, "dst")
        shutil.rmtree(d, ignore_errors=True)
        w, c, rc = timed([vcs, "clone", url, d], work, env)
        shutil.rmtree(d, ignore_errors=True)
        w2, c2, rc2 = timed([vcs, "clone", url, d], work, env)
        w, c, rc = w + w2, c + c2, rc or rc2
        sp.send_signal(signal.SIGTERM)
        _, st, ru = os.wait4(sp.pid, 0)
        shutil.rmtree(d, ignore_errors=True)
        shutil.rmtree(root, ignore_errors=True)
        # the server CPU covers push + clone serving; the client CPU is the clone
        return {"server_cpu": (0.0, (ru.ru_utime + ru.ru_stime) * 1000, 0), "client_clone": (w, c, rc)}
    else:
        sys.exit("unknown scenario " + sid)
    return {sid: (w, c, rc)}


def run(a):
    ctx = Ctx(a)
    # --variant NAME=BINDIR[@VAR=VAL[,VAR=VAL]]: the same binary with extra environment is a variant too
    base_env, extra = dict(ENV), {}
    variants = []
    for v in a.variant:
        n, d = v.split("=", 1)
        d, _, e = d.partition("@")
        variants.append((n, d))
        extra[n] = dict(kv.split("=", 1) for kv in e.split(",") if kv)
    scen = a.scen.split(",") if a.scen else SCEN
    res = {}  # (scen, metric, variant) -> [cpu]
    load0 = os.getloadavg()[0]
    for rnd in range(a.runs + 1):  # round 0 is a warmup and is dropped
        order = [(s, n, d) for s in scen for n, d in variants]
        random.Random(rnd).shuffle(order)
        for s, n, d in order:
            vcs = os.path.join(d, "vcs")
            ctx.vcs = vcs
            ENV.clear(); ENV.update(base_env); ENV.update(extra[n])
            if s == "status_dirty" and not ctx.dirty_done:
                dirty(ctx.repo, "ab")
                ctx.dirty_done = True
            if s != "status_dirty" and ctx.dirty_done:
                sh(f"{vcs} restore --staged . ; {vcs} restore .", ctx.repo)
                ctx.dirty_done = False
            for m, (w, c, rc) in scenario(s, ctx, vcs, d).items():
                if rc != 0:
                    print(f"WARN {s}/{n} exit {rc}", file=sys.stderr)
                if rnd:
                    res.setdefault((s, m, n), []).append((c, w))
    if ctx.dirty_done:
        sh(f"{ctx.vcs} restore --staged . ; {ctx.vcs} restore .", ctx.repo)
    print(f"load avg before {load0:.1f} after {os.getloadavg()[0]:.1f}; {a.runs} interleaved runs; CPU ms median (min) [MAD%]")
    out = {}
    names = [n for n, _ in variants]
    for (s, m) in sorted({(s, m) for s, m, _ in res}, key=lambda k: (scen.index(k[0]) if k[0] in scen else 99, k[1])):
        row, base = [], None
        for n in names:
            xs = [c for c, _ in res[(s, m, n)]]
            med = statistics.median(xs)
            mad = statistics.median([abs(x - med) for x in xs]) / med * 100 if med else 0
            row.append(f"{n} {med:.1f} ({min(xs):.1f}) [{mad:.0f}%]" + ("" if base is None else f" {med / base - 1:+.1%}"))
            base = med if base is None else base
            out[f"{s}/{m}/{n}"] = xs
        print(f"{m:14s} " + "   ".join(row))
    if a.out:
        json.dump(out, open(a.out, "w"))


def main():
    ap = argparse.ArgumentParser()
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("setup")
    s.add_argument("--work", required=True); s.add_argument("--datagen", required=True); s.add_argument("--vcs", required=True)
    s.add_argument("--n", type=int, default=100000)
    r = sp.add_parser("run")
    r.add_argument("--work", required=True); r.add_argument("--variant", action="append", required=True)
    r.add_argument("--scen"); r.add_argument("--runs", type=int, default=15); r.add_argument("--out")
    sp.add_parser("list")
    a = ap.parse_args()
    {"setup": setup, "run": run, "list": lambda a: print("\n".join(SCEN))}[a.cmd](a)


if __name__ == "__main__":
    main()
