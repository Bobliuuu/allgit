#!/usr/bin/env python3
"""Executable cloud-storage cost model for allgit storage-engine research (issue #181).

Reads the pricing table (docs/research/storage/data/pricing-*.json) and a workload JSON,
prints the monthly (or N-month) cost per provider/class, broken down by component.
No network access, no credentials. Standard library only.

Usage:
  storage_cost_model.py --workload w.json [--pricing p.json] [--classes a,b] [--json]
  storage_cost_model.py --example        # prints the hand-calculated example

Workload fields (all optional except stored_bytes), amounts are per month:
  stored_bytes        physical bytes stored (after dedup/compression)
  object_count        number of objects (enables min-billable-size modelling)
  puts, lists, gets, deletes   request counts per month (PUT/COPY/POST map to Class A)
  egress_bytes        bytes leaving the provider network to the internet per month
  read_bytes          bytes read (retrieval-fee classes); defaults to egress_bytes
  avg_lifetime_days   mean object lifetime; below min_duration_days triggers early-delete charge
  months              horizon (default 1); storage and requests are multiplied by it
  apply_free_tiers    bool, default false (list prices; free tiers are account-level)
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_PRICING = os.path.join(HERE, "..", "docs", "research", "storage", "data", "pricing-2026-10-31.json")

EXAMPLE_WORKLOAD = {
    "stored_bytes": 10**12,
    "puts": 10_000_000,
    "gets": 100_000_000,
    "egress_bytes": 500 * 10**9,
}


def tiered(units, tiers):
    """Cost of `units` through [[upto_inclusive_or_None, price], ...] tiers (cumulative bounds)."""
    cost, prev = 0.0, 0.0
    for upto, price in tiers:
        if units <= prev:
            break
        top = units if upto is None else min(units, upto)
        cost += (top - prev) * price
        prev = top if upto is None else upto
    return cost


def class_cost(c, w):
    months = float(w.get("months", 1))
    free = bool(w.get("apply_free_tiers", False))
    stored = float(w["stored_bytes"])
    n = w.get("object_count")
    mn = c.get("min_object_bytes", 0)
    billed_stored = stored
    if n and mn:
        billed_stored = max(stored, n * mn)  # each object billed at least mn bytes (approximation: uses mean)
    ub = c["unit_bytes"]
    storage_units = billed_stored / ub
    ft = c.get("free_tier", {}) if free else {}
    storage = max(0.0, storage_units - ft.get("storage_units", 0)) * c["storage_per_unit"] * months
    life = w.get("avg_lifetime_days")
    md = c.get("min_duration_days", 0)
    if life and md and life < md:
        storage *= md / float(life)  # early-deletion: billed as if kept md days
    n_put = float(w.get("puts", 0))
    n_list = float(w.get("lists", 0))
    n_get = float(w.get("gets", 0))
    n_del = float(w.get("deletes", 0))
    class_a_free = ft.get("class_a_ops", 0)
    class_b_free = ft.get("class_b_ops", 0)
    a_cost = max(0.0, n_put + n_list - class_a_free)
    # split a_cost back onto put/list prices proportionally (they are equal for all current classes)
    if n_put + n_list > 0:
        put_c = a_cost * (n_put / (n_put + n_list)) / 1000 * c["put_per_1k"]
        list_c = a_cost * (n_list / (n_put + n_list)) / 1000 * c["list_per_1k"]
    else:
        put_c = list_c = 0.0
    get_c = max(0.0, n_get - class_b_free) / 1000 * c["get_per_1k"]
    del_c = n_del / 1000 * c.get("delete_per_1k", 0.0)
    requests = (put_c + list_c + get_c + del_c) * months
    eub = c.get("egress_unit_bytes", ub)
    egress_units = float(w.get("egress_bytes", 0)) / eub
    if free and c.get("egress_free_monthly_units"):
        egress_units = max(0.0, egress_units - c["egress_free_monthly_units"])
    if c.get("egress_free_multiple_of_stored"):
        # B2: free egress up to Nx average monthly storage
        egress_units = max(0.0, egress_units - c["egress_free_multiple_of_stored"] * (stored / eub))
    egress = tiered(egress_units, c["egress_tiers"]) * months
    read_units = float(w.get("read_bytes", w.get("egress_bytes", 0))) / ub
    retrieval = read_units * c.get("retrieval_per_unit", 0.0) * months
    total = storage + requests + egress + retrieval
    return {
        "storage": storage, "requests": requests, "egress": egress, "retrieval": retrieval,
        "total": total,
    }


def load_pricing(path=None):
    with open(path or DEFAULT_PRICING) as f:
        return json.load(f)


def run(workload, pricing=None, classes=None):
    p = pricing or load_pricing()
    out = {}
    for name, c in p["classes"].items():
        if classes and name not in classes:
            continue
        out[name] = class_cost(c, workload)
    return out


def format_table(res, pricing):
    rows = [("class", "provider/region", "storage", "requests", "egress", "retrieval", "total")]
    for k, v in sorted(res.items(), key=lambda kv: kv[1]["total"]):
        c = pricing["classes"][k]
        rows.append((k, f'{c["provider"]} {c["region"].split(" ")[0]}',
                     *(f"{v[x]:.2f}" for x in ("storage", "requests", "egress", "retrieval", "total"))))
    w = [max(len(r[i]) for r in rows) for i in range(7)]
    return "\n".join("  ".join(r[i].ljust(w[i]) if i < 2 else r[i].rjust(w[i]) for i in range(7)) for r in rows)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--workload")
    ap.add_argument("--pricing")
    ap.add_argument("--classes")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--example", action="store_true")
    a = ap.parse_args(argv)
    if a.example:
        w = EXAMPLE_WORKLOAD
    elif a.workload:
        with open(a.workload) as f:
            w = json.load(f)
    else:
        ap.error("need --workload or --example")
    p = load_pricing(a.pricing)
    res = run(w, p, a.classes.split(",") if a.classes else None)
    if a.json:
        print(json.dumps({"pricing_retrieved": p["_meta"]["retrieved"], "workload": w, "cost_usd": res}, indent=2))
    else:
        print(f'pricing retrieved {p["_meta"]["retrieved"]}; workload {json.dumps(w)}')
        print(format_table(res, p))
    return 0


if __name__ == "__main__":
    sys.exit(main())
