# ADR-S-OCM-004: OCOMP quorum atomically applies a typed result

- **Status:** Accepted; q-forming atomic apply implemented on
  `feat/ocomp-poc`; final PoC closure evidence pending
- **Date:** 2026-07-26
- **Decision owners:** System Space, block-execution and participating domain owners
- **Scope:** JobIntent lifecycle, finality binding, full-result vote window,
  quorum/expiry, q-forming result verification, private authority,
  atomic receipts, replay and protocol bundle
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

The job lifecycle and four bounded full-result vote transactions therefore form
one typed consensus protocol. Result transport, quorum formation and typed
application are not separate liveness steps: the q-forming transaction already
carries the exact `LysisResultV1` and applies it atomically.

## Decision

### Request and finality

In the terminal Metadosis phase, an eligible non-empty READY day atomically:

1. validates bounded pre-admission and freezes the request-time economic values;
2. splits `day_limit` into `lysis_budget` and `auction_base`;
3. dispatches one GREEN Desis brief, or credits RED `auction_base` to carry-over;
4. stores `JobIntentV1`, its intent-block height, OCOMP
   `AWAITING_FINALITY` and Metadosis day `OFFCHAIN_PENDING`;
5. emits `OffchainJobRequested(IntentId)`; and
6. returns without invoking Lysis or creating Nod/contributor effects.

Desis is a request-phase effect, not a Lysis result or activation owner. The
brief is never topped up and is not repeated by a retry.

The intent stores immutable activation preconditions. It does not copy a
pessimistic reservation record into Desis, Intex or PromisLimit.

The event is a wake-up hint. The intent record is authority. Its request block
must finalize before the job can become signable or accept a vote. The existing
consensus-certified finalization path records the exact finalized block/state
binding in OCOMP state; a Supervisor event, local cursor or vote payload cannot
assert finality.

Consensus opens voting only after both conditions hold:

```text
the exact intent block is finalized
four additional blocks have elapsed after consensus records that finality
```

The transition records exactly once:

```text
open_height = checked_add(finality_recorded_height, 4)
deadline_height = checked_add(open_height, response_window_blocks)
```

At begin-zone `open_height`, OCOMP writes the finalized `JobId`, inserts the
response-deadline index and moves the job to
`VOTING_OPEN(open_height, deadline_height)`. A pre-finality reorg removes the
candidate and all derived local work remains non-signable. Finalized export and
local computation may prepare artifacts during the four blocks after finality,
but the node attestation gate cannot sign and the OCOMP module cannot accept a
vote before `VOTING_OPEN`.

Consensus state contains no `RUNNING`; that is local supervisor progress.
It also contains no state per worker shard. One `JobIntentV1` covers the complete
authenticated WWD input; all shard progress is replaceable local execution
state.

Several Job attempts may be live at once. Consensus and every validator-local
OCOMP module address lifecycle, vote slots, artifacts and deadlines by exact
`JobId`; there is no global “current Job”. A bounded active-job admission limit
may apply to local resources, but reaching it causes local backpressure or
abstention and never overwrites another Job, rejects part of a Tribute
population or changes consensus validity. Retry is a new Job attempt and may
share an Authenticated Input Lease with its retained predecessor under
ADR-S-OCM-002.

```text
READY
  -> AWAITING_FINALITY(IntentId, intent_height)
       -> VOTING_OPEN(JobId, open_height, deadline_height)
            -> COMPLETED(ResultDigest, quorum_height)
            -> EXPIRED
            -> CONFLICTED
            -> CANCELED
  -> READY(next attempt after terminal release, when policy permits)
```

### Exclusive response deadline and ordering

`deadline_height` is the exclusive validator response deadline. A
`ResultVoteV1` is timely only when its canonical
inclusion block satisfies `h < deadline_height`.

At block height `h`, the begin-zone closes vote windows with
`deadline_height <= h` before ordinary transactions:

