# Concurrency and correctness: metadata designs, GC, failure matrix

Issue #186 (experiments K and L, Phase 8), epic #190. Branch `research/storage-meta`, base `research/storage-engine`.
Status of every claim is marked **CONFIRMED** (checked by code in this repo, test named) or **HYPOTHESIS** (argued, not
executed). Nothing here changes production code or formats. The vocabulary is "zone".

Evidence added by this change, all under `crates/vcs-remote/tests/`:

| File | What | Result (Mac, debug build, 2026-10-31) |
|---|---|---|
| `model_sharded.rs` | model of sharded manifests with a two-phase cross-zone commit, 2 writers + 1 reader, one fault per schedule | 4 safety scenarios pass; 5 mutation tests fail as required; 1 negative-result test (10 tests) |
| `model_gc.rs` | model of a GC protocol (tombstone handshake) against a concurrent publisher | 1 safety test passes; 7 mutation tests fail as required |
| `faults.rs` | fault injection against the real fetch/publish code: corrupt/truncated/missing packs, index corruption, missing chunks, undecodable objects, damaged remote | 10 tests pass, two document known gaps |

State counts are **distinct states** of a memoised search (not schedules): sharded conflict scenario 42,803, multi-vs-single
15,922, disjoint 9,926, three writers 4,327, GC model 4,532 + 3,572 (two publisher variants). The existing `model.rs` /
`model_locks.rs` count schedules (the issue quotes about 1.7M; not re-measured here). The two kinds of number are not comparable.
Run time of the new models: about 1 s total. Reproduce: `cargo test -p vcs-remote --test model_sharded --test model_gc --test faults -- --nocapture`.

## 1. Metadata designs vs atomic multi-zone commits

The requirement that dominates everything: **one transaction may update branches in several zones and either all or none
become visible**, readers see a consistent set of zone heads, a lost acknowledgement never double-applies, and a crashed
writer can always be recovered by re-running the same transaction.

Current design (CONFIRMED from `protocol.rs`, `manifest.rs`, `model.rs`): one object `manifest` holding generation, all branch
heads, snapshots, applied-tx window (`MAX_APPLIED` = 4096 ids), zones, labels, locks, fencing counter, validator time
high-water mark. Publish = upload objects post-order, create-only `tx/<txid>`, load manifest, validate every `old`, one CAS.
The CAS is the only linearization point; a reader gets a consistent snapshot with a single GET. Atomicity of multi-zone
transactions is free because there is one object.

Costs of the current design (CONFIRMED by reading the code; magnitudes not measured here): every publish rewrites the whole
manifest, so CAS payload grows with the number of branches, locks and applied ids; every publisher, regardless of zone,
contends on one key (disjoint-zone writers succeed but only after a CAS loss and a reload, `disjoint_zones_both_succeed`);
no history is kept (`gc.md` item 2).

### Comparison

Backend primitives assumed (as in `backend.rs`): read-after-write, create-only PUT, CAS PUT on a version tag, list. "Needs
CAS" means If-Match; "create-only" means If-None-Match. Request counts are for a transaction touching k zones.

