# ADR-C-MET-001: Metadosis owns the WorldwideDay state machine

- **Status:** Proposed; fresh-devnet implementation present, exact-revision Linux evidence pending
- **Date:** 2026-07-30
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

Creation derives all windows from canonical block time, the UTC+14 WorldwideDay
calendar and immutable `GenesisProtocolParametersV1`. Metadosis receives resolved
seconds from `outbe-chain-constants` and does not branch on chain id or on whether
a value came from JSON or a default. It persists the resulting absolute
boundaries, inserts the record in the active set, and seals its Tribute partition.
Creation is idempotent by day identity.

Fresh-devnet genesis must contain one canonical OCOMP fork install bound to the
chain id, genesis hash and exact install hash, with activation height `1`. Node
startup rejects a missing, malformed, mismatched or later install. The persisted
request profile and activation authority therefore exist before the first Cycle
command; absence of either at runtime is an invariant failure, never a switch to
a second execution mode.

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

### Outer time reducer

Active-day advancement is decided by the pure exhaustive
`plan_wwd_advance(current, block_time)` reducer. It returns an ordered list of
legal edges, a named terminal outcome, a no-op, or rejects backward/impossible
time. Effects are applied only after that decision through the purpose-bound
Metadosis command checkpoint.

Catch-up preserves every edge that actually occurred: a day that already
entered `OFFERING` is sealed exactly once before it advances to `WAITING` and
then `READY`. A day still in `FORMING` or `LOOKBACK_DELAY` at or after
`offering_end` never opened an economically valid offering window and instead
commits `MissedOffering`:

- the immutable formed day limit is credited exactly once to Promis carry-over;
- the Tribute partition must be initialized, sealed and empty, then returns a
  typed retirement outcome;
- populated state is corruption and rolls back storage, events and CE work;
- the day moves directly to `FAILED`, never `READY`;
- a durable typed receipt records value routing, retirement and block number;
- Desis and Lysis are not called.

The direct raw timestamp-to-status writer is test-fixture-only. Production
status changes use the reducer and exact edge effects.

Day type is GREEN only for a strictly rising valid VWAP. Missing/zero observations
and equality follow the currently implemented RED behavior pending economic review.

### Active order and retained-cap admission

Every validated active snapshot is sorted by the protocol key
`(scheduled_process_time, worldwide_day)`. Both time advancement and READY
selection consume that snapshot; physical set order is never economic order.
One tick admits at most one due `WAITING` candidate and settles at most one
READY day.

The scan bound is derived rather than configured:

```text
normal pipeline = ceil((50h + 502h + 50h + 12h) / 24h) + 1 restart insertion
                = 27 WorldwideDays
retained work   <= canonical MAX_RECORDS_KEPT
MAX_ACTIVE_WWDS = normal pipeline + canonical record-retention bound
```

The production genesis default advances at midnight and noon, so its 12-hour catch-up cadence is
strictly faster than the 24-hour creation cadence. With continuing ticks an
already-active candidate has at most 27 admission ticks (324 hours) of older
pipeline work ahead. Missing external finality is classified as retained OCOMP
progress, not scheduler starvation.

Fresh LocalNet genesis may shorten phase durations and the advancement interval.
Only timing changes: reducer transitions, effects and persisted absolute
deadlines are identical to production.

There is no smaller OCOMP concurrent-job admission limit. The aggregate remains
bounded by the canonical WWD record-retention policy used by all lifecycle
storage. If that underlying record-retention population is exhausted, the new
(therefore newest) admission candidate commits `CapacityForfeiture`; existing
retained days and all OCOMP indexes remain unchanged. The ordered atomic effect is:

1. validate aggregate, protocol order and exact cap;
2. authenticate and forfeit the sealed Tribute generation by aggregate
   count/nominal/root, request its CE retirement and advance generation;
3. credit only the victim's full formed Metadosis limit to Promis carry-over;
4. persist the linked terminal/detail receipts, move the WWD to `FAILED`, and
   emit the status and capacity terminal events.

Tribute nominal and issuance values are not converted into Promis, Nod or
Intex. Desis, Lysis and OCOMP request formation are not called. Storage,
events and CE work share one rollback domain, and replay observes the immutable
receipt without repeating an effect.

### READY command

At most one protocol-oldest selected READY day is processed per command:

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

The zero-limit and `UNKNOWN` rows are defensive state-integrity guards, not
states produced by the current production lifecycle. Cycle forms the day limit
before Metadosis runs, and the transition out of `FORMING` resolves the day type
to `GREEN` or `RED`. Tests may cover those guards at the Metadosis module seam,
but an end-to-end fixture must not manufacture either impossible READY state.

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
Only the daily Cycle terminal allocation may supply `base_limit` and form an
OCOMP day. Non-daily terminal headroom, including a `LateFinalizeCredits`
residue, must checked-add to carry-over; it must never call the formation sink
or win a first-writer race against Cycle.

## Interfaces and invariants

Production commands are day creation, active-day advancement, limit application,
READY processing, and terminal retention cleanup. Deep mutators must accept only
typed states and legal predecessor states.

### Purpose-bound mutation entry

Metadosis consensus mutation has one module-specific entry contract. Production
callers receive no raw schema, state, reducer, finality or fork initializer.
Those surfaces are crate-private; `test-utils` exposes fixtures only.

Every public production command enters the private `commit_transition` seam.
Before its callback and before any aggregate read or effect, the EVM storage
provider must consume one single-use authority binding for the command's exact
purpose:

- Cycle lifecycle binds the authenticated `CycleTick` route to a fixed set of
  distinct block-scoped command identities (genesis creation, terminal
  allocation, READY processing and active-day advancement); an identity cannot
  be consumed twice;
- certified finality binds the immediate-parent certificate metadata to the
  executor-preloaded finalized state root; caller-supplied
  number/hash/root values and `BlockRuntimeContext` are not authority;
- fork/profile mutation is authorized only by the immutable install hash supplied
  by the pinned chain manifest through `OcompLifecycleBegin` at the exact
  activation height; no separate fresh-devnet compatibility command exists;
- OCOMP lifecycle and result-vote mutation bind the authenticated system route
  or exact public vote calldata digest respectively.

Purposes are not interchangeable. The internal non-cloneable lease is held by
the primitives frame guard and is never returned or passed to the command
callback, so it cannot be obtained from `StorageHandle`. The callback receives
no untrusted/reentrant function supplied by calldata. The provider also checks
chain and block identity, rejects nesting, consumes each binding once, and
closes the active frame by exact purpose/binding identity.

`commit_transition` validates a complete typed `ValidatedWwdAggregate` before
and after the effect. It is the sole production rollback owner for provider
state and ordered EVM events: the provider-granted frame and journal checkpoint
cover record/index/event writes, and any error before commit restores them
together. Commands that mutate `ExecutionScope` CE work additionally use the
private command-level `with_ce_checkpoint`, because CE work is outside that
provider journal. No domain helper owns a nested production savepoint. The test
storage provider consumes the same single-use purpose budget so unit tests
cannot hide an extra production command behind a reusable entitlement.

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
event in one command-owned checkpoint. It does not create a response deadline.

Four blocks after the existing consensus path records request finality, OCOMP
atomically records `VOTING_OPEN(open_height, deadline_height)` and its deadline
index under the same command rollback owner. This internal transition does not
repeat any Metadosis economic effect.

The q-forming full-result vote commits immutable terminal state with all
Lysis-owned effects and the `unused_lysis` carry-over credit in the command
checkpoint; q-forming does not introduce a nested provider or CE savepoint.
It does not close the separate dynamic accountability record before the
response deadline, and later accountability writes cannot change terminal
receipt, active generation or exact-retry identity.

Response-window close expires/retries only an attempt that never reached its
snapshot-derived quorum. A timely quorum was already applied by its q-forming
system vote. At the exact job-pinned deadline (production default 1,800 blocks) every missing pinned participant
is recorded; only one whose current ValidatorSet status is still `ACTIVE` moves
to `JAILED`, while every non-ACTIVE status remains unchanged. Timely minority
votes count as present. Neither quorum apply, jail nor expiry rolls back or
repeats the already committed auction split.

An invalid request rolls back to READY. An invalid vote or failed quorum apply
leaves the first-vote/quorum and domain state unchanged as defined by
ADR-S-OCM-003/004.
Exact terminal retry returns the recorded receipt;
a different result cannot re-enter Metadosis effects. No synchronous Lysis
fallback is reachable in the PoC profile.

Invalid stored state, broken membership or impossible windows are invariant
failures. Missing Oracle data is a specified business input to day typing; a nested
module failure is propagated unless a branch explicitly defines fallback.

Metadosis has no synchronous Lysis production branch. The four local READY
outcomes—zero limit, `UNKNOWN` type, empty Tribute day and zero Gratis
allocation—are closed typed variants. Every populated positive-gratis READY day
creates OCOMP pre-admission; only a verified OCOMP result can apply Lysis-owned
effects.

## Security, compatibility and evidence

Status/type discriminants, date conversion, windows, retention bound and branch
semantics are consensus formats. `OFFCHAIN_PENDING`, pending nonce, request
bindings, budget split and carry-over rules activate with ADR-S-OCM-004.
Dev-only shortened windows must be chain-spec committed and cannot leak into
production by local environment.

Production runtime and tests cover creation, exhaustive window advancement,
Oracle snapshots, offer seal/unseal, local terminal branches, MissedOffering
value routing/retirement/rollback, ordered READY selection, derived active
caps, CapacityForfeiture aggregate retirement/value routing/replay/rollback,
OCOMP request admission and FIFO cleanup.
Fresh-devnet startup tests reject missing, malformed, mismatched and late OCOMP
installs, and Cycle block 1 fails before WWD effects when the persisted profile
is absent. Linux E2E evidence remains governed by the Metadosis closure plan.
The only retained Metadosis savepoints outside the command seam are
`#[cfg(test)]` fixture builders. Lysis activation authority remains a
capability/order frame, not an independent rollback boundary.

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

1. `IN_PROGRESS` is stored but unused. Remove it compatibly; it must not be
   repurposed for local OCOMP worker progress.
2. UTC accounting dates and UTC+14 WorldwideDay dates share integer encoding. Add
   distinct types and boundary fixtures.
3. Terminal FIFO deletion removes the record while Oracle snapshots, events, Nods
    and retired Tribute history still reference the day. Define the durable history
    source and query semantics.
4. Extend the independent OCOMP model for every future protocol-bundle version;
   the current version covers `OFFCHAIN_PENDING`, pending nonce, one-time brief,
    activation completion, expiry, conflict and exact terminal retry.

The Citadel implementation closes the former overwrite, partial-error and missing
model debts for the selected fresh-devnet profile: day-limit formation has a
durable idempotent receipt, failed transitions restore storage/events/CE work, and
the outer reducer plus OCOMP model cover the current protocol version. These are
implemented safeguards, not open debt.
