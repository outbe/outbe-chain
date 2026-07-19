# ADR-S-CYC-001: Deterministic Cycle scheduling

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
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
compile-time `ACTIVE_TRIGGERS` table is fork-governed protocol data. Each entry
defines a stable id, label, schedule, handler, and whether certified-parent
accounting must already exist.

The currently registered schedule is:

| Id | Slot | Command invoked | Accounting gate |
|---:|---|---|---|
| 0 | daily 00:00 UTC | previous UTC-day emission command | required |
| 1 | daily 00:00 UTC | Intex call scan | not required |
| 2 | daily 12:00 UTC | WorldwideDay advancement command | not required |

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

### Time and catch-up semantics

Cycle schedules against UTC. Domain calendars such as WorldwideDay UTC+14 are
typed inputs constructed by the receiving module. Cycle passes canonical block
context or an explicitly specified scheduled context; it does not reinterpret
domain dates.

The current implementation catches up at most one overdue slot per trigger per
block. This behavior is recorded as implementation evidence, not accepted policy,
until bounds and stale-command semantics are decided below.

## Authoritative interfaces

| Responsibility | Production owner |
|---|---|
| System-transaction placement | `CycleLifecycle` under ADR-B-EVM-001 |
| Trigger declaration/order | `ACTIVE_TRIGGERS` |
| Slot calculation | schedule/`next_fire_at` functions |
| Cursor and history | Cycle storage schema |
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

## Security and compatibility

Registry contents, ids, order, schedules, date arithmetic, first-activation rule,
and accounting gates are consensus-critical. Any change requires fork activation
and cursor migration policy. Environment variables may not alter them in a
production chain unless the chain specification commits the value.

A handler must not be registered until its worst-case work, failure mode and
idempotency are known. Cycle is not a generic cron facility for operator jobs.

## Production-interface verification evidence

Inspected production paths include Cycle begin-zone dispatch, trigger anchoring,
slot calculation, accounting gating, checkpoints, cursor writes, midnight/noon
fixtures, retry tests, and timestamp-gap tests. This establishes the implemented
shape but not the unresolved activation and catch-up policies, so the ADR remains
Proposed.

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

1. `ACTIVE_TRIGGERS` currently describes order as informational although handlers
   share downstream state. Make order normative and add a structural order test,
   or prove commutativity for every pair.
2. A large timestamp jump replays one historical slot per block against current
   state. Define maximum catch-up, stale-slot skip/aggregate rules and recovery.
3. First observation anchors to the observed block rather than a genesis- or
   activation-derived slot. Define deterministic initialization for late forks,
   snapshots and reindexing.
4. Prove that the enclosing `CycleTick` failure class really permits the documented
   next-block retry without producing a partially canonical block.
5. Decide whether one failing trigger blocks later ids in the same block. Encode
   this explicitly in the scheduling contract and tests.
6. The generic allocation/fallback machinery historically associated with Cycle
   is not scheduler responsibility. Remove dead code or relocate it to the owning
   economic module.
7. Define behavior when a registered command has no work (for example a zero daily
   cap): handler success/no-op must be distinguishable from an unprocessed slot.
8. Add generated model tests spanning leap days, timestamp equality, multiple due
   triggers, long gaps, failures, retries, reorgs and restarts.
9. Add a versioned migration test proving that trigger id reuse, removal or schedule
   change cannot reinterpret an existing cursor.
10. This decision still requires human acceptance before status can change.
