# T33 — Variant A body-read adapters (minimal, Stage 1 testnet)

Status: todo
Source: T29 contract; SCOPE RE-CUT 2026-07-13 (owner decision: execution-read choreography removed — the
former readiness machine, prefetch choreography, coverage comparison, and automatic recovery are out of
scope; this task is the minimal read seam)
Depends on: T07 (same-block predicates), T09 (system-tx path + Lysis-first ordering), T18 (leaf read for
verification), T20 (Part B projection), T23 (schemas + `VerifiedBodyInput` constructors), T29, T30
(outcome names/codes)
Blocks: T27

## Summary

Wire the body-dependent Tribute/Nod operations to the validator-local Mongo projection under the T29
minimal contract: read → leaf-verify → use; anything wrong → the reading operation fails. A node with bad
local data diverges and falls out of certification; recovery is operator-driven.

## Context

After T27 removes per-record EVM bodies, the body readers are: Lysis (partition lists),
`NodFactory::mine_gratis()` (NodItemState), Tribute burn/processing (Tribute fields). Gem is deferred —
its paths keep reading legacy Gem EVM storage and are not Variant A adapters.

## Scope

- `CanonicalBodyReader` — the one read seam (sync facade over a small dedicated Mongo I/O thread with a
  bounded queue and timeout; the EVM execution thread is a blocking thread, so no `block_on` occurs in
  the async consensus actor):
  - `read_body(parent_checkpoint, domain_id, partition_key_or_none, raw_id) -> Result<VerifiedBody, BodyReadFailed>`
  - `read_partition(parent_checkpoint, domain_id, partition_key) -> Result<OrderedBodies, BodyReadFailed>`
    (canonical identity order; duplicate/conflicting/malformed rows ⇒ `BodyReadFailed`).
- Verification (T29 item 3): every returned body re-derives identity → `tree_key` → `leaf_value` and
  compares against the executing parent tree view (T12 parent view / T18 read path); missing, corrupt,
  stale, or mismatched ⇒ `BodyReadFailed` — the leaf check subsumes checkpoint-staleness detection for
  point reads.
- Failure semantics (T29 item 5): `BodyReadFailed` deterministically fails the reading operation FOR THIS
  NODE — a user tx reverts / is not built; a system phase fails its operation; a node whose data diverges
  simply stops matching the quorum and falls out. No global role gate, no catch-up state, no recovery
  orchestration.
- Same-block guard: check T07 `entity_mutated_this_block` AND `collection_retired_this_block` BEFORE any
  Mongo I/O ⇒ `SameBlockBodyUnavailable` (ordinary deterministic domain revert, identical on all nodes);
  reverted mutations/retirements create no ban.
- Runtime integration `reader → T23 VerifiedBodyInput constructors → CES writers` (T23 tests its writers
  against stubs; the Mongo-backed producer E2E lives here and in T25).
- `BodyReadContext` created by the node/executor layer per block execution and passed ALONGSIDE
  `StorageHandle` into Variant A-enabled adapters only; never enters EVM state or any root, never
  outlives block execution scope, identical on proposer and validator paths. FORBIDDEN: process-global
  Mongo clients, implicit-context lookups, `block_on()` in the async consensus actor,
  wall-clock-dependent behavior.
- Legacy-read inventory: every pre-CES body accessor and its call sites listed with its NAMED replacement
  adapter (replacement coverage is this task's gate); the post-cutover grep-proof that no execution-time
  legacy body read remains is T27's acceptance (postfix PF-B05).
- Testnet-only wiring: production-profile startup with adapters enabled fails with the structured error
  (T29 item 1).

## Out of scope

- Readiness/coverage/recovery machinery (removed by owner decision — scope re-cut); projection building
  (T20); CES writers (T23); the profile contract itself (T29); production off-chain computation (future
  gate); Q11 consensus constants (T24).

## Acceptance criteria

1. Adapter inventory merged: every body-dependent operation named with its read kind (point/list) and
   replacement adapter; no body-dependent path exists outside the inventory.
2. Data-fault fixtures: tampered row (leaf mismatch), missing row, stale row ⇒ the reading operation
   fails with `BodyReadFailed`; the node keeps running; no receipt ever claims success for a failed read.
3. Same-block matrix: mint→body-read, update→body-read, retire_partition→member-body-read ⇒ deterministic
   `SameBlockBodyUnavailable`; reverted-first-mutation does NOT ban; next-block operation succeeds.
4. Production-profile startup with adapters enabled fails with the structured error.
5. Localnet divergence scenario (minimal model, replaces the former recovery matrix): validator B loses a
   Mongo row → a block reading that row makes B compute a diverging result → B stops certifying (its vote
   does not match / its proposal dies) → network proceeds on quorum → operator resync/restore → B
   rejoins. Manual path only; no automatic rejoin is tested because none exists.
6. I/O thread: bounded queue and timeout (a timeout ⇒ `BodyReadFailed`, node-local); cancellation on
   tx/block abort; no response delivered into an expired block scope.
7. Lysis ordering (with T09's executable gate): same-block mutation of a partition before the Lysis phase
   is unrepresentable under the executor order; proposer/validator identical order.

## Invariants

- Every consumed body is verified against the CES commitment; Mongo is used, never trusted.
- Proposer and validator run identical adapter code; outcomes differ only when local data differs
  (accepted testnet risk — the diverging node falls out).
- No adapter state outlives block execution scope.

## Files

- `crates/core/compressed_entities/src/body_read/{reader.rs,context.rs,io.rs}`
- `crates/blockchain/evm/src/executor.rs` (context injection)
- `crates/core/{lysis,nod,nodfactory,tribute}/…` (adapter call sites; gem/gemfactory deferred)