- a `VOTING_OPEN` job without `q=3` becomes `EXPIRED`;
- a `COMPLETED` or `CONFLICTED` job retains its terminal result and receives its closed
  `OcompAccountabilitySummaryV1`;
- a result-vote transaction included at the deadline is late and cannot alter
  the summary.

When a third matching full-result vote is included before the deadline,
consensus verifies and stores that canonical `LysisResultV1` once, records the
immutable quorum digest/height/evidence and runs the constant-size typed apply
inside the same outer checkpoint. A successful apply moves directly to
`COMPLETED`; an expected target-precondition mismatch records the quorum and
deterministic `CONFLICTED`/retry outcome without owner effects. No separate
activation transaction, activator or relay can delay a valid quorum.

Expiry returns the day to its retry state without synchronous fallback. It
retains the frozen Lysis budget and never repeats the committed auction brief.

A terminal no-retry outcome credits the whole `lysis_budget` to carry-over once.

The block lifecycle reserves stable positions for fork-bound installation and
future pause/revocation. On the first active block, the existing empty-body
`OcompLifecycleBegin` envelope atomically installs the exact request profile,
protocol bundle and complete result committee from the authenticated chain
manifest before running bounded expiry. Later active blocks run only the
ordinary lifecycle/expiry operation. The SystemTx selector, body and ordering
do not change.

### Fork-bound authority installation

The PoC uses one immutable `OcompForkInstallV1` consensus input containing:

- a `Measurement` or `Final` classification;
- `AtBlock(H)`;
- the complete `OcompRequestProfile`;
- the complete `ProtocolBundleV1`;
- the complete `OcompCommitteeSnapshotV1`.

The reproducible generator creates the base genesis first, derives its exact
hash, then creates the bundle, committee registrations/PoPs and canonical
chain-manifest binding. The install artifact is therefore not embedded back
into the genesis state and does not create a genesis-hash cycle.

Every node loads and validates the canonical binding before it starts block
production, import or replay. It must match the selected chain ID, exact base
genesis hash, fork ID, activation height and every profile/bundle/committee
cross-hash. The parsed value is immutable for the process lifetime; local
environment variables, command-line height overrides, runtime file reload and
OCOMP process readiness cannot select consensus semantics.

The PoC closure network uses one checked-in four-validator `Final` fixture.
Its genesis, committee identities, DKG material and validator keys are copied
unchanged into each fresh scenario. The harness may rewrite only loopback
consensus/reth endpoint ports and apply a process-local logical-clock offset;
it must not regenerate identities, mutate the chain manifest or replace the
install. Fixture private keys are test-only data under the E2E crate and are
not embedded in release binaries.

At exactly `H`, all three objects are validated before the first write and are
installed under one outer storage checkpoint. Replaying the exact install is
idempotent; partial state or any different value is fatal. The same typed
install and activation height are supplied to proposer, importer, historical
replay, consensus and txpool paths. Pre-fork blocks contain no OCOMP lifecycle
envelopes.

The existing protocol-version-1 Update handler remains the sole initializer of
Tribute, Fidelity, Oracle and Metadosis pre-admission profiles. A fresh PoC
network schedules that Update for the same height `H`; the fork install does
not duplicate those owner mutations. The executor activates the Update in its
deterministic pre-execution hooks. The receipt-visible begin zone then runs
`OcompLifecycleBegin` (fork install followed by expiry), `CycleTick` and the
remaining begin-zone phases; ordinary transactions and the terminal request
slot follow.

### Full-result vote verification

`submitLysisResult` is a normal signed bounded EVM transaction carried
through RPC, txpool, gossip, proposal, import and replay. The validator does not
pay its native fee: a dedicated exact-selector ZeroFee hook, constrained like
Oracle's hook, authorizes only the eligible validator EVM signer, zero value and
bounded envelope. That hook does not decide protocol validity.

