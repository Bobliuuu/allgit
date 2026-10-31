#!/usr/bin/env python3
"""Tests for storage_score.py using the hand-made synthetic example (not measurements).

Run: python3 scripts/test_storage_score.py
"""
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import storage_score as s  # noqa: E402

EXAMPLE = os.path.join(HERE, "..", "docs", "research", "storage", "data", "synthetic-example-bench.json")


class Score(unittest.TestCase):
    def frontier_names(self, group, objs, maximize=()):
        rows = [r for r in s.load([EXAMPLE]) if (r["workload"], r["cache"]) == group]
        res = s.score(rows, objs, maximize)
        (g,) = res
        return {e["design"] for e in g["entries"] if e["on_frontier"]}, g

    def test_three_objective_frontier_by_hand(self):
        # cold: (bytes, gets, p95 median)  A(1000,900,90) B(800,100,70) C(500,100,100) D(600,20,210)
        # A dominated by B (all worse). B vs C: C fewer bytes, B faster -> incomparable.
        # D vs C: D fewer gets, C fewer bytes & faster -> incomparable. E is incorrect -> disqualified.
        names, g = self.frontier_names(("w-sparse", "cold"), ["stored_bytes", "gets", "p95_ms"])
        self.assertEqual(names, {"B-pack", "C-pack-zstd", "D-bigblock"})
        e = {x["design"]: x for x in g["entries"]}
        self.assertEqual(e["A-loose"]["dominated_by"], ["B-pack"])
        self.assertTrue(e["E-broken"].get("disqualified"))

    def test_two_objective_frontier(self):
        # drop gets: bytes vs p95. B(800,70) C(500,100) D(600,210) A(1000,90)
        # D dominated by C (500<600, 100<210); A dominated by B
        names, _ = self.frontier_names(("w-sparse", "cold"), ["stored_bytes", "p95_ms"])
        self.assertEqual(names, {"B-pack", "C-pack-zstd"})

    def test_warm_group_and_median(self):
        names, g = self.frontier_names(("w-sparse", "warm"), ["stored_bytes", "p95_ms"])
        self.assertEqual(names, {"B-pack"})  # B: 800 bytes, median p95 = 4; A: 1000, 5
        b = [e for e in g["entries"] if e["design"] == "B-pack"][0]
        self.assertEqual(b["values"]["p95_ms"], 4.0)
        self.assertAlmostEqual(b["spread"]["p95_ms"], 0.5)  # (6-4)/4

    def test_maximize_flips_direction(self):
        rows = [{"design": "x", "values": {"m": 1.0}}, {"design": "y", "values": {"m": 2.0}}]
        fr = {r["design"]: d for r, d in s.frontier(rows, ["m"], ["m"])}
        self.assertEqual(fr["y"], [])
        self.assertEqual(fr["x"], ["y"])

    def test_equal_points_both_kept(self):
        rows = [{"design": "x", "values": {"m": 1.0}}, {"design": "y", "values": {"m": 1.0}}]
        self.assertTrue(all(not d for _, d in s.frontier(rows, ["m"])))

    def test_render_smoke(self):
        rows = s.load([EXAMPLE])
        out = s.render(s.score(rows, ["stored_bytes", "gets"]), ["stored_bytes", "gets"])
        self.assertIn("DISQUALIFIED", out)
        self.assertIn("workload=w-sparse", out)

    def test_price_class_adds_cost(self):
        rows = s.load([EXAMPLE], "s3_standard")
        self.assertIn("monthly_cost_usd", rows[0]["values"])


if __name__ == "__main__":
    unittest.main()
