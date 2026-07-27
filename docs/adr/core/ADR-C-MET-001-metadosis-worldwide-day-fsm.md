# ADR-C-MET-001: Metadosis owns the WorldwideDay state machine

- **Status:** Proposed; OCOMP PoC target defined, current implementation is synchronous
- **Date:** 2026-07-26
- **Decision owners:** Protocol economics maintainers
- **Scope:** `crates/core/metadosis`, its WorldwideDay records and auction/PROMIS seams
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-CYC-001,
  ADR-S-ORC-001, ADR-S-OCM-004
- **Related:** ADR-S-OCM-001, ADR-C-TRB-001, ADR-C-TRB-002,
  ADR-C-LYS-001, PFS-002
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
                                                   |  |  |
                                                   |  |  +-> FAILED
                                                   |  +----> COMPLETED
                                                   +------> OFFCHAIN_PENDING
                                                               |
                                                               +-> OCOMP AWAITING_FINALITY
                                                               |      |
                                                               |      +-> VOTING_OPEN
                                                               |             |
                                                               |             +-> COMPLETED
                                                               |             +-> CONFLICTED
                                                               +-> READY(next attempt)
```

`IN_PROGRESS` is reserved by the current schema but is not a valid production
transition. Local OCOMP execution progress is deliberately not represented by
that status.

Creation derives all windows from canonical block time and the UTC+14
WorldwideDay calendar, inserts the record in the active set, and seals its Tribute
partition. Creation is idempotent by day identity.

Transition side effects are owned by the transition, not Cycle:

- FORMING completion snapshots current/previous Oracle VWAP and determines the
  day type;
- entry to OFFERING unseals Tribute offers;
- exit from OFFERING seals offers;
- READY processing splits the limit into `lysis_budget` and `auction_base`;
- a GREEN day dispatches `auction_base` to Desis before OCOMP starts;
- a RED day credits `auction_base` to carry-over and dispatches no auction;
- the same transition creates a bounded `JobIntentV1` for `lysis_budget`;
- the q-forming full-result vote consumes the frozen request through a private
  typed apply capability, credits `unused_lysis` to carry-over and completes the
  day in the same outer checkpoint;
- expiry/conflict returns the day to READY without repeating the Desis brief;
- a terminal no-retry outcome credits the full `lysis_budget` to carry-over;
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
| eligible GREEN | dispatch exact `auction_base`; create `JobIntentV1` for `lysis_budget` | OFFCHAIN_PENDING |
| eligible RED | credit `auction_base`; create `JobIntentV1` for `lysis_budget` | OFFCHAIN_PENDING |
| q-forming full-result vote | atomically apply Nod/contributors/retirement; credit `unused_lysis` | COMPLETED |
| expiry/conflict with retry | keep Lysis budget and prior brief; create no duplicate effect | READY |
| terminal no-retry outcome | credit full `lysis_budget` once | FAILED |

Metadosis owns the day transition and frozen request values. ADR-S-OCM-004 owns
job evidence/vote/quorum-apply ordering. Metadosis does not own Lysis allocation
mathematics, Promis ledger rules or Tribute/Nod storage.

### Limit split and carry-over

The split is checked and immutable for one day:

```text
lysis_budget = min(gratis_demand, day_limit)
auction_base = day_limit - lysis_budget
```

`unused_lysis` is unknown until the certified Lysis result reaches quorum:

```text
unused_lysis = lysis_budget - sum(nod.gratis_load)
```

A live auction is never topped up. This avoids timing-dependent mutation after
Desis has advanced its stage.

`PromisLimit.total_unallocated` is the carry-over accumulator. The next
Metadosis limit formation atomically adds and clears its current value.

Credit arriving after a limit was formed waits for the next not-yet-formed day.

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
- one day has at most one live OCOMP attempt;
- one day dispatches at most one Desis brief before all OCOMP attempts;
- `OFFCHAIN_PENDING` has no Lysis/Nod/contributor/Tribute-retirement effect;
- its OCOMP attempt cannot open voting before
  `finality_recorded_height + 4`;
- quorum is derived only from that attempt's bounded on-chain result-vote slots;
- completion consumes only the canonical result carried by the q-forming vote;
- retries cannot repeat the auction or credit carry-over;
- every budget unit reaches Desis, a Nod or carry-over exactly once;
- FIFO eviction cannot leave active/index references.

## Atomicity, retries and failure

Request creation stores the immutable split, one Desis/carry-over base effect,
intent, OCOMP `AWAITING_FINALITY`, Metadosis `OFFCHAIN_PENDING` and request
event in one checkpoint. It does not create a response deadline.

Four blocks after the existing consensus path records request finality, OCOMP
atomically records `VOTING_OPEN(open_height, deadline_height)` and its deadline
index. This internal transition does not repeat any Metadosis economic effect.

The q-forming full-result vote commits immutable terminal state with all
Lysis-owned effects and the `unused_lysis` carry-over credit in the outer OCOMP
checkpoint.
It does not close the separate fourth-slot accountability record before the
response deadline, and later accountability writes cannot change terminal
receipt, active generation or exact-retry identity.

Response-window close expires/retries only an attempt that never reached q=3.
A timely q=3 was already applied by its q-forming vote transaction. Neither
that apply nor expiry rolls back or repeats the already committed auction split.

An invalid request rolls back to READY. An invalid vote or failed quorum apply
leaves the first-vote/quorum and domain state unchanged as defined by
ADR-S-OCM-003/004.
Exact terminal retry returns the recorded receipt;
a different result cannot re-enter Metadosis effects. No synchronous Lysis
fallback is reachable in the PoC profile.

Invalid stored state, broken membership or impossible windows are invariant
failures. Missing Oracle data is a specified business input to day typing; a nested
module failure is propagated unless a branch explicitly defines fallback.

## Security, compatibility and evidence

Status/type discriminants, date conversion, windows, retention bound and branch
semantics are consensus formats. `OFFCHAIN_PENDING`, pending nonce, request
bindings, budget split and carry-over rules activate with ADR-S-OCM-004.
Dev-only shortened windows must be chain-spec committed and cannot leak into
production by local environment.

Production runtime and tests were inspected for creation, window advancement,
Oracle snapshots, offer seal/unseal, synchronous terminal branches and FIFO
cleanup. The selected finality+4/direct-vote/separate-accountability OCOMP
states are not yet implemented end to end. There is not yet a complete
generated transition model, request/expiry path or cross-module
certified-activation suite.

## Consequences

Metadosis remains the sole day-state owner while OCOMP supplies a certified
asynchronous transition mechanism. Scheduler correctness, worker progress and
Lysis correctness no longer stand in for legal WorldwideDay transitions.

## Rejected alternatives

- **Let Cycle write statuses:** this splits one FSM across modules.
- **Best-effort partial terminal processing:** it breaks limit and partition
  conservation.
- **Infer terminality from events:** indexes and records remain authoritative.
- **Store worker `RUNNING` progress in Metadosis:** local scheduling is not a
  domain state transition.
- **Run synchronous Lysis when OCOMP times out:** it changes block work and
  bypasses the selected failure semantics.

## Open questions and technical debt

1. READY selection uses storage-set iteration and processes only one record. Define
   canonical ordering, fairness and bounded backlog catch-up.
2. Timestamp jumps can cross several phases while edge side effects are written for
   only some transitions. Model and test every multi-edge jump or require stepwise
   advancement.
3. `IN_PROGRESS` is stored but unused. Remove it compatibly; it must not be
   repurposed for local OCOMP worker progress.
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
9. Replace best-effort auction dispatch with one exact typed split/brief outcome.
10. Terminal FIFO deletion removes the record while Oracle snapshots, events, Nods
    and retired Tribute history still reference the day. Define the durable history
    source and query semantics.
11. An error path may write FAILED/event and then return an error, causing rollback
    of its diagnostic. Add non-consensus observability without implying committed
    state.
12. Add an independent property model for all statuses, timestamp jumps, failures,
    retries, limit writers, partition state and FIFO eviction.
13. Model `OFFCHAIN_PENDING`, pending nonce, one-time auction dispatch,
    activation completion, expiry, conflict and exact terminal retry.
14. Make limit formation atomically consume `PromisLimit.total_unallocated`.
    Prove that late credit waits for the next unformed day.
15. Amend PFS-009 before supported-network use so it consumes the certified
    Metadosis outcome rather than the synchronous Lysis sequence.
