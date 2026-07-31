# ADR-C-INX-001: Intex owns series identity, lifecycle and contributor distribution state

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-22
- **Decision owners:** Intex protocol maintainers
- **Scope:** `crates/core/intex`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-LYS-001, ADR-C-INX-002
- **Supersedes:** Intex ledger portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

Intex is the canonical Rust-side registry for series identity and lifecycle. It also
stores creator/contributor provenance and the cursor state of native proceeds
distributions. IntexFactory owns pricing, scans, external ERC-1155/vault/bridge
calls and payouts; this ADR covers only Intex-owned records and indexes.

## Decision

### Series lifecycle

Each unique `series_id` has immutable issuance/reference currencies, issued count,
Promis load, entry/floor/call prices, call-window parameters and issuance time. The
only legal transitions are:

```text
Absent --create--> Issued
Issued --mark_qualified--> Qualified
Issued | Qualified --mark_called(canonical_time)--> Called
```

Called is terminal in the Rust ledger. Creation requires nonzero `issued_at`, the
record existence sentinel. Lifecycle is stored as `u8` but every read/mutation must
decode it through the typed enum and reject unknown values.

Series are append-only in a dense global enumeration. Identity fields are not
rewritten by lifecycle transitions.

### Contributors and distribution progress

Lysis records one pre-deduplicated, deterministic `(owner, nominal)` list per
series, plus exact contributor total. Proceeds deliveries credit a per-series pot
tagged with their source chain; the ledger tracks the expected winning chains, the
arrived set, the fan-in deadline and an awaiting-deadline membership set. A
distribution round materializes one progress record (amount, total nominal,
paid-so-far, cursor, active sentinel) once the fan-in completes or the deadline
passes. Active distributions are held in a dense swap-and-pop set.

Progress advances monotonically. A round forced by the deadline retains the
contributor list so a late arrival funds a supplementary round over the same
proportions; only completion with every expected chain arrived clears the list
and the fan-in records. Intex stores pot and progress; the factory performs
native transfers and calculates shares.

## Authority and interfaces

Public/precompile reads expose series and enumeration state. Rust mutation APIs are
privileged seams:

- IntexFactory creates and transitions series and manages distribution progress;
- Lysis records contributors exactly once before source Tributes are consumed.

“Rust-to-Rust only” is not an authorization proof. Structural tests must enumerate
production callers and fail when a new mutator caller appears.

## Persistent state and invariants

- Every global enumeration slot points to one existing unique series; every series
  appears exactly once and `total_series` equals enumeration length.
- Lifecycle follows only the transition graph; `called_at` is zero before Called and
  a valid canonical timestamp after it.
- Immutable identity/economic fields never change after creation.
- Contributor slots are dense and unique under the accepted aggregation rule;
  stored total equals the checked sum of all nominals.
- At most one distribution is active per series; the pot only grows between
  rounds and is zeroed by the round that spends it.
- Progress cursor is at most contributor count; `paid_so_far <= amount`.
- Active-set membership iff a valid active progress record exists, with reverse slot
  map agreeing in both directions.
- A fully settled series (all expected chains arrived, rounds drained) leaves
  neither progress, active membership, pot nor contributor data; a deadline-forced
  partial round leaves exactly the retained contributor list and fan-in records.

## Atomicity, replay and failure

Creation and each transition update record/index state atomically with their owning
factory workflow. Contributor write rolls back with Lysis. Distribution progress
advances in the same checkpoint as corresponding native payouts; completion cleanup
must not commit if final event/transfer fails.

Series id, existing progress and active membership are replay guards. Duplicate
creation/contributor/distribution attempts revert unless an explicit idempotent
command validates byte-identical input. Corrupt enum/index/totals are invariant
failures, not absent records.

## Compatibility, bounds and evidence

Storage order, enum values, series-id encoding, timestamp width, contributor key
encoding and dense index semantics are consensus formats. `u32` issuance/call times
end in 2106 and require migration policy.

Inspected schema, API transition guards, record/global indexes, contributor storage,
progress lifecycle and active distribution set. Tests cover normal transitions and
storage operations but not exhaustive caller closure, corrupt structures or all
cross-module rollback cases.

## Consequences

The ledger has one clear state authority while IntexFactory remains the workflow
module. Creator payout provenance is explicitly part of Intex state, not inferred
later from retired Tributes.

## Rejected alternatives

- **Store lifecycle only in the external ERC-1155:** Rust block hooks and economics
  would have no local typed authority.
- **Let Factory own raw maps directly:** ledger invariants become unenforceable.
- **Keep contributors after payout without policy:** repeated proceeds could be
  ambiguously paid again.
- **Treat unknown state as Issued/absent:** corruption becomes a legal transition.

## Open questions and technical debt

1. Reconcile the three-state Rust FSM with the richer Solidity ERC-1155 lifecycle,
   including expiry/sweep after Called deadline.
2. `mark_called` accepts zero or caller-supplied timestamp at the internal seam.
   Validate canonical nonzero block time inside the module or narrow the API.
3. Creation validates almost no economic fields beyond sentinel time. Define which
   representational invariants (nonzero count/load/prices, currency validity, ordered
   call thresholds) belong in the ledger versus factory.
4. Contributor recording says “pre-deduplicated” but does not itself prove uniqueness
   or checked sum. Enforce the canonical aggregation contract.
5. Define whether a second proceeds distribution for a series is forbidden. Current
   completion clears contributor provenance, making legitimate repeat delivery
   impossible or unsafe.
6. Add compare-and-set expectations to progress saves so stale internal writers
   cannot move cursor/paid values backward.
7. Replace saturating cursor arithmetic in the factory seam with checked bounds and
   make malformed progress an invariant error.
8. Add structural closure tests for global enumeration and both directions of the
   active distribution dense set.
9. Add generated lifecycle/distribution models with every injected failure and
   retry.
10. Define maximum series and contributor counts, pagination and begin-block work
    bounds.
11. Plan migration beyond `u32` UNIX timestamps before 2106 and test boundary
    rejection now.
12. Add structural caller tests for every mutation API; private crate visibility is
    insufficient authority.
