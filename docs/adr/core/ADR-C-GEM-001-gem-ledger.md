# ADR-C-GEM-001: Gem owns Gem identity, owner indexes and qualification state

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Gem protocol maintainers
- **Scope:** `crates/core/gem`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-ORC-001
- **Related:** ADR-C-GEM-002
- **Supersedes:** Gem ledger portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

A Gem is a non-fungible protocol record representing a typed Promis-bearing load
with immutable prices/currencies and an Oracle-qualified lifecycle. GemFactory owns
issuance policy, settlement payment and Promis mining. Gem owns records, identity,
enumeration, owner indexes, supply and the unqualified price-bin index.

## Decision

Gem identity is deterministically derived from owner, load and creation block under
the current scheme and must be unique. A record captures owner, type, load,
entry/cost/floor prices, issuance/reference currencies, issue time and typed state.

Canonical lifecycle:

```text
Absent --add ordinary--> Issued
Absent --add genesis--> Qualified
Issued --qualifier rate > floor and currency matches--> Qualified
Qualified --factory settlement--> Settled
Settled --factory burn after PoW--> Absent/Burned
```

Only Issued records are present in the unqualified floor-bin tree. Qualification
uses canonical Oracle rate and time, removes bin membership, updates state and emits
the event atomically. Burn is allowed only from Settled and removes record, global
enumeration, reverse index and owner enumeration, then decrements supply.

## Authority and interfaces

Reads expose record, supply, global/owner enumeration and metadata. GemFactory is
the intended add/state/burn owner; the qualifier block hook may perform only the
Issued-to-Qualified transition. Raw `set_state` is privileged and must enforce the
transition graph rather than accept arbitrary enum movement.

Gem does not calculate prices/cost coefficients, move settlement assets, verify
mining PoW or mint Promis.

## Persistent state and invariants

- `total_supply == global gem-id array length == number of live records`.
- Global array and `gem_index` are bidirectionally closed and unique.
- Each live record appears exactly once in its owner's dense index; owner count
  equals entries.
- Only Issued Gems appear exactly once in the correct floor bin; tree bits agree
  with nonempty bins.
- Non-Issued Gems appear in no unqualified bin.
- Immutable identity/economic fields do not change during state transitions.
- State follows only the legal graph; unknown `u8` is corruption.
- Burn removes all indexes and decrements supply exactly once.

## Atomicity, replay and failure

Add, qualify, state transition and burn are atomic with events and their owning
factory/hook frame. Gem id uniqueness and record existence guard issuance/burn
replay. A failed settlement or Promis mint rolls back state/burn.

Missing input record and ineligible rate are normal no-op/error as specified.
Missing required index membership, duplicate ids, supply zero on burn or invalid
state are invariant failures. Cleanup must not silently treat absent bin membership
as success when state says Issued.

## Compatibility, capacity and evidence

Id derivation, type/state discriminants, currencies/scales, bin mapping/tree layout,
array compaction and event ABI are consensus formats. Owner/global/bin scans require
bounds; burn currently scans the owner's full list.

Inspected schema/API, add/burn/global and owner indexes, bin insertion/removal,
qualifier runtime and tests. Current code contains silent cleanup and supply clamps
that prevent a complete architecture-conformance verdict.

## Consequences

Gem remains the authoritative indexed FSM while GemFactory controls economic
workflows. Qualification can be optimized by bins without making the factory own
ledger internals.

## Rejected alternatives

- **Scan every Gem for qualification:** work grows with all historical/live records.
- **Let factory mutate maps directly:** index closure cannot be centralized.
- **Treat burn as a state flag only:** live supply/index queries retain consumed Gems.
- **Silently repair missing indexes:** corruption becomes nondeterministic hidden
  behavior.

## Open questions and technical debt

1. `set_state` accepts arbitrary target states and validates no predecessor. Enforce
   the exact FSM in the deepest mutation boundary.
2. Burn decrements supply only when greater than zero. A zero supply with a live Gem
   must be an invariant failure, not a successful burn.
3. `add_gem` increments supply and owner count without checked arithmetic. Add bounds
   and checked operations.
4. Bin removal returns success when bin or Gem membership is absent. Require exact
   membership for an Issued transition and test corruption.
5. Owner-index compaction linearly scans all owner Gems. Add reverse owner slot or a
   protocol cardinality/gas bound.
6. Prove deterministic Gem id cannot collide for two same-owner/same-load issuances
   within one block; include nonce/type or reject the second intentionally.
7. Qualification silently skips reference-currency mismatch. Prevent incompatible
   issuance or expose explicit unsupported/configuration state.
8. Add structural closure/property tests for global, owner and every bin/tree index
   across arbitrary add/qualify/settle/burn sequences.
9. Validate state/type `u8` through typed decoding on every read and make unknown
   values invariant failures.
10. Define record/history source after burn; events alone may be insufficient for
    audit queries.
11. Add pagination and maximum sizes for global/owner enumeration and bin hook work.
12. Add structural mutation-caller tests for GemFactory and qualifier hook.
