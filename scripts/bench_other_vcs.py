#!/usr/bin/env python3
"""Benchmark allgit `vcs` against other version control systems on the bench_scale.sh datasets.

usage: bench_other_vcs.py <flat|deep|binary> [n=100000] [runs=5] [tools=comma list]
env:   OV_ROOT (default ~/rt/ov): holds bin/vcs, bin/datagen, tools/, mamba/envs/*; OV_WORK: scratch parent.

Runs cell by cell and, inside each cell, round-robin over the tools (run 1 of every tool, then run 2, ...), so
slow drifts in machine load hit every tool alike. Every run records wall ms and child user+sys CPU ms
(wait4 rusage of the timed command) and load1 before and after. Prep is untimed. Output: one JSON line per
run on stdout (and a summary table at the end). Slow cells: if the first run of a cell takes > SLOW_S the
remaining runs are cut to 3 (the cell is flagged), > VERYSLOW_S to 1.
"""
import json, os, re, shutil, subprocess, sys, time, statistics

LAYOUT = sys.argv[1] if len(sys.argv) > 1 else "flat"
N = int(sys.argv[2]) if len(sys.argv) > 2 else 100000
RUNS = int(sys.argv[3]) if len(sys.argv) > 3 else 5
ROOT = os.environ.get("OV_ROOT", os.path.expanduser("~/rt/ov"))
PARENT = os.environ.get("OV_WORK", os.path.join(ROOT, "work"))
TAG = os.environ.get("OV_TAG", "")  # suffix for the results file name
SLOW_S, VERYSLOW_S, CMD_TIMEOUT = 90, 600, 2400
E = f"{ROOT}/mamba/envs"
VCS = f"{ROOT}/bin/vcs"
DATAGEN = f"{ROOT}/bin/datagen"
os.makedirs(PARENT, exist_ok=True)
WORK = os.path.join(PARENT, f"{LAYOUT}-{os.getpid()}")
os.makedirs(WORK)

ENV = dict(os.environ)
ENV.update(
    GIT_AUTHOR_NAME="b", GIT_AUTHOR_EMAIL="b@x.invalid", GIT_COMMITTER_NAME="b", GIT_COMMITTER_EMAIL="b@x.invalid",
    GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null", HGUSER="b <b@x.invalid>", HGRCPATH="",
    BRZ_EMAIL="b <b@x.invalid>", FOSSIL_HOME=f"{WORK}/fossilhome", USER="b", LOGNAME="b",
    VCS_AUTHOR_NAME="b", VCS_AUTHOR_EMAIL="b@x.invalid", LC_ALL="C.UTF-8",
)
# wrap/ holds tiny sh wrappers (hg, brz, svn, svnadmin) that put their own conda env first on PATH
ENV["PATH"] = ":".join([f"{ROOT}/wrap", f"{ROOT}/tools/fossil", f"{ROOT}/tools/pijul/bin", ENV["PATH"]])
os.makedirs(ENV["FOSSIL_HOME"], exist_ok=True)
ENV["HOME"] = f"{WORK}/home"
ENV["PIJUL_CONFIG_DIR"] = f"{WORK}/home/pijulcfg"
os.makedirs(ENV["HOME"], exist_ok=True)
AGENT_PID = None


def setup_pijul():
    """pijul refuses to record without an identity, and identities need an ssh-agent holding a key."""
    global AGENT_PID
    out = subprocess.run(["ssh-agent", "-s"], capture_output=True, text=True).stdout
    ENV["SSH_AUTH_SOCK"] = re.search(r"SSH_AUTH_SOCK=([^;]+);", out).group(1)
    AGENT_PID = int(re.search(r"SSH_AGENT_PID=(\d+);", out).group(1))
    subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", f"{ENV['HOME']}/k"], check=True, env=ENV)
    subprocess.run(["ssh-add", f"{ENV['HOME']}/k"], check=True, env=ENV, capture_output=True)
    subprocess.run(["pijul", "identity", "new", "b", "--display-name", "b", "--email", "b@x.invalid", "--no-link"],
                   check=True, env=ENV, stdin=subprocess.DEVNULL, capture_output=True)


