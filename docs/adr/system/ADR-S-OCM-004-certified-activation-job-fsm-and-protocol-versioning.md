# ADR-S-OCM-004: OCOMP activation is a typed certified job transition

- **Status:** Proposed; PoC not implemented
- **Date:** 2026-07-23
- **Decision owners:** System Space, block-execution and participating domain owners
- **Scope:** JobIntent lifecycle, finality binding, deadline/expiry, activation
  verification, private authority, atomic receipts, replay and protocol bundle
- **Depends on:** ADR-S-OCM-001, ADR-S-OCM-003, ADR-B-WIR-001,
  ADR-B-CNS-001, ADR-B-CNS-003, ADR-B-EVM-001, ADR-B-EVM-003,
  ADR-B-EVM-005, ADR-B-TXP-001, ADR-S-GOV-003
- **Related:** ADR-B-OCD-011, ADR-C-MET-001, ADR-C-LYS-001,
  ADR-C-TRB-001, ADR-C-NOD-001, ADR-C-NOD-002, ADR-C-PRM-003,
  ADR-C-DES-001, PFS-002
- **Supersedes:** None

## Context

An off-chain result cannot mutate canonical state merely because several
validators signed bytes. Consensus must know which finalized input was computed,
whether the attempt is still live, whether the result is structurally valid and
which exact domain effects it may authorize. Re-executing Lysis on-chain is
forbidden, while interpreting arbitrary writes would discard all domain
invariants.

The job lifecycle and activation transaction therefore form one typed consensus
protocol.

## Decision

### Request and finality

In the terminal Metadosis phase, an eligible non-empty READY day atomically:

1. validates bounded pre-admission and freezes the request-time economic values;
2. splits `day_limit` into `lysis_budget` and `auction_base`;
3. dispatches one GREEN Desis brief, or credits RED `auction_base` to carry-over;
4. stores `JobIntentV1`, due/expiry indexes and `OFFCHAIN_PENDING`;
5. emits `OffchainJobRequested(IntentId)`; and
6. returns without invoking Lysis or creating Nod/contributor effects.

Desis is a request-phase effect, not a Lysis result or activation owner. The
brief is never topped up and is not repeated by a retry.

The intent stores immutable activation preconditions. It does not copy a
pessimistic reservation record into Desis, Intex or PromisLimit.

The event is a wake-up hint. The intent record is authority. After its request
block finalizes, `JobId` binds the canonical intent to the exact finalized block,
state root and finality proof. A pre-finality reorg makes that candidate and all
derived work non-signable.

Consensus state contains no `RUNNING`; that is local supervisor progress.
It also contains no state per worker shard. One `JobIntentV1` covers the complete
authenticated WWD input; all shard progress is replaceable local execution
state.

```text
READY
  -> OFFCHAIN_PENDING(IntentId)
       -> COMPLETED
       -> EXPIRED
       -> CONFLICTED
       -> CANCELED
  -> READY(next attempt after terminal release, when policy permits)
```

### Exclusive deadline and ordering

At block height `h`, the begin-zone expires jobs with
`deadline_height <= h` before ordinary activation transactions. Therefore an
activation is valid only for `h < deadline_height`; at the deadline it observes
the already terminal job.

Expiry returns the day to its retry state without synchronous fallback. It
retains the frozen Lysis budget and never repeats the committed auction brief.

A terminal no-retry outcome credits the whole `lysis_budget` to carry-over once.

The block lifecycle reserves stable positions for future pause/revocation and
protocol activation, but the PoC keeps those slots no-op.

### Activation verification

`activateLysis` is a normal bounded public transaction carried through RPC,
txpool, gossip, proposal, import and replay. Before large decode or
cryptographic work, the executor checks byte/count/crypto caps. It then:

1. returns the recorded receipt for an exact retry of an already completed
   binding/digest;
2. rejects a completed job with a different binding/digest and all other
   terminal states;
3. verifies the finalized intent proof, `JobId`, attempt, bundle, committee,
   deadline and current target preconditions;
4. reconstructs the exact `ActivationPayloadV1`/`ResultDigest`;
5. verifies the `q=3/4` execution certificate;
6. invokes the closed Lysis structural/result verifier without executing Lysis;
7. constructs a private unforgeable `CertifiedLysisActivation`;
8. verifies and installs only the certified old-root-to-new-root generation
   transition, plus constant-size scalar effects, through closed domain-owner
   methods and typed receipts; and
9. commits all domain effects, active generation, terminal receipt and
   `COMPLETED` inside one outer checkpoint.

The activated result covers the complete parent Job Intent. A shard artifact,
prefix of completed shards or per-shard certificate cannot enter activation.
If any required shard is absent, the job remains pending or expires; it never
creates a partial set of Nod.

The capability has no public constructor, codec or generic supertype that grants
effect authority. Effect owners accept only the exact Lysis-scoped capability or
an owner-scoped derivative created inside the certified path.

### Atomic effect contract

The Lysis apply sequence installs the certified Nod/contributor/output
collection roots, logically retires the exact sealed Tribute generation, credits
the scalar `unused_lysis` carry-over and marks Metadosis complete. It does not
iterate over `N` Nod or contributor actions on-chain. Bounded
`ResultChunkV1` bodies are authenticated by `result_chunk_list_root` and feed
projection, availability and proof-serving paths; they are not independent
activation transactions.

