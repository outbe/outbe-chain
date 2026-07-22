# ADR-C-MET-001: Metadosis owns the WorldwideDay state machine

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-22
- **Decision owners:** Protocol economics maintainers
- **Scope:** `crates/core/metadosis`, its WorldwideDay records and auction/PROMIS seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-CYC-001, ADR-S-ORC-001
- **Related:** ADR-C-TRB-001, ADR-C-TRB-002, ADR-C-LYS-001
- **Supersedes:** Metadosis sections of former broad pre-space Cycle/daily-orchestration document (previously numbered 029)

## Context

Metadosis represents a protocol day with offer windows, Oracle-derived day type,
an economic limit, auction coordination and a terminal transformation. This is a
stateful domain module, not scheduling infrastructure. Cycle merely invokes its
commands at configured times.

## Decision

Metadosis is the sole owner of the WorldwideDay identity, windows, active/closed
indexes, limit, type and status. The canonical FSM is:

```text
FORMING -> LOOKBACK_DELAY -> OFFERING -> WAITING -> READY
                                                   |  |
                                                   |  +-> FAILED
                                                   +----> COMPLETED
```

`IN_PROGRESS` is reserved by the current schema but is not a valid production
transition until its semantics are accepted.

Creation derives all windows from canonical block time and the UTC+14
WorldwideDay calendar, inserts the record in the active set, and seals its Tribute
partition. Creation is idempotent by day identity.

Transition side effects are owned by the transition, not Cycle:

- FORMING completion snapshots current/previous Oracle VWAP and determines the
  day type;
- entry to OFFERING unseals Tribute offers;
- exit from OFFERING seals offers;
- READY processing consumes the day's limit through the branch table below and
  hands Desis its one-shot auction brief (supply, entry price, day type) — the
  auction schedule is Desis-owned from that point;
- terminal transition removes the active member and appends it to the bounded
  closed FIFO.

Day type is GREEN only for a strictly rising valid VWAP. Missing/zero observations
and equality follow the currently implemented RED behavior pending economic review.

### READY command

At most one selected READY day is processed per command today:

| Condition | Economic command | Terminal status |
|---|---|---|
| zero limit | no brief is dispatched | FAILED |
| UNKNOWN type | add limit to unallocated Promis | FAILED |
| no Tributes | clear supply, add remainder to Promis, retire empty partition | COMPLETED |
| Tributes, Lysis succeeds | commit returned remainder and retire partition | COMPLETED |
| Lysis fails | propagate error; enclosing checkpoint rolls back | remains READY |

Metadosis orchestrates these calls but does not own Lysis allocation mathematics,
Promis ledger rules or Tribute/Nod storage.

## Interfaces and invariants

Production commands are day creation, active-day advancement, limit application,
READY processing, and terminal retention cleanup. Deep mutators must accept only
typed states and legal predecessor states.

Required invariants:

- every nonterminal day appears exactly once in the active set;
- every retained terminal day appears exactly once in the closed FIFO;
- active and closed membership are disjoint;
- windows are ordered and immutable after creation;
- type snapshots and status transitions occur at most once;
- Tribute sealing agrees with the window status;
- a terminal outcome consumes/retains the limit exactly once;
- FIFO eviction cannot leave active/index references.

## Atomicity, retries and failure

One advancement or READY command executes within its caller's EVM checkpoint.
Status, auction calls, Promis changes, Lysis, Tribute retirement and index changes
must roll back together on propagated failure. Retrying reads the prior status and
must not duplicate transition side effects.

Invalid stored state, broken membership or impossible windows are invariant
failures. Missing Oracle data is a specified business input to day typing; a nested
module failure is propagated unless a branch explicitly defines fallback.

## Security, compatibility and evidence

Status/type discriminants, date conversion, windows, retention bound and branch
semantics are consensus formats. Dev-only shortened windows must be chain-spec
committed and cannot leak into production by local environment.

Production runtime and tests were inspected for creation, window advancement,
Oracle snapshots, offer seal/unseal, terminal branches and FIFO cleanup. There is
not yet a complete generated transition model or cross-module rollback suite.

## Consequences

Metadosis can receive its own architecture-conformance verdict. Scheduler correctness no longer
stands in for legal WorldwideDay transitions, and Lysis can evolve without
rewriting the day FSM decision.

## Rejected alternatives

- **Let Cycle write statuses:** this splits one FSM across modules.
- **Best-effort partial terminal processing:** it breaks limit and partition
  conservation.
- **Infer terminality from events:** indexes and records remain authoritative.

## Open questions and technical debt

1. READY selection uses storage-set iteration and processes only one record. Define
   canonical ordering, fairness and bounded backlog catch-up.
2. Timestamp jumps can cross several phases while edge side effects are written for
   only some transitions. Model and test every multi-edge jump or require stepwise
   advancement.
3. `IN_PROGRESS` is stored but unused. Remove it compatibly or define durable
   cursor/recovery semantics for it.
4. `mark_wwd_failed` accepts overly broad predecessor states. Restrict the deepest
   mutator to the legal transition graph.
5. Raw `u8` status/type entrypoints can admit unknown values. Use typed decoding and
   make corruption an invariant failure.
6. Window arithmetic contains unchecked additions; establish timestamp bounds and
   checked operations.
7. UTC accounting dates and UTC+14 WorldwideDay dates share integer encoding. Add
   distinct types and boundary fixtures.
8. Limit application can overwrite rather than accumulate. Prove unique ordered
   writers or introduce an idempotent contribution ledger.
9. Normalize terminal auction state for RED, GREEN and UNKNOWN zero-limit paths.
10. Terminal FIFO deletion removes the record while Oracle snapshots, events, Nods
    and retired Tribute history still reference the day. Define the durable history
    source and query semantics.
11. An error path may write FAILED/event and then return an error, causing rollback
    of its diagnostic. Add non-consensus observability without implying committed
    state.
12. Add an independent property model for all statuses, timestamp jumps, failures,
    retries, limit writers, partition state and FIFO eviction.