The `OffchainLysis Supervisor` owns transaction construction, submission,
inclusion/finality tracking and reorg rebroadcast. It requests both inner OCOMP
attestation and outer EVM signing through restricted node-owned seams and never
receives private keys. The PoC submission path uses the canonical `latest`
account nonce and the frozen gas limit; it does not use `eth_estimateGas` or a
`pending` block build. The durable single-writer journal reuses the exact
node-signed raw transaction for retry/reorg. Before recording a vote, the OCOMP
executor:

1. loads the exact `VOTING_OPEN`, `COMPLETED` or `CONFLICTED` job and its pinned
   committee;
2. requires consensus-recorded finality, `open_height <= h <
   deadline_height`, and the exact open attempt;
3. bounded-decodes and validates the canonical `LysisResultV1`, reconstructs
   `ResultDigest`, then verifies `JobId`, attempt, bundle, committee hash,
   validator index, key epoch, signature domain and low-`s` OCOMP signature;
4. stores the first vote in that validator's fixed slot with the
   consensus-assigned inclusion height;
5. treats an exact retry as idempotent;
6. records bounded equivocation evidence for a different second signed digest
   without replacing or counting it; and
7. scans the four slots; when one digest first reaches `q=3`, records
   `quorum_digest`, `quorum_height`, signer bitmap and evidence hash, stores the
   q-forming canonical result once and performs the typed apply before the
   outer checkpoint commits.

The vote transaction never executes Lysis or traverses result chunks. The first
two matching submissions change only bounded vote state. The q-forming
submission installs roots and constant-size owner effects. The four slots and
closed summary live in a separate bounded `OcompVoteAccountabilityV1` keyed by
`JobId`; they store digests/signatures/heights, not four copies of the result.
After completion or conflict, the missing fourth first-vote slot and first
bounded conflicting-vote evidence remain writable until the response deadline.

`LysisTerminalV1`, the apply receipt, active-generation hash, applied
domain state and exact retry identity are immutable and exclude the
mutable/closing accountability fields. A fourth vote or deadline close can
change only `OcompVoteAccountabilityV1`.

### Q-forming result verification and apply

There is no public `activateLysis`, `PoCActivationV1`, activator or
post-quorum delivery step. Before recording any slot the executor checks
byte/count/crypto caps. When the current submission forms q=3 it:

1. uses the consensus-owned finalized JobIntent/JobId/attempt/bundle binding;
   no transaction-carried finalized-intent proof is required;
2. requires the
   LYSIS_V1 zero-count/canonical-empty pre-result semantic-event commitment and
   binds every Metadosis completion field to the finalized `JobIntentV1`;
3. invokes the closed Lysis structural/result verifier without executing Lysis;
4. binds the apply receipt to `quorum_height`,
   `quorum_signer_bitmap` and `quorum_evidence_hash`;
5. constructs a private unforgeable `CertifiedLysisActivation`;
6. verifies and installs only the certified old-root-to-new-root generation
   transition, plus constant-size scalar effects, through closed domain-owner
   methods and typed receipts; and
7. commits the q-forming slot, quorum, one canonical result, all domain effects,
   active generation, terminal receipt and `COMPLETED` together.

An exact resubmission after completion returns the recorded identity without
effects. A different result from a validator whose first slot is already set
enters bounded equivocation handling. An expected stale target precondition
commits `CONFLICTED` and the declared retry/carry-over transition with no owner
effects. Any invalid evidence, unexpected verifier error or owner/receipt
failure reverts the complete q-forming transaction.

The applied result covers the complete parent Job Intent. A shard artifact,
prefix of completed shards or per-shard quorum cannot enter application.
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
result submissions.

Neither the node attestation gate nor the q-forming verifier executes
`ROOT_REDUCE`, invokes the Lysis finalizer or traverses result chunks. The
attestation gate reloads finalized intent/export authority and checks the
constant-size result bindings, equations, digest and sign-once subject.
Each vote transaction verifies its OCOMP signature and constant-size result.
The q-forming path reuses the already-decoded current result; it neither
rebuilds a certificate nor performs a second public call. Correctness of bulk
computation is the matching
`q=3/4` independent-domain on-chain evidence defined by ADR-S-OCM-003.

