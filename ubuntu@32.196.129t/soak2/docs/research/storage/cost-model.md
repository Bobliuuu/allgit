# Cloud cost model

Issue #181, epic #190. Pricing retrieved **2026-10-31**, USD, list (pay-as-you-go) prices, no discounts, committed use,
or taxes. Executable: `scripts/storage_cost_model.py` (stdlib Python 3); price table:
`data/pricing-2026-10-31.json`; tests: `python3 scripts/test_storage_cost_model.py`.

```
python3 scripts/storage_cost_model.py --example                       # the worked example below
python3 scripts/storage_cost_model.py --workload w.json [--classes s3_standard,r2_standard] [--json]
```

Workload JSON (per month): `stored_bytes`, `puts` (PUT/COPY/POST, maps to "Class A" on R2/GCS), `lists`, `gets`
(GET/HEAD, "Class B"), `deletes`, `egress_bytes`, optional `read_bytes` (for retrieval-fee classes, default =
`egress_bytes`), `object_count`, `avg_lifetime_days`, `months`, `apply_free_tiers`.

## 1. Prices used and where they came from

"Revision" is the document version the provider exposes; most provider pages expose none, in which case the fetch
date is the revision. All accessed 2026-10-31.

| Provider / class | Storage | PUT/COPY/POST/LIST | GET | Egress to internet | Other |
|---|---|---|---|---|---|
| AWS S3 Standard, us-east-1 | $0.023 /GB-mo (first 50 TB; $0.022 next 450 TB; $0.021 over) | $0.005 /1k | $0.0004 /1k | $0.09 /GB first 10 TB, $0.085 next 40 TB, $0.07 next 100 TB, $0.05 over 150 TB | DELETE free |
| AWS S3 Standard-IA, us-east-1 | $0.0125 /GB-mo | $0.01 /1k | $0.001 /1k | as above | retrieval $0.01/GB; min billable object 128 KB; min duration 30 d |
| Cloudflare R2 Standard | $0.015 /GB-mo | Class A $4.50 /M | Class B $0.36 /M | $0 | free tier 10 GB-mo, 1M Class A, 10M Class B per month; DELETE/Abort free |
| Cloudflare R2 Infrequent Access | $0.01 /GB-mo | Class A $9.00 /M | Class B $0.90 /M | $0 | retrieval $0.01/GB; min duration 30 d; no free tier |
| GCS Standard, us-central1 (Iowa) | $0.02 /GiB-mo (derived: $0.000027397/GiB-hour x 730 h) | Class A $0.005 /1k | Class B $0.0004 /1k | Premium tier $0.12 /GiB first 1 TiB, $0.11 to 10 TiB, $0.08 above (worldwide excl. Australia) | none min duration |
| GCS Nearline, us-central1 | $0.01 /GiB-mo ($0.000013699 x 730) | $0.01 /1k | $0.001 /1k | as above | retrieval $0.01/GiB; min duration 30 d |
| Backblaze B2 | $6.95 /TB-mo = $0.00695 /GB | Class A free | Class B free | free up to 3x average monthly stored bytes, then $0.01/GB | Class C free; Class D (notifications) $0.004/10k after 2,500/day; no min size or duration fees; first 10 GB storage free |

Sources:

* AWS S3 storage and request prices: AWS Price List bulk API, offer AmazonS3, version `20261015173540`,
  publication 2026-10-15, `https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonS3/current/index.json`
  (SKUs `TimedStorage-ByteHrs`, `TimedStorage-SIA-ByteHrs`, `Requests-Tier1/2`, `Requests-SIA-Tier1/2`,
  `Retrieval-SIA`, filtered to `regionCode=us-east-1`). The public pricing page
  `https://aws.amazon.com/s3/pricing/` renders its tables with JavaScript and could not be read directly; it was
  used for: DELETE/CANCEL free, first 100 GB/month data transfer out free (aggregated across services),
  Standard-IA 128 KB minimum billable size and 30-day minimum duration, Glacier Instant Retrieval 128 KB / 90 days.
