# ADR-B-CNS-003: Finalized consensus blocks cross one acknowledged execution and persistence pipeline

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-17
- **Scope:** consensus executor/marshal integration, `ConsensusExecutionBridge`, Reth Engine handles, CE finalized committer
- **Depends on:** ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-CNS-001,
  ADR-B-OCD-005 and ADR-B-OCD-008 through ADR-B-OCD-013
- **Related:** ADR-B-EVM-001 block execution order, ADR-B-OCD-014 recovery reconciliation

## Context

Consensus and execution share a process but retain separate durable histories and
asynchronous actors. “Finalized by Simplex” does not itself mean “durably applied
by Reth and compressed-entity storage”. Marshal must not prune or report progress
past a block the local execution path has not actually committed.

The production delivery seam is Marshal `Update::Block(block, ExactAck)` into the
single `ExecutorActor` mailbox (`consensus/src/executor/ingress.rs:91-130`).

## Decision

For every non-genesis finalized block at height `N`, the executor performs this
ordered protocol:

```text
Marshal finalized block + ExactAck
  -> require Mongo projection ready at exact parent (N-1, parent_hash)
  -> Reth new_payload(block)
  -> accept Valid or Syncing; reject every other status/error
  -> fork_choice_updated(head=safe=finalized=block)
  -> commit actor's LastCanonicalized only after non-invalid FCU response
  -> commit finalized CE block/marker through FinalizedCeCommitter
  -> acknowledge Marshal ExactAck
  -> notify finalized subscribers and execution-height observers
```

Genesis is acknowledged only when the delivered digest equals the recovered
canonical genesis anchor. Startup backfill replays every height in the contiguous
range `(execution_finalized, marshal_finalized]`; a missing Marshal block or a
locally unexecutable finalized block is fatal, never skipped
(`executor/actor.rs:348-421`).

### Readiness and authority

Mongo is an exact-parent read prerequisite for deterministic execution, not part
of the finalization vote. `ProjectionAhead`, fatal readiness, channel closure or
an impossible budget expiry fail local finalized application. CE finalization is
after successful FCU and before ACK. Mongo projection of the new block remains an
asynchronous finalized consumer and is not inserted into the ACK critical path.

Reth canonical state owns executed block/state; Marshal owns consensus-finalized
archive/progress; CE MDBX owns its finalized authenticated materialization; Mongo
owns its projection checkpoint. ADR-B-OCD-007 must reconcile their identities at restart.

### Canonicalization semantics

`LastCanonicalized` is immutable-until-success actor state. `canonicalize` computes
a candidate forkchoice, sends FCU, and assigns the new state only after a response
that is not invalid. Payload-building requests additionally require a payload ID;
absence does not commit the actor state (`executor/actor.rs:554-657`). Finalized
height never regresses.

`Syncing` from `new_payload` or FCU is currently treated as non-fatal and proceeds.
The heartbeat periodically resends the last committed forkchoice without payload
attributes; heartbeat errors are diagnostic and do not mutate actor state.

## Delivery FSM

| Current | Event | Guard | Effects | Next/error |
|---|---|---|---|---|
| Recovered | Marshal ahead | every intermediate block retrievable | sequential replay through full pipeline | Live or fatal |
| Recovered | execution ahead | no reconciliation performed here | warn, skip backfill | Live with ADR-B-OCD-007 debt |
| Live | finalized genesis | exact recovered anchor | ACK only | Live |
| Live | finalized successor | exact parent projection ready | new_payload -> FCU -> CE commit -> ACK | Advanced |
| Live | duplicate/older canonicalize | immutable update is no-op | ACK only if full delivery already durable | Unchanged |
| Applying | EL invalid/error | finalized block cannot apply | no ACK; structured fatal error | Node shutdown |
| Applying | FCU invalid/error/dropped response | canonicalization failed | no actor commit, no CE commit, no ACK | Node shutdown |
| Applying | CE commit error | EL may already be canonical | no ACK; fatal recovery required | Node shutdown |
| Live | mailbox closes | lifecycle shutdown | actor exits cleanly | Stopped |