SKIP_DIRS = {".git", ".vcs", ".hg", ".svn", ".bzr", ".pijul"}


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def sh(cmd, cwd, timeout=CMD_TIMEOUT):
    """Run, returns (wall_ms, cpu_ms, rc, stderr_tail)."""
    err = open(f"{WORK}/err.tmp", "wb")
    t = time.perf_counter()
    p = subprocess.Popen(["bash", "-c", cmd], cwd=cwd, env=ENV, stdout=subprocess.DEVNULL, stderr=err, stdin=subprocess.DEVNULL, start_new_session=True)
    try:
        _, st, ru = os.wait4(p.pid, 0) if timeout is None else wait_timeout(p, timeout)
    except TimeoutError:
        return None, None, -9, "timeout"
    wall = (time.perf_counter() - t) * 1000
    err.close()
    rc = os.waitstatus_to_exitcode(st)
    tail = open(f"{WORK}/err.tmp", "rb").read()[-300:].decode("utf-8", "replace")
    return wall, (ru.ru_utime + ru.ru_stime) * 1000, rc, tail


def wait_timeout(p, timeout):
    end = time.time() + timeout
    while True:
        r = os.wait4(p.pid, os.WNOHANG)
        if r[0] != 0:
            return r
        if time.time() > end:
            os.killpg(p.pid, 9)
            os.wait4(p.pid, 0)
            raise TimeoutError
        time.sleep(0.0005)  # 0.5 ms poll: a 20 ms poll once quantised every short cell to multiples of 20 ms


def files_sorted(root):
    fs = []
    for d, ds, files in os.walk(root):
        ds[:] = [x for x in ds if x not in SKIP_DIRS]
        fs += [os.path.join(d, f) for f in files if f not in ("_FOSSIL_", ".fslckout")]
    fs.sort()
    return fs


def modify_1pct(root, tag):
    for i, p in enumerate(files_sorted(root)[::100]):
        with open(p, "wb") as fh:
            fh.write((f"modified {tag} {i}\n").encode() * 8)


def spath(t, s):
    return f"{t.w}/{s}" if s.startswith(".") else f"{WORK}/{s}"


def du_kb(*paths):
    t = 0
    for p in paths:
        r = subprocess.run(["du", "-sk", p], capture_output=True, text=True)
        t += int(r.stdout.split()[0]) if r.stdout else 0
    return t


class Tool:
    name = ""
    full = True  # participates in every cell
    # command strings run in the working copy (cwd = self.w)
    init = add = commit = status = log = addall = reset = None
    co_prep = co_time = None
    stores = ()  # paths (relative to WORK) counted as the repository store
    extra_overhead = ()  # paths counted as "working copy metadata" (svn .svn)
    side = None  # hook: called after commit 1 (e.g. create a branch or remember a hash)
    clone = None
    version_cmd = ""

    def __init__(self):
        self.w = f"{WORK}/{self.name}"
        self.notes = []

    def v(self):
        r = subprocess.run(["bash", "-c", self.version_cmd], capture_output=True, text=True, env=ENV)
        return (r.stdout or r.stderr).strip().splitlines()[0] if (r.stdout or r.stderr) else "?"


class Git(Tool):
    name = "git"
    version_cmd = "git --version"
    init = "git init -q -b main"
    add = "git add ."
    commit = "git commit -q -m c"
    status = "git status --porcelain"
    log = "git log --oneline -n 50"
    addall = "git add -A"
    reset = "rm -rf .git"
    co_prep = "git checkout -q side"
    co_time = "git checkout -q main"
    stores = (".git",)

    def side_hook(self):
        sh("git branch side", self.w)
        sh("git config uploadpack.allowFilter true; git config uploadpack.allowAnySHA1InWant true", self.w)

    clone = "git clone -q file://{w}/.git {dst}"