* AWS data transfer out: Price List offer AWSDataTransfer, publication 2026-09-16,
  `https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AWSDataTransfer/current/index.json`
  (`fromRegionCode=us-east-1`, `transferType=AWS Outbound`).
* Cloudflare R2: `https://developers.cloudflare.com/r2/pricing/` (no revision id exposed).
* Google Cloud Storage: `https://cloud.google.com/storage/pricing` (storage per GiB-hour, operations table "single
  region", retrieval and minimum-duration tables) and `https://cloud.google.com/vpc/network-pricing` (internet data
  transfer, Premium Tier). See caveat C1.
* Backblaze B2: `https://www.backblaze.com/cloud-storage/pricing` and
  `https://www.backblaze.com/cloud-storage/transaction-pricing` (both state classes A, B, C "Cost: Free").

## 2. Assumptions and caveats

* A1. Unit conversions: AWS, R2 and B2 prices are applied per 10^9 bytes ("GB"); GCS prices are explicitly per gibibyte
  and are applied per 2^30 bytes. Whether AWS bills "GB" as 10^9 or 2^30 bytes was not verified in the documents
  fetched; using 10^9 for AWS understates AWS cost by up to 7.4% [unverified]. R2 and B2 decimal units are also
  [unverified]. B2's "TB" is taken as 10^12 bytes.
* A2. A month is 730 hours for the GCS hourly-to-monthly conversion. S3 storage is applied flat at the first-tier
  rate (correct below 50 TB).
* A3. Free tiers are off by default (`apply_free_tiers=false`) because they are account-level and would hide
  unit economics. When on: R2 free tier amounts and the AWS 100 GB/month egress free allowance are subtracted. B2's 3x
  stored-bytes egress allowance is always applied because it is part of the published rate.
* A4. Request counts are caller-supplied. The model does not infer request counts from a design; the benchmark
  harness measures them (benchmark-methodology.md, 2.3). One ranged GET is one GET request.
* A5. LIST is billed at the PUT/Class A rate on S3, GCS and R2. (S3: stated on the pricing page; R2 and GCS Class A
  includes list operations by their operation-class tables. GCS list classification was read from the pricing page
  operation-class text only partially [unverified for individual operations].)
* A6. Min object size is modelled by billing each object at least the minimum (`object_count * min`) when
  `object_count` is given. Min duration is modelled as `storage_cost * min_days / avg_lifetime_days` when the
  average lifetime is shorter; both are approximations using means.
* A7. Not modelled: multipart request counts (each part is a PUT), retries, KMS/encryption request charges, versioning
  and non-current version storage, replication and inter-region transfer, S3 Intelligent-Tiering or Glacier classes,
  tax, support plans, S3 Express One Zone, CDN/cache egress paths, and volume tiers for storage.
* A8. The cloud region of the client matters: egress is zero for same-region compute on S3 (not modelled; set
  `egress_bytes` to 0 for in-region clients) [not verified in this session].

Caveats:

* C1. GCS: the storage price table in the fetched HTML is a region selector. Parsing the page text shows the first
  region in each selector list is Iowa (us-central1) and the first price row is $0.000027397/GiB-hour (= $0.02/GiB-month
  at 730 h). The mapping "first price = first region" is an inference from the page layout [partly unverified]; other
  regions differ. Data transfer prices are given per source-region groups; the first table fetched (Premium tier,
  worldwide) is used.
* C2. B2 publishes a single price with a "U.S. West" comparison note; B2 has no per-region price difference stated on
  the fetched page.
* C3. Provider prices change. Re-run the retrieval and bump the date in the filename. The AWS Price List URLs above
  are machine-readable and are the preferred refresh source for AWS.

## 3. Formulae

```
storage   = months * max(0, billed_stored / unit - free_storage) * storage_per_unit * early_delete_factor
requests  = months * (puts * put_per_1k + lists * list_per_1k + gets * get_per_1k + deletes * delete_per_1k) / 1000
egress    = months * tiered(max(0, egress_bytes / egress_unit - free_units), egress_tiers)      (B2: minus 3 * stored)
retrieval = months * read_bytes / unit * retrieval_per_unit                                   (IA/Nearline classes)
total     = storage + requests + egress + retrieval
```