| Design | Atomic k-zone commit | Primitive | Publish requests (metadata) | Reader snapshot | Contention | Recovery of crashed writer | History / GC interplay | Verdict |
|---|---|---|---|---|---|---|---|---|
| **Single-manifest CAS** (today) | Free: one CAS | CAS | 1 load + 1 CAS | 1 GET, trivially consistent | All writers on one key; manifest size grows with repo | Re-run same tx; `applied` window detects done | No history; roots = one manifest | Keep as default. Optimise size (split cold fields) before changing protocol. |
| **WAL / log** (`log/<n>` create-only, tail by listing or hint) | Free: one create-only of entry n+1 is the commit | create-only only (no If-Match needed) | 1 list/hint + 1 create | Replay checkpoint + tail | One key per position; losers retry at n+2 | Entry exists or not; txid inside entry gives idempotence | Full history; checkpoint + truncate = retention; GC roots are explicit | Best fallback for backends without If-Match (HYPOTHESIS, same safety argument as CAS since create-only is a CAS on an absent key). Reader cost needs a checkpoint. |
| **Append-only generations** (`manifest/<g>` create-only, full state each) | Free | create-only | 1 list/hint + 1 create | 1 GET of newest | Same as CAS | Same | Old generations are the history; deleting them is retention | Same as CAS plus history for the price of storing each generation (HYPOTHESIS: cost not measured). |
| **Snapshot metadata** (Iceberg/Delta style: pointer to a tree of manifest files, pointer CAS) | Free: pointer CAS | CAS (pointer) | Writes new small manifest files + 1 CAS | 1 pointer GET + tree walk | Same pointer; but payload per commit is small and unchanged parts are shared | Orphan metadata files are garbage | Snapshots are first-class roots; expiry is explicit | Right shape if the manifest outgrows one object (HYPOTHESIS). Adds GC of metadata files. |
| **Sharded manifests** (per-zone shard + cross-zone protocol) | NOT free: needs the two-phase protocol of section 1.1 | CAS per shard + create-only decision | 3k+1 writes (k prepare, 1 decide, k finalize, finalize can be helped lazily) vs 2 | Needs double collect (k+ reads, retry under write) | Only writers sharing a zone contend | Pending records must be resolved by anyone; livelock needs priority | Per-zone history possible; no global generation | Only worth it when single-key contention is measured to be the bottleneck. Modelled, safe under the rules below. |
| **Database-backed** (SQLite/Postgres/FoundationDB behind the server) | Free (transaction) | A trusted server | 1 transaction | Consistent read txn | Row/range locks | DB handles it | History by table | Gives up "any S3-compatible bucket is authoritative"; the validator must be a server. Reasonable for a hosted product, orthogonal to the object-store protocol (HYPOTHESIS). |

Takeaways. **CONFIRMED**: the sharded design can be made atomic and all its rules are necessary (mutation checks below).
**HYPOTHESIS**: it is not worth its complexity until contention on the single manifest is measured; no such measurement exists in
this repo ("manifest CAS contention from renewals: unmeasured", `docs/locks.md`). Negative result recorded: without a priority
rule the sharded protocol is safe but can livelock (below).

### 1.1 Cross-zone transaction protocol for sharded manifests

This is the protocol that `crates/vcs-remote/tests/model_sharded.rs` checks. It is a Percolator-like prepare/decide scheme
with the decision record as the commit point (prior art: Percolator, Peng and Dabek, OSDI 2010 [unverified: cited from memory,
not fetched this session]).

Keys: `shard/<zone>` = `(ver, head, pending: Option<(txid, new)>)`, changed only by CAS; `decision/<txid>` = `Committed |
Aborted`, create-only; `tx/<txid>` as today.

Writer for transaction T with updates `(zone, old, new)` sorted by zone:

1. **Prepare**, zones in ascending order: read shard. If it has a pending record of another transaction U, resolve it
   (rule R below) and re-read. If it has no pending record: if `head != old` write `decision/T = Aborted` (create-only) and go
   to Cleanup; else CAS `pending = (T, new)`, `ver += 1`. A pending record of T itself means a previous attempt got here: continue.
2. **Decide**: create `decision/T = Committed`. If the create fails and the record says `Aborted`, go to Cleanup. If it says
   `Committed` (earlier attempt whose ack was lost) continue. **This create is the single linearization point of T.**
3. **Finalize**, every zone: CAS `head = new, pending = None, ver += 1` iff `pending == (T, new)`. Idempotent; any helper may do it.
4. **Cleanup**: CAS `pending = None, ver += 1` on every shard whose pending record is T's.

