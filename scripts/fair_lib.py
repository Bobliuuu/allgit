"""Shared helpers for the fair-pair benches (bench_clone_fair.sh, bench_status_fair.sh, bench_groupcommit_fair.sh).
Linux only. Timing sections take the shared box lock (fcntl.flock on BOX_LOCK, same lock as `flock /tmp/box-timing.lock`)."""
import contextlib, fcntl, os, platform, resource, subprocess, time

BOX_LOCK = os.environ.get("BOX_LOCK", "/tmp/box-timing.lock")


@contextlib.contextmanager
def box_lock():
    """Hold the shared timing lock; waiting time is not counted."""
    if not BOX_LOCK:
        yield 0.0
        return
    f = open(BOX_LOCK, "a")
    t0 = time.time()
    fcntl.flock(f, fcntl.LOCK_EX)
    try:
        yield time.time() - t0
    finally:
        fcntl.flock(f, fcntl.LOCK_UN)
        f.close()


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def run_timed(argv, env=None, cwd=None):
    """Run argv, return dict(wall_ms, user_ms, sys_ms, rc, load1, out_lines). CPU = rusage of waited-for children (the command
    and everything it waited on; NOT detached daemons - count those separately)."""
    e = dict(os.environ)
    e.update(env or {})
    l0 = load1()
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    p = subprocess.run(argv, env=e, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wall = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    return dict(wall_ms=round(wall * 1000, 1), user_ms=round((r1.ru_utime - r0.ru_utime) * 1000, 1),
                sys_ms=round((r1.ru_stime - r0.ru_stime) * 1000, 1), rc=p.returncode, load1=l0,
                out_lines=p.stdout.count(b"\n"), err_tail=p.stderr[-200:].decode(errors="replace") if p.returncode else "")


def proc_cpu_ms(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) * 1000.0 / os.sysconf("SC_CLK_TCK")
    except OSError:
        return 0.0


def pids_under(root, name_sub):
    """pids whose cmdline contains name_sub and whose cwd is under root (never touches other agents' processes)."""
    out = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cmd = open(f"/proc/{d}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace")
            cwd = os.readlink(f"/proc/{d}/cwd")
        except OSError:
            continue
        if name_sub in cmd and (cwd == root or cwd.startswith(root + "/")):
            out.append(int(d))
    return out


def evict(path):
    """Drop clean page-cache pages of every file under path (posix_fadvise DONTNEED) after a sync. Does not touch other
    users' data, unlike drop_caches on this shared box. Dentry/inode caches stay warm (recorded as such)."""
    subprocess.run(["sync"])
    n = 0
    for dp, dn, fn in os.walk(path):
        for x in fn:
            try:
                fd = os.open(os.path.join(dp, x), os.O_RDONLY)
            except OSError:
                continue
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
                n += 1
            finally:
                os.close(fd)
    return n


def sh(cmd, check=True, **kw):
    e = kw.pop("env", None)
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True, env=e, **kw)
    if check and r.returncode:
        raise RuntimeError(f"{cmd!r}: {r.stderr[-300:]}")
    return r.stdout.strip()


def meta(root, bins):
    """Provenance block: versions, commit, load, cache state, kernel."""
    mi = {l.split(":")[0]: int(l.split()[1]) for l in open("/proc/meminfo")}
    m = dict(date_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), host=platform.node(), kernel=platform.release(),
             nproc=os.cpu_count(), load_avg=open("/proc/loadavg").read().strip(), git=sh("git --version"),
             repo_commit=(open(root + "/.build-commit").read().strip() if os.path.exists(root + "/.build-commit") else sh(f"git -C {root} rev-parse HEAD", check=False)),
             mem_available_mb=mi["MemAvailable"] // 1024, page_cache_mb=mi["Cached"] // 1024)
    for k, b in bins.items():
        m[k + "_path"] = b
        m[k + "_sha256_16"] = sh(f"sha256sum {b} | cut -c1-16", check=False)
        m[k + "_version"] = sh(f"{b} --version 2>&1 | head -1", check=False)
    return m


def median(v):
    v = sorted(v)
    return v[len(v) // 2] if len(v) % 2 else (v[len(v) // 2 - 1] + v[len(v) // 2]) / 2
