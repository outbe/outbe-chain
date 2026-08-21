# ADR-S-CYC-001: Deterministic Cycle scheduling

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-30
- **Decision owners:** Execution and protocol-scheduling maintainers
- **Scope:** `crates/system/cycle`, `CycleLifecycle`, and the Cycle system transaction
- **Depends on:** ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-S-VAL-001, ADR-S-SLS-001, ADR-C-MET-001,
  ADR-C-AGR-001, ADR-C-LYS-001
- **Supersedes:** `029-daily-economic-orchestration-and-domain-fsm.md`

## Context

Consensus code needs a deterministic way to run calendar-triggered protocol
commands. Cycle is that scheduler. It does not own emission formulas, rewards,
WorldwideDay state, auctions, or Lysis. Those modules expose scheduled commands;
Cycle decides only **when**, **in what order**, and **with what retry cursor** they
are invoked.

Combining scheduler mechanics with handler business logic obscures authority and
makes a module audit ask the wrong module to defend foreign state. This record
therefore treats Cycle as a narrow scheduling service.

## Decision

### Cycle owns the trigger registry and cursor

The begin-zone `CycleTick` system transaction invokes `CycleLifecycle`. The
trigger table is fork-governed protocol data. Each entry defines a stable id,
label, schedule, handler, and whether certified-parent accounting must already
exist. The WorldwideDay advancement period is the immutable resolved
`metadosis.advanceIntervalSeconds` from genesis; Cycle does not parse genesis
JSON or read mutable configuration.

The currently registered schedule is:

| Id | Label | Slot | Command invoked | Accounting gate | Coalesces backlog |
|---:|---|---|---|---|---|
| 0 | `protocol_cycle` | hourly at `HH:00` UTC | contiguous-day emission or missed-day forfeiture, then one WWD/Metadosis pass | required | yes |
| 1 | `intex_call_daily` | daily 00:00 UTC | Intex call scan | not required | no |
| 2 | reserved | inactive historical id | none | n/a | n/a |
| 3 | `auction_advance` | every 12h | auction schedule advancement | required | no |
| 4 | `gem_call_daily` | daily 00:00 UTC | Gem force-call / forfeit-burn scan | not required | no |
| 5 | `auction_clearing` | every 10 min | auction clearing sweep | not required | yes |
| 6 | `intex_notify` | every 10 min | Intex lifecycle-notice drain | not required | yes |
| 7 | `credis_call_daily` | daily 00:00 UTC | Credis price-path scan: latch, call, void | not required | no |

Ids are permanent: they are emitted as the indexed `id` on `CycleTriggerExecuted` and key
the `Cycle` mappings, so new triggers append and existing ones are never renumbered.

Names and order are normative even if handlers happen to commute today. A handler
is an imported command boundary. Its calculations, state transitions, sinks and
events belong to that module's ADR.

### Scheduling algorithm

For every trigger, Cycle persists its last successfully executed scheduled time
and block. On first observation it anchors the cursor without firing. Thereafter
`next_fire_at` returns the first configured slot strictly after that cursor.

At each block Cycle:

1. reads consensus block time, never local wall time;
2. iterates the registry in stable id order;
3. skips triggers whose next slot is not due;
4. enforces the declared parent-accounting prerequisite;
5. invokes one due slot inside a checkpoint; and
6. writes the scheduled cursor and success event only after the handler succeeds.

A failed handler leaves its cursor unchanged, so the same scheduled slot is
eligible for retry. Cycle never fabricates a handler-specific completion marker.

ProtocolCycle is the sole calendar owner. It reads `Cycle.active_utc_day`. If the
block day is its immediate calendar successor, Cycle settles that one completed
day and advances the cursor after successful settlement. If the block day is
more than one day ahead, every completed day in the gap is forfeited: Cycle
advances the cursor directly without synthesizing emission, rewards, receipts,
WorldwideDays, Metadosis limits, Promis credit, or OCOMP work for the missed
days. It then invokes the existing Metadosis command exactly once; Metadosis
creates the current WWD when needed, advances every active WWD across all due
phase boundaries, and selects at most one READY WWD in its canonical order.
Cycle does not own those WWD economic branches.

Each completed-day settlement has two coordinated facts: Rewards'
`daily_settled[previous_day]` schedule marker and Metadosis'
`DayLimitFormationReceipt`. Both are written in the same outer checkpoint.
`true/receipt` replays without effects; `false/no receipt` proceeds;
`true/no receipt` or `false/receipt` is fatal before mutation.

### Time and catch-up semantics

Cycle schedules against UTC. Domain calendars such as WorldwideDay UTC+14 are
typed inputs constructed by the receiving module. Cycle passes canonical block
context or an explicitly specified scheduled context; it does not reinterpret
domain dates.

