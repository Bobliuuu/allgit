#!/usr/bin/env python3
"""Client-kill chaos: SIGKILL vcs at a random moment during commit / switch / sync / publish / clone,
then check that (1) `vcs fsck` is ok, (2) the repo is usable without manual repair, (3) the retried
operation succeeds and the working tree equals the expected content, (4) a remote is never half-published.

  chaos_client.py --out DIR --iters 60 [--remote dir|http]
Results: DIR/client-kill.json (+ per-iteration lines in DIR/client-kill.jsonl).
"""
import argparse, hashlib, json, os, random, shutil, signal, subprocess, sys, time

BIN = os.environ.get("VCS_BIN") or os.path.expanduser("~/rt/soakchaos/target/release")
ENV = dict(os.environ, PATH=BIN + ":" + os.environ["PATH"], VCS_AUTHOR_NAME="k", VCS_AUTHOR_EMAIL="k@x",
           VCS_TOKEN="tok-admin", VCS_PRINCIPAL="admin")


STALE = [0]   # stale .vcs/lock files (dead pid) removed by hand after a SIGKILL: the "repair" a user is told to do


def _repair_lock(cwd):
    lk = os.path.join(cwd, ".vcs", "lock")
    try:
        txt = open(lk).read()
        if txt.strip():
            os.kill(int(txt.split()[1]), 0)
            return False
        if time.time() - os.path.getmtime(lk) < 3:
            return False
        raise ProcessLookupError   # zero-byte lock: killed between create_new and the pid write
    except ProcessLookupError:
        os.unlink(lk)
        STALE[0] += 1
        return True
    except (OSError, ValueError, IndexError):
        return False