class GitFsm(Git):
    name = "git-fsmonitor"
    version_cmd = "git --version"
    init = "git init -q -b main && git config core.fsmonitor true && git config core.untrackedCache true"
    reset = "git fsmonitor--daemon stop 2>/dev/null; rm -rf .git"
    clone = None
    # status: the daemon is primed by one untimed `git status` in the cell prep (see PRIME)


class Hg(Tool):
    name = "hg"
    version_cmd = "hg --version"
    init = "hg init"
    add = "hg add -q ."
    commit = "hg commit -q -m c"
    status = "hg status"
    log = "hg log -l 50"
    addall = "true"  # only modified tracked files in the history commits: commit finds them itself
    reset = "rm -rf .hg"
    co_prep = "hg update -q -r 0"
    co_time = "hg update -q -r tip"
    stores = (".hg",)
    clone = "hg clone -q --pull {w} {dst}"


class Svn(Tool):
    name = "svn"
    version_cmd = "svn --version"
    # repo created beside the working copy; wc checked out over the generated files
    init = "svnadmin create ../svn.repo && svn checkout -q --force file://{repo} ."
    add = "svn add -q --force ."
    commit = "svn commit -q -m c"
    status = "svn status"
    log = "svn log -l 50"
    addall = "true"  # only modified tracked files in the history commits: commit finds them itself
    reset = "rm -rf .svn ../svn.repo"
    co_prep = "svn update -q -r 1"
    co_time = "svn update -q"
    stores = ("svn.repo",)
    extra_overhead = ("svn/.svn",)
    clone = "svn checkout -q file://{repo} {dst}"

    def __init__(self):
        super().__init__()
        self.init = self.init.replace("{repo}", f"{WORK}/svn.repo")
        self.clone = self.clone.replace("{repo}", f"{WORK}/svn.repo")


class Fossil(Tool):
    name = "fossil"
    version_cmd = "fossil version"
    init = "fossil init -q ../fossil.repo >/dev/null && fossil open --force ../fossil.repo >/dev/null"
    add = "fossil add -q . 2>/dev/null || fossil add . >/dev/null"
    commit = "fossil commit -m c --no-warnings >/dev/null"
    status = "fossil changes"
    log = "fossil timeline -n 50"
    addall = "true"  # only modified tracked files in the history commits: commit finds them itself
    reset = "fossil close --force >/dev/null 2>&1; rm -f _FOSSIL_ ../fossil.repo"
    stores = ("fossil.repo",)
    co_time = "fossil update trunk >/dev/null"

    def side_hook(self):
        r = subprocess.run(["bash", "-c", "fossil timeline -n 5 -t ci"], cwd=self.w, env=ENV, capture_output=True, text=True)
        ids = re.findall(r"\[([0-9a-f]{10,})\]", r.stdout)
        self.first = ids[-1] if ids else None
        self.co_prep = f"fossil update {self.first} >/dev/null"

    clone = "fossil clone -A b {w}/../fossil.repo {dst}.repo >/dev/null && mkdir {dst} && cd {dst} && fossil open ../{base}.repo >/dev/null"


class Brz(Tool):
    name = "bzr"
    version_cmd = "brz --version"
    init = "brz init -q"
    add = "brz add -q ."
    commit = "brz commit -q -m c"
    status = "brz status"
    log = "brz log -l 50 --line"
    addall = "true"  # only modified tracked files in the history commits: commit finds them itself
    reset = "rm -rf .bzr"
    co_prep = "brz revert -q -r 1"
    co_time = "brz revert -q -r -1"
    stores = (".bzr",)
    clone = "brz branch -q {w} {dst}"


