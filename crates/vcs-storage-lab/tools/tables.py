#!/usr/bin/env python3
"""Render markdown tables from the lab's JSON-lines output.
usage: tables.py <data_dir> <template.md> > out.md   ({{tbl:EXP:DATASET[:opt]}} placeholders are replaced)"""
import json, sys, re, glob, os

def load(d):
    rows = {}
    for f in glob.glob(os.path.join(d, '*.jsonl')):
        for l in open(f):
            r = json.loads(l)
            rows.setdefault((r['exp'], r['dataset']), []).append(r)
    return rows

def mib(x): return f"{x/1048576:.1f}"
def f1(x): return f"{x:.1f}"
def f2(x): return f"{x:.2f}"
def us(x): return f"{x/1000:.0f}"

def table(hdr, rows):
    out = ["| " + " | ".join(hdr) + " |", "|" + "|".join("---" for _ in hdr) + "|"]
    out += ["| " + " | ".join(str(c) for c in r) + " |" for r in rows]
    return "\n".join(out)

def tbl_A(R, ds):
    rows = R[('A', ds)]
    ds_row = [r for r in rows if r['variant'] == 'dataset'][0]
    git = [r for r in rows if r['variant'].startswith('git repack') and '--window=10 ' in r['variant']]
    gitb = git[0]['stored_bytes'] if git else None
    out = []
    for r in rows:
        v = r['variant']
        if v == 'dataset': continue
        sb = r['stored_bytes']
        rel = f"{sb/gitb:.2f}" if gitb else ""
        if v.startswith('git repack'):
            out.append([v.replace('git repack -adf ', 'GIT: repack '), 'zlib (git 2.34.1)', mib(sb), f"{sb/ds_row['raw_bytes']:.4f}", rel, r['deltas'], f2(r['depth_mean']), r['depth_max'], '', '', '', '', f1(r.get('wall_secs', 0))])
        elif 'rand_reads_mean' in r:
            out.append([v, r['codec'], mib(sb), f"{r['ratio']:.4f}", rel, r['deltas'], f2(r['depth_mean']), r['depth_max'], f1(r['rand_reads_mean']), f1(r['head_reads_mean']), us(r['rand_ns_median']), us(r['head_ns_median']), f1(r['build_secs'])])
        else:
            out.append([v, 'zstd-3 stream', mib(sb), f"{r['ratio']:.4f}", rel, '', '', '', '', '', '', '', f1(r['build_secs'])])
    h = ['variant', 'codec', 'stored MiB', 'stored/raw', 'vs git 10/50', 'deltas', 'mean depth', 'max depth', 'reads per random obj', 'reads per HEAD obj', 'us per random obj', 'us per HEAD obj', 'build s']
    return table(h, out)

def tbl_C(R, ds, which):
    rows = [r for r in R[(which, ds)]]
    raw = [r for r in R[('A', ds)] if r['variant'] == 'dataset'][0]['raw_bytes']
    out = []
    for r in rows:
        stored = mib(r.get('blob_stored_bytes', r['stored_bytes'])) if which == 'C2' else mib(r['stored_bytes'])
        base = [r['variant'].replace(' (window 50, git-like order)', ''), stored, f"{r['stored_bytes']/r['raw_bytes']:.4f}" if which != 'C2' else '', r['deltas'], f2(r['depth_mean']), r['depth_max'], f1(r['rand_reads_mean']), f2(r['rand_blocks_mean']), f2(r['rand_bytes_mean']/1024), us(r['rand_ns_median']), f1(r['head_reads_mean']), f2(r['head_bytes_mean']/1024), us(r['head_ns_median']), us(r['headcache_ns_median'])]
        if which == 'C3':
            base.insert(3, r['cross_block_deltas'])
            base.insert(4, f"{r['mean_physical_block_bytes']/1024:.0f}")
        out.append(base)
    h = ['variant', 'stored MiB' + (' (blobs only)' if which == 'C2' else ''), 'stored/raw' if which != 'C2' else '', 'deltas', 'mean depth', 'max depth', 'reads/random', 'blocks/random', 'KiB read/random', 'us/random', 'reads/HEAD', 'KiB read/HEAD', 'us/HEAD', 'us/HEAD (cached bases)']
    if which == 'C3':
        h.insert(3, 'cross-block deltas'); h.insert(4, 'mean physical block KiB')
    return table(h, out)

