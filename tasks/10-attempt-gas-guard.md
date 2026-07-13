# T10 — CE attempt counters, gas charge, payload-builder contract

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §15.1 (Q15; Q11 numerics provisional)
Depends on: T07, T08 (`encoded_len` metering input), T09 (operation classes, system-lane deferral), T31 (provisional bound values)
Blocks: T12 (typed reserved-totals input), T23, T24

## Summary

Implement the provisional resource guard: executor-local per-tx/per-block attempt counters with normative
reserve ordering, the flat 50k gas charge, and the payload-builder / validator contract
(`TransactionLimitExceeded` vs `BlockCapacityExhausted`).

## Context

Counters are executor-local, non-persistent, non-journaled; not EVM state, not in any root or artifact.
Proposer and validator recompute them deterministically over the same sequential call path. Provisional
values: `CE_MUTATION_GAS_PROVISIONAL = 50_000`, per-tx and per-block caps 600. Reserve order is normative:
gas first, then per-tx cap, then per-block capacity; over-per-tx is always `TransactionLimitExceeded` even
when it would also exceed block capacity. The block cap spans user and system lanes (30M user /
`SYSTEM_TX_ARTIFACT_GAS_LIMIT = 10_000_000_000` system).

## Scope

- Reserve at core entry for `mint/update/delete/retire_partition`: atomic check/reserve of per-tx slot,
  per-block slot, and the gas charge before hashing/journal/event work; failed reserve = attempt not started.
- Post-reserve semantics: lifecycle rejection, nested revert, or full tx revert does not remove the attempt
  from the block counter when the tx is included.
- Builder integration: speculative-tx exclusion restores the counter to its pre-transaction checkpoint with
  execution state; `BlockCapacityExhausted` keeps the tx in pool (not `InvalidTransaction`, no eviction);
  `TransactionLimitExceeded` rejects/reverts, never defers forever.
- STICKY overflow (audit P1-2, proposer/validator parity): `BlockCapacityExhausted` is a
  transaction-level sticky outcome carried in an executor-local flag — a contract catching the subcall
  revert and continuing CANNOT clear it; at transaction end the whole tx is excluded (builder) or the
  block rejected (validator) regardless of the tx's own success flow. Checkpoint/restore of the counter
  happens ONLY when the whole speculative tx is excluded. Typed propagation to the payload builder — the
  condition never degenerates into an ordinary EVM revert. Charge boundary defined: if the gas check
  passed but a cap check failed, the reserve did not start and the 50k charge is NOT taken (per §15.1
  "if gas or quota reserve fails, the attempt did not start"); a Stage A per-operation BODY-SIZE
  rejection is likewise "attempt not started" — no slot, no charge, deterministic domain/core rejection
  only (postfix PF-H03, pinned in amended §15.1).
- Validator path: block crossing the cap is rejected without creating a receipt for the overflow.
- Constants in one module, marked provisional pending Q11; only bounded/fixed Tribute/Nod schemas may
  use the flat price (registry gas profile hook from T06).
- Limit-kind interface + PROVISIONAL enforcement (§8.3; audit v5 P0-4 REVERSES the earlier
  attempts-only decision — Gate D2 exists precisely so runtime integration never starts unbounded):
  T10 implements attempts+gas AND the ACTIVE provisional counters for aggregate body/calldata/event
  bytes, unique keys per block, and the system-lane policy, with values from T31 (all `PROVISIONAL_Q11`).
  Staged-tree bytes and speculative-cache bounds are T12's slice; per-operation body limits are T07/T23's.
  T24 later replaces values (and structure only if benchmark evidence requires it) with a full
  re-baseline. Requires concept §15.1 amendment #6 (temporary guard = attempts/gas + provisional D2
  bounds — APPLIED).
- `retire_partition` is explicitly under the provisional flat charge and the attempt caps — **spec
  amendment #4 (§15.1) — APPLIED**: the constant's definition now reads "per attempted core
  mint/update/delete/retire_partition"; the charge applies per attempted core operation of all four kinds.