Desis is absent because its exact `auction_base` brief committed before compute.
PromisLimit receives only a checked additive carry-over credit.

Each apply owner returns a constant-size receipt bound to the call and
`JobId`. Old roots/generations, new roots, counts, totals and both budget
equations are checked before terminal commit.

For `APPLIED`, receipt verification recomputes four owner state-event digests
in the fixed order Nod, Contributor, Tribute, CarryOver and stores their
`ApplyEventSummaryHash`. For `CONFLICT_RESOLVED`, no owner effect or owner
receipt exists and the aggregate receipt stores
`H("OUTBE_OCOMP_APPLY_EVENT_SUMMARY_V1", empty)`. Neither value is the signed
pre-result empty `SemanticEventRecords` root; the equal wire field name does
not imply equal semantics.

Any owner error or receipt mismatch reverts the entire q-forming transition.

Result application never accepts storage addresses, keys, opcodes, generic calls or a
list of user-selected writes.

### Protocol identity and evolution

Every consensus or durable OCOMP object pins one `ProtocolBundleHash`. For the
PoC/BoundedMVP Lysis V1 path, the bundle already fixes intent/action/result
codecs, the Tribute body codec, the distinct Fidelity/Oracle opening codecs,
Lysis semantics, planner/reducer, evidence/signature domain, capacity profile,
apply semantics, effect/codec registries, historical decoders and required
handlers. The opening-registry hash is derived from the two bundle fields rather
than being a second configurable authority. A one-entry `ProgramRegistry` would
duplicate this authority and is not created.

Compatibility negotiation can make a local validator abstain; it never selects
job semantics. Live jobs finish or expire under their pinned bundle. New
semantics use a new bundle/fork and never reinterpret historical bytes.

PoC-to-BoundedMVP evolution preserves the job/result/apply meaning while replacing
demo key storage, scheduler, isolation, retention and operations. A changed input,
planner, result or apply contract is a new protocol, not operational hardening.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| intent, finality/open marker, job FSM and expiry | OCOMP consensus state |
| immutable terminal result and receipt | `LysisTerminalV1` in OCOMP consensus state |
| result-vote slots and closed summary | separate bounded `OcompVoteAccountabilityV1` |
| vote submission/rebroadcast | validator-domain `OffchainLysis Supervisor` |
| result-vote fee waiver | exact-selector validator-only ZeroFee hook |
| trigger/day status | ADR-C-MET-001 |
| finality identity | ADR-B-CNS-001/003 proof contract |
| evidence validity | ADR-S-OCM-003 |
| Lysis result validity | ADR-C-LYS-001 typed verifier |
| domain mutations | certified owner APIs and receipts |
| atomic rollback | one outer execution checkpoint |
| protocol interpretation | job-pinned `ProtocolBundleHash` |
| validator-local Job/source retention | ADR-S-OCM-002 Job Registry and Authenticated Input Lease Registry |

## Invariants

- Request creation has no Lysis/Nod/contributor/Tribute-retirement effect.
- Desis receives exactly `auction_base` before compute and is never topped up.
- Intex has no reservation state; contributors are created only at quorum apply.
- Promis has no reservation state; carry-over uses checked commutative addition.
- Only a finalized live exact attempt can be signed and submitted.
- Lifecycle, voting and application are keyed by exact `JobId`; one Job's
  retention or failure never blocks an unrelated Job.
- Voting cannot open before `finality_recorded_height + 4`.
- Only a timely eligible signed transaction can fill a result-vote slot.
- Three matching first-vote slots atomically establish quorum and produce
  `COMPLETED` or the defined `CONFLICTED` outcome.
- The fourth validator can still vote until the response deadline after quorum
  or completion.
- Fourth-vote/accountability writes never change immutable terminal, apply
  receipt, active generation or exact-retry identity.
- Missing-response and equivocation evidence are consensus-visible; PoC applies
  no monetary slashing policy.
