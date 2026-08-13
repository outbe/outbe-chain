# ADR-S-OCM-004: OCOMP quorum certifies typed results and starts bounded materialization

- **Status:** Accepted
- **Date:** 2026-08-12
- **Owners:** OCOMP protocol, Metadosis, EVM/txpool, and certified domain owners
- **Depends on:** ADR-S-OCM-001, ADR-S-OCM-003, ADR-B-CNS-001,
  ADR-B-EVM-003, ADR-B-TXP-001
- **Related:** ADR-C-LYS-001, ADR-C-NOD-002

## Context

An off-chain result becomes canonical only through a validator quorum over one
finalized job. Applying arbitrary result bytes or rerunning an unbounded program
on-chain is forbidden. The protocol therefore binds membership, finality,
computation semantics, result shape, and typed domain activation in one job
lifecycle.

## Job membership and finality

OCOMP has no independent voting committee. Each attempt snapshots the ordered
ACTIVE ValidatorSet, including the validators' registered OCOMP keys. The job
derives `N` and `simplex_n3f1_quorum(N)` from that pinned snapshot; callers
cannot select membership, `N`, or quorum. Later ValidatorSet changes do not alter
an open job. A retry is a new attempt with the then-current ACTIVE snapshot.

Metadosis stores `JobIntentV1` and emits `OffchainJobRequested(intent_id)`. The
event is only a wake hint. The intent record and the finalized request block are
authority. Voting opens only after the exact request block is finalized and the
configured opening delay has elapsed.

The response window covers computation and vote inclusion. Quorum selects and
applies a canonical result but does not close accountability. Every pinned
validator may still vote until the exclusive deadline. At deadline, missing
validators that are still ACTIVE are jailed; missing members no longer ACTIVE
produce evidence only. A late vote fails its transaction without invalidating
the block.

## Vote and certified activation

A result vote binds the job, intent, protocol semantics, result commitment,
historical membership, and the validator's OCOMP key. The existing OCOMP key
registration and role-delegate maps identify and authorize the sender; no new
reverse map is introduced.

The quorum-forming transaction:

1. verifies the typed `LysisResultV1` and all pinned bindings;
2. records immutable quorum and the canonical result;
3. atomically applies the constant-size certified effects for Tribute,
   Metadosis, contributor state, carry-over, and NOD generation metadata; and
4. enqueues the certified NOD generation for later bounded materialization.

No individual NOD is issued in this transaction. The certified `nod_root` is
the authority for the later batches.

## NOD materialization carrier

The existing OCOMP system carrier has two independently classified candidates:

- result vote, authorized against the job's historical snapshot;
- NOD materialization, authorized against the current ACTIVE OCOMP role.

Both use ordinary EVM transaction transport with the exact system-carrier fee,
value, gas, and priority rules. No separate gas lane or consensus message is
introduced.

`materializeCertifiedNods` proves a bounded sequential batch against the FIFO
head's certified root and creates ordinary NOD ledger entries through
`issue_nod`. The canonical head and cursor are the authority. Any current ACTIVE
OCOMP delegate may win the submission race; proposer identity is only a local
wake hint.

Malformed envelopes and unauthorized senders are invalid for inclusion. Exact
typed race and proof failures return a failed receipt with full rollback while
the block continues. Storage corruption remains fatal. Arbitrary reverts are
never converted into soft failures.

## Restart and replay

All consensus-visible progress is in chain state:

- exact vote and sign-once subjects;
- canonical result and activation receipt;
- NOD generation projection;
- FIFO head/tail and sequence entries;
- NOD cursor, latest progress height, and per-block attempt counter.

Exact certified-result replay and exact local artifact replay are idempotent.
After restart, Supervisor rereads the canonical head and reconstructs only the
current bounded proof. Lost and duplicate wakes are safe. FullNodes execute and
verify materialization transactions but never submit them.

## Liveness

The embedded OCOMP ExEx observes finalized canonical blocks. When the local
validator was the finalized proposer, it sends a nonblocking local wake after
generation certification, materialization progress, or the configured no-
progress interval. Wake carries no authority and cannot delay block production,
import, canonicalization, or finality.

Missing local result artifacts cause that Supervisor to abstain and report the
failure. Another ACTIVE OCOMP delegate can submit the same canonical next batch.
There is no materialization deadline, jail condition, or replacement generation.

## Invariants

- Membership is the pinned ordered ACTIVE ValidatorSet for each job attempt.
- Quorum certification is the only authority for a Lysis result.
- Certified activation is constant-size and atomic.
- NOD materialization is sequential, bounded, proof-backed, and atomic.
- Vote and materialization authorization use their respective historical and
  current state without caller-selected membership.
- Restart and replay cannot duplicate votes, queue entries, NODs, or cursor
  progress.
