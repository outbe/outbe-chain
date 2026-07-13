# T20 — Finalized ExEx → MongoDB projector (Docker harness)

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §11.3 (Q4, Q9), §7.2
Depends on: T08, T30 (event/schema contracts) (Part A); T23, T29 (Part B)
Blocks: T21, T22 (baseline-init cursor API), T26 (Part B), T33 (Part B)

## Summary

Implement the read-only, finality-gated Reth ExEx that projects canonical events into MongoDB: current-body
rows and secondary indexes, idempotent block application, durable high-water, and `FinishedHeight` gating.
MongoDB runs in Docker for development and tests.

## Context

The projector gates on a finalized `{height, block_hash}` stream (executor's finalized signal + provider
hash lookup — finalized blocks are never reorged). For each target it reads every missing canonical
block/receipt in `high_water+1..finalized_tip` (gaps never skipped), applies events in tx/log order.
Mint/update upsert row + index memberships; delete removes them (missing local row allowed); partition
retirement removes the current projection range by `{domain_id, partition_key}` without synthesizing
per-entity events. Row and index addressing consumes the AMENDED event shape (T08 spec amendment #1): for
`DeleteV1` there is no body to consult, so the `by_wwd`/partition-scoped index membership and any
`tree_key`-derived locator are derived from the event's `{domain_id, partition_key_or_none, id_bytes}`;
the field's canonical shape is validated against the fork-active partition policy (the raw_id binding is
emitter-guaranteed and not re-checkable) — mirroring T17's rebuild rule. Idempotency identity: `{block_hash, transaction_index, log_index_in_receipt}`; equal
redelivery no-op; conflicting payload stops the projector. High-water `{height, block_hash}` advances only
after the whole block is durably applied; `FinishedHeight` is sent only after that durable commit — a Mongo
outage holds Reth pruning and grows the ExEx WAL (documented, monitored backpressure). Only events emitted
by `0xEE0B` are accepted.

## Structure (audit P0-6): two parts

- **Part A — generic projector** (Depends: T08): finalized event ledger, current canonical body rows
  (opaque bytes keyed by canonical entity key), cursor/baseline machinery, retirement markers. The generic
  projector CANNOT extract owner/day from opaque bodies — it builds no domain indexes.
- **Part B — domain projection adapters** (Depends: T23 schemas + T29 Variant A contract): decode
  Tribute/Nod bodies via the T23/T30 schemas and maintain `by_owner`/`by_wwd`/day-domain index
  memberships. T26 and the Variant A body-read adapters (T33 — postfix PF-M01; T23 owns writers only)
  consume Part B, not Part A.

## Scope

- ExEx installation in the node builder (observability/indexing only — consensus logic in an ExEx is
  forbidden by repo rules; this projector never participates in validity).
- Finalized-stream adapter: height signal → `{height, hash}` via provider by-number lookup.
- Baseline-init cursor API (consumed by T22 snapshot bootstrap): atomically install a projection baseline
  `{height: H, block_hash, profile, body_coverage}` and set high-water to `H` on a node that never
  processed `genesis..H`. High-water semantics are therefore "contiguous event processing SINCE the
  baseline", not since genesis; a baseline is present on every node (ordinary genesis start installs the
  trivial `{height: 0}` baseline). Rows/indexes outside the baseline's `body_coverage` are absent by
  construction and recover lazily (T21).
- Mongo schema: current-body collection (keyed by canonical entity key), index-membership collections
  (`by_owner`, `by_wwd`, domain/day), cursor collection; idempotent block-apply transaction with the
  event-identity ledger.
- Partition-retired handling is two-phase to keep the durable block apply bounded: (1) inside the block
  apply, atomically write a retired-partition marker and exclude the partition from all queries — this is
  the durable, idempotent part gating high-water/`FinishedHeight`; (2) physical deletion of the row range
  runs as a background, chunked, idempotent sweep after the marker. A large WWD retirement must not stall
  the projector cursor or hold back Reth pruning.
- Fail-closed handling: unknown event versions, malformed events, body/leaf mismatch, finalized-hash conflict.
- Backpressure metrics: WAL size, pruning hold-back, high-water lag; operator alarms.
- Docker: `docker compose` service (pinned MongoDB version) for local dev; ephemeral MongoDB container
  harness (testcontainers-style) for integration tests; no test may depend on a host-installed MongoDB.
  Multi-document transactions require a replica-set deployment — the compose service and test containers
  run single-node replica sets (P1-6).