def tbl_E1(R, ds, wl):
    rows = [r for r in R[('E1', ds)] if r['workload'].startswith(wl.split('|')[0])]
    pols = []
    for r in rows:
        if r['policy'] not in pols: pols.append(r['policy'])
    fr = sorted({r.get('zone_fraction') for r in rows if r.get('zone_fraction') is not None})
    out = []
    for p in pols:
        line = [p]
        if fr:
            for f in fr:
                r = [x for x in rows if x['policy'] == p and x.get('zone_fraction') == f][0]
                line.append(f"{r['gets_g256k_mean']:.1f} / {r['read_amp_g256k_mean']:.1f}x")
        else:
            r = [x for x in rows if x['policy'] == p][0]
            line.append(f"{r['gets_g256k_mean']:.1f} / {r['read_amp_g256k_mean']:.1f}x")
        out.append(line)
    if fr:
        ks = {f: [x for x in rows if x.get('zone_fraction') == f][0]['zones_k'] for f in fr}
        h = ['layout (GETs / read amp)'] + [f"{int(f*100)}% zones (k={ks[f]})" for f in fr]
    else:
        h = ['layout', 'GETs / read amp']
    return table(h, out)

def tbl_E2(R, ds):
    rows = R[('E2', ds)]
    pols = []
    for r in rows:
        if r['policy'] not in pols: pols.append(r['policy'])
    out = []
    for p in pols:
        rs = [x for x in rows if x['policy'] == p]
        line = [p] + [f"{x['cost_eq_bytes_mean']/1e6:.1f}" for x in rs]
        line.append(mib(sum(x['reorg_rewrite_bytes_before_phase'] for x in rs)))
        line.append(f"{rs[-1]['cum_saved_minus_reorg_cost_eq_bytes_vs_static_zone_commit']/1e6:.0f}")
        out.append(line)
    return table(['policy', 'A (train)', 'B (same)', 'C (drift)', 'D (same as C)', 'rewritten MiB (total)', 'net gain vs static zone,commit after D (MB-eq)'], out)

def tbl_F(R, ds, dup):
    rows = [r for r in R[('F', ds)] if abs(r['duplicate_put_rate'] - float(dup)) < 1e-9]
    out = []
    for r in rows:
        out.append([r['buffer_bytes'] >> 10, r['policy'], f2(r['write_amp_total']), f2(r['space_amp_final']), f2(r['peak_space_amp']), f1(r['sorted_runs_final']), f2(r['read_amp_hit_probes_nobloom_final']), f2(r['read_amp_hit_probes_bloom1pct_final']), f2(r['read_amp_miss_probes_nobloom_final']), f2(r['read_amp_miss_probes_bloom1pct_final']), r['objects_put'], mib(r['compaction_read_bytes'])])
    return table(['buffer KiB', 'policy', 'write amp', 'space amp', 'peak space amp', 'sorted runs', 'hit probes (no bloom)', 'hit probes (bloom 1%)', 'miss probes (no bloom)', 'miss probes (bloom 1%)', 'objects PUT', 'compaction read MiB'], out)

def tbl_G(R, ds, args):
    layout, phase = args.split('|')
    rows = [r for r in R[('G', ds)] if r['layout'].startswith(layout) and r['phase'].startswith(phase)]
    out = []
    for r in rows:
        out.append([f"{int(r['hot_capacity_fraction']*100)}%", r['policy'], f"{r['block_hit_rate']*100:.0f}%", f2(r['cold_gets_per_req']), f2(r['cold_MiB_per_req']), r['promoted_MiB_total'] + r['migration_MiB_epochs_total'] + r['initial_placement_MiB'], f"{r['model_ms_per_req_cold20ms']:.0f} / {r['model_ms_per_req_cold50ms']:.0f} / {r['model_ms_per_req_cold100ms']:.0f}"])
    return table(['hot capacity', 'policy', 'block hit rate', 'cold GETs/req', 'cold MiB/req', 'MiB migrated in', 'model ms/req (cold GET 20/50/100 ms)'], out)

