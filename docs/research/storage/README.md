# Storage engine research (epic #190)

Index of documents under `docs/research/storage/`. Add new documents here, one line each.

| Document | Issue | Content |
|---|---|---|
| [benchmark-methodology.md](benchmark-methodology.md) | #181 | Metric definitions, measurement protocol, Pareto scoring, result schema |
| [cost-model.md](cost-model.md) | #181 | Provider pricing (retrieved 2026-10-31) and the executable cost model |
| [provider-comparison.md](provider-comparison.md) | #181 | Capability matrix: S3, R2, GCS, B2, MinIO, RustFS, Ceph RGW |
| [conformance-test-plan.md](conformance-test-plan.md) | #181 | `Backend` trait contract tests and per-provider operator commands |
| [workloads.md](workloads.md) | #182 | The 16 seeded dataset generators of `storebench` (parameters, purpose, limits) |

Scripts: `scripts/storage_cost_model.py`, `scripts/storage_score.py` (tests: `scripts/test_storage_*.py`). Data: `data/`.
