#!/usr/bin/env python3
"""Mixed-workload soak: N simulated collaborators (one process each) against one vcs-server
(or a git daemon, for the comparison). Usage:

  soak.py run --kind vcs|git --n 250 --minutes 30 --out DIR [--backend local|s3] [--think 4]
  soak.py worker ...            (internal: one collaborator)

Every operation is a CLI invocation timed with a monotonic clock; results go to DIR/ops-<id>.jsonl.
Every acknowledged publish/push is recorded in DIR/acked-<id>.jsonl and verified at the end
against a fresh clone. See docs/research/soak-chaos.md for the workload description.
"""
import argparse, glob, hashlib, json, os, random, shutil, signal, subprocess, sys, time

NZ = 10          # zones (vcs) / per-zone branches (git)
NART = 16        # shared lockable binaries
BIN = os.path.expanduser("~/rt/soakchaos/target/release")


def sh(args, cwd=None, env=None, timeout=300):
    t0 = time.monotonic()
    try:
        p = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr, time.monotonic() - t0
    except subprocess.TimeoutExpired:
        return 124, "", "timeout", time.monotonic() - t0


def sha(b):
    return hashlib.sha256(b).hexdigest()


# ------------------------------------------------------------------ worker
class Worker:
    def __init__(self, a):
        self.a = a
        self.i = a.id
        self.z = f"z{self.i % NZ}"
        self.rng = random.Random(a.seed * 100003 + self.i)
        self.work = os.path.join(a.out, "work", f"c{self.i}")
        self.tmp = os.path.join(a.out, "work", f"t{self.i}")
        self.ops = open(os.path.join(a.out, f"ops-{self.i}.jsonl"), "a", buffering=1)
        self.ackf = open(os.path.join(a.out, f"acked-{self.i}.jsonl"), "a", buffering=1)
        self.seq = 0
        self.pending = []   # (relpath, sha) committed locally, not yet acked
        self.env = dict(os.environ)
        self.env.update(VCS_AUTHOR_NAME=f"u{self.i}", VCS_AUTHOR_EMAIL=f"u{self.i}@soak.test",
                        VCS_PRINCIPAL=f"u{self.i}", VCS_TOKEN=f"tok-u{self.i}",
                        GIT_AUTHOR_NAME=f"u{self.i}", GIT_AUTHOR_EMAIL=f"u{self.i}@soak.test",
                        GIT_COMMITTER_NAME=f"u{self.i}", GIT_COMMITTER_EMAIL=f"u{self.i}@soak.test",
                        PATH=BIN + ":" + os.environ["PATH"], GIT_TERMINAL_PROMPT="0")
        if a.backend == "s3":
            self.env.update(AWS_ACCESS_KEY_ID="rustfsadmin", AWS_SECRET_ACCESS_KEY="rustfsadmin")
        self.bin = "vcs" if a.kind == "vcs" else "git"

    def run(self, op, args, cwd=None, expect=()):
        """Run + record. `expect`: substrings in stderr that make a nonzero exit an expected outcome."""
        rc, out, err, dt = sh(args, cwd=cwd or self.work, env=self.env)
        kind = "ok"
        if rc != 0:
            kind = "expected" if any(e in err + out for e in expect) else "error"
        rec = {"t": round(time.time(), 3), "c": self.i, "op": op, "dt": round(dt, 4), "rc": rc, "k": kind}
        if kind != "ok":
            rec["err"] = (err.strip() or out.strip())[-300:]
        self.ops.write(json.dumps(rec) + "\n")
        return rc, out, err

    # ---- setup
    def clone_initial(self):
        shutil.rmtree(self.work, ignore_errors=True)
        os.makedirs(os.path.dirname(self.work), exist_ok=True)
        for attempt in range(5):
            rc, _, _ = self.run("clone_initial", [self.bin, "clone", self.a.url, self.work], cwd=self.a.out)
            if rc == 0:
                break
            shutil.rmtree(self.work, ignore_errors=True)
            time.sleep(2 + attempt * 2)
        else:
            return False
        if self.bin == "git":
            self.run("switch_zone", ["git", "switch", "-c", f"w{self.i}", f"origin/{self.z}"])
        return True

    # ---- ops
    def write_files(self, nfiles):
        """New unique files in my zone dir + modify an existing one. Returns [(rel, sha)]."""
        d = os.path.join(self.work, self.z, f"u{self.i}")
        os.makedirs(d, exist_ok=True)
        files = []
        for _ in range(nfiles):
            self.seq += 1
            rel = f"{self.z}/u{self.i}/f{self.seq:05d}.txt"
            body = (f"{self.i}:{self.seq}\n".encode() + bytes(self.rng.getrandbits(8) for _ in range(64))
                    * self.rng.randint(1, 80))
            with open(os.path.join(self.work, rel), "wb") as f:
                f.write(body)
            files.append((rel, sha(body)))
        # edit an existing file of mine (a log that grows)
        rel = f"{self.z}/u{self.i}/log.txt"
        with open(os.path.join(self.work, rel), "ab") as f:
            f.write(f"edit {self.seq} {time.time()}\n".encode())
        return files

    def op_commit(self, undo=False):
        files = self.write_files(self.rng.randint(1, 3))
        rc, _, _ = self.run("add", ["git", "add", "-A"] if self.bin == "git" else ["vcs", "add", self.z])
        if rc != 0:
            return
        rc, _, _ = self.run("commit", [self.bin, "commit", "-q", "-m", f"c{self.seq} by u{self.i}"] if self.bin == "git"
                            else ["vcs", "commit", "-m", f"c{self.seq} by u{self.i}"])
        if rc != 0:
            return
        if undo:
            if self.bin == "git":
                self.run("undo", ["git", "reset", "-q", "--hard", "HEAD~1"])
            else:
                self.run("undo", ["vcs", "op", "undo"])
            for rel, _ in files:  # git reset removes tracked files; vcs op undo removes them too; be sure
                try:
                    os.unlink(os.path.join(self.work, rel))
                except FileNotFoundError:
                    pass
            # the log.txt edit is lost with the undone commit: that is fine, it is not verified
        else:
            self.pending.extend(files)

    def op_sync(self):
        if self.bin == "git":
            rc, out, err = self.run("sync", ["git", "fetch", "-q", "origin"])
            if rc == 0:
                self.run("merge", ["git", "merge", "-q", "--no-edit", f"origin/{self.z}"])
            return
        rc, out, err = self.run("sync", ["vcs", "sync"])
        if rc == 0 and "diverged" in out:
            self.run("merge", ["vcs", "merge", "--remote", "main", "--zone", self.z])

    def ack(self, op_ok):
        if op_ok:
            for rel, h in self.pending:
                self.ackf.write(json.dumps({"c": self.i, "file": rel, "sha": h, "t": time.time()}) + "\n")
            self.pending = []

    def op_publish(self):
        if not self.pending:
            return self.op_sync()
        auto = self.rng.random() < 0.3 and self.bin == "vcs"
        for attempt in range(6):
            if self.bin == "git":
                rc, out, err = self.run("publish", ["git", "push", "-q", "origin", f"HEAD:refs/heads/{self.z}"],
                                        expect=("rejected", "fetch first", "non-fast-forward"))
                rejected = rc != 0
            else:
                cmd = ["vcs", "publish"] + (["--auto-merge"] if auto else [])
                rc, out, err = self.run("publish_auto" if auto else "publish", cmd,
                                        expect=("advanced", "diverged", "Cannot publish"))
                rejected = rc != 0
            if rc == 0:
                self.ack(True)
                return
            if not rejected or not any(s in err + out for s in ("advanced", "diverge", "rejected", "fetch first", "Cannot publish")):
                return
            self.op_sync()   # reconcile and retry
        return

    def op_lock_cycle(self):
        k = self.rng.randrange(NART)
        path = f"art/f{k}.bin"
        rc, out, err = self.run("lock", ["vcs", "lock", path, "--lease-secs", "120"], expect=("is locked by", "held by"))
        if rc != 0:
            return
        try:
            self.run("sync", ["vcs", "sync"])
            with open(os.path.join(self.work, path), "wb") as f:
                f.write(os.urandom(self.rng.randint(1000, 30000)))
            self.seq += 1
            body = f"{self.seq}\n".encode()
            rel = f"art/u{self.i}.txt"
            with open(os.path.join(self.work, rel), "wb") as f:
                f.write(body)
            rc, _, _ = self.run("add", ["vcs", "add", "art"])
            if rc == 0:
                rc, _, _ = self.run("commit", ["vcs", "commit", "-m", f"art {path} by u{self.i}"])
            if rc == 0:
                for attempt in range(6):
                    rc, out, err = self.run("publish_lock", ["vcs", "publish", "--auto-merge"],
                                            expect=("advanced", "diverged", "Cannot publish"))
                    if rc == 0:
                        self.ackf.write(json.dumps({"c": self.i, "file": rel, "sha": sha(body), "t": time.time(),
                                                    "last_wins": True}) + "\n")
                        break
                    self.op_sync()
        finally:
            self.run("unlock", ["vcs", "unlock", path])

    def op_clone_fresh(self):
        shutil.rmtree(self.tmp, ignore_errors=True)
        rc, _, _ = self.run("clone_fresh", [self.bin, "clone", self.a.url, self.tmp], cwd=self.a.out)
        if rc == 0 and self.bin == "vcs" and self.rng.random() < 0.3:
            self.run("fsck_clone", ["vcs", "fsck"], cwd=self.tmp)
        shutil.rmtree(self.tmp, ignore_errors=True)

    def op_branch(self):
        b = f"br-u{self.i}-{self.seq}"
        self.seq += 1
        if self.bin == "git":
            self.run("branch_switch", ["git", "switch", "-q", "-c", b])
            self.run("branch_switch", ["git", "switch", "-q", f"w{self.i}"])
            self.run("branch_delete", ["git", "branch", "-q", "-D", b])
        else:
            self.run("branch_create", ["vcs", "branch", b])
            self.run("branch_switch", ["vcs", "switch", b])
            self.run("branch_switch", ["vcs", "switch", "main"])
            self.run("branch_delete", ["vcs", "branch", "-d", b, "--force"])

    def loop(self):
        a = self.a
        time.sleep(self.rng.random() * a.ramp)
        if not self.clone_initial():
            return
        end = a.deadline
        weights = [("commit", 40), ("sync", 12), ("publish", 28), ("undo", 4), ("branch", 5), ("clone", 3)]
        if self.bin == "vcs":
            weights.append(("lock", 8))
        names = [w[0] for w in weights]
        ws = [w[1] for w in weights]
        while time.time() < end:
            op = self.rng.choices(names, ws)[0]
            if op == "commit":
                self.op_commit()
            elif op == "undo":
                self.op_commit(undo=True)
            elif op == "sync":
                self.op_sync()
            elif op == "publish":
                self.op_publish()
            elif op == "branch":
                self.op_branch()
            elif op == "clone":
                self.op_clone_fresh()
            elif op == "lock":
                self.op_lock_cycle()
            time.sleep(self.rng.expovariate(1.0 / a.think))
        # final: publish whatever is pending (tail), so the ack file is complete
        for _ in range(3):
            if not self.pending:
                break
            self.op_publish()
        self.ops.write(json.dumps({"t": time.time(), "c": self.i, "op": "done", "dt": 0, "rc": 0, "k": "ok",
                                   "unpublished": len(self.pending)}) + "\n")