def tbl_C4(R, ds):
    rows = R.get(('C4', ds))
    if not rows:
        return '_(not run for this dataset)_'
    out = []
    for r in rows:
        out.append([r['variant'].replace('independent ', ''), mib(r['stored_bytes']), f"{r['ratio']:.4f}", f"{r['expected_raw_bytes_decompressed_per_random_read']/1048576:.2f}", f"{r['compress_secs_wall']:.1f}"])
    return table(['blocks', 'stored MiB', 'stored/raw', 'MiB decompressed per random read (expected)', 'compress s (wall, 24 threads)'], out)

def tbl_SB(R):
    out = []
    for (e, ds), rows in sorted(R.items()):
        if e != 'A' or not ds.startswith('sb_'): continue
        d = [r for r in rows if r['variant'] == 'dataset'][0]
        def find(pred):
            x = [r for r in rows if pred(r)]
            return x[0] if x else None
        g0 = find(lambda r: r['variant'].startswith('git repack') and '--window=0' in r['variant'])
        g10 = find(lambda r: r['variant'].startswith('git repack') and '--window=10 ' in r['variant'])
        z = find(lambda r: r['variant'].startswith('no deltas') and r.get('codec') == 'zstd-3')
        o = find(lambda r: r['variant'].startswith('delta git-like order window=10') and r.get('codec') == 'zstd-3')
        pd = find(lambda r: 'newest first' in r['variant'] and r['variant'].startswith('delta window=10'))
        wp = find(lambda r: r['variant'].startswith('whole-pack') and 'git-like' in r['variant'])
        m = lambda r: mib(r['stored_bytes']) if r else ''
        out.append([ds[3:], d['history_commits'], f"{d['raw_bytes']/1048576:.1f}", m(g0), m(g10), m(z), m(o), m(pd), m(wp), f1(o['depth_mean']) if o else '', f1(o['head_reads_mean']) if o else ''])
    return table(['workload (scale 8)', 'commits', 'raw MiB', 'git window=0', 'git 10/50', 'ours per-object zstd', 'ours delta W10 D50 zstd (git order)', 'ours delta, path newest-first', 'whole-pack zstd long', 'mean depth', 'reads/HEAD obj'], out)

def tbl_meta(R, ds):
    m = R[('meta', ds)][0]
    d = [r for r in R[('A', ds)] if r['variant'] == 'dataset'][0]
    return f"{ds}: {d['commits']+d['trees']+d['blobs']} objects ({d['commits']} commits, {d['trees']} trees, {d['blobs']} blobs), {d['raw_bytes']/1e6:.0f} MB raw, {d['history_commits']} commits, {d['zones']} zones, head {d['head_objects']} objects; repo head {m['repo_head'][:12]}; {m['cpu']}, {m['cores']} cores, {m['kernel']}, load at start {m['load_at_start']}, {m['rustc']}, {m['git']}, {m['lab_threads']} lab threads, {m['date']}."

def datasets_table(R):
    out = []
    for (e, ds), rows in sorted(R.items()):
        if e != 'A': continue
        d = [r for r in rows if r['variant'] == 'dataset'][0]
        out.append([ds, d['history_commits'], d['commits'] + d['trees'] + d['blobs'], f"{d['blobs']}", f"{d['raw_bytes']/1e6:.0f}", d['zones'], d['head_objects']])
    return table(['dataset', 'commits', 'objects', 'blobs', 'raw MB', 'zones (lab rule)', 'HEAD objects'], out)

def main():
    R = load(sys.argv[1])
    t = open(sys.argv[2]).read()
    def rep(m):
        a = m.group(1).split(':')
        k = a[0]
        if k == 'datasets': return datasets_table(R)
        if k == 'A': return tbl_A(R, a[1])
        if k in ('C1', 'C2', 'C3'): return tbl_C(R, a[1], k)
        if k == 'E1': return tbl_E1(R, a[1], a[2])
        if k == 'E2': return tbl_E2(R, a[1])
        if k == 'F': return tbl_F(R, a[1], a[2])
        if k == 'G': return tbl_G(R, a[1], a[2])
        if k == 'C4': return tbl_C4(R, a[1])
        if k == 'SB': return tbl_SB(R)
        if k == 'meta': return tbl_meta(R, a[1])
        raise SystemExit('bad placeholder ' + m.group(0))
    print(re.sub(r'\{\{tbl:([^}]*)\}\}', rep, t))
main()