- Worker-shard completion is never a consensus terminal state and cannot be
  applied independently.
- A late vote cannot race response-window closure.
- Evidence verification does not execute Lysis.
- Only the private typed capability reaches effect methods.
- All owner effects and terminal job state commit together or not at all.
- Exact terminal retry is idempotent; different terminal replay rejects.
- Old state remains authoritative until the certified generation switch commits.
- No synchronous Lysis fallback exists in the PoC profile.
- Local readiness/version state cannot alter consensus validity.

## Atomicity, replay and failure

The request split, early Desis/carry-over effect, intent and
`AWAITING_FINALITY` share one checkpoint. Four blocks after
consensus-certified finality, the minimum gate installs `JobId`, `open_height`,
`deadline_height`, its due index and `VOTING_OPEN` in one checkpoint.

Each accepted vote, optional equivocation record and resulting quorum transition
share one checkpoint. Vote failure has no partial slot/tally state.

The q-forming transition performs verification before owner writes and shares
one outer checkpoint with its slot, quorum, apply and terminal record. A
representative owner failure leaves no partial vote, quorum, owner effect or
terminal success.

Unexpected live target state produces `CONFLICTED`, not best-effort application.
Invalid evidence rejects with no vote or apply state change.

## Determinism and bounds

Vote-state cost is bounded by exactly four slots, one signature verification per
new vote and a four-slot tally scan. Q-forming apply cost is bounded by a
constant-size typed result commitment and closed root-transition receipts; it
is independent of total Tribute count and does not repeat vote cryptography.
Per-transaction bytes and crypto caps are checked before decode/allocation and
expensive crypto. Result chunks and work shards have their own bounded
interfaces but no apply authority. Semantic time comes from the request's
frozen logical context; vote inclusion/quorum height and apply height/time
affect only fields explicitly defined as protocol metadata.

## Production-interface verification evidence

No complete full-result job-vote FSM, certified capability or
receipt path exists in the baseline inspected by this ADR.
The production current path still invokes Lysis synchronously from Metadosis.
Required evidence includes the complete PFS-002 public transaction path,
result-vote cap-1/cap/cap+1 and q-forming maximum-work cases through
proposer/import/replay, a Tribute population crossing a work-shard boundary,
healthy `4/4`, one-unavailable `3/4`, two-unavailable expiry, duplicate/late/
wrong-key/equivocation vote cases, finality mutations, response-deadline
boundary, exact retry, wrong binding, representative q-forming owner rollback,
receipt mutation, fourth-vote-after-completion and public output/proof reads.

## Consequences

Consensus performs bounded vote accounting, verification and typed application
rather than unbounded recomputation. Domain owners retain their invariants.
Compute processes remain replaceable and untrusted, and no relay becomes a
second consensus or an obstacle to objective participation evidence.

## Rejected alternatives

- **Store intermediate `RUNNING` progress on-chain:** local scheduling would
  become consensus state without adding correctness.
- **Collect result votes in an off-chain certificate:** hides timely
  participation from consensus and makes missed-response slashing
  non-attributable.
- **Use a digest-only vote plus later public activation:** duplicates result
  delivery and introduces a second liveness step after consensus has already
  established q=3.
- **Execute Lysis again during q-forming apply:** restores the original scale problem.
- **Apply a generic write set:** bypasses domain authority and conservation.
- **Allow a late q-forming submission and expiry in the same height:** block
  proposer ordering would decide economic validity.
- **Fallback to synchronous Lysis on timeout:** turns compute outage into
  unpredictable block work and changes protocol semantics.
- **Convert live jobs during an upgrade:** reinterprets signed bytes and breaks
  replay.

## Open questions and technical debt

1. Freeze the exact job/intent/finality/full-result-vote/quorum/accountability/
   receipt codecs and golden vectors before implementation.
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
8. Define monetary slashing and appeal policy separately; this ADR freezes only
   objective OCOMP evidence.
9. Define BoundedMVP retention/GC for terminal result and accountability
   records.