Rule R (resolving a foreign pending `(U, n)` found on a shard): look up `decision/U`. `Committed`: finalize it. `Aborted`:
clear it. Absent: create `decision/U = Aborted` (create-only; if that loses, re-read, the answer is now `Committed`), then
clear. Anybody may run R at any time. A crashed writer is therefore recovered by whoever next touches its zones, or by re-running T.

Priority rule (wound-wait): a writer that finds an undecided pending of a transaction with a **lower** id does not abort it, it waits
(retries later); recovery after a lease timeout is allowed to abort anything. Needed for progress only, never for safety.

Reader (multi-zone snapshot): read all requested shards; effective head of a shard = `pending.new` if `decision(pending.txid)` is
`Committed`, else `head`. Repeat the whole collect until two consecutive collects give identical `(ver, effective head)`
vectors. Lock-free, not wait-free: a reader can starve under continuous writes (HYPOTHESIS: acceptable; today's one-GET read does not).

**Invariants and proof sketch.**

* **S-DECIDE** (uniqueness). `decision/T` is created at most once and never changes (create-only). So Committed and Aborted
  exclude each other, and every actor that reads a decision agrees.
* **S-EXCL** (prepare exclusivity). A shard has at most one pending record; it is set only from a state with no pending record and
  `head == old`; it is removed only by the owner's Finalize/Cleanup or by R, and R removes it only after a decision exists.
* **S-COMMIT-IMPLIES-PREPARED**. Suppose T creates `Committed` at time t. For each of T's zones the pending record was set at some
  time before t. It cannot have been removed before t: removal needs `decision/T` to exist (S-EXCL), and the only decision that
  could exist before t is `Aborted`, which would make T's create fail. So at t every zone of T holds T's pending record,
  `head == old` on every zone (S-EXCL froze them), and the effective state changes atomically at t from "all old" to "all new".
* **S-ATOMIC**. Define effective state E = shard heads with Committed-pending records applied. E changes only at decision
  creates (one transaction, all zones, by the previous bullet) and at Finalize/Cleanup/R steps, which do not change E. Hence E is
  at all times the replay of the committed transactions in decision order, and each saw `head == old` on every zone: no lost
  update, no partial transaction ever visible. Checked after every step of the model.
* **S-SNAP**. If two consecutive collects agree on `(ver, effective head)` for every shard, then no shard's stored state changed
  between its two reads (versions are monotone), and an effective head can only change by a decision flip, which is monotone
  (undecided -> Committed) and would show as a different effective head; so all shards were constant over the interval between
  the two collects, and the result is E at a point in that interval. It is no older than any transaction decided before the
  reader started. Checked by the model against all prefixes of the commit order.
* **S-OUTCOME**. A writer reports success iff `decision/T = Committed`; `Done(true)` is only reached after reading that.
* **S-QUIESCE**. After recovery no pending record remains.

**What the model explores** (CONFIRMED, `model_sharded.rs`): every interleaving of 2 or 3 writers (multi vs multi, multi vs
single, disjoint, three) and one reader, with at most one fault per schedule: crash of any writer at any step (recovered by
re-running the same transaction solo), or a lost acknowledgement at any step (effect applied, writer repeats the step).

**Mutation checks** (each deletes one rule; the checker must fail, and does):

| Removed rule | Test | Caught as |
|---|---|---|
| reader does not repeat until stable (single collect) | `mutation_single_collect_reader_sees_torn_state` | S-SNAP: reader saw a state no commit prefix has |
| writer decides before all zones are prepared | `mutation_decide_before_all_zones_prepared` | S-ATOMIC |
| prepare overwrites a foreign pending record | `mutation_prepare_overwrites_foreign_pending` | S-ATOMIC (lost update) |
| helper applies an undecided pending record instead of aborting | `mutation_helper_applies_undecided_pending` | S-ATOMIC |
| reader ignores the decision record (reads `head` only) | `mutation_reader_ignores_decision_record_gives_stale_read` | S-SNAP (stale read after commit decided) |

**Negative result** (CONFIRMED, `without_wound_wait_mutual_abort_is_possible_but_safe`): without the priority rule, 12 of 88 fault-free
terminal states of the two-conflicting-multi-zone scenario commit nothing (each writer aborts the other's pending record). Safety
holds, progress does not. With wound-wait, 0 fault-free terminal states have zero commits (asserted).

**Not modelled, therefore not claimed**: more than 2 zones per transaction; more than one reader; two faults in one schedule;
real clocks and lease timeouts for pending records (liveness only); the zone registry, labels, lock records, `time_hw`, fencing
counter and the `applied` window of today's manifest (in a sharded design these need a home: lock records can live in their zone's
shard; the registry and the fencing counter would be extra shards that join each transaction that touches them; `applied` is
replaced by permanent `decision/<txid>` records subject to retention); the single-zone fast path (a single-zone transaction
could skip Prepare/Decide and CAS the shard directly after resolving any foreign pending, HYPOTHESIS); GC of decision records
(section 2); backend linearizability of `decision/*` reads (S3 strong consistency [unverified: not fetched this session]).
Losing the global `generation` also loses a total order that callers may rely on (HYPOTHESIS: audit needed before adopting).

**Recommendation.** Do not adopt. The model shows a sharded design is implementable safely, at roughly 3x the metadata writes for
a single-zone publish unless the fast path is proven, plus reader retries, plus new failure modes (stuck pending records,
livelock). Revisit only with a measurement showing the single manifest is the bottleneck. If the manifest outgrows one object,
the snapshot-metadata design (pointer CAS to immutable manifest files) keeps the one-linearization-point property and is the
smaller step (HYPOTHESIS).

## 2. GC and reclamation

Today (`docs/gc.md`): local dry run only; no destructive sweep, no remote GC. The design notes there identify the publisher
race and the dedup hole. This section analyses the options and gives a protocol whose key rules are model-checked.

### 2.1 Design space

| Technique | What it gives | What it needs | Verdict |
|---|---|---|---|
| Reachability tracing (mark from roots) | Exact liveness of committed data | A complete root set; reading all of it | Mandatory baseline. |
| Grace period on object age | Protects uploads not yet referenced | Trusted backend timestamps; bound on publisher hold time; touch on dedup | Necessary, not sufficient (shown below). |
| Epochs (GC generation number, writers record the epoch they started in) | Lets the sweeper wait out in-flight publishers without clocks | A registry of active writers | HYPOTHESIS: leases are the clock-based equivalent; epochs need liveness tracking, which object stores lack. |
| Leases (`lease/<txid>`, create-only, expiry) | Roots for in-flight publishes without guessing a grace | Backend clock; extra requests per publish | HYPOTHESIS (not modelled). Protocol G1 below avoids it with a handshake. |
| Snapshot pinning (named snapshots, retained manifests are roots) | Time travel, safe long readers | Retained history list | Already roots (`snapshots` in the manifest); manifest history does not exist yet. |
| Generations (retain the last N manifests / S days) | Bounded history = bounded roots | Versioned or append-only manifests (section 1) | Needed for retention; trivial with append-only/log designs, not available with the single overwritten manifest. |
| Tombstones (two-phase delete: mark, re-check, delete) | Closes the check-then-delete race when delete cannot be conditional | One extra create-only key per candidate | **Required** on a backend without conditional delete: model shows unconditional delete after an age check is unsafe even with touch-on-dedup. Conditional DeleteObject on ETag/Last-Modified would replace the handshake (HYPOTHESIS, [unverified]: availability and semantics not checked). |
| Compaction-aware GC | Reclaims packs, not loose objects | Packs are the deletion unit: a pack may go only when no live object needs it | See 2.4. |
| Retention policy | Decides what counts as a root | Product decision | Parameters, not mechanism. |

### 2.2 Why "grace + touch" is not enough (CONFIRMED by the model)

Naive sweep: list, mark from the manifest, delete unreferenced objects older than G. Two holes, both found by the model checker
by removing the corresponding rule:

1. **Dedup hole** (already in `gc.md`): publisher's Collect prunes at an old orphan C that is present; it uploads only the new
   commit P. Without refreshing C's timestamp, C stays old and is swept before the CAS. Rule: touch on dedup
   (`mutation_publisher_does_not_touch_on_dedup`). Note this one is caught only by the variant where the publisher uploads nothing
   (the new head is the old object itself); when it uploads a fresh root, the young root protects its closure, which is why the
   rule can look redundant in simple tests.
2. **Check-then-delete race**: even with touch, the sweeper evaluates "old and unreferenced" at time t1 and deletes at t2 > t1;
   a publisher can touch and CAS in between. On a backend without conditional delete no timing assumption fixes this (the touch
   happens after the check, so the sweeper's clock cannot see it). Rule: the Dekker-style tombstone handshake of G1.

### 2.3 Protocol G1 (model-checked in `model_gc.rs`)

Time is the backend's clock. `G` = grace, `H` = hold time (max time from a publisher's first write to its CAS, enforced by the
publisher). Assumption A1: `G > H` plus a bound on the rate difference of the publisher's elapsed-time measurement.

Publisher additions (everything else is today's protocol): after its uploads, for each object at which Collect pruned (present
remotely, not re-uploaded): (a) **Touch** it (refresh `LastModified`, fails if gone, then redo Collect); (b) **CheckTomb**: if
`tomb/<id>` exists, abort and redo Collect; (c) **CheckExists**: re-check the object still exists; (d) **CAS fence**: perform the
manifest CAS only if at most H has elapsed since the publisher's first write of this publish, else redo from Collect.

Sweeper (single instance, see I8):

1. **Candidates**: read the manifest; candidates = objects with `age > G` not in closure(head).
2. **Tomb** every candidate (create-only `tomb/<id>`). All tombs are written before step 3.
3. **Remark**: read the manifest again; `keep = closure(head ∪ {objects with age <= G})`; `delete = candidates \ keep`.
4. For each candidate, parents before children: if in `delete`, **Delete** it; then **Untomb** it.

**Invariants (the "cannot delete referenced data" argument).** Let R(t) = closure(head) at time t, plus the closure of every
retained snapshot and retained or in-flight transaction (the roots of section 2.5).

* **I1 Closure** (publisher invariant that GC must preserve): if an object exists, all its dependencies exist. Established by
  post-order upload; GC keeps it by deleting parents before children and only whole unreferenced closures (step 3: `keep` is
  closed under dependencies, so `delete` is closed under "parents within candidates").
* **I2 Touch-then-check** (publisher): for every pruned object q, Touch(q) precedes CheckTomb(q) precedes CheckExists(q) precedes
  the CAS, and the CAS is within H of the first write.
* **I3 Tomb-then-read** (sweeper): every tomb is written before the Remark reads any timestamp or the manifest, and a tomb is
  removed only after its object was deleted or decided to be kept.
* **I4 Roots**: `keep` includes the head's closure and the closure of every young object.
* **I5 Order**: parents are deleted before children.
* **I6 Retention**: only history older than the retention window is dropped from the root set, and before the sweep that relies on it.

**Theorem (safety).** If I1-I6 and A1 hold, no object of any R(t) is deleted. *Sketch.* Take a deleted object o at Delete time d,
o in R(t) for a t. (i) If o in closure(head) at Remark time r, then o in keep: contradiction. (ii) Otherwise o became reachable
after r, through a CAS at c > r whose publisher reached o through some object q that was present when Collect ran: either q
= o's ancestor that the publisher uploaded (then q is young: uploaded within H < G of c, so at r it is either young and in
`keep`, or the CAS happened before r and q is in the head, case (i)), or q was pruned. For pruned q: Touch(q) at a, CheckTomb at
b > a, CheckExists at e > b, CAS at c > e. If `tomb(q)` was written before b, the publisher aborts (if the tomb was already
removed because q was deleted, CheckExists fails at e after the deletion, abort). If `tomb(q)` was written after b, then it was
written after a, so Remark's read of q's timestamp (after the tomb, I3) sees the touch: q is young, o in closure(q) is in `keep`.
The fence bounds how long q can stay young-but-unreferenced (A1). Each step of this argument is one of the mutation tests.
Hence o is never deleted. QED (modulo A1 and the abstractions listed below).

