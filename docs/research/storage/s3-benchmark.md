# S3-compatible provider benchmark (`s3bench`)

Status: **no cloud run has been made yet** (the lab IAM user cannot create buckets, see issue #187). Local runs against RustFS, MinIO (source build), SeaweedFS, Garage and moto are in `provider-comparison.md` under "LOCAL (not cloud)"; they are not cloud results.

`crates/vcs-bench/src/bin/s3bench.rs` checks the contract allgit needs from an authoritative backend and measures latency, using the product's own `S3Backend` (so it exercises the real request signing, conditional headers and range reads):

- Contract checks: create-only PUT (`If-None-Match: *`), refused second create, CAS with `If-Match`, stale-tag refusal, CAS on a missing key, **ETag A-B-A hazard** (does rewriting old content reuse the old ETag? if yes a manifest must embed an epoch), exact range reads, range past end, list, and "exactly one winner among 8 concurrent CAS writers" for 5 rounds.
- Latency (p50/p95/p99/max): `put_if_absent`, `get`, `head`, `get_range`, refused create, for 1 KiB, 64 KiB and 1 MiB objects; an 8 KiB manifest CAS loop; GET throughput at 1, 4, 16 and 32 threads.
- Cost guard: aborts when requests or bytes exceed `--max-requests` (default 4000) or `--max-mb` (default 300). With the defaults a run is roughly 2,500 requests and under 100 MB transferred; at AWS list prices (see `cost-model.md`) that is well under one US dollar.

## Run it (operator)

```sh
cargo build --release -p vcs-bench --features s3-tls --bin s3bench
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... [AWS_SESSION_TOKEN=...]
aws s3api create-bucket --bucket allgit-bench-$RANDOM --region us-east-1      # private by default
target/release/s3bench https://s3.us-east-1.amazonaws.com <bucket> us-east-1 out.json
aws s3 rm s3://<bucket> --recursive && aws s3api delete-bucket --bucket <bucket>
```

Other providers: pass their endpoint and region (R2: `https://<account>.r2.cloudflarestorage.com`, region `auto`; MinIO: `http://host:9000`). Record the machine and its network location with the results; client-side latency depends on it.

Local servers: add `--create-bucket` to create the bucket first, `--ca-pem FILE` to trust a private CA for `https://` endpoints, and `--no-probe` to characterise a provider the product would refuse (research only).