class Pijul(Tool):
    name = "pijul"
    version_cmd = "pijul --version"
    init = "pijul init -q 2>/dev/null || pijul init"
    add = "pijul add -r ."
    commit = "pijul record -a -m c"
    status = "pijul diff --short"
    log = "pijul log --limit 50"
    addall = "true"  # only modified tracked files in the history commits: commit finds them itself
    reset = "rm -rf .pijul"
    stores = (".pijul",)
    # no cheap equivalent of "switch to an older state" measured here (channel fork + reset): see doc
    clone = "pijul clone {w} {dst}"


class Vcs(Tool):
    name = "vcs"
    version_cmd = f"{VCS} --version"
    init = f"{VCS} init"
    add = f"{VCS} add ."
    commit = f"{VCS} commit -m c"
    status = f"{VCS} status"
    log = f"{VCS} log -n 50"
    addall = f"{VCS} add ."
    reset = "rm -rf .vcs"
    co_prep = f"{VCS} switch side"
    co_time = f"{VCS} switch main"
    stores = (".vcs",)

    def side_hook(self):
        sh(f"{VCS} branch side", self.w)

    clone = VCS + " clone {remote} {dst}"


class CloneVariant:
    """Clone-only variants cloning from the plain git tool's repository."""
    def __init__(self, name, cmd):
        self.name, self.cmd = name, cmd


ALL = {c.name: c for c in (Git, GitFsm, Hg, Svn, Fossil, Brz, Pijul, Vcs)}
CLONE_VARIANTS = [
    CloneVariant("git-partial-blobnone", "git clone -q --filter=blob:none file://{gw}/.git {dst}"),
    CloneVariant("git-partial-blobnone-nocheckout", "git clone -q --no-checkout --filter=blob:none file://{gw}/.git {dst}"),
    CloneVariant("git-shallow-depth1", "git clone -q --depth 1 file://{gw}/.git {dst}"),
    CloneVariant("git-clone-hardlink-path", "git clone -q {gw} {dst}"),
]

want = (sys.argv[4].split(",") if len(sys.argv) > 4 else list(ALL))
tools = [ALL[n]() for n in want]
results = []


def emit(cell, tool, wall, cpu, rc, l0, l1, err=""):
    rec = dict(layout=LAYOUT, cell=cell, tool=tool, wall_ms=wall, cpu_ms=cpu, rc=rc, load0=l0, load1=l1)
    if rc != 0:
        rec["err"] = err
    results.append(rec)
    print(json.dumps(rec), flush=True)


CELLS = os.environ.get("OV_CELLS")  # comma list: run only these cells (setup still runs)


def cell(name, plan, prep_fn=None, prime=None, only=None):
    """plan: tool -> (prep_cmd or None, timed_cmd, cwd). Round-robin over tools."""
    if CELLS and name not in CELLS.split(","):
        return
    ts = [t for t in plan]
    runs = {t: RUNS for t in ts}
    done = {t: 0 for t in ts}
    r = 0
    while any(done[t] < runs[t] for t in ts):
        for t in ts:
            if done[t] >= runs[t]:
                continue
            prep, cmd, cwd = plan[t]
            if prep:
                sh(prep, cwd, timeout=CMD_TIMEOUT)
            if prime and t in prime:
                sh(prime[t], cwd)
            l0 = load1()
            wall, cpu, rc, err = sh(cmd, cwd)
            emit(name, t, wall, cpu, rc, l0, load1(), err)
            done[t] += 1
            if wall is not None and done[t] == 1:
                s = wall / 1000
                if s > VERYSLOW_S:
                    runs[t] = 1
                elif s > SLOW_S:
                    runs[t] = min(runs[t], 3)
        r += 1


def gen(t):
    os.makedirs(t.w, exist_ok=True)
    subprocess.run([DATAGEN, LAYOUT, str(N), t.w], check=True, stdout=subprocess.DEVNULL)