**Mutation checks** (CONFIRMED, each fails with `G-REACH: head needs object that was deleted`):

| Removed rule | Test |
|---|---|
| touch on dedup | `mutation_publisher_does_not_touch_on_dedup` (adopt variant) |
| CheckTomb | `mutation_publisher_does_not_check_tomb` |
| CheckExists after CheckTomb (tomb is removed after delete) | `mutation_publisher_does_not_recheck_existence_after_tomb_check` |
| CAS fence by hold time | `mutation_publisher_cas_not_fenced_by_hold_time` |
| tombs written after the sweeper decided | `mutation_sweeper_writes_tombs_after_deciding` |
| young objects are not roots | `mutation_young_objects_are_not_roots` |
| manifest ignored (no head closure, no exclusion of reachable) | `mutation_sweeper_ignores_the_manifest` |

**Not modelled / open (HYPOTHESIS or gap, stated so nobody relies on them):**

* **I8 two sweepers**: tombs are shared; sweeper A's Untomb can erase B's tomb and break B's handshake. Needs `gc/lock` with
  expiry, or owner-tagged tombs that publishers check under any owner. Not modelled.
* Sweeper crash: stale tombs make publishers abort forever on those objects (safe, not live). Needs tomb expiry/cleanup.
* Grace too small (A1 violated) is not mutation-tested because G and H are constants in the model; the dependency on A1 is argued, not executed.
* Non-atomic scans: the model reads all timestamps in one atomic step. The argument is per object, so a scan is fine
  as long as each object is read after all tombs; not executed.
