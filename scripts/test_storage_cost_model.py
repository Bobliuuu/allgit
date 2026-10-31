#!/usr/bin/env python3
"""Tests: the model must reproduce the hand-calculated example in cost-model.md.

Run: python3 scripts/test_storage_cost_model.py
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import storage_cost_model as m  # noqa: E402

# Hand calculation (see docs/research/storage/cost-model.md, section "Hand-calculated example"):
# workload: 1 TB (1e12 B) stored, 10M PUT, 100M GET, 500 GB egress, 1 month, list prices.
HAND = {
    # 1000*0.023 + 10000*0.005 + 100000*0.0004 + 500*0.09
    "s3_standard": 23.00 + 50.00 + 40.00 + 45.00,
    # 1000*0.015 + 10M/1M*4.50 + 100M/1M*0.36 + 0 egress
    "r2_standard": 15.00 + 45.00 + 36.00,
    # 1e12/2^30 GiB * 0.02 + 50 + 40 + (5e11/2^30 GiB)*0.12
    "gcs_standard": (1e12 / 2**30) * 0.02 + 50.00 + 40.00 + (5e11 / 2**30) * 0.12,
    # 1000 GB * 6.95/1000; transactions free; 500 GB egress < 3x stored (3000 GB) so free
    "b2_standard": 6.95,
}


class CostModel(unittest.TestCase):
    def test_hand_example(self):
        res = m.run(m.EXAMPLE_WORKLOAD)
        for k, want in HAND.items():
            self.assertAlmostEqual(res[k]["total"], want, places=6, msg=k)

    def test_hand_numbers_literal(self):
        # literal values worked out by hand with a calculator, independent of the formulae above
        res = m.run(m.EXAMPLE_WORKLOAD)
        self.assertAlmostEqual(res["s3_standard"]["total"], 158.00, places=2)
        self.assertAlmostEqual(res["r2_standard"]["total"], 96.00, places=2)
        self.assertAlmostEqual(res["gcs_standard"]["total"], 164.51, places=2)
        self.assertAlmostEqual(res["b2_standard"]["total"], 6.95, places=2)

    def test_tiered_egress(self):
        # 20000 units through [[10240,.09],[51200,.085]...]: 10240*.09 + 9760*.085
        self.assertAlmostEqual(m.tiered(20000, [[10240, 0.09], [51200, 0.085], [None, 0.05]]),
                               10240 * 0.09 + 9760 * 0.085, places=6)

    def test_b2_egress_overage(self):
        w = {"stored_bytes": 10**12, "egress_bytes": 4 * 10**12}  # 4 TB out, 3 TB free, 1 TB at $0.01/GB
        self.assertAlmostEqual(m.run(w)["b2_standard"]["egress"], 10.0, places=6)

    def test_min_object_size_and_duration_ia(self):
        # 1M objects of 10 KB (10 GB stored) in S3 IA: billed at 128 KiB each = 131072e6 B
        w = {"stored_bytes": 10**10, "object_count": 10**6}
        st = m.run(w)["s3_standard_ia"]["storage"]
        self.assertAlmostEqual(st, 131072e6 / 1e9 * 0.0125, places=6)
        # lifetime 3 days with 30-day minimum -> 10x storage charge
        w2 = {"stored_bytes": 10**12, "avg_lifetime_days": 3}
        self.assertAlmostEqual(m.run(w2)["s3_standard_ia"]["storage"], 1000 * 0.0125 * 10, places=6)

    def test_retrieval_fee_ia(self):
        w = {"stored_bytes": 0, "egress_bytes": 100 * 10**9}
        self.assertAlmostEqual(m.run(w)["s3_standard_ia"]["retrieval"], 100 * 0.01, places=6)
        self.assertEqual(m.run(w)["s3_standard"]["retrieval"], 0.0)

    def test_free_tier_flag(self):
        w = {"stored_bytes": 10 * 10**9, "puts": 10**6, "gets": 10**7, "apply_free_tiers": True}
        self.assertAlmostEqual(m.run(w)["r2_standard"]["total"], 0.0, places=9)


if __name__ == "__main__":
    unittest.main()