Desis is absent because its exact `auction_base` brief committed before compute.
PromisLimit receives only a checked additive carry-over credit.

Each activation owner returns a constant-size receipt bound to the call and
`JobId`. Old roots/generations, new roots, counts, totals and both budget
equations are checked before terminal commit.

Any owner error or receipt mismatch reverts every activation effect.

Activation never accepts storage addresses, keys, opcodes, generic calls or a
list of user-selected writes.

### Protocol identity and evolution

Every consensus or durable OCOMP object pins one `ProtocolBundleHash`. For the
PoC/BoundedMVP Lysis V1 path, the bundle already fixes intent/action/result
codecs, Lysis semantics, planner/reducer, evidence/signature domain, capacity
profile, apply semantics, effect/codec registries, historical decoders and
required handlers. A one-entry `ProgramRegistry` would duplicate this authority
and is not created.

Compatibility negotiation can make a local validator abstain; it never selects
job semantics. Live jobs finish or expire under their pinned bundle. New
semantics use a new bundle/fork and never reinterpret historical bytes.

PoC-to-BoundedMVP evolution preserves the job/result/apply meaning while replacing
demo key storage, scheduler, isolation, retention and operations. A changed input,
planner, result or apply contract is a new protocol, not operational hardening.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| intent, job FSM, expiry and terminal receipt | OCOMP consensus state |
| trigger/day status | ADR-C-MET-001 |
| finality identity | ADR-B-CNS-001/003 proof contract |
| evidence validity | ADR-S-OCM-003 |
| Lysis result validity | ADR-C-LYS-001 typed verifier |
| domain mutations | certified owner APIs and receipts |
| atomic rollback | one outer execution checkpoint |
| protocol interpretation | job-pinned `ProtocolBundleHash` |

## Invariants

- Request creation has no Lysis/Nod/contributor/Tribute-retirement effect.
- Desis receives exactly `auction_base` before compute and is never topped up.
- Intex has no reservation state; contributors are created only at activation.
- Promis has no reservation state; carry-over uses checked commutative addition.
- Only a finalized live exact attempt can be signed or activated.
- Worker-shard completion is never a consensus terminal state and cannot be
  activated independently.
- Activation at or after the exclusive deadline cannot race expiry.
- Evidence verification does not execute Lysis.
- Only the private typed capability reaches effect methods.
- All owner effects and terminal job state commit together or not at all.
- Exact terminal retry is idempotent; different terminal replay rejects.
- Old state remains authoritative until the certified generation switch commits.
- No synchronous Lysis fallback exists in the PoC profile.
- Local readiness/version state cannot alter consensus validity.

## Atomicity, replay and failure

The request split, early Desis/carry-over effect, intent, expiry index and
`OFFCHAIN_PENDING` share one checkpoint.

Activation verification is side-effect free until its outer apply checkpoint.
A representative owner failure leaves no partial activation or terminal success.

Unexpected live target state produces `CONFLICTED`, not best-effort application.
Invalid evidence rejects with no activation state change.

## Determinism and bounds

Activation cost is bounded by a constant-size typed result commitment, closed
root-transition receipts and the fixed signature threshold; it is independent
of total Tribute count. Per-transaction bytes and crypto caps are checked before
decode/allocation and expensive crypto. Result chunks and work shards have
their own bounded interfaces but no activation authority. Semantic time comes
from the request's frozen logical context; activation height/time affects only
fields explicitly defined as activation metadata.

## Production-interface verification evidence

No job FSM, activation transaction, certified capability or receipt path exists.
The production current path still invokes Lysis synchronously from Metadosis.
Required evidence includes the complete PFS-002 public transaction path,
activation-byte cap-1/cap/cap+1 through proposer/import/replay, a Tribute
population crossing a work-shard boundary, finality mutations, deadline
boundary, exact retry, wrong binding, representative owner rollback, receipt
mutation, delayed activation equivalence and public output/proof reads.

## Consequences

Consensus performs bounded verification and typed application rather than
unbounded recomputation. Domain owners retain their invariants, while relayers
and compute processes remain replaceable and untrusted.

## Rejected alternatives

- **Store intermediate `RUNNING` progress on-chain:** local scheduling would
  become consensus state without adding correctness.
- **Execute Lysis again during activation:** restores the original scale problem.
- **Apply a generic write set:** bypasses domain authority and conservation.
- **Allow activation and expiry in the same height by transaction order:** block
  proposer ordering would decide economic validity.
- **Fallback to synchronous Lysis on timeout:** turns compute outage into
  unpredictable block work and changes protocol semantics.
- **Convert live jobs during an upgrade:** reinterprets signed bytes and breaks
  replay.

## Open questions and technical debt

1. Freeze the exact job/intent/finality/activation/receipt codecs and golden
   vectors before implementation.
2. Resolve every section 22 PoC constant, including deadline, caps, finality
   proof source and receipt schemas.
3. Define precise `CONFLICTED`, `CANCELED`, release and requeue transitions in
   the generated FSM model.
4. Add certified owner APIs and structurally close all raw Lysis/effect bypasses.
5. Prove logical Tribute retirement and active-generation public reads across
   restart/replay.
6. Bind all required runtime/upgrade handlers to the active bundle and fail
   arming when any is absent.
7. Amend PFS-005/PFS-009 before any supported-network MVP claim; they are not PoC
   acceptance prerequisites.
