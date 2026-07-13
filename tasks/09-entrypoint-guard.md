# T09 — Entrypoint dispatch guard and system-tx mutation path

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §3.1, §8.1 (Q2), §7 (raw hooks)
Depends on: T06 (fork-designated entrypoint resolution), T07, T08 (canonical events for the
system-mutation AC), T29 (operation classes and the Lysis-first invariant are T29 decisions)
Blocks: T10 (operation classes / system-lane deferral behavior), T23, T33 (operation classes + Lysis ordering)

## Summary

Enforce the fork-active call-graph authority: mutating domain entrypoints accept only ordinary `CALL` at
their registered address, the core has no public mutating EVM ABI, and system mutations run through
receipt-visible system transactions.

## Context

Authority is the fixed consensus call graph — no capability tokens between trusted Rust modules. The
dispatcher rejects static frames and foreign-context schemes (`STATICCALL`, `DELEGATECALL`, `CALLCODE`, any
future scheme) before domain logic, because the internal core does not receive an EVM call scheme. Raw hooks
cannot call the core (their logs are not receipt-visible). Direct user calls to `0xEE0B` cannot invoke
mutations.

## Scope

- Dispatcher-level call-scheme guard for mutating domain entrypoints (shared helper in the precompile
  dispatch layer; reject-before-domain-logic).
- `0xEE0B` surface: no mutating selector dispatch; read-only surface only if/where the spec's domain view
  requires it (core exposes none in v1).
- `msg.value != 0` rejection on non-payable entrypoints per repo security rules.
- Receipt-visible system-transaction path: system/lifecycle mutations enter via the same domain runtime and
  store interface inside a system tx (wire into the existing begin-zone system-tx machinery; ordering after
  the existing Phase 1–4 system txs stays hard-fork governed).
- CES mutation producer call-graph (audit P0-2 — this is the executable inventory, not a generic note):
  enumerate EVERY producer across crates/core/{tribute,tributefactory,lysis,nod,nodfactory} (gem/
  gemfactory deferred — not CES producers in Stage 1)
  and lifecycle hooks; for each: entry kind (user EVM CALL vs a CONCRETE system-tx kind), ordering
  relative to begin-zone Phases 1–4 and user transactions, gas lane,
  and — for bulk operations (Lysis mass NOD-mint/Tribute-delete) — a deterministic progress cursor
  splitting work across blocks.
- Raw-hook rule: begin-block raw-hook state changes to fields that live in CES bodies are forbidden —
  such changes must be receipt-visible system transactions emitting `WriteV1(Update)` (raw hook logs are
  not canonical events). With Gem deferred, NO Stage 1 producer needs such a conversion; the inventory
  proves it and the rule guards future onboarding (Gem's qualification path will need it).
- `BlockCapacityExhausted` on the system lane: define the deferral behavior for system bulk work (cursor
  holds position; work resumes next block; no receipt for the overflow — consistent with T10).
- Lysis-first ordering invariant (audit v5 P0-3, decided): the inventory FIXES that the Lysis system
  phase executes before user transactions and before any other CE-mutating system work touching the same
  partitions; the executor order is hard-fork governed and validator-verified. This invariant is what
  makes T33's prefetched partition lists safe without a collection dirty predicate.
- Raw-hook rejection: no code path from pre/post-exec hooks into `mint/update/delete/retire_partition`
  (compile-visible boundary; verified by review + test).

## Out of scope

- Domain-specific authorization/business rules (domain adapters, T23); attempt/gas reserve (T10).

## Acceptance criteria

1. Adversarial suite: STATICCALL/DELEGATECALL/CALLCODE/reentrancy into a mutating entrypoint all reject
   before domain logic (§19.5).
2. Direct user call to `0xEE0B` with any mutating-looking calldata cannot mutate (§19.7 public-core-call).
3. Receipt-visible system mutation succeeds and emits canonical events; raw-hook mutation attempt is
   unrepresentable/rejected (§19.9) — tested with a GENERIC synthetic system mutation. Producer-specific
   receipt/event tests (Lysis bulk operations) are T23/T33 acceptance
   criteria — T09 delivers the generic infrastructure + the producer inventory (audit_plan_v2 P0-2).
3b. Producer call-graph document merged; every producer named with its entry kind, ordering, lane, and
   cursor; no CES mutation path exists outside the inventory (grep-verified against the crates list).
3c. EXECUTABLE Lysis-ordering gate (audit v6 P1-2): an executor-trace assertion proves the Lysis phase
   runs before any CE producer of the same partitions; a test asserts existing begin-zone Phases 1–4
   mutate no CES partitions; proposer and validator use the identical hard-fork-governed order; adding a
   new earlier producer BREAKS the test (order is pinned, not incidental).
4. Emitter-authority contract stated HERE (only `0xEE0B` is the canonical emitter); the pure
   filter/decoder unit test is T08 AC4; the end-to-end projector integration test (foreign emitter
   ignored) is T20's acceptance (audit-final B-08: T09 acceptance must not depend on downstream T20).
5. System-only route acceptance matrix (audit-final M-07): a USER transaction crafted to look like a CES
   system-tx kind is rejected; a duplicate system tx in one block is rejected; a system tx replayed from
   an earlier block is rejected; wrong-phase/wrong-order system txs are rejected deterministically — all
   referencing the executor-constructed typed system-transaction authority (system txs are built by the
   executor, never accepted from the pool).

## Invariants

- Only fork-designated entrypoints reach the store; a Rust bypass is a protocol-code bug, not a caller capability.

## Tests

- Execution-level adversarial tests via localnet-style harness or executor tests; system-tx path test.

## Files

- `crates/blockchain/evm/src/precompiles.rs` (dispatch guard seam)
- `crates/core/compressed_entities/src/precompile.rs`
- `docs/ces-mutation-producer-inventory.md` (consensus-critical design artifact — APPROVED before the
  implementation parts of T23/T33 begin; audit v5 P1-6)