Missed hourly ProtocolCycle slots coalesce to one invocation. A same-day gap has
no daily economic action. A contiguous day transition settles exactly the prior
day. A multi-day gap forfeits every completed day and advances the calendar
cursor to the block day; lost calendar economics are never replayed. Other
non-coalescing triggers retain their existing per-slot behavior.

## Authoritative interfaces

| Responsibility | Production owner |
|---|---|
| System-transaction placement | `CycleLifecycle` under ADR-B-EVM-001 |
| Trigger declaration/order | `ACTIVE_TRIGGERS` |
| Slot calculation | schedule/`next_fire_at` functions |
| Cursor and history | Cycle storage schema |
| UTC-day ownership | `Cycle.active_utc_day` |
| Handler transaction boundary | per-trigger checkpoint |
| Parent-accounting prerequisite | trigger metadata plus Rewards query |

No user ABI may advance cursors or claim a trigger completed.

## Persistent state and invariants

- Trigger ids are unique and stable across a protocol version.
- A cursor is monotonic in scheduled time and changes only after handler success.
- A successful `(trigger_id, scheduled_at)` pair is committed at most once.
- Failure commits neither cursor, success event, nor handler state.
- Registry traversal order is identical on every node.
- Block time and chain configuration are the only time/configuration inputs.
- `active_utc_day <= block_utc_day`; invalid, zero, or future cursors are fatal.
- A contiguous completed day is settled exactly once. A multi-day halt has no
  settlement side effects for missed days and advances the cursor atomically
  with the remaining ProtocolCycle work.
- Stored cursor data refers to a trigger whose semantic identity has not changed
  without an explicit migration.

Structural tests must enumerate every registry entry, prove unique ids, and run
the same timestamp sequences through a reference scheduler.

## Atomicity, replay, and recovery

Each trigger invocation has one checkpoint containing the handler call, cursor
write, and success event. The enclosing Cycle system transaction follows the
failure classification in ADR-B-CNS-003. Retry is safe only when the handler command is
itself atomic and replay-safe; that proof belongs to the handler ADR.

Cycle's replay key is `(trigger_id, scheduled_at)`. A node restart reconstructs no
schedule from wall time; it resumes from canonical storage. Reorg behavior follows
normal EVM state rollback.

Metadosis extends that schedule identity only inside its owner receipt; it does
not create a second Cycle cursor. Handler storage/events/CE work, the semantic
receipt, cursor and success event commit or roll back as one execution scope.

## Security and compatibility

Registry contents, ids, order, schedules, date arithmetic, first-activation rule,
and accounting gates are consensus-critical. This design targets a fresh network:
genesis seeds `active_utc_day` from the root header timestamp, so no migration or
legacy activation path exists. Environment variables may not alter these values.

A handler must not be registered until its worst-case work, failure mode and
idempotency are known. Cycle is not a generic cron facility for operator jobs.

## Production-interface verification evidence

Inspected production paths include Cycle begin-zone dispatch, trigger anchoring,
slot calculation, accounting gating, checkpoints, cursor writes, hourly/day-boundary
fixtures, retry tests, and multi-day timestamp-gap tests. This establishes the implemented
shape, fresh-chain activation, and missed-day forfeiture policy; the ADR remains
Proposed until the end-to-end evidence listed by the implementation issue lands.

## Consequences

Cycle becomes a small, auditable scheduler. Economic and domain modules can be
audited independently while still declaring their scheduling dependency. Adding a
trigger now requires an explicit cross-link rather than expanding Cycle's domain.

## Rejected alternatives

- **Independent module timers:** ordering and retry state could diverge.
- **Wall-clock jobs:** they are not consensus-replayable.
- **Advance cursor before calling:** a failure would permanently skip work.
- **Put handler FSMs in this ADR:** it assigns foreign invariants to the scheduler
  and prevents a module-level architecture-conformance verdict.

## Open questions and technical debt

1. Monitor the economic and operational impact of deliberate missed-day
   forfeiture during a multi-day chain halt.
2. First observation anchors to the observed block rather than a genesis- or
   activation-derived slot. Define deterministic initialization for late forks,
   snapshots and reindexing.
3. Prove that the enclosing `CycleTick` failure class really permits the documented
   next-block retry without producing a partially canonical block.
4. Decide whether one failing trigger blocks later ids in the same block. Encode
   this explicitly in the scheduling contract and tests.
5. The generic allocation/fallback machinery historically associated with Cycle
   is not scheduler responsibility. Remove dead code or relocate it to the owning
   economic module.
6. Define behavior when a registered command has no work (for example a zero daily
   cap): handler success/no-op must be distinguishable from an unprocessed slot.
7. Add generated model tests spanning leap days, timestamp equality, multiple due
   triggers, long gaps, failures, retries, reorgs and restarts.
8. Add a versioned migration test proving that trigger id reuse, removal or schedule
   change cannot reinterpret an existing cursor.
9. This decision still requires human acceptance before status can change.