- Neutral `resource.rs` (audit v8 P0-1/P0-2): owns the PURE `estimate_staged_delta(...)` implementation
  (T31 formula), the Stage A/B reservation counters/checkpoints, the sticky outcomes, and the trusted
  byte-metering seam: `CeResourceUsageDelta {invocation_calldata_bytes, body_bytes, event_bytes,
  canonical_identity, staged_estimate}` is built ONLY by shared protocol helpers — invocation calldata is
  metered exactly once per canonical invocation by the dispatcher/system-tx builder from actual input
  bytes; body bytes from the actual canonical slice; event bytes from the T08 canonical
  encoder's `encoded_len`; identity/staged estimate from the shared derivation/resource path. The guard
  never accepts caller-supplied counts; user-forgeable length inputs are unrepresentable.
  `resource.rs` also owns the executor-local seen-key/seen-collection first-touch sets (audit-final B-01)
  and exports a TYPED immutable per-block resource summary (reserved totals per bound) handed to T12
  through the block runtime context — T12 never re-derives reserved totals.

## Out of scope

- Final Q11 numeric limits/formula (T24); ZeroFee policy changes (existing caps already bound the free lane).

## Acceptance criteria

1. Reserve-order tests incl. the exact overlap point: entry violating both caps classifies as
   `TransactionLimitExceeded` (per-tx precedence).
2. Included-but-reverted tx keeps its attempts counted; excluded speculative tx restores the checkpoint.
3. Proposer/validator parity test: same block → same counter verdicts; over-cap proposed block rejected.
3b. Sticky-overflow test: a contract that catches the overflowing subcall's revert and returns success is
   still excluded/rejected as a whole transaction on both paths (P1-2).
4. System-lane test: cap effective under the 10B lane where OOG does not bind first.
5. Charge deducted before hashing/journal/event; revert does not refund performed computation.
6. Provisional bounds enforced per the T31 reserve/failure MATRIX (v7-completed): boundary/rejection/
   parity tests for EACH T10-owned bound at the T31 values, incl. the TWO-STAGE reserve (Stage A pre-hash:
   gas/body-size/attempts/bytes; Stage B post-derivation: first-touch unique key + staged-delta; Stage B
   block-capacity overflow rolls back the whole speculative tx incl. Stage A), the EMPTY-BLOCK FIT byte
   classification (never-fits ⇒ `TransactionLimitExceeded`; fits-empty-but-not-remaining ⇒ sticky
   `BlockCapacityExhausted`), overlap precedence, first-touch-only unique-key counting, retryable vs
   permanent failure classes (no cursor spin on permanent errors), and included-revert/excluded-tx
   reservation discipline.
7. `retire_partition` attempt reserves a slot and pays the charge like the other three operations.
8. Metering tests (audit v8 P0-2): one invocation → multiple mutations counts calldata ONCE; nested CE
   entrypoint calls each count their own actual invocation once; reverted-but-included nested work stays
   counted; excluded speculative tx restores ALL deltas; user and system paths produce identical canonical
   size accounting; event-byte duplication counted per the T31 formula; a malformed caller cannot
   under-report any length (computed, not supplied).
9. Reserved-totals handoff (audit-final B-01): the typed per-block summary equals the sum of successful
   reservations (incl. included-but-reverted work), is immutable once seal begins, and is T12's ONLY
   `reserved` source.
(Former ACs 10–11 — read reservation and outcome arbitration — removed by the 2026-07-13 scope re-cut:
body reads are not resource-metered in Stage 1 and `BodyReadFailed` is an ordinary deterministic
operation failure needing no arbiter.)

## Invariants

- Counters never enter EVM state, roots, or artifacts; determinism from sequential execution only.

## Tests

- Executor-level tests (user + system lanes), payload-builder deferral test, parity test (pairs with §19.4).

## Files

- `crates/core/compressed_entities/src/{guard.rs,resource.rs,constants.rs}` (resource.rs: estimator +
  Stage A/B reservations + CeResourceUsageDelta metering seam)
- `crates/blockchain/node/src/payload_builder.rs` (deferral integration)