## 4. Hand-calculated example (the acceptance test)

Workload: 1 TB = 10^12 bytes stored, 10,000,000 PUT, 100,000,000 GET, 500 GB (5x10^11 B) egress, 1 month, list prices.

| | storage | requests | egress | total |
|---|---|---|---|---|
| S3 Standard us-east-1 | 1000 GB x 0.023 = 23.00 | 10,000 x 0.005 + 100,000 x 0.0004 = 50.00 + 40.00 = 90.00 | 500 x 0.09 = 45.00 | **158.00** |
| R2 Standard | 1000 x 0.015 = 15.00 | 10 x 4.50 + 100 x 0.36 = 45.00 + 36.00 = 81.00 | 0 | **96.00** |
| GCS Standard us-central1 | 1e12 / 2^30 = 931.3226 GiB x 0.02 = 18.63 | 50.00 + 40.00 = 90.00 | 5e11 / 2^30 = 465.6613 GiB x 0.12 = 55.88 | **164.51** |
| B2 | 1000 x 0.00695 = 6.95 | 0 | 500 GB < 3 x 1000 GB free: 0 | **6.95** |

`scripts/test_storage_cost_model.py::test_hand_numbers_literal` asserts these literal totals (158.00, 96.00, 164.51,
6.95), and `test_hand_example` asserts the formula form. Running `--example` prints the same numbers. Result: the
model reproduces the hand calculation (acceptance criterion).

## 5. Unit economics (derived from the table; useful for design reasoning)

| | S3 | R2 | GCS | B2 |
|---|---|---|---|---|
| per 1M PUT | $5.00 | $4.50 | $5.00 | $0 |
| per 1M GET | $0.40 | $0.36 | $0.40 | $0 |
| GET costs as much as egress of N bytes (first tier) | 4,444 B | not applicable (egress free) | 3,579 B | not applicable |
| storage per TB-month | $23.00 | $15.00 | $20.48 per TiB ($18.63 per 10^12 B) | $6.95 |

Reading: on S3 and GCS, a GET request costs the same as moving only about 3.6 to 4.4 KB of egress, so for
internet-egress-paying clients, byte transfer dominates request cost for reads over a few KB, and PUT count
dominates write cost (12.5x the GET price). On R2 and B2 egress is free or mostly free, so request count (R2) or
nothing but storage (B2, within the 3x egress allowance) is what is billed. This is arithmetic on the list prices;
what it means for allgit's pack sizing is a HYPOTHESIS to be tested by #184.

Implication for object sizing (HYPOTHESIS): per-object PUT cost on S3 is $5 per million, so uploading 100 million
1 KiB loose objects costs $500 in PUT requests alone versus about $0.50 for the same bytes in 1 MiB packs
(100,000 PUTs of 1 MiB = about 100 GB). Packing therefore changes cost by orders of
magnitude on S3/GCS/R2 and not at all on B2.

## 6. Cost estimate for the conformance runs (no spend is authorised by this document)

`conformance-test-plan.md` defines a worst-case workload of 3,000 PUT, 300 LIST, 6,000 GET, 3,000 DELETE and 1 GiB
stored and egressed. Model output (upper bound, storage charged for a whole month):
S3 $0.14, GCS $0.16, R2 $0.03, B2 $0.01 (before free tiers). Command:
`python3 scripts/storage_cost_model.py --workload w.json --classes s3_standard,r2_standard,gcs_standard,b2_standard`.

## 7. Negative / unverifiable items

* AWS public pricing page tables and GCS region tables are JavaScript-driven; values were taken from the AWS Price
  List API and the GCS HTML text respectively, not from rendered tables.
* R2 and B2 rounding rules beyond "rounds up usage to the next billing unit" (R2) were not found.
* Prices for S3 Glacier classes, S3 Express One Zone, GCS Coldline/Archive and B2 Overdrive exist on the same
  pages but are not modelled.
