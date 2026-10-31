#!/usr/bin/env python3 -I
"""Render compress-lab JSON results as markdown tables. usage: render.py <codecs|chunking|blocks> file.json [codec-filter]"""
import json, sys

def mib(b): return f"{b/1048576:.2f}"
def kind(a):
    return a
def codecs(d):
    print(f"#### {d['dataset']['name']} (unique {mib(d['dataset']['unique_bytes'])} MiB, {d['dataset']['revisions']} revisions)\n")
    for c in d["classes"]:
        print(f"Class `{c['class']}`: sample {c['sample_files']} files, {mib(c['sample_bytes'])} MiB\n")
        print("| codec | physical MiB | ratio | compress MB/s (median [min-max]) | decompress MB/s | compress peak RSS delta KiB |\n|---|---|---|---|---|---|")
        for r in c["codecs"]:
            lo, hi = r["compress_MBps_min_max"]
            print(f"| {r['codec']} | {mib(r['physical_bytes'])} | {r['ratio']:.3f} | {r['compress_MBps_median']:.0f} [{lo:.0f}-{hi:.0f}] (n={r['reps']}) | {r['decompress_MBps_median']:.0f} | {r['compress_peak_rss_delta_KiB']} |")
        print("\n| adaptive policy | physical MiB | note |\n|---|---|---|")
        for a in c["adaptive"]:
            extra = f"stored raw: {a['files_stored_raw']} files; probe {a['probe_MBps']:.0f} MB/s" if "files_stored_raw" in a else ""
            print(f"| {a['policy']} | {mib(a['physical_bytes'])} | {extra} |")
        dc = c.get("dictionary")
        if isinstance(dc, list):
            print("\n| dict | level | size bucket | files | zstd no-dict KiB | zstd dict KiB | gain % |\n|---|---|---|---|---|---|---|")
            for e in dc:
                if "buckets" not in e: print(f"| {e.get('dict_bytes')} | | error {e.get('error')} | | | | |"); continue
                for b in e["buckets"]:
                    print(f"| {e['dict_bytes']//1024} KiB (train {e['train_secs']:.1f}s, {e['train_files']} files) | {e['level']} | {b['size_bucket']} | {b['files']} | {b['zstd_nodict']/1024:.0f} | {b['zstd_dict']/1024:.0f} | {b['gain_pct']:.1f} |")
        elif isinstance(dc, dict) and dc: print(f"\nDictionary: {dc}")
        print()
def chunking(d):
    ds = d["dataset"]
    print(f"#### {ds['name']} ({ds['revisions']} revisions, logical {mib(ds['logical_bytes_all_revisions'])} MiB, unique blobs {mib(ds['unique_bytes'])} MiB)\n")
    print("| strategy | unique chunks | avg chunk B | unique zstd3 MiB | manifest KiB* | index KiB* | total MiB | total/logical | cross-rev gain % | per-zone store extra KiB | chunk+hash MB/s |\n|---|---|---|---|---|---|---|---|---|---|---|")
    for r in d["strategies"]:
        z = r["zoned_stores"]
        print(f"| {r['strategy'].split('(')[0]} | {r['unique_chunks']} | {r['avg_chunk_bytes']:.0f} | {mib(r['unique_comp_bytes_zstd3'])} | {r['manifest_bytes_model']/1024:.1f} | {r['index_bytes_model']/1024:.1f} | {mib(r['physical_total_model'])} | {r['physical_over_logical']:.4f} | {r['cross_revision_dedup_gain_comp_pct']:.1f} | {(z['extra_comp_vs_global_bytes']+z['extra_index_bytes_model'])/1024:.1f} | {r['chunk_hash_MBps_median_1thread']:.0f} |")
    print("\n(* modelled from the overhead model in the JSON)\n")
    print("Bytes added by the LAST revision (zstd3, before overhead):\n\n| strategy | new raw KiB | new comp KiB |\n|---|---|---|")
    for r in d["strategies"]:
        l = r["per_revision"][-1]
        print(f"| {r['strategy'].split('(')[0]} | {l['new_raw']/1024:.1f} | {l['new_comp']/1024:.1f} |")
    print()
def blocks(d):
    print(f"#### {d['dataset']['name']} stream order: {d['stream_order']}\n")
    for c in d["classes"]:
        print(f"Class `{c['class']}`, stream {mib(c['stream_bytes'])} MiB\n")
        print("| codec | block | ratio | compress MB/s | decompress MB/s | idx KiB* | 4K warm us med/p99 | 4K read amp (compressed bytes/4K) | 4K file-cold us med | 1M warm us med | comp. peak RSS KiB |\n|---|---|---|---|---|---|---|---|---|---|---|")
        for r in c["rows"]:
            b = r["block_bytes"]; bs = b if b == "solid" else (f"{b>>20}M" if b >= 1<<20 else f"{b>>10}K")
            k = r.get("read4K", {}); m = r.get("read1M", {})
            print(f"| {r['codec']} | {bs} | {r['ratio']:.3f} | {r['compress_MBps_median']:.0f} | {r['decompress_MBps_median']:.0f} | {r['block_index_bytes_model']/1024:.1f} | {k.get('warm_us_median',0):.0f}/{k.get('warm_us_p99',0):.0f} | {k.get('read_amplification_vs_requested',0):.0f}x | {r.get('read4K_file_cold_us_median',float('nan')):.0f} | {m.get('warm_us_median',float('nan')):.0f} | {r['compress_peak_rss_delta_KiB']} |")
        print()
f = {"codecs": codecs, "chunking": chunking, "blocks": blocks}[sys.argv[1]]
f(json.load(open(sys.argv[2])))