- Recovery of a broken projection is operator-driven (scope re-cut 2026-07-13): from-scratch
  finalized-history replay (AC1) or snapshot restore (T22) — no recent-version archive, no automatic
  per-key recovery machinery exists.
- Event-identity ledger lifecycle (P1-6): retention/compaction policy for the
  `{block_hash, tx_index, log_index}` idempotency ledger, unique indexes, and crash atomicity across
  current row + index membership + cursor (one Mongo transaction per block apply).
- Validator posture under Stage 1 Variant A (T29): every validator deployment runs its OWN MongoDB
  projection; `--ce-body-service=external` is FORBIDDEN for Variant A validators (a separate container in
  the same deployment counts as local; a shared/remote service for multiple validators does not). A
  validator whose projection is broken/lagging computes diverging results and falls out of certification
  (T29 item 5) — there is no readiness gate; Mongo never becomes a production authority. Full nodes may
  still run split/external topologies.

## Out of scope

- Body-store read/recovery semantics (T21); authenticated list queries (non-goal).

## Acceptance criteria

1. From-scratch rebuild: replaying finalized history into an empty Mongo reproduces identical current rows
   and indexes (idempotent re-run is a no-op).
1b. Baseline init: installing a baseline at `H` on an empty Mongo and applying `H+1..head` produces rows
   equivalent to a genesis-replayed node for keys covered by `body_coverage`; the projector never attempts
   heights ≤ `H`; a second baseline install with an EQUAL payload/`import_id` is an idempotent no-op
   (crash-resume legally re-runs the install step — postfix PF-M02); a CONFLICTING baseline is
   corruption and rejected.
2. ExEx test matrix (§19.10): finality gate (non-finalized notifications ignored), duplicate delivery no-op,
   gap replay (missing range fetched), delete-row consistency (missing row tolerated, cursor advances once;
   partitioned-domain `DeleteV1` removes the correct `by_wwd` membership via the event's
   `partition_key_or_none` — test covers both Singleton and Partitioned), conflicting cursor stops the
   projector; wrong-emitter E2E (postfix PF-M04): a deployed contract emitting the canonical signature
   from a foreign address is ignored END-TO-END by the projector filter (pure decoder test is T08 AC4;
   the emitter-authority contract is T09's).
3. Crash before high-water → safe replay; crash between Mongo commit and `FinishedHeight` → replay no-op
   (§19.11 Mongo/high-water/FinishedHeight crash points).
3b. Mongo atomic-boundary fault test (audit-final M-01, retargeted after the scope re-cut): an induced
   fault (disk pressure / kill) mid block-apply proves no partial current/index/ledger/high-water update
   is observable, no `FinishedHeight` is emitted for the incomplete block, the pruning hold is retained,
   and replay after recovery is idempotent.
4. Partition retirement: marker visible and partition query-excluded within the block apply; background
   sweep removes exactly the range (rows outside untouched); crash mid-sweep resumes idempotently; a large
   synthetic partition does not stall high-water/`FinishedHeight`.
5. Mongo outage behavior (minimal model): during an outage the projector holds `FinishedHeight` (WAL
   grows, visible in metrics) and body reads on this node fail (`BodyReadFailed` — the node diverges and
   falls out per T29 item 5); after the outage the projector resumes by replay; network continues on
   quorum throughout (localnet fixture).
6. Validator-mode enforcement: `--ce-body-service=external` on a Variant A validator is a startup error;
   full-node external topology still works.

## Invariants

- Part A (generic projector) never applies tree mutations and never participates in block validity.
- Under Variant A (Stage 1), Part B is a LOCAL dependency: a broken/lagging projection makes THIS node's
  body reads fail and its results diverge — it falls out of certification (T29 item 5). Consensus STATE
  is never altered by Mongo; if a quorum of validators loses Mongo simultaneously, the testnet may halt —
  an owner-accepted risk exercised in the T34 soak plan.
- Cursor independent of the SMT marker; no coupling to Marshal ACK.

## Tests

- Integration tests with Dockerized MongoDB; fault injection per matrix; localnet run with projector enabled.

## Files

- `crates/core/compressed_entities/src/projector/` (ExEx + Mongo adapter)
- `docker-compose.yml` (mongo service), `bin/outbe-chain` (ExEx wiring; default-on for validators; the
  opt-out/external flag exists ONLY for full-node/non-Variant-A topologies — on a Variant A validator it
  is a startup error)