* Packs, compaction, decision-record and `tx/` record GC, manifest `applied` window interplay (`gc.md` item 6), multi-object
  `tomb` costs (one extra request per candidate and per dedup hit: cost not measured).
* Backend clock monotonicity and the S3 touch mechanism (copy-to-self with REPLACE metadata, `gc.md`): [unverified] here.

### 2.4 Compaction-aware GC (design only, no experiment yet)

Packs are immutable and shared, so the deletion unit is a pack, not an object. Safe compaction: (1) write the new pack and
index (pack first, `packfetch::upload_pack` order); (2) the new index becomes visible by key; readers search every index, so
duplicates are harmless (reads verify hashes: `fetch_from_pack`); (3) only then may an old pack be deleted, and only if every
object of it that is in a root's closure is present in another pack or loose. Deleting the old pack is a GC delete: it
goes through the same tomb handshake with the pack as the object and "contains a live object not duplicated elsewhere" as the
reachability test. Crash points: after (1) (duplicate data, harmless), between (2) and (3) (harmless), mid-delete (pack present
but index gone or index present and pack gone: `pack_missing_but_index_present_is_an_error_not_an_empty_result` shows readers
fail cleanly, but a mixed state would make a live object unreadable, so delete the `.idx` first, so no reader is directed to a vanished
pack, then the `.pack`; HYPOTHESIS, not tested). Compaction crash tests are listed as
not covered in the matrix.

