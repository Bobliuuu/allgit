#!/usr/bin/env python3
"""Markdown tables for the monorepo stress doc from the run directories.
  summarize.py RUNS_DIR NAME [NAME...]
Per run: scale (writers, readers, duration), throughput, per-op latency percentiles (writers and readers), error classes,
server RSS / CPU peaks (samples.csv), loopback bytes and disk growth (lo-bytes.txt), sar CPU/disk, integrity (verify.json),
chaos log counts.
"""
import collections, csv, glob, json, os, re, sys


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p / 100 * len(v)))] if v else float("nan")


def run(d, name):
    ops = collections.defaultdict(list)
    kinds = collections.Counter()
    errs = collections.Counter()
    tmin, tmax = 1e18, 0
    for fn in glob.glob(os.path.join(d, "ops-*.jsonl")):
        for l in open(fn):
            r = json.loads(l)
            if r["op"] == "done":
                continue
            tmin, tmax = min(tmin, r["t"]), max(tmax, r["t"])
            kinds[(r["op"], r["k"])] += 1
            if r["k"] == "ok":
                ops[r["op"]].append(r["dt"])
            elif r["k"] == "error":
                m = re.sub(r"\d+", "N", re.sub(r"[0-9a-f]{12,64}", "<id>", r.get("err", "")))[-110:].replace("\n", " ")
                errs[(r["op"], m)] += 1
    span = max(tmax - tmin, 1)
    print(f"### {name}\n")
    v = json.load(open(os.path.join(d, "verify.json"))) if os.path.exists(os.path.join(d, "verify.json")) else {}
    print(f"span {span:.0f} s of client activity, server alive at end: {v.get('server_alive_at_end')}\n")
    print("| op | ok | expected fail | error | per s | p50 ms | p95 ms | p99 ms | max ms |\n|---|---|---|---|---|---|---|---|---|")
    for op in sorted(set(o for o, _ in kinds)):
        ok, ex, er = kinds[(op, "ok")], kinds[(op, "expected")], kinds[(op, "error")]
        x = ops[op]
        print(f"| {op} | {ok} | {ex} | {er} | {(ok + ex + er) / span:.2f} | {pct(x, 50) * 1000:.0f} | {pct(x, 95) * 1000:.0f} | "
              f"{pct(x, 99) * 1000:.0f} | {max(x) * 1000 if x else float('nan'):.0f} |")
    if errs:
        print("\nError classes (count, op, tail of message):\n")
        for (op, m), n in errs.most_common(8):
            print(f"- {n}x `{op}`: `{m}`")
    sf = os.path.join(d, "samples.csv")
    if os.path.exists(sf):
        rows = list(csv.DictReader(open(sf)))
        def col(k):
            return [float(r[k]) for r in rows if float(r[k]) >= 0]
        rss, cpu, load = col("rss_kb"), col("cpu_pct"), col("load1")
        print(f"\nServer: peak RSS {max(rss) / 1048576:.1f} GiB (mean {sum(rss) / len(rss) / 1048576:.1f}), CPU mean {sum(cpu) / len(cpu):.0f}% "
              f"max {max(cpu):.0f}% of one core, peak fds {max(col('fds')):.0f}, peak threads {max(col('threads')):.0f}, "
              f"packs {col('packs')[0]:.0f} -> {col('packs')[-1]:.0f} (max {max(col('packs')):.0f}), manifest {col('manifest_bytes')[0] / 1024:.0f} -> "
              f"{col('manifest_bytes')[-1] / 1024:.0f} KiB, box load1 peak {max(load):.0f}.")
    lf = os.path.join(d, "lo-bytes.txt")
    if os.path.exists(lf):
        rows = [l.split() for l in open(lf) if len(l.split()) >= 4]
        if rows:
            t = float(rows[-1][0]) - float(rows[0][0])
            rx, tx = float(rows[-1][1]) - float(rows[0][1]), float(rows[-1][2]) - float(rows[0][2])
            used = (float(rows[-1][3]) - float(rows[0][3])) / 1048576
            print(f"Loopback traffic {rx / 2**30:.1f} GiB rx / {tx / 2**30:.1f} GiB tx over {t:.0f} s "
                  f"({rx / t / 2**20:.0f} MiB/s rx); disk used on the data volume +{used:.1f} GiB over the run.")
    sar = os.path.join(d, "sar.txt")
    if os.path.exists(sar):
        idle = []
        wr = []
        for l in open(sar):
            f = l.split()
            if len(f) >= 8 and f[1] == "all" and re.match(r"[\d.]+$", f[-1]):
                idle.append(float(f[-1]))
            if len(f) >= 10 and f[1] == "nvme1n1":
                try:
                    wr.append((float(f[2]), float(f[3])))   # tps, rkB/s -> kept for max only
                except ValueError:
                    pass
        if idle:
            print(f"sar: box CPU busy mean {100 - sum(idle) / len(idle):.0f}% (peak {100 - min(idle):.0f}%).")
    if v:
        print("\nIntegrity:\n")
        keys = ["acked_files", "acked_missing", "acked_wrong_content", "last_wins_files", "last_wins_mismatch",
                "last_wins_newer_than_ack", "big_acked_files", "big_bad", "big_newer_than_ack", "clone_rc", "clone_s"]
        print("- " + ", ".join(f"{k}={v[k]}" for k in keys if k in v))
        print(f"- fresh clone fsck: {v.get('fresh_clone_fsck')}\n- locks at end: {v.get('locks_at_end')}")
        for k in ("audits_bad", "stale_gc_locks_cleared", "stale_cas_locks_cleared_age_s"):
            if k in v:
                print(f"- {k}: {v[k]}")
        if "audits" in v:
            print(f"- audits during the run: {len(v['audits'])}, bad: {sum(a['bad'] for a in v['audits'])}")
        if "full" in v:
            f = v["full"]
            print(f"- full verification (root + big zones): {json.dumps({k: f.get(k) for k in ('clone_rc', 'clone_s', 'acked_missing', 'acked_wrong_content', 'fresh_clone_fsck', 'store_dircopy_clone_fsck', 'gc_remote_dry_run')})}")
        if "big_zone" in v:
            print(f"- prebuilt big files: {v['big_zone']}")
    cl = os.path.join(d, "chaos.log")
    if os.path.exists(cl):
        t = open(cl).read()
        print(f"\nChaos: {len(re.findall(r'kill -9 server', t))} server kills, {len(re.findall(r'KILLED -9', t))} gc kills, "
              f"{len(re.findall(r'finished rc=0', t))} gc runs finished rc=0, {len(re.findall(r'AUDIT', t))} audits.")
    print()


if __name__ == "__main__":
    for n in sys.argv[2:]:
        run(os.path.join(sys.argv[1], n), n)
