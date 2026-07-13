# T32 — Gate D3: Reth/Commonware persistence feasibility spike

Status: todo
Source: `audit_plan.md` §5 P1-3, §8 Gate D3; concept §13.2 (Q14 mechanism must be proven on the pinned
revision before T16/T17 are implemented)
Depends on: —  (read-only investigation)
Blocks: T16, T17

## Summary

Read-only spike on the pinned Reth revision + Commonware marshal proving the durable-persistence barrier
is implementable exactly as §13.2 describes, and fixing the numbers/diagrams T16/T17 need.

## Questions to answer (deliverables)

1. Concrete pinned-Reth APIs: how `PersistedBlockSubscriptions` behaves with `persistence_threshold = 0`
   and `memory_block_buffer_target = 0`; how a DB-only provider verifies at height H — exact canonical
   hash, durable receipts, EVM slot (`0xEE0B.slot1`), persisted tip — including under configured
   pruning/history settings.
2. Numeric crash-recovery retention floor: replace "one in-flight block plus safety margin" with a
   number derived from the actual redelivery/ack window.
3. ACK ownership and redelivery semantics on the Commonware side: who owns the ack handle across restart,
   exactly-once behavior, `MAX_PENDING_ACKS = 1` interaction with backfill.
4. Sequence diagram of the full order (Marshal durable → FCU → durable barrier → SMT commit → ACK) and a
   fault map: for each crash point, which §13.3 row it lands in.
5. Failure classification (audit-final M-10): which pinned-Reth persistence/provider failures are
   TRANSIENT (safe bounded retry holding the ACK/pruning posture) vs which observations prove a
   durable-state contradiction (fail-closed corruption row) — consumed by T16's typed classifier.

## Acceptance criteria

1. Spike report merged at `outbe-plan/ces-persistence-spike.md` (stable path): named APIs with file:line into the pinned
   checkout, the retention-floor number, sequence diagram, fault map.
2. T16/T17 updated to consume the concrete APIs and floor (their "wait for PersistedBlockSubscriptions,
   then verify via DB-only provider" wording becomes named calls).
3. Any infeasibility discovered is escalated as a concept amendment proposal, not worked around silently.

## Invariants

- Read-only: no production code changes in this task.