### 2.5 Roots

Remote roots for G1: closure(head of every branch), `snapshots`, manifests in the retention window (not available with a single
overwritten manifest, see section 1), `tx/<txid>` records for transactions inside the idempotence window, `decision/*` records inside
retention (sharded design), unexpired publisher state (none needed in G1: the handshake replaces leases), and the sweeper's own
tombs. Local roots are already listed in `docs/gc.md`.

## 3. Failure matrix

Legend. Covered: **E** = existing test before this change, **N** = added here, **-** = not covered. Experiment = the check that
establishes the invariant. "model.rs" etc. are in `crates/vcs-remote/tests/`.

| # | Failure | Invariant | Experiment | Status |
|---|---|---|---|---|
| 1 | Concurrent publishes, same branch | exactly one wins, loser gets Diverged, no lost update | `model.rs::same_branch_race_exactly_one_wins` (exhaustive interleavings) | E |
| 2 | Concurrent publishes, disjoint zones | both succeed after reconcile | `model.rs::disjoint_zones_both_succeed`; `sync.rs::works_on_fs_backend_with_concurrent_threads` (real threads, FsBackend) | E |
| 3 | Multi-zone vs single-zone | all-or-nothing, no leak of the loser's second zone | `model.rs::multi_zone_is_atomic_against_single_zone` | E |
| 4 | Interrupted upload (crash between objects) | closure invariant: every stored object has its deps; unreferenced objects are harmless; re-run converges | `model.rs` crash at every step + `check_backend` closure check | E |
| 5 | Lost ack (CAS applied, response lost) | re-run reports AlreadyApplied, applied once | `model.rs` LostAck; `sync.rs::lost_ack_then_retry_is_idempotent` | E |
| 6 | CAS request lost / precondition failure | nothing applied; retry re-validates | `model.rs` FailBefore; reload path | E |
| 7 | Stale metadata (writer's `old` out of date) | Diverged with both heads, never overwrite | `sync.rs::diverged_publish_reports_both_heads`; model | E |
| 8 | Network partition / backend unavailable | `Unknown` outcome, never false success; re-run safe | `sync.rs::unavailable_backend_yields_unknown_not_success` | E |
| 9 | Throttling (429/503 bursts) | same as 8: bounded retries, then Unknown. No backoff/jitter policy is specified or tested | modelled only as `Unavailable`; no throttle-specific test | - (behavioural gap: no backoff) |
| 10 | Duplicate submission | applied exactly once | `model.rs::duplicate_submission_applies_once` | E |
| 11 | Writer crash at any step | recovery by re-run, atomic outcome | `model.rs`, `model_locks.rs` (crash at any step) | E |
| 12 | Corrupt pack body | corrupt bytes never reach a store; clean error | `faults.rs::corrupt_pack_body_is_detected_on_every_read_path`; `packfetch.rs` unit test | N (partly E) |
| 13 | Truncated pack | clean error on both read paths; FINDING: trailer is never read remotely, a pack missing only its trailer is accepted (objects still hash-verified) | `faults.rs::truncated_pack_is_a_clean_error` | N |
| 14 | Pack missing, index present | error "missing", not an empty result | `faults.rs::pack_missing_but_index_present_is_an_error_not_an_empty_result` | N |
| 15 | Index corruption (bit flips, truncation, valid-checksum lies: swapped entries, entries past the real end) | rejected by checksum or by per-object hash; never a wrong object | `faults.rs::index_corruption_is_detected`; local parser: `vcs-store/tests/hardening.rs` | N (partly E) |
| 16 | Missing chunk | fetch fails, no dependent (chunk manifest, tree, commit) stored | `faults.rs::missing_chunk_fails_the_fetch_and_stores_no_dependents`; local fsck: `vcs-repo/tests/chunked.rs` | N (partly E) |
| 17 | Missing chunk under lazy fetch | KNOWN GAP: lazy fetch does not notice; error appears when the content is read | `faults.rs::known_gap_lazy_fetch_does_not_notice_a_missing_chunk` | N (documents gap) |
| 18 | Remote lost a descendant of an object the publisher believes present | KNOWN GAP: publish prunes at present objects and succeeds on a broken closure; a reader then fails. Needs remote fsck or a verifying publish | `faults.rs::known_gap_publish_trusts_a_present_object_whose_descendant_is_gone` | N (documents gap) |
| 19 | Invalid encoding with a matching hash | decode error, nothing stored | `faults.rs::correctly_hashed_but_undecodable_object_is_rejected`; manifest: `hardening.rs` (proptest + mutation loop) | N (manifest E) |
| 20 | Hash mismatch (tampered loose object) | rejected before storing | `sync.rs::tampered_remote_object_is_rejected`; batch path `faults.rs::hash_mismatch_loose_object_is_rejected_on_batch_path` | E + N |
| 21 | Garbage manifest | clean error | `sync.rs::garbage_manifest_is_a_clean_error` | E |
| 22 | Compaction crash (local repack) | repo valid after crash at any point | `vcs-cli/tests/cli.rs::repack_keeps_repo_working_and_fsck_catches_pack_corruption`, `vcs-repo/tests/chunked.rs`; no crash-injection of repack itself | - (partial) |
| 23 | Compaction crash (remote packs) | live objects stay readable; see 2.4 | no remote compaction exists | - |
| 24 | GC races: sweeper vs publisher | I1-I6 (section 2.3) | `model_gc.rs` (candidate protocol only; no destructive GC exists) | N (model only) |
| 25 | GC: dedup re-adoption of an old orphan | object age refreshed on dedup | local: `repo::readopted_old_orphan_is_kept_after_dedup_put` (cited from `docs/gc.md`, not re-run here); remote: model only | E (local) / N (model) |
| 26 | Reader/writer race (reader sees partial multi-zone state) | today: impossible, one GET; sharded: S-SNAP | `model_sharded.rs` (candidate); fetch under concurrent publish is not tested end to end | N (candidate) / - (current, end to end) |
| 27 | Sharded cross-zone atomicity under crash/lost ack | S-ATOMIC, S-OUTCOME, S-QUIESCE | `model_sharded.rs` | N (candidate) |
| 28 | Lock lease expiry / fencing / clock jumps | L-ATOMIC, L-EXCL, L-FENCE, L-EXPIRY, L-IDEM, L-VISIBLE | `model_locks.rs` with its own mutation checks | E |
| 29 | Local durability / crash before barrier | repo valid, objects invisible | `vcs-repo/tests/durability.rs`, `ops.rs`, `vcs-repo/tests/model.rs` | E |

Rows 9, 22, 23, 26 (end to end) remain open.

## 4. What the new tests do not prove

* The sharded and GC models are abstractions written for this study; they are not the code that would ship. A model that passes
  does not prove an implementation, only that the rules are coherent and each is necessary for the invariants as written.
* Fault tests use `MemBackend` and in-memory stores; no real bucket, no MinIO run was made here.
* No performance numbers were produced. Anything about request counts above is arithmetic from the protocol, not measurement.

## Sources

Not fetched in this session (all marked [unverified]); listed so a reviewer can check them.

* Peng, Dabek, "Large-scale Incremental Processing Using Distributed Transactions and Notifications", OSDI 2010. https://research.google/pubs/large-scale-incremental-processing-using-distributed-transactions-and-notifications/
* Apache Iceberg table spec (snapshot/manifest-list metadata with atomic pointer swap). https://iceberg.apache.org/spec/
* Delta Lake protocol (log of create-only commits plus checkpoints). https://github.com/delta-io/delta/blob/master/PROTOCOL.md
* Amazon S3 conditional writes and strong consistency documentation. https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html
* Git `gc` / prune expiry and cruft packs. https://git-scm.com/docs/git-gc
* In-repo: `docs/gc.md`, `docs/locks.md`, `crates/vcs-remote/src/{protocol,manifest,backend,packfetch}.rs`, revision `cd0fd09` of `research/storage-engine`.
