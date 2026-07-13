# T21 — Current-body store, per-key verification, recovery

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §3.2, §12.1, §13.3 (Q1, Q10)
Depends on: T15, T18, T20
Blocks: T19, T22 (body-profile import destination), T26 (leaf-check gate)

## Summary

Implement the required validator materialization for point service: current-body retrieval keyed by
canonical entity identity, mandatory per-key leaf verification before serving, cursor-skew resolution, and
lazy per-key recovery from peers/events/snapshots.

## Context

Every validator deployment retains current bodies and provides point body/proof service capability (the
signing host need not be publicly exposed; the service may be separated from the signing process). Bytes are
checked against leaf commitments — the store is never a root authority; no completeness predicate exists.
Missing/corrupt rows → per-key `unavailable` + local recovery. Body-store persistence is outside the
Marshal/SMT ACK critical path. Cursor skew (§13.3): body high-water ahead of `proof_ready_height` means a
mismatch may be a newer body — wait for tree catch-up or serve the body version for the served root; only
after cursor alignment does a remaining mismatch trigger replay/peer recovery. No global materialization scan.

## Scope

- Body read API over the T20 Mongo store: fetch by canonical entity key, recompute identity → `tree_key` →
  `leaf_value`, compare with the selected current tree (T15 snapshot / T18 leaf source).
- Skew resolution state machine: {body_hw, proof_ready_height} comparison; catch-up wait; per-root body
  selection where retained; alignment-then-recover ordering (no futile refetch loops).
- Scope re-cut 2026-07-13: this store and its recovery serve the RPC/point-service surface ONLY —
  execution never waits on recovery (a failed execution-side read is `BodyReadFailed`, T29/T33); no
  recent-version archive or parent-checkpoint recovery exists. Recovery sources fetch CURRENT bodies and
  verify them against the current leaf.
- Cursor policy DECISION (v1, audit P0-7): generic point reads use WAIT-ONLY alignment — on body-store
  cursor ahead of the served tree checkpoint the node waits for tree catch-up (or serves the body version
  for the served root where retained incidentally); no versioned-body history store is built in v1.
- Lazy recovery behind a typed `RecoverySource` interface defined HERE, each recovered body re-verified
  against the current leaf before storage/serving. v1 sources: (1) replay from the applicable finalized
  event range (T08 decoder over retained receipts); (3) snapshot-chunk fetch (T22/T28 range source —
  activates once T22 lands; gated by no T21 AC). Source (2) PEER FETCH is a `RecoverySource`
  implementation over `outbe_getBody` that lands as a follow-up AFTER T19 defines the request/status
  transport (T18 defines only the proof package, not the client protocol — audit P0-7); T21 itself ships
  the interface + a fake-server conformance test for it. Peer endpoints are operator-configured; no
  discovery protocol in v1.
- `unavailable` surfacing contract consumed by T19.

## Out of scope

- Media bytes (best-effort, domain policy); archive/historical retention guarantees (non-goals).

## Acceptance criteria

1. Serving path never returns bytes that fail the current-leaf check (corrupt-row and stale-row fixtures).
2. Cursor-skew suite (§19.19): body-ahead-of-tree and tree-ahead-of-body; proof-height body selection;
   catch-up without futile peer fetch; recovery after aligned mismatch.
3. Local body loss (row deleted from Mongo) → `unavailable`, then successful per-key recovery from retained
   events via `RecoverySource`; the interface's conformance suite runs against a FAKE/stub source serving
   T18 point-proof packages (the real `outbe_getBody` peer source is a post-T19 follow-up; the live
   T19↔T21 two-node E2E runs under T25); fetched bytes failing the leaf check rejected; re-verification
   gate covered.
4. Recovery never mutates consensus state and never blocks the persistence coordinator.
5. Recovery serves RPC only (scope re-cut): a fetched body failing the CURRENT-leaf check is rejected;
   recovery never blocks or feeds execution (no execution-side caller exists).

## Invariants

- A concrete body is accepted only after checking against the current leaf; no cursor is a completeness authority.

## Tests

- Integration with Dockerized Mongo + localnet; fault fixtures for loss/corruption/skew.

## Files

- `crates/core/compressed_entities/src/body_store.rs`
