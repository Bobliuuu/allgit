"""Regression test for #502: the soak harness must never unlink FsBackend's flock file `.cas.lock`.

Unlinking it lets a locker create a new inode and take the lock while the old holder still owns the
old one: two concurrent CAS writers, lost publishes. Run: python3 scripts/soak/test_casguard.py
"""
import fcntl, importlib.util, os, tempfile, threading, time, types

spec = importlib.util.spec_from_file_location("soak", os.path.join(os.path.dirname(__file__), "soak.py"))
soak = importlib.util.module_from_spec(spec)
spec.loader.exec_module(soak)

d = tempfile.mkdtemp()
lk = os.path.join(d, ".cas.lock")
holder = open(lk, "w")
fcntl.flock(holder, fcntl.LOCK_EX)           # a healthy, live holder
old = time.time() - 600
os.utime(lk, (old, old))                      # whose file mtime looks "older than 12 s"
c = soak.Chaos(types.SimpleNamespace(out=d), [None], d)
t = threading.Thread(target=c.casguard)
t.start()
time.sleep(5)
c.stop.set()
t.join()
assert os.path.exists(lk), "casguard unlinked a live flock file"
probe = open(lk, "rb")
try:
    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
    raise AssertionError("two owners of .cas.lock")
except BlockingIOError:
    pass
print("ok")