def main():
    print("# env", json.dumps(dict(
        date=time.strftime("%FT%TZ", time.gmtime()), uname=os.uname().release, layout=LAYOUT, files=N, runs=RUNS,
        load1_start=load1(), cores=os.cpu_count(), tools={t.name: t.v() for t in tools},
        free_gb=shutil.disk_usage(WORK).free / 2**30)), flush=True)
    if any(t.name == "pijul" for t in tools):
        setup_pijul()
    for t in tools:
        gen(t)
    P = {t.name: t for t in tools}
    tf = {t.name: t for t in tools}

    def need(attr):
        return [t for t in tools if getattr(t, attr)]

    # add (fresh index): prep = reset + init
    cell("add", {t.name: (f"{t.reset}; {t.init}", t.add, t.w) for t in tools})
    # commit (all staged): prep = reset + init + add
    cell("commit", {t.name: (f"{t.reset}; {t.init} && {t.add}", t.commit, t.w) for t in tools})
    # commit 1 state
    for t in tools:
        sh(f"{t.reset}; {t.init} && {t.add} && {t.commit}", t.w)
    first_store = {}
    for t in tools:
        first_store[t.name] = du_kb(*[spath(t, s) for s in t.stores])
        if hasattr(t, "side_hook"):
            t.side_hook()
    time.sleep(3)
    prime = {t.name: t.status for t in tools if t.name == "git-fsmonitor"}
    cell("status_clean", {t.name: (None, t.status, t.w) for t in tools}, prime=prime)
    for t in tools:
        modify_1pct(t.w, "a")
    cell("status_1pct", {t.name: (None, t.status, t.w) for t in tools})
    for t in tools:
        sh(f"{t.addall}; {t.commit}", t.w)
    for k in range(1, 49):
        for t in tools:
            fs = files_sorted(t.w)
            with open(fs[k * 997 % len(fs)], "wb") as fh:
                fh.write(f"history {k}\n".encode())
            sh(f"{t.addall}; {t.commit}", t.w)
    cell("log50", {t.name: (None, t.log, t.w) for t in tools})
    cell("checkout_1pct", {t.name: (t.co_prep, t.co_time, t.w) for t in tools if t.co_prep})
    # clone
    plan = {}
    vcs = P.get("vcs")
    if vcs:
        remote = f"{WORK}/vcs-remote"
        os.makedirs(remote, exist_ok=True)
        sh(f"{VCS} remote set {remote} && {VCS} publish", vcs.w)
    for t in tools:
        if not t.clone:
            continue
        dst = f"{WORK}/clone-{t.name}"
        cmd = t.clone.format(w=t.w, dst=dst, remote=f"{WORK}/vcs-remote", base=f"clone-{t.name}", repo=f"{WORK}/svn.repo")
        plan[t.name] = (f"rm -rf {dst} {dst}.repo", cmd, WORK)
    g = P.get("git")
    if g:
        for v in CLONE_VARIANTS:
            dst = f"{WORK}/clone-{v.name}"
            plan[v.name] = (f"rm -rf {dst}", v.cmd.format(gw=g.w, dst=dst), WORK)
    cell("clone", plan)
    # sizes
    for t in tools:
        store = du_kb(*[spath(t, s) for s in t.stores])
        over = du_kb(*[f"{WORK}/{s}" for s in t.extra_overhead]) if t.extra_overhead else 0
        print(json.dumps(dict(layout=LAYOUT, cell="size_kib", tool=t.name, store_after_commit1=first_store[t.name],
                              store_after_50=store, wc_metadata=over)), flush=True)
    for t in tools:
        sh(t.reset, t.w)
        if t.name == "git-fsmonitor":
            sh("git fsmonitor--daemon stop", t.w)


try:
    main()
finally:
    json.dump(results, open(os.path.join(PARENT, f"results-{LAYOUT}{TAG}.json"), "w"))
    if AGENT_PID:
        try:
            os.kill(AGENT_PID, 15)
        except OSError:
            pass
    shutil.rmtree(WORK, ignore_errors=True)