The CE-error row crosses atomicity domains: Reth can be ahead of CE after failure.
It is safe only because Marshal remains unacknowledged and restart reconciliation
repairs or rejects the state before participation.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Commit/receipt | Replay |
|---|---|---|---|---|
| Enqueue Marshal update | Reporter mailbox | unbounded process mailbox | `Feedback::Ok/Closed` | Marshal Exact ACK remains outstanding |
| Execute/import block | Reth Engine handle | Reth DB/journal | payload status | same block identity must be idempotent |
| Canonicalize/finalize | Reth FCU | Reth canonical forkchoice | non-invalid FCU response | heartbeat repeats committed state |
| Update actor state | ExecutorActor | process memory | assignment after FCU success | seeded from recovered finalized state |
| Commit CE materialization | `FinalizedCeCommitter` | CE MDBX transaction/marker | propagated result | exact block marker/idempotency rules |
| ACK Marshal | Exact acknowledgement | Marshal progress/pruning | `acknowledge()` | only after all required durable effects |
| Notify subscribers | ExecutorActor | process-local wakeup | oneshot/channel signal | advisory; provider hash must be rechecked |

## Persistent invariants

- `actor.finalized_height/hash` never regresses and describes the last successful
  FCU plus CE barrier observed by the actor.
- Marshal ACK height cannot exceed locally completed required durable effects.
- Backfill is contiguous; no missing finalized height is tolerated.
- Exact-parent Mongo checkpoint identity matches the block's parent before execution.
- CE finalized marker advances in the same order as finalized blocks and matches
  canonical receipts/root artifacts.
- Subscriber notification is not durable proof; consumers re-read canonical state.

## Concurrency, replay and bounds

One executor actor serializes canonicalize/build/Marshal messages. Its live select
is biased toward mailbox work over heartbeat. The mailbox is unbounded by design;
the expected input rate is consensus block rate, but this is an availability
assumption rather than structural backpressure.

Marshal redelivery after uncertain failure is required. EL import, FCU and CE
commit must therefore converge for the same height/hash without repeating
non-idempotent protocol effects. A different hash at an already finalized height
is corruption/fatal, not “last writer wins”.

## Verification evidence

Unit/deterministic-runtime evidence covers recovered state, non-regression,
heartbeat replay, syncing delivery, ACK ordering, CE commit gating, finalized
subscriptions and missing-backfill fail-fast. Node/e2e tests cover restart and
follower catch-up through the production binary.

## Consequences

- Consensus progress is not acknowledged from an enqueue or an EL response alone.
- A finalized block that one node cannot execute causes fail-closed shutdown.
- Cross-store failures intentionally require restart reconciliation rather than
  unsafe compensation of canonical chain state.
- The actor is a deep serialization seam but its unbounded mailbox can accumulate
  local availability debt.

## Rejected alternatives

### ACK immediately when Marshal reports a block

Rejected because Marshal could prune recovery data before execution/CE durability.

### Skip a missing/unexecutable finalized block during backfill

Rejected because it creates a non-contiguous local chain and hides divergence.

### Roll back Reth after CE commit failure

Rejected without a proven cross-store rollback protocol; canonical compensation
would be more dangerous than retaining the unacknowledged recovery point.

## Open questions and technical debt

- `new_payload` and FCU `Syncing` are treated as success for progression. Prove
  that subsequent CE commit and ACK cannot occur before Reth has durably imported
  and canonicalized the block, or tighten the accepted status contract.
- The critical executor mailbox is unbounded and has no depth metric, cap or
  overload shutdown policy.
- `execution ahead of consensus` only warns and skips backfill. ADR-B-OCD-007 must define
  the only legal identities and whether this state is repairable or fatal.
- CE failure occurs after FCU may have advanced Reth. A fault-injection matrix must
  cover crash before/after FCU durability, CE transaction commit, marker write and
  ACK delivery.
- Dropping the unacknowledged token may also trigger a caught Marshal panic; there
  are two fatal signals. Supervision must prove deterministic shutdown and preserve
  the structured root cause.
- Notifications ignore send failure. They are advisory, but liveness of peer-manager
  and other consumers needs explicit ownership/monitoring.
- The test-only “drop new_payload” fault path can continue to FCU; ensure no
  production configuration can activate it and tests do not normalize an invalid
  production sequence.
- Exact idempotency evidence for repeated `new_payload`/FCU/CE commit of the same
  finalized block is incomplete at the full production interface.
- There is no typed transaction capability spanning the required steps; correctness
  relies on ordering plus recovery. ADR-B-OCD-007 must be accepted before calling G4 closed.
- This ADR requires human acceptance before its `Proposed` status changes.