# ------------------------------------------------------------------ orchestrator
def rss_kb(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmRSS"):
                return int(l.split()[1])
    except OSError:
        pass
    return -1


def proc_cpu_ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return int(f[11]) + int(f[12]) + int(f[13]) + int(f[14])
    except OSError:
        return -1


def tree(pid):
    """pid plus live descendants (git daemon forks per connection)."""
    kids = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                pp = int(open(f"/proc/{d}/stat").read().rsplit(")", 1)[1].split()[1])
                kids.setdefault(pp, []).append(int(d))
            except (OSError, IndexError, ValueError):
                pass
    out, st = [], [pid]
    while st:
        p = st.pop()
        out.append(p)
        st.extend(kids.get(p, []))
    return out


def nfds(pid):
    try:
        return len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        return -1


def nthreads(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("Threads"):
                return int(l.split()[1])
    except OSError:
        pass
    return -1


def du(path):
    tot = 0
    for r, _, fs in os.walk(path):
        for f in fs:
            try:
                tot += os.lstat(os.path.join(r, f)).st_size
            except OSError:
                pass
    return tot


def start_vcs_server(a, root):
    toks = os.path.join(a.out, "tokens")
    with open(toks, "w") as f:
        f.write(f"admin admin * {hash_token('tok-admin')}\n")
        for i in range(a.n):
            f.write(f"u{i} write * {hash_token(f'tok-u{i}')}\n")
    backend = ["--root", root] if a.backend == "local" else ["--backend", a.s3url]
    env = dict(os.environ)
    if a.backend == "s3":
        env.update(AWS_ACCESS_KEY_ID="rustfsadmin", AWS_SECRET_ACCESS_KEY="rustfsadmin")
    log = open(os.path.join(a.out, "server.log"), "a")
    p = subprocess.Popen([BIN + "/vcs-server", *backend, "--tokens", toks, "--listen", "127.0.0.1:8491",
                          "--max-connections", "4096", "--rate-limit", "100000,100000"], stdout=log, stderr=log, env=env)
    for _ in range(100):
        time.sleep(0.1)
        if subprocess.run(["curl", "-sf", "http://127.0.0.1:8491/healthz"], capture_output=True).returncode == 0:
            return p
    raise SystemExit("server did not start")


_ht = {}


def hash_token(t):
    if t not in _ht:
        _ht[t] = subprocess.run([BIN + "/vcs-server", "hash-token", t], capture_output=True, text=True).stdout.strip()
    return _ht[t]


def seed_vcs(a):
    seed = os.path.join(a.out, "seed")
    env = dict(os.environ, VCS_AUTHOR_NAME="seed", VCS_AUTHOR_EMAIL="seed@soak.test", VCS_TOKEN="tok-admin",
               VCS_PRINCIPAL="admin", PATH=BIN + ":" + os.environ["PATH"])
    if a.backend == "s3":
        env.update(AWS_ACCESS_KEY_ID="rustfsadmin", AWS_SECRET_ACCESS_KEY="rustfsadmin")
    def r(*c):
        p = subprocess.run(c, cwd=seed if os.path.isdir(seed) else a.out, env=env, capture_output=True, text=True)
        if p.returncode:
            raise SystemExit(f"seed: {c}: {p.stderr}")
    r("vcs", "init", seed)
    for z in range(NZ):
        r("vcs", "zone", "create", f"z{z}", "--path", f"z{z}")
        os.makedirs(f"{seed}/z{z}", exist_ok=True)
        open(f"{seed}/z{z}/README", "w").write(f"zone {z}\n")
    os.makedirs(f"{seed}/art", exist_ok=True)
    open(f"{seed}/art/.vcsattributes", "w").write("*.bin lockable\n")
    for k in range(NART):
        open(f"{seed}/art/f{k}.bin", "wb").write(os.urandom(5000))
    open(f"{seed}/README", "w").write("soak\n")
    r("vcs", "add", ".")
    r("vcs", "commit", "-m", "seed")
    r("vcs", "remote", "set", a.url)
    r("vcs", "publish")


def seed_git(a, bare):
    seed = os.path.join(a.out, "seed")
    env = dict(os.environ, GIT_AUTHOR_NAME="seed", GIT_AUTHOR_EMAIL="s@x", GIT_COMMITTER_NAME="seed", GIT_COMMITTER_EMAIL="s@x")
    def r(*c, cwd=None):
        p = subprocess.run(c, cwd=cwd or seed, env=env, capture_output=True, text=True)
        if p.returncode:
            raise SystemExit(f"seed: {c}: {p.stderr}")
    r("git", "init", "-q", "-b", "main", seed, cwd=a.out)
    for z in range(NZ):
        os.makedirs(f"{seed}/z{z}", exist_ok=True)
        open(f"{seed}/z{z}/README", "w").write(f"zone {z}\n")
    os.makedirs(f"{seed}/art", exist_ok=True)
    for k in range(NART):
        open(f"{seed}/art/f{k}.bin", "wb").write(os.urandom(5000))
    open(f"{seed}/README", "w").write("soak\n")
    r("git", "add", "-A")
    r("git", "commit", "-q", "-m", "seed")
    r("git", "init", "-q", "--bare", bare, cwd=a.out)
    r("git", "-C", bare, "config", "http.receivepack", "true")
    for z in range(NZ):
        r("git", "push", "-q", bare, f"main:refs/heads/z{z}")
    r("git", "push", "-q", bare, "main:refs/heads/main")
    r("git", "-C", bare, "symbolic-ref", "HEAD", "refs/heads/main")



def sudo(*c, check=False):
    return subprocess.run(["sudo", "-n", *c], capture_output=True, text=True)


class Chaos:
    """Fault injection thread. `holder` = [server Popen]; events logged to chaos.log."""

    def __init__(self, a, holder, store):
        self.a, self.holder, self.store = a, holder, store
        self.stop = __import__("threading").Event()
        self.log = open(os.path.join(a.out, "chaos.log"), "a", buffering=1)
        self.rng = random.Random(7)

    def note(self, m):
        self.log.write(f"{time.time():.1f} {m}\n")

    def sleep(self, s):
        self.stop.wait(s)

    def kill9(self):
        n = 0
        while not self.stop.is_set():
            self.sleep(self.rng.uniform(2, 8))
            if self.stop.is_set():
                break
            p = self.holder[0]
            p.kill()
            p.wait()
            n += 1
            self.note(f"kill -9 server #{n}")
            self.sleep(self.rng.uniform(0.1, 1.0))
            self.holder[0] = start_vcs_server(self.a, self.store)
            self.note("server restarted")

    def diskfull(self):
        ball = os.path.join(self.store, ".ballast")
        self.sleep(40)
        for cyc in range(3):
            if self.stop.is_set():
                break
            # fill to ENOSPC
            subprocess.run(["dd", "if=/dev/zero", f"of={ball}", "bs=64k"], capture_output=True)
            self.note(f"disk filled (cycle {cyc}); df: " + subprocess.run(["df", "-k", self.store], capture_output=True, text=True).stdout.splitlines()[-1])
            self.sleep(30)
            os.unlink(ball)
            self.note("ballast removed")
            self.sleep(40)

    def netdrop(self):
        while not self.stop.is_set():
            self.sleep(20)
            sudo("tc", "qdisc", "add", "dev", "lo", "root", "netem", "delay", "40ms", "10ms", "loss", "8%")
            self.note("netem delay 40ms loss 8% on lo")
            self.sleep(20)
            sudo("tc", "qdisc", "del", "dev", "lo", "root")
            rule = ["-p", "tcp", "--dport", "8491", "-m", "statistic", "--mode", "random", "--probability", "0.02",
                    "-j", "REJECT", "--reject-with", "tcp-reset"]
            sudo("iptables", "-A", "INPUT", *rule)
            self.note("iptables random tcp-reset p=0.02 per packet")
            self.sleep(20)
            sudo("iptables", "-D", "INPUT", *rule)
            self.note("rules removed")

    def cleanup(self):
        sudo("tc", "qdisc", "del", "dev", "lo", "root")
        while sudo("iptables", "-D", "INPUT", "-p", "tcp", "--dport", "8491", "-m", "statistic", "--mode", "random",
                   "--probability", "0.02", "-j", "REJECT", "--reject-with", "tcp-reset").returncode == 0:
            pass

    def run(self):
        try:
            getattr(self, self.a.chaos)()
        finally:
            self.cleanup()


def setup_store_mount(a, store):
    """diskfull: small tmpfs; slowdisk: dm-delay over a loop-backed ext4."""
    os.makedirs(store, exist_ok=True)
    if a.chaos == "diskfull":
        r = sudo("mount", "-t", "tmpfs", "-o", f"size={a.disk_mb}m,uid={os.getuid()}", "tmpfs", store)
        if r.returncode:
            raise SystemExit("mount: " + r.stderr)
    elif a.chaos == "slowdisk":
        img = os.path.join(a.out, "slow.img")
        subprocess.run(["truncate", "-s", "4G", img], check=True)
        subprocess.run(["mkfs.ext4", "-q", "-F", img], check=True, capture_output=True)
        loop = sudo("losetup", "-f", "--show", img).stdout.strip()
        sectors = int(subprocess.run(["blockdev", "--getsz", loop], capture_output=True, text=True).stdout)
        sudo("dmsetup", "create", "soakslow", "--table", f"0 {sectors} delay {loop} 0 {a.delay_ms}")
        sudo("mount", "/dev/mapper/soakslow", store)
        sudo("chown", f"{os.getuid()}", store)
        a.loopdev = loop


def teardown_store_mount(a, store):
    if a.chaos in ("diskfull", "slowdisk"):
        sudo("umount", store)
    if a.chaos == "slowdisk":
        sudo("dmsetup", "remove", "soakslow")
        sudo("losetup", "-d", getattr(a, "loopdev", ""))


def sampler(a, pid, store, stop, rows):
    t0 = time.time()
    hz = os.sysconf("SC_CLK_TCK")
    last_ticks, last_t = proc_cpu_ticks(pid), time.time()
    f = open(os.path.join(a.out, "samples.csv"), "w", buffering=1)
    f.write("t,rss_kb,cpu_pct,fds,threads,manifest_bytes,packs,store_bytes,load1\n")
    while not stop.is_set():
        time.sleep(a.sample)
        now = time.time()
        tk = proc_cpu_ticks(pid)
        cpu = (tk - last_ticks) / hz / (now - last_t) * 100 if tk >= 0 and last_ticks >= 0 else -1
        last_ticks, last_t = tk, now
        man = 0
        npk = 0
        sb = 0
        if store:
            try:
                man = os.path.getsize(os.path.join(store, "manifest"))
            except OSError:
                pass
            npk = len(glob.glob(os.path.join(store, "packs", "*.pack")))
            sb = du(store) if int(now - t0) % 60 < a.sample else -1
        pids = tree(pid) if a.kind == "git" else [pid]
        rss = sum(max(rss_kb(p), 0) for p in pids)
        fds = sum(max(nfds(p), 0) for p in pids)
        f.write(f"{now - t0:.0f},{rss},{cpu:.1f},{fds},{nthreads(pid)},{man},{npk},{sb},{os.getloadavg()[0]:.1f}\n")


def verify(a, store):
    """Fresh clone + every acked file present with the recorded hash; fsck; server-store checks."""
    res = {}
    vdir = os.path.join(a.out, "verify")
    shutil.rmtree(vdir, ignore_errors=True)
    env = dict(os.environ, VCS_TOKEN="tok-admin", VCS_PRINCIPAL="admin", PATH=BIN + ":" + os.environ["PATH"],
               VCS_AUTHOR_NAME="v", VCS_AUTHOR_EMAIL="v@x")
    if a.backend == "s3":
        env.update(AWS_ACCESS_KEY_ID="rustfsadmin", AWS_SECRET_ACCESS_KEY="rustfsadmin")
    t0 = time.time()
    rc, out, err, dt = sh([a.kind if a.kind == "git" else "vcs", "clone", a.url, vdir], cwd=a.out, env=env, timeout=3600)
    res["clone_rc"], res["clone_s"] = rc, round(dt, 2)
    if rc:
        res["clone_err"] = err[-500:]
        return res
    if a.kind == "git":
        # per-zone branches: check each zone branch
        shas = {}
        for z in range(NZ):
            sh(["git", "checkout", "-q", f"origin/z{z}"], cwd=vdir, env=env)
        res["note"] = "git verify below"
    missing, bad, total = 0, 0, 0
    last_wins = {}
    for fn in glob.glob(os.path.join(a.out, "acked-*.jsonl")):
        for l in open(fn):
            r = json.loads(l)
            if r.get("last_wins"):
                last_wins[r["file"]] = r  # later acks overwrite
                continue
            total += 1
            if a.kind == "git":
                z = r["file"].split("/")[0]
                pr = subprocess.run(["git", "show", f"origin/{z}:{r['file']}"], cwd=vdir, env=env, capture_output=True)
                if pr.returncode != 0:
                    missing += 1
                elif sha(pr.stdout) != r["sha"]:
                    bad += 1
                continue
            p = os.path.join(vdir, r["file"])
            try:
                b = open(p, "rb").read()
                if sha(b) != r["sha"]:
                    bad += 1
            except FileNotFoundError:
                missing += 1
    lw_bad = 0
    if a.kind == "vcs":
        for rel, r in last_wins.items():
            try:
                if sha(open(os.path.join(vdir, rel), "rb").read()) != r["sha"]:
                    lw_bad += 1
            except FileNotFoundError:
                lw_bad += 1
    res.update(acked_files=total, acked_missing=missing, acked_wrong_content=bad,
               last_wins_files=len(last_wins), last_wins_mismatch=lw_bad)
    if a.kind == "vcs":
        rc, out, err, dt = sh(["vcs", "fsck"], cwd=vdir, env=env, timeout=3600)
        res["fresh_clone_fsck"] = {"rc": rc, "out": out.strip()[-300:], "err": err.strip()[-300:], "s": round(dt, 1)}
        if store:
            d2 = os.path.join(a.out, "verify-store-clone")
            shutil.rmtree(d2, ignore_errors=True)
            rc, out, err, dt = sh(["vcs", "clone", store, d2], cwd=a.out, env=env, timeout=3600)
            rc2, out2, err2, dt2 = sh(["vcs", "fsck"], cwd=d2, env=env, timeout=3600) if rc == 0 else (rc, "", err, 0)
            res["store_dircopy_clone_fsck"] = {"clone_rc": rc, "fsck_rc": rc2, "out": out2.strip()[-300:], "err": (err2 or err).strip()[-300:]}
            rc, out, err, dt = sh(["vcs", "gc", "--remote", "--dry-run"], cwd=vdir, env=env, timeout=3600)
            res["gc_remote_dry_run"] = {"rc": rc, "out": out.strip()[-600:], "err": err.strip()[-300:]}
            shutil.rmtree(d2, ignore_errors=True)
    else:
        rc, out, err, dt = sh(["git", "fsck", "--no-dangling"], cwd=vdir, env=env, timeout=3600)
        res["fresh_clone_fsck"] = {"rc": rc, "err": err.strip()[-300:]}
    shutil.rmtree(vdir, ignore_errors=True)
    return res


def cmd_run(a):
    import threading
    a.out = os.path.abspath(a.out)
    shutil.rmtree(a.out, ignore_errors=True)
    os.makedirs(os.path.join(a.out, "work"))
    store = os.path.join(a.out, "store")
    srv = None
    if a.kind == "vcs":
        a.url = "http://127.0.0.1:8491"
        if a.backend == "s3":
            a.s3url = a.s3url
            store = a.s3store  # on-disk data dir of the S3 server, for sampling only
        if a.chaos:
            setup_store_mount(a, store)
        srv = start_vcs_server(a, store)
        a.url = "http://127.0.0.1:8491"
        seed_vcs(a)
    else:
        bare = os.path.join(a.out, "repo.git")
        seed_git(a, bare)
        a.url = "git://127.0.0.1:9418/repo.git"
        srv = subprocess.Popen(["git", "daemon", "--reuseaddr", f"--base-path={a.out}", "--export-all",
                                "--enable=receive-pack", "--max-connections=2000", "--listen=127.0.0.1", bare],
                               stdout=subprocess.DEVNULL, stderr=open(os.path.join(a.out, "server.log"), "w"))
        store = bare
        time.sleep(1)
    spid = srv.pid
    holder = [srv]
    chaos = None
    if a.chaos:
        chaos = Chaos(a, holder, store)
        cth = threading.Thread(target=chaos.run)
        cth.start()
    a.deadline = time.time() + a.minutes * 60
    stop = threading.Event()
    th = threading.Thread(target=sampler, args=(a, spid, store if a.backend == "local" or a.kind == "git" else a.s3store, stop, None))
    th.start()
    me = os.path.abspath(__file__)
    ws = []
    for i in range(a.n):
        ws.append(subprocess.Popen([sys.executable, me, "worker", "--id", str(i), "--out", a.out, "--kind", a.kind,
                                    "--url", a.url, "--deadline", str(a.deadline), "--think", str(a.think),
                                    "--ramp", str(a.ramp), "--seed", str(a.seed), "--backend", a.backend],
                                   stdout=subprocess.DEVNULL, stderr=open(os.path.join(a.out, f"worker-{i}.err"), "w")))
    for w in ws:
        w.wait()
    stop.set()
    th.join()
    if chaos:
        chaos.stop.set()
        cth.join()
    srv = holder[0]
    alive = srv.poll() is None
    json.dump({"server_alive_at_end": alive, "server_exit": srv.poll()}, open(os.path.join(a.out, "server_end.json"), "w"))
    res = verify(a, store if a.backend == "local" else None)
    res["server_alive_at_end"] = alive
    if a.kind == "vcs" and a.backend == "local":
        res["final_manifest_bytes"] = os.path.getsize(os.path.join(store, "manifest"))
        res["final_packs"] = len(glob.glob(os.path.join(store, "packs", "*.pack")))
        res["store_bytes"] = du(store)
    json.dump(res, open(os.path.join(a.out, "verify.json"), "w"), indent=1)
    print(json.dumps(res, indent=1))
    srv.send_signal(signal.SIGTERM)
    try:
        srv.wait(20)
    except subprocess.TimeoutExpired:
        srv.kill()
    if a.chaos:
        teardown_store_mount(a, store)
    shutil.rmtree(os.path.join(a.out, "work"), ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    sp = ap.add_subparsers(dest="cmd", required=True)
    r = sp.add_parser("run")
    r.add_argument("--kind", default="vcs")
    r.add_argument("--backend", default="local")
    r.add_argument("--s3url", default="s3://soak/repo?endpoint=http://127.0.0.1:9000&region=us-east-1")
    r.add_argument("--s3store", default=os.path.expanduser("~/soak/rustfs/data/soak"))
    r.add_argument("--n", type=int, default=100)
    r.add_argument("--minutes", type=float, default=30)
    r.add_argument("--think", type=float, default=4.0)
    r.add_argument("--ramp", type=float, default=60)
    r.add_argument("--sample", type=int, default=10)
    r.add_argument("--seed", type=int, default=1)
    r.add_argument("--chaos", default="", help="kill9|diskfull|netdrop|slowdisk")
    r.add_argument("--disk-mb", type=int, default=24)
    r.add_argument("--delay-ms", type=int, default=20)
    r.add_argument("--out", required=True)
    w = sp.add_parser("worker")
    for k, t in (("id", int), ("seed", int), ("think", float), ("ramp", float), ("deadline", float)):
        w.add_argument("--" + k, type=t, required=True)
    for k in ("out", "kind", "url", "backend"):
        w.add_argument("--" + k, required=True)
    a = ap.parse_args()
    if a.cmd == "run":
        cmd_run(a)
    else:
        Worker(a).loop()


if __name__ == "__main__":
    main()