def run(args, cwd, timeout=300):
    p = subprocess.run(args, cwd=cwd, env=ENV, capture_output=True, text=True, timeout=timeout)
    if p.returncode != 0 and "repository lock held" in p.stderr and _repair_lock(cwd):
        p = subprocess.run(args, cwd=cwd, env=ENV, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr


def killed_run(args, cwd, delay):
    p = subprocess.Popen(args, cwd=cwd, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        p.wait(delay)
        done = True
    except subprocess.TimeoutExpired:
        p.send_signal(signal.SIGKILL)
        done = False
    out, err = p.communicate()
    return done, p.returncode, out, err


def tree_hash(root):
    h = {}
    for r, ds, fs in os.walk(root):
        ds[:] = [d for d in ds if d != ".vcs"]
        for f in fs:
            p = os.path.join(r, f)
            rel = os.path.relpath(p, root)
            if os.path.islink(p):
                continue
            h[rel] = hashlib.sha256(open(p, "rb").read()).hexdigest()
    return h


def mk_repo(base, nfiles, rng, remote=None):
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    run(["vcs", "init", base], base)
    for z in range(3):
        run(["vcs", "zone", "create", f"z{z}", "--path", f"z{z}"], base)
    for i in range(nfiles):
        d = f"{base}/z{i % 3}/d{i % 40}"
        os.makedirs(d, exist_ok=True)
        open(f"{d}/f{i}.txt", "wb").write(os.urandom(rng.randint(100, 20000)))
    run(["vcs", "add", "."], base)
    run(["vcs", "commit", "-m", "base"], base)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--iters", type=int, default=40)
    ap.add_argument("--files", type=int, default=3000)
    ap.add_argument("--url", default="", help="vcs-server URL for publish/clone kill tests (else a dir remote)")
    a = ap.parse_args()
    rng = random.Random(3)
    os.makedirs(a.out, exist_ok=True)
    jl = open(os.path.join(a.out, "client-kill.jsonl"), "w", buffering=1)
    summary = {}
    base = os.path.join(a.out, "kc")
    remote = a.url or os.path.join(a.out, "kc-remote")
    if not a.url:
        shutil.rmtree(remote, ignore_errors=True)
        os.makedirs(remote)

    def rec(scn, **kw):
        s = summary.setdefault(scn, {"iters": 0, "killed_mid_op": 0, "completed_before_kill": 0, "fsck_bad": 0,
                                     "retry_failed": 0, "tree_mismatch": 0, "status_dirty_after_retry": 0,
                                     "post_kill_errors": {}})
        s["iters"] += 1
        s["killed_mid_op" if not kw["done"] else "completed_before_kill"] += 1
        for k in ("fsck_bad", "retry_failed", "tree_mismatch", "status_dirty_after_retry"):
            s[k] += int(kw.get(k, 0))
        for m in kw.get("msgs", []):
            s["post_kill_errors"][m] = s["post_kill_errors"].get(m, 0) + 1
        jl.write(json.dumps({"scn": scn, **kw}) + "\n")

    def fsck_ok(d):
        rc, out, err = run(["vcs", "fsck"], d)
        return rc == 0 and "ok" in out, (err or out)[-200:]

    # ---- commit kill
    mk_repo(base, a.files, rng)
    # typical duration of a commit of 600 changed files
    for it in range(a.iters):
        names = rng.sample([os.path.join(r, f) for r, ds, fs in os.walk(base) if ".vcs" not in r for f in fs], 600)
        for n in names:
            open(n, "wb").write(os.urandom(rng.randint(100, 20000)))
        run(["vcs", "add", "."], base)
        t0 = time.monotonic()
        done, rc, out, err = killed_run(["vcs", "commit", "-m", f"k{it}"], base, rng.uniform(0, 0.25))
        msgs = []
        ok, m = fsck_ok(base)
        if not ok:
            msgs.append("fsck: " + m)
        rc2, o2, e2 = run(["vcs", "status"], base)
        if rc2 != 0:
            msgs.append("status-after-kill: " + (e2 or o2)[-160:])
        # recovery: re-add and commit (if the commit did not land)
        run(["vcs", "add", "."], base)
        rcc, oc, ec = run(["vcs", "commit", "-m", f"k{it}b"], base)
        retry_failed = rcc != 0 and "nothing" not in (ec + oc).lower()
        if retry_failed:
            msgs.append("retry-commit: " + (ec or oc)[-160:])
        rcs, so, se = run(["vcs", "status"], base)
        dirty = "clean" not in so
        ok2, _ = fsck_ok(base)
        rec("commit", done=done, fsck_bad=int(not ok or not ok2), retry_failed=int(retry_failed),
            status_dirty_after_retry=int(dirty), msgs=msgs)

    # ---- switch kill (checkout of ~all files differing)
    mk_repo(base, a.files, rng)
    run(["vcs", "branch", "other"], base)
    run(["vcs", "switch", "other"], base)
    for r, ds, fs in os.walk(base):
        if ".vcs" in r:
            continue
        for f in fs:
            if rng.random() < 0.5:
                open(os.path.join(r, f), "wb").write(os.urandom(rng.randint(100, 20000)))
    run(["vcs", "add", "."], base)
    run(["vcs", "commit", "-m", "other-change"], base)
    expect_other = tree_hash(base)
    run(["vcs", "switch", "main"], base)
    expect_main = tree_hash(base)
    for it in range(a.iters):
        target, exp = ("other", expect_other) if it % 2 == 0 else ("main", expect_main)
        done, rc, out, err = killed_run(["vcs", "switch", target], base, rng.uniform(0, 0.25))
        msgs = []
        ok, m = fsck_ok(base)
        if not ok:
            msgs.append("fsck: " + m)
        rc2, o2, e2 = run(["vcs", "status"], base)
        if rc2 != 0:
            msgs.append("status-after-kill: " + (e2 or o2)[-160:])
        elif not done:
            msgs.append("status-after-kill-ok: " + o2.strip().replace("\n", " | ")[:120])
        rcs, os_, es = run(["vcs", "switch", target], base)
        retry_failed = rcs != 0
        if retry_failed:
            msgs.append("retry-switch: " + (es or os_)[-200:])
            # does a restore recover? record separately
            run(["vcs", "restore", "."], base)
            rcs, os_, es = run(["vcs", "switch", target], base)
        mism = int(tree_hash(base) != exp)
        rcs2, so, se = run(["vcs", "status"], base)
        ok2, _ = fsck_ok(base)
        rec("switch", done=done, fsck_bad=int(not ok or not ok2), retry_failed=int(retry_failed),
            tree_mismatch=mism, status_dirty_after_retry=int("clean" not in so), msgs=msgs)
        if mism:   # resync for next iteration
            run(["vcs", "restore", "."], base)

    # ---- publish kill: remote must be old-or-new, never half; retry must succeed
    mk_repo(base, 800, rng)
    run(["vcs", "remote", "set", remote], base)
    run(["vcs", "publish"], base)
    acked = []
    for it in range(a.iters):
        fn = f"z{it % 3}/pub{it}.txt"
        body = os.urandom(rng.randint(1000, 300000))
        open(f"{base}/{fn}", "wb").write(body)
        run(["vcs", "add", "."], base)
        run(["vcs", "commit", "-m", f"p{it}"], base)
        done, rc, out, err = killed_run(["vcs", "publish"], base, rng.uniform(0, 0.12))
        msgs = []
        rcr, o, e = run(["vcs", "publish"], base)
        retry_failed = rcr != 0
        if retry_failed:
            msgs.append("retry-publish: " + (e or o)[-200:])
        acked.append((fn, hashlib.sha256(body).hexdigest()))
        # a fresh clone of the remote must contain everything acked so far and fsck clean
        if it % 5 == 4 or it == a.iters - 1:
            c = os.path.join(a.out, "kc-verify")
            shutil.rmtree(c, ignore_errors=True)
            rcc, oc, ec = run(["vcs", "clone", remote, c], a.out)
            bad = 0
            if rcc != 0:
                msgs.append("clone-after-kill: " + ec[-200:])
                bad = 1
            else:
                for f, h in acked:
                    p = os.path.join(c, f)
                    if not os.path.exists(p) or hashlib.sha256(open(p, "rb").read()).hexdigest() != h:
                        bad += 1
                okc, mc = fsck_ok(c)
                if not okc:
                    msgs.append("clone fsck: " + mc)
            rec("publish", done=done, tree_mismatch=bad, retry_failed=int(retry_failed), msgs=msgs)
            shutil.rmtree(c, ignore_errors=True)
        else:
            rec("publish", done=done, retry_failed=int(retry_failed), msgs=msgs)

    # ---- clone kill
    for it in range(a.iters):
        c = os.path.join(a.out, "kc-clone")
        shutil.rmtree(c, ignore_errors=True)
        done, rc, out, err = killed_run(["vcs", "clone", remote, c], a.out, rng.uniform(0, 0.15))
        msgs = []
        # retry into the same dir (what a user would do first), then into a fresh dir if refused
        rcr, o, e = run(["vcs", "clone", remote, c], a.out)
        retry_failed = 0
        if rcr != 0 and not done:
            msgs.append("reclone-same-dir: " + (e or o).strip()[-160:])
            shutil.rmtree(c, ignore_errors=True)
            rcr, o, e = run(["vcs", "clone", remote, c], a.out)
            retry_failed = int(rcr != 0)
        elif rcr != 0 and done:
            msgs.append("reclone-existing-complete-dir: " + (e or o).strip()[-160:])
            shutil.rmtree(c, ignore_errors=True)
            rcr, o, e = run(["vcs", "clone", remote, c], a.out)
            retry_failed = int(rcr != 0)
        ok, m = fsck_ok(c) if rcr == 0 else (False, "no repo")
        if not ok:
            msgs.append("fsck: " + m)
        rec("clone", done=done, fsck_bad=int(not ok), retry_failed=retry_failed, msgs=msgs)
        shutil.rmtree(c, ignore_errors=True)

    summary["_stale_locks_removed"] = STALE[0]
    json.dump(summary, open(os.path.join(a.out, "client-kill.json"), "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
