# ADR-C-PRM-003: PromisLimit owns the global unallocated Promis accumulator

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Metadosis/auction economics maintainers
- **Scope:** `crates/core/promislimit`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-C-MET-001, ADR-C-PRM-001, ADR-C-PRM-002 and the Desis ADR
- **Supersedes:** PromisLimit portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

PromisLimit stores a single `total_unallocated` amount used across Metadosis and
Desis clearing. It is neither Promis token supply nor an account balance. Although
small, it has independent mutation authority and conservation risk: an incorrect
overwrite or returned remainder can create or lose future allocation capacity.

## Decision

PromisLimit is the sole owner of `total_unallocated: U256`. Public ABI is read-only.
Privileged internal operations are:

- `add(amount)`: checked accumulation of newly unallocated capacity;
- `set(total)`: replace the whole accumulator only when the caller has atomically
  consumed/cleared the previous complete pool and provides its exact remainder.

Metadosis may add terminal day amounts and replace the pool with a Desis clearing
remainder. Desis may return unused clearing capacity through its defined command.
Every caller and reason code must be enumerable; raw `set` is not a generic setter.

## Invariants and authority

The accumulator must equal:

```text
all accepted unallocated contributions
- all capacity atomically consumed by successful clearings
+ exact unused clearing remainders
```

`add` never overflows. `set` may not increase value unless the same transaction
contains an independently named contribution; a clearing remainder must be no
greater than the pool supplied to that clearing. Public callers cannot mutate it.

Because there is only one scalar, provenance must be available through command
events or a contribution/consumption ledger if full reconciliation cannot be
derived from surrounding module records.

## Atomicity, replay and failure

Limit mutation commits in the same checkpoint as the Metadosis terminal branch or
Desis clearing it represents. A failed auction, Lysis, Promis mint or day transition
rolls it back. Replay guards belong to those commands; the scalar alone cannot
distinguish a repeated contribution.

Overflow and an excessive remainder revert. A raw unexplained overwrite is an
authority/invariant violation, not recovery.

## Compatibility and evidence

Storage slot, amount scale and add/set semantics are consensus economics. Inspected
schema, read-only dispatch, checked add tests and production callers in Metadosis
and Desis. Current code exposes `set` directly to internal callers and contains an
unclosed remainder bound TODO, so status remains Proposed.

## Consequences

The module remains small but no longer escapes architecture review. Its caller set and
conservation equation become explicit rather than being buried in Metadosis.

## Rejected alternatives

- **Merge with Promis total supply:** unallocated capacity is not minted ownership.
- **Allow arbitrary public set/add:** any caller could alter future economics.
- **Trust Desis remainder without a bound:** a faulty result can increase capacity.
- **Use saturating add:** overflow would silently discard value.

## Open questions and technical debt

1. Enforce `clearing_remainder <= pool_supplied` before every replacement; the
   current Metadosis path records this as a TODO.
2. Replace raw `set_total_unallocated` with intention-revealing commands such as
   `replace_after_clearing(expected_before, remainder)` using compare-and-set guards.
3. Add structural caller tests covering every add/set call in Metadosis, Desis and
   tests/genesis code.
4. Add durable provenance events or an auditable contribution ledger sufficient to
   reconcile the scalar without replaying all history.
5. Define which module owns consumption/minting of accumulated Promis and prove
   relationship to actual ADR-C-PRM-001 supply.
6. Verify multiple same-day fee/day contributions add rather than overwrite and are
   idempotent by source identity.
7. Add generated sequences of contributions, failed/successful clearings,
   remainders, retries and overflow boundaries.
8. Define genesis initialization and migration reconciliation for a nonzero pool.
9. Clarify whether zero-value add/set emits/records anything or is forbidden.
10. Add a system invariant/debug RPC exposing expected-versus-actual accumulator
    provenance for operations.
11. Human economics review is required before accepting the scalar pooling model.
