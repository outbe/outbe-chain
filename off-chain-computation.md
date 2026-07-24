# Off-chain computation for Outbe — system design v0.6

Date: 2026-07-23
Status: audited architecture proposal. Production code is unchanged. The
selected PoC is a small protocol-forked devnet profile, not an on-chain Lysis
comparison. PoC, transition and MVP are separate release gates; none is marked
implemented by this document.

The pre-remediation evidence and blockers are recorded in
[the Citadel audit](outbe-plan/off-chain-computation-citadel-audit.md).
The independent review of the reusable-kernel boundary is recorded in
[the OCOMP framework review](outbe-plan/ocomp-framework-review.md).
The proposed normative owners are
[ADR-S-OCM-001](docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
through
[ADR-S-OCM-004](docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md);
the canonical test flow is
[PFS-002](docs/flows/002-off-chain-poc-protocol-flow.md).

The computation process is not part of the node process, and the job input is not
an intermediate state inside a block. Metadosis commits an `OFFCHAIN_PENDING`
intent in a terminal system phase with no later semantic writers. The finalized
end-of-block state is then the immutable input snapshot.

## 0. Architecture boundary: OCOMP kernel and typed programs

OCOMP is not “Lysis moved to another process” and is not a runtime for arbitrary
uploaded computation. It is a reusable **operational kernel** that carries
closed, fork-pinned, typed domain protocols:

```text
OCOMP operational kernel
  finality + lifecycle + process isolation + artifacts + evidence + atomic dispatch
        |
        +-- Lysis V1 protocol          <- the only PoC/BoundedMVP program
        |
        +-- future typed program       <- only after its own domain design and fork
```

The boundary is ownership, not type erasure:

| OCOMP kernel owns | Lysis V1 owns |
|---|---|
| finalized discovery, `JobId`, pending/expiry/terminal lifecycle | WWD/Metadosis split, preconditions and request meaning |
| checkpoint lease and untrusted artifact transport | authenticated Tribute/Fidelity/Oracle input contract |
| worker isolation, leases, retries and content addressing | planner, Lysis phases, units and fixed reduction |
| committee snapshot, sign-once and certificate mechanics | result schema, ordering, totals and conservation |
| common admission, replay/drain shell and outer checkpoint | `CertifiedLysisActivation`, effect calls, receipts and cleanup |

The PoC must implement that module boundary internally, but its consensus wire is
deliberately concrete. `JobIntentV1`, `UnitSpecV1`, `ActivationPayloadV1`,
`LysisResultV1`, `ProtocolBundleV1` and `activateLysis` are Lysis V1
objects even where their historical names omit “Lysis”. The pinned
`ProtocolBundleHash` is the exact identity of this sole program. There is no
PoC consensus `ProgramRegistry`, generic program envelope, public
`TaskAdapter`, `execute(program_id, bytes)` entrypoint or generic write
capability.

This is an evolutionary seam without a premature framework claim:

| Fixed now | Implemented by the PoC | Deferred until a qualified second program |
|---|---|---|
| kernel/domain ownership and no-arbitrary-code rule | internal lifecycle/evidence kernel wired directly to Lysis V1 | consensus `ProgramSpec`/registry |
| future programs use new typed object kinds and signature domains | Lysis-specific V1 bytes and `CertifiedLysisActivation` | generic intent/unit/result envelopes or dispatcher |
| old bytes are never reinterpreted | unchanged thirteen-step Lysis acceptance | cross-program scheduling, preconditions and capacity |

## 1. The whole system in one story

```text
Metadosis reaches a sealed READY day
        |
        | 1. short deterministic on-chain transition
        v
day_limit is split; GREEN auction_base goes to Desis, RED to carry-over
JobIntent(lysis_budget) + OFFCHAIN_PENDING + event are committed in block H
        |
        | 2. H becomes final
        v
outbe-ocomp-supervisor discovers the finalized job through the node control API
        |
        | 3. fetches untrusted bytes, verifies them against finalized roots
        | 4. derives deterministic work units
        v
sandboxed workers execute/prove units in parallel
        |
        | 5. fixed reducers build one result digest
        v
Bounded: q validators independently execute and attest
Large:   validators verify one recursive proof + data-availability certificate
        |
        | 6. one constant-size activation transaction
        v
result root/generation becomes active; old state is retained for recovery
```

The actors are concrete:

- `outbe-chain` owns consensus, finality, the on-chain job state machine and a
  tiny fixed-size attestation gate. It never scans a billion records and never
  starts workers.
- `outbe-ocomp-supervisor` is a separate OS process or container. It discovers
  finalized jobs, reserves resources, verifies source artifacts, derives the
  plan, schedules workers and journals progress. It has no validator keys and no
  write access to the node databases.
- `outbe-ocomp-snapshot-exporter` is a separate read-only process. The node gives
  it an opaque handle to one storage-engine-created immutable checkpoint, never
  the live database. It streams canonical Fidelity/Oracle/CE openings into the
  source CAS under I/O and disk quotas and proves the rebuilt roots before the
  checkpoint pin may be released.
- `outbe-ocomp-worker` is an ephemeral sandboxed process/container. It executes
  one immutable `UnitId`, writes a content-addressed result and can be killed or
  retried without changing semantics.
- in MVP/production, the launch broker is a tiny service-manager-owned policy
  adapter. It is the only process allowed to create/cancel worker units and
  enforces the aggregate slice/namespace quota plus admitted-plan membership.
  The PoC instead uses one fixed unprivileged local worker template.
- source and artifact stores transport bytes. They are untrusted: roots and
  proofs, not storage location, establish authority.
- any relayer may collect result signatures or a proof plus custody receipts and
  submit the bounded activation transaction. The relayer is not trusted and has no
  exclusive role; every validator rechecks the evidence against the job record.
- the service manager starts and restarts the node and supervisor independently.
  The node does not spawn the supervisor.

If the supervisor crashes, the job pauses and the node continues consensus. If
all supervisors are unavailable, the job expires or remains pending; the base
chain and the previously active domain state continue to work.

### 1.1 Selected PoC: the whole real chain at a small bound

The selected PoC is **not** Shadow and does not run Lysis on-chain for comparison.
It is the first complete protocol vertical slice:

```text
Tribute issued and day sealed
  -> Metadosis splits day_limit and applies GREEN Desis/RED carry-over once
  -> terminal Metadosis creates JobIntent(lysis_budget); no Lysis or Nod yet
  -> request block finalizes
  -> four validator domains independently read and execute the same job off-chain
  -> any relayer collects q=3 matching result signatures
  -> one activation transaction carries certificate + typed result commitment
  -> that transaction installs output roots and applies
     Tribute/unused-carry-over scalars atomically
  -> Metadosis becomes COMPLETED and the Tribute partition is logically retired
```

There is no synchronous fallback. If fewer than `q` validators produce the same
result before the deadline, the attempt expires and the day returns to READY
with the same Lysis budget and no repeated auction. The node never calls Lysis
to rescue the job.

This PoC proves the architecture, not scale:

- `JobIntent`, finality binding, discovery and expiry are real consensus state;
- supervisors, snapshot exporters and workers are real separate processes;
- every signing validator independently reads and executes the whole job;
- result signatures use separate OCOMP keys and the pinned result committee;
- the relayer is untrusted;
- evidence verification and activation happen through normal block execution;
- Nod, contributors, Desis, Promis, Metadosis and Tribute retirement are real;
- volumes, concurrency, recovery and adversarial coverage are deliberately small.

The core seam is:

```text
pure off-chain:
  execute_lysis(JobSpec, authenticated InputBundle)
      -> ResultChunkV1 catalog + LysisResultV1

on-chain:
  apply_certified_lysis(JobIntent, LysisResultV1,
                        ExecutionCertificateV1)
      -> atomic domain root/state transition
```

`apply_certified_lysis` is a deep protocol module, not a generic write interpreter.
Its interface accepts one typed Lysis result and hides the exact fixed state
writes. It cannot authorize arbitrary storage keys or uploaded code.

In implementation this concrete path is split into `OcompLifecycle`,
`LysisProgramV1` and `CertifiedLysisApply`. That internal kernel seam is a PoC
requirement; a heterogeneous program registry is not. One Lysis-shaped example
is insufficient evidence for a program-neutral wire protocol.

### 1.2 What Metadosis does in the PoC

For a non-empty eligible WWD, terminal Metadosis does only:

1. read bounded sealed day metadata and calculate the frozen Metadosis scalars;
2. reserve the WWD, Nod namespace, contributor series, Desis brief and Promis
   consequence;
3. store `JobIntentV1`, its expiry entry and `OFFCHAIN_PENDING`;
4. emit `OffchainJobRequested(IntentId)`;
5. return without calling Lysis, issuing Nod, recording contributors, consuming
   Tribute, dispatching Desis or changing Promis.

The request block can be inspected immediately: the intent exists and no output
Nod exists. This negative observation is part of the PoC acceptance test.

```text
READY
  -> OFFCHAIN_PENDING(IntentId)
  -> COMPLETED

OFFCHAIN_PENDING
  -> EXPIRED
  -> CONFLICTED
  -> CANCELED
  -> READY(next_pending_nonce)
```

`RUNNING` is never consensus state; it is only local supervisor progress.

After finality, each validator's supervisor discovers the job through the
finalized cursor. The event only reduces latency. Its snapshot exporter opens the
exact immutable checkpoint named by the finalized job and full-folds the small
Tribute collection. It verifies every body and Fidelity/Oracle opening against
the finalized roots before execution.

### 1.3 Independent execution and q

The first devnet is fixed to:

```text
n = 4 result validators
f = 1 tolerated faulty/offline validator
q = 3 matching signatures
```

Each of the four validator domains has its own node, supervisor, exporter,
artifact directory and workers. Four workers under one supervisor improve speed
but still count as **one** validator execution. Independence comes from the four
validator domains, not from worker count.

Each domain executes the complete Lysis program and produces canonical:

```text
ResultChunkV1 {
  chunk_ordinal, first_nod_ordinal,
  bounded ordered_nod_actions,
  bounded ordered_eligible_contributors
}

LysisResultV1 {
  protocol_bundle_hash, JobId, attempt,
  result_chunk_count, result_chunk_list_root,
  tribute_count, tribute_nominal_total,
  unused_lysis,
  exact roots, conservation totals, arithmetic commitment and event summary
}

ActivationPayloadV1 contains the result-chunk count/root and the recomputed job,
profile, output-root, count, conservation and event-summary fields. The single
normative `ResultChunkHash` and `ResultDigest` definitions are in section 8.2;
this summary introduces no alternate preimage.
```

Nod actions are ordered by the canonical Tribute order. Contributors are ordered
by owner and exclude exactly the bodies with
`exclude_from_intex_issuance == true`. The result is typed: the stream cannot
contain calls, storage addresses or arbitrary opcodes.

After the separate compute plane checks complete chunk coverage, the supervisor
submits the constant-size candidate commitment to its local node. The node
checks the finalized job, profile, candidate binding/summary and local
sign-once record, then signs `ResultDigest` with a separate OCOMP key. It does
not read bulk result chunks.
The supervisor never receives that key. The PoC pins canonical low-`s`
`secp256k1` signatures over the domain-separated digest; its static committee
snapshot maps each validator index to a separate OCOMP public key. The
certificate contains an ordered signer bitmap and exactly three
`(validator_index, signature)` entries. It never reuses a consensus private key.
Aggregate signatures are an optimization, not core mechanics.

The local node returns the attestation to its supervisor. For the PoC, the
supervisor posts
`CandidateAnnouncement(JobId, LysisResultV1, validator_index, signature)`
to a trivial HTTP relay adapter. The relay groups announcements by the signed
digest, accepts at most one entry per validator index, constructs exactly one
`PoCActivationV1` after reaching three and submits
`activateLysis(PoCActivationV1)`. It owns no key and makes no decision:
dropping, reordering, changing or mixing announcements can delay the job but
cannot create a valid certificate. A validator or another client may perform the
same activation submission if the demo relay is absent. There is no separate
result-evidence transaction or second consensus wire type.

For `n=4, q=3`, two quorum certificates intersect in at least two validators.
With at most one faulty domain and honest sign-once behavior, two conflicting
certificates cannot both form. This does not protect against a common software
bug shared by all four implementations; differential fixtures remain required.

### 1.4 What on-chain activation verifies and applies

Any relayer may submit one constant-size activation transaction:

```text
PoCActivationV1 {
  IntentId,
  FinalizedIntentProofV1,
  ActivationPayloadV1,
  LysisResultV1,
  ExecutionCertificateV1
}
```

Every node:

1. derives the same `JobId` from the finalization proof;
2. loads the exact historical result-committee snapshot;
3. requires three distinct valid signatures over one `ResultDigest`;
4. checks the result-chunk count/root bound into `LysisResultV1`;
5. checks activation byte/crypto caps, canonical encoding, committed counts,
   live old-root/generation preconditions and arithmetic relationships between
   the summary fields;
6. constructs a private `CertifiedLysisActivation` capability and enters the
   apply phase in the same transaction.

It explicitly does **not**:

- enumerate Tribute or Fidelity;
- call `fidelity::league`;
- read Oracle values;
- calculate FI fractions, prices, Gratis loads or Nod bodies;
- iterate over `N` Nod/contributor actions;
- rerun Lysis.

The same transaction checkpoint then applies the typed certified result:

```text
install the certified Nod/bucket/contributor/output roots and exact counts
logically retire the exact sealed Tribute generation
credit exact unused_lysis to carry-over
mark Metadosis COMPLETED
emit the canonical aggregate/domain events
remove expiry
```

Hashing, decoding, signature verification, constant-size structural validation
and installing certified roots are activation work; they are not Lysis
computation.
Any mismatch or write failure reverts the entire activation. Before commit there
is still no Nod; after commit all certified effects exist together.

The action bytes do not occur in the activation transaction body. No full
result is parked in EVM slots. The terminal record stores active roots, counts,
the certificate hash and the activation receipt. The three signing domains have
validated and retain the result chunks for the PoC projection/proof-serving
path, but this is not an independent production DA/custody claim. Erasure
coding, custody handover and recursive proof remain later-profile concerns.

`CertifiedLysisActivation` has no public constructor. Only the OCOMP verifier
inside the production executor can create it after all checks above. The
activation module owns the checkpoint and calls closed root-transition APIs:

```text
NodFactory::install_certified_generation(capability, roots, counts, totals)
  -> NodBatchReceipt
Intex::install_certified_contributor_root(capability, root, count, total)
  -> ContributorReceipt
Tribute::retire_certified_partition(capability, root, count, totals)
  -> TributeReceipt
PromisLimit::credit_certified_carry_over(capability, unused_lysis)
  -> CarryOverReceipt
```

The capability is move-only, non-cloneable, non-serializable and scoped to one
outer executor checkpoint. Its private binding is:

```text
EffectBindingV1 {
  IntentId, JobId, attempt, protocol_bundle_hash, ResultDigest,
  activation_preconditions_hash, activation_call_id
}

ActivationCallCoreV1 {
  IntentId, JobId, attempt, protocol_bundle_hash,
  ResultDigest, activation_preconditions_hash, terminal_pending_nonce
}

ActivationCallId(core) =
  H("OUTBE_OCOMP_ACTIVATION_CALL_V1", canonical(core))

EffectBindingV1.activation_call_id =
  ActivationCallId(the exact ActivationCallCoreV1 above)
```

Every effect owner has the only constructor for its own move-only receipt.
The bundle registry fixes every hash and integer width/endianness in
`ActivationCallCoreV1`; golden vectors cover zero/max attempt and pending nonce.
Receipts cannot be accepted in another transaction, checkpoint, job or retry:

```text
NodBatchReceipt {
  binding, nod_target_precondition, nod_count, nod_root,
  nod_amount_total, nod_gratis_consumed, issued_at, state_event_digest
}
ContributorReceipt {
  binding, contributor_target_precondition,
  contributor_count, contributor_root, eligible_nominal_total,
  state_event_digest
}
TributeReceipt {
  binding, tribute_input_binding, sealed_collection_root,
  consumed_count, consumed_nominal_total, retired_generation,
  state_event_digest
}
CarryOverReceipt {
  binding, accumulator_key,
  before_value, credited_unused_lysis, after_value, state_event_digest
}
```

The request stores `RequestBudgetSplitReceiptV1`. GREEN dispatches exactly
`auction_base` to Desis; RED credits it to carry-over. Activation never calls
Desis or tops up the live auction.

`PromisLimit.total_unallocated` is the carry-over accumulator. Day-limit
formation atomically takes its current value into the next not-yet-formed day.
A later credit waits for the following unformed day.

Retry preserves `lysis_budget` and the request split. A terminal no-retry
outcome credits the whole `lysis_budget` exactly once.

Before commit the activation module consumes four activation receipts and the
request split receipt. It checks:

```text
all receipt.binding == capability.binding
NodBatchReceipt.nod_count == TributeReceipt.consumed_count
NodBatchReceipt.nod_count == ActivationPayload.exact_nod_count
NodBatchReceipt.nod_root == ActivationPayload.nod_root
NodBatchReceipt.nod_gratis_consumed + unused_lysis
  == frozen_lysis_budget                              [bundle arithmetic]
frozen_day_limit == frozen_lysis_budget + frozen_auction_base
TributeReceipt.(root,count,nominal)
  == JobIntent.(sealed_tribute_collection_root,
                authenticated_count, authenticated_nominal)
ContributorReceipt.(root,count,eligible_nominal_total)
  == the decoded eligible action stream and ActivationPayload
RequestBudgetSplitReceipt
  == exact GREEN Desis auction_base or RED carry-over auction_base
CarryOverReceipt.credited_unused_lysis == signed unused_lysis
CarryOverReceipt.after_value
  == checked_bundle_add(CarryOverReceipt.before_value, unused_lysis)
every precondition identity/version and state_event_digest
  == the decoded action, pre-state and post-state owned by that module
```

Only then may it write Metadosis `COMPLETED` plus `LysisActivated`. A `bool`,
best-effort result, stale receipt or raw `issue_nod` call cannot satisfy this
path. Any missing/mismatched receipt rolls back the whole outer checkpoint. The
old public/internal seams remain legacy-only until the fork and must be
unreachable from the active certified path.

Time is also fixed explicitly:

| Field/effect | Authoritative time |
|---|---|
| Fidelity/Oracle snapshot, Metadosis scalars, Nod `issued_at`, Desis brief anchor and pre-fork-compatible semantic event fields | frozen request `logical_evaluation_height/time` |
| certificate formation | no wall-clock field; it signs only the canonical payload |
| activation receipt/log location and `activated_at_height/time` | actual activation block |

The certified Nod batch receives `issued_at`; it may not call
`storage.timestamp()` for that field. Desis receives the frozen brief anchor,
not `ctx.block.timestamp`. Delaying a valid activation changes only the explicit
activation metadata, never Nod economics, Desis scheduling, Promis amounts or
the signed semantic result.

### 1.5 Initial consensus-frozen PoC envelope

These are proposed initial devnet constants. Per-interface values are lowered
if their worst encoded object cannot fit its resource envelope, then frozen in
the PoC fork:

| Limit | PoC value | Why |
|---|---:|---|
| result committee | `n=4, q=3` | demonstrate one faulty/offline domain |
| `MAX_TRIBUTES_PER_WORK_SHARD` | `256` | bounded worker job; shard-cap+1 starts another job |
| workers per validator | `1..4` | prove worker-count independence |
| `MAX_RECORDS_PER_INPUT_CHUNK` | `768` | bounded input decode/allocation |
| `MAX_RESULT_CHUNK_BYTES` | candidate ceiling `<=512 KiB`; fork value generated before activation | bounded result artifact decode/allocation |
| `MAX_RESULT_SUMMARY_BYTES` | candidate ceiling `<=1 MiB`; fork value generated before activation | constant-size result/activation envelope |
| `MAX_PENDING_JOBS` | `1` | no production queue/recovery claim |
| intents/activations per block | `1 / 1` | statically bounded executor work |
| distinct reference currencies | `8` | exercise Oracle branching with a cap |
| Fidelity cohorts per owner | `64` | bounded controlled-devnet input shape |
| result deadline | `64 blocks` | exercise success and deterministic expiry |

The table is not a total-job capacity proof. Before the fork can activate, one
generator must construct maximum-shaped input/result chunks, constant-size
`LysisResultV1`, certificate, finality proof, transaction, receipts/logs and
every mandatory block artifact. Each selected interface constant must fit its
decode, memory, transaction, full RLP block, gas/internal-work, root-transition
and finality budgets with measured headroom on the declared minimum devnet
machine. The generator runs at
`cap-1/cap/cap+1` from public JSON-RPC through transaction decoding, txpool
admission/replacement, four-node transaction gossip, proposer selection, block
gossip, validation, import and replay. It may not inject the activation directly
into the executor. `ReleaseManifest` and `NetworkManifest` pin compatible RPC,
transaction-input, txpool, P2P, block-body, gas and internal-work limits; the
smallest layer is the protocol cap. One `JobIntent` covers all `T` Tribute and
the constant-size `PlanCommitmentV1` commits
`ceil(T / MAX_TRIBUTES_PER_WORK_SHARD)` ordered worker shards. Filling a shard
never rejects the next Tribute. Total `T` has no PoC/protocol ceiling. Local
configuration may run fewer workers, but it may not raise per-interface caps or
reinterpret them as total-population admission. Resource exhaustion causes
local abstention/expiry; it never falls back to on-chain Lysis.

### 1.6 Demonstration that defines PoC success

The acceptance demonstration starts four validator deployments and an untrusted
relayer, then:

1. issues a bounded WWD containing different Fidelity leagues, currencies and at
   least one `exclude_from_intex_issuance` Tribute;
2. seals the WWD and reaches terminal Metadosis;
3. shows the finalized split and `JobIntent`, proves the request-phase effect
   happened once, and proves there are still zero new Nod/contributor/
   Tribute-consume effects;
4. stops one validator's supervisor;
5. shows the remaining three domains independently rebuilding the same input
   root and producing the same `ResultDigest`;
6. submits their certificate and exact typed result in one activation
   transaction through the untrusted relayer;
7. finalizes that transaction and queries every expected Nod, contributor total,
   Metadosis state, request-phase Desis brief, carry-over and retired Tribute
   partition;
8. compares the result to an offline reference/golden corpus, never to an
   on-chain Lysis execution;
9. repeats with `1`, `2` and `4` workers and randomized completion order;
10. asks one validator to sign a second digest for the same job and observes a
    sign-once refusal; then mutates one result byte, signer, JobId and ordering
    field and observes consensus rejection;
11. delays otherwise identical activations by different block counts and proves
    byte-identical Nod/contributor/Tribute/carry-over results with no repeated
    request effect;
12. runs a second job with two unavailable validators and observes expiry,
    preserved budget, no repeated auction and no Nod;
13. records an execution trace showing no on-chain call to the Lysis, Fidelity or
    Oracle calculation paths.

PoC is complete only when this story passes from public Tribute issuance through
public Nod reads on a multi-validator devnet. Calling the executor directly in a
test, injecting a result into storage or using one central calculator does not
count.

### 1.7 PoC versus MVP

The PoC keeps every core consensus seam but uses deliberately weak operational
implementations:

| Concern | PoC | MVP |
|---|---|---|
| purpose | prove the complete protocol chain | operate the bounded profile reliably |
| network | disposable forked devnet | supported network rollout |
| computation | q full off-chain Lysis executions | same core mechanism with measured caps |
| committee | static four-validator snapshot | epoch changes, historical snapshots and handover |
| result availability | capped bytes in block | capped bytes plus production retention/bootstrap policy |
| keys | separate local OCOMP keys + simple durable sign-once store | HSM/remote signer, audited anti-equivocation recovery |
| snapshots | one pinned local checkpoint; restart may recompute | crash-safe pin/export FSM, pruning reconciliation and restore |
| scheduler | one parent job, deterministic shard queue and shard retry | bounded queues, fair scheduling, resumable journals |
| worker launch | unprivileged fixed local worker template | audited launch broker and aggregate lease accounting |
| isolation | real separate processes and basic cgroups | hardened identities, broker, aggregate quotas and policy audit |
| failure testing | one worker/domain loss, tampering and expiry | exhaustive crash boundaries, disk/OOM/IPC/upgrade/Byzantine chaos |
| observability | structured job log and demo metrics | SLOs, alerts, dashboards and runbooks |
| security claim | protocol mechanics under `f=1` devnet assumption | reviewed threat model, key lifecycle, audit and incident recovery |
| scale claim | none beyond the generated multi-shard PoC envelope | only the benchmarked bounded cap |

Minimum cryptographic authenticity, sign-once behavior, finality binding, atomic
activation and process separation are not postponed: without them the PoC would
not exercise the real system. MVP adds hardening and operational confidence
around those same interfaces rather than replacing a fake PoC architecture.

### 1.8 PoC implementation slices

The PoC should be built as six independently testable vertical slices:

1. **Pure semantics:** extract current Lysis calculation from storage mutation as
   `execute_lysis`; prove its typed result matches existing golden fixtures.
2. **Real request:** fork terminal Metadosis to create/expire `JobIntent` and
   remove the synchronous Lysis call for the PoC profile.
3. **One validator domain:** exporter, supervisor and workers discover a finalized
   job, rebuild the input and produce a candidate without any direct state write.
4. **Certificate:** four domains use separate OCOMP keys; an untrusted relayer can
   form evidence only from three identical digests.
5. **Real activation:** `apply_certified_lysis` verifies the constant-size typed
   result commitment and atomically installs all current observable domain
   roots/effects without iterating the result population.
6. **System demonstration:** execute section 1.6, including one offline domain,
   tampered evidence, worker-count determinism and expiry without fallback.

Each slice is tested at the same external seam used by the next slice. In-memory
checkpoint/control adapters are allowed in module tests; the final demonstration
must use the real UDS, separate processes, consensus blocks and public domain
read interfaces.

## 2. What exactly happens when Metadosis fires

Today `process_metadosis` calls Lysis synchronously, then dispatches Desis,
updates Promis, marks the day completed and retires Tribute in the same execution
path ([current code](crates/core/metadosis/src/runtime.rs#L366)). That path must
remain authoritative until a fork enables this design.

After the fork, the non-empty successful branch is split into two on-chain
transactions separated by an off-chain job.

### 2.1 Request block

The fork moves job creation to a **terminal deterministic system phase**: after
ordinary transactions and CE sealing, before the block is committed. No semantic
write is allowed after that phase. This ordering is consensus-visible and covered
by executor tests. Calling it from today's begin-zone would be incorrect because
later transactions in the same block could change Fidelity or Oracle after the
supposed snapshot.

In that terminal phase Metadosis performs only bounded work:

1. validate the day, limit, sealed `DayTotals` and `PreAdmissionEnvelopeV1`;
2. validate worst-case protocol capacity before any retention pin;
3. derive and persist
   `day_limit = lysis_budget + auction_base`;
4. for GREEN, strictly dispatch `auction_base` to Desis; for RED, credit it to
   carry-over;
5. freeze the Tribute binding and Nod/contributor/Metadosis activation
   preconditions;
6. store `JobIntentV1`, insert `(deadline_height, IntentId)` into the expiry
   index and change the day to `OFFCHAIN_PENDING`;
7. emit `OffchainJobRequested(IntentId)`;
8. finish the block without any later semantic write.

The fork fixes `MAX_OCOMP_INTENTS_PER_BLOCK` and a canonical READY-day priority
order. Extra eligible days remain READY for a later block; neither a proposer nor
the local supervisor chooses which one jumps the queue.

READY work is stored in a consensus due-index ordered by
`(next_check_height, WWD, pending_nonce)`. The terminal phase inspects at most
`MAX_OCOMP_READY_INSPECTIONS_PER_BLOCK` due entries and creates at most
`MAX_OCOMP_INTENTS_PER_BLOCK` intents. A deferred/ineligible entry is reinserted
with a fork-fixed capped backoff and reason; it cannot remain at the head and
starve later days. Queue pop, defer or intent creation is one metered checkpoint
with a receipt. No implementation may discover READY days by scanning all days.

`PreAdmissionEnvelopeV1` is authenticated state maintained before Metadosis,
not a page count supplied by a supervisor. It contains the sealed Tribute count
and canonical-body bytes, applicable protocol profile, and conservative bounds
for input openings, output records/bytes, proof work, DA and retention. The fork
also introduces an enforced per-owner Fidelity cohort ceiling `H_max`; existing
state must be exhaustively migrated/validated and an authenticated global
`fidelity_profile_ready` flag set before either off-chain profile is enabled.
After that gate every Fidelity mutation enforces the ceiling. The bounded upper
limit is based on `T * H_max`, not on discovering the actual histories by
scanning `T` owners in the terminal phase. Oracle reads use a fork-fixed maximum
per Tribute/currency set. `Unknown`, arithmetic overflow in the capacity
formula, a false profile-ready flag or an exhausted global capacity ledger
leaves the day `READY` and creates no intent.

The fork fixes `MAX_PENDING_JOBS`, maximum retention blocks and a deterministic
resource-unit formula. Validators provision the corresponding minimum snapshot
capacity before joining the profile. Local free disk never changes consensus
eligibility: if the protocol reservation fits but a validator has violated its
declared capacity, that validator disables OCOMP signing rather than changing
the job result.

It does not call Lysis, issue Nod, write contributors or consume Tribute in this
block. The exact request-phase Desis/RED carry-over effect is part of the same
atomic checkpoint and is never repeated by a retry.

```text
JobIntentV1 {
  chain_id, genesis_hash, fork_id,
  wwd, pending_nonce, attempt,
  protocol_bundle_hash,
  ce_sealed_root,
  sealed_tribute_collection_key,
  sealed_tribute_collection_root,
  authenticated_day_count_and_nominal,
  pre_admission_envelope_hash,
  source_availability_ref,
  frozen_metadosis_values,
  logical_evaluation_height, logical_evaluation_time,
  activation_preconditions,
  result_committee_snapshot_hash,
  custody_committee_epoch_hash,
  deadline_height
}

IntentId = H("OUTBE_OCOMP_INTENT_V1", canonical(JobIntentV1))
```

The block's state root commits both the intent and the current CE sealed root,
and every input read precedes the terminal intent write with no later writer.
There is no self-reference: the intent does not contain its own end-of-block
state root.

The split, request effect, intent writes and event are one executor checkpoint with
an explicit system gas/work meter and a typed outcome. `Deferred(reason,
next_check_height)` commits only the atomic due-index pop/reinsert, READY metadata
and receipt; it creates no intent, split effect, event or pin.
`IntentCreated(IntentId)` commits the full transition above. An execution/storage
or invariant error rolls the entire outer checkpoint back, emits no receipt and
invalidates the candidate block. There is no state in which the old head key was
restored while a Deferred receipt was committed, and no error path exposes a
partial split, state change or event.

### 2.2 Finality and discovery

No authoritative computation begins before the request block is finalized.
Before a node advertises/votes for a candidate containing an intent, and before
its pruning cursor may pass that height, it durably records the already-reserved
snapshot pin. The pin record is small; the retained data may be large but cannot
exceed the protocol reservation envelope.

The local restart-safe pin FSM is:

```text
TENTATIVE(candidate_block_hash, IntentId, state_root)
  -> FINALIZED(JobId, deadline_height)
  -> EXPORTED(source_snapshot_certificate)
  -> RELEASED

TENTATIVE -> RELEASED                 candidate orphaned
FINALIZED | EXPORTED -> RELEASED      expired/completed/conflicted/canceled
                                      and all retention gates pass
```

The journal is write-before-prune and reconciled against canonical/finalized
chain state before pruning resumes after restart. Corruption immediately
disables OCOMP signing and enters bounded quarantine; it never guesses that a pin
was released. The node derives a conservative prune floor from
`head - (MAX_SNAPSHOT_RETENTION_BLOCKS + finality_margin)`, reconstructs at most
`MAX_PENDING_JOBS` finalized pins plus the bounded candidate window from chain
state, and writes a clean journal before normal pruning/signing resumes. New
blocks may still finalize during quarantine; new tentative data is covered by
that conservative window. If resync keeps failing, the validator remains an
OCOMP abstainer but may prune only below the conservative floor, so retained
growth stays within the profile capacity provision instead of growing forever.
Finalized authenticated state openings are exported to the separately
quota-limited snapshot/CAS volume, gain source custody receipts, and only then
may the main state pin be released. This is retention/export, not speculative
semantic computation, and prevents historical Fidelity/Oracle state from being
pruned while finality is pending.

The O(1) node handoff creates a storage-engine-consistent immutable checkpoint
descriptor `(checkpoint_id, finalized_block_hash, state_root, ce_root, schema)`
and a read-only capability for the exporter UID. It never hands out a live MDBX/
Reth writer or an arbitrary path. The exporter journals a canonical page cursor,
can resume any page after crash, writes only content-addressed output, and at the
end rebuilds the complete WWD root/count and verifies every expected
Fidelity/Oracle opening
against the finalized state root; it does not claim to rebuild the entire EVM
state from a partial witness bundle. A mismatch quarantines the export and keeps
the pin. Export progress, failure and volume pressure never block block
execution; the already-reserved pin remains until a valid source
snapshot certificate or the terminal retention rule releases it.

After finality:

```text
JobId = H(
  "OUTBE_OCOMP_JOB_V1",
  IntentId,
  finalized_request_block_hash,
  finalized_request_state_root
)
```

Every node can derive the same value from the finalization proof and the intent's
state inclusion. `IntentId` remains the consensus lifecycle key: request creation
has already inserted `(deadline_height, IntentId)` into the expiry index. The
activation transaction supplies `FinalizedIntentProofV1`, derives `JobId`, proves
inclusion of the same `IntentId` and checks the unique `IntentId -> JobId`
binding before any apply work.

`FinalizedIntentProofV1` is a fork-registered, strictly bounded codec, not a bag
of caller-selected roots:

```text
FinalizedIntentProofV1 {
  chain_id, genesis_hash, fork_id, protocol_bundle_hash,
  canonical_request_header,
  CertifiedParentAccountingMetadataV2 {
    finalized_block_number, finalized_block_hash,
    finalized_epoch, finalized_view, parent_view,
    ordered_committee, signer_bitmap,
    canonical_commonware_finalization_proof,
    committee_set_hash, vrf_material_version,
    vrf_group_public_key_hash,
    proof_kind = FINALIZATION,
    missed_proposers = []
  },
  historical_committee_membership_proof,
  canonical_job_intent,
  intent_account_proof,
  intent_storage_proof
}

IntentStorageKeyV1 =
  H("OUTBE_OCOMP_INTENT_SLOT_V1", IntentId)
```

The bundle fixes the RLP header hash/codec, exact
`CertifiedParentAccountingMetadataV2` codec, pinned Commonware
`Finalization<HybridScheme<MinSig>, Digest>` codec/commit, committee-bound
`finalize` namespace, BLS scheme, `N3f1` quorum/bitmap rules and
`outbe-consensus-proof::verify_v2_proof` verifier ID. That complete tuple is
`finality_verifier_and_vote_domain_id`; an implementation may not reconstruct a
different ad-hoc vote digest. The signed Commonware subject is exactly the
decoded finalization proposal `(epoch, view, parent_view,
payload=finalized_block_hash)` under that committee-bound namespace.

The verifier:

1. rejects a wrong chain/genesis/fork/bundle and recomputes the header hash;
2. requires metadata height/hash to equal the request header, proof kind
   `FINALIZATION` and an empty `missed_proposers`;
3. resolves the ordered committee and HybridScheme/VRF material from the node's
   accepted `ConsensusCommitteeHistoryV1` for `finalized_epoch`, or verifies
   their inclusion
   against the committee-history root of a prior accepted finalized checkpoint;
   `ordered_committee`, `committee_set_hash`, VRF version/group-key hash and
   scheme must equal that authority; relayer bytes alone are never authority;
4. decodes the proof with the authoritative committee length and rejects
   trailing bytes; requires proposal epoch/view/parent/payload equality, verifies
   the Commonware finalization certificate under the authoritative scheme and
   requires its derived signer bitmap to equal the canonical 0/1 bitmap and the
   `N3f1` quorum;
5. verifies the OCOMP system account proof and the fixed
   `IntentStorageKeyV1` storage path against the request header state root;
6. decodes the exact canonical `JobIntentV1`, recomputes `IntentId` and `JobId`,
   and matches its pending nonce, attempt, deadline and
   activation-preconditions hash to the current live record;
7. rejects wrong/missing/duplicate/non-minimal/trailing fields before
   proportional allocation.

Bootstrap checkpoints include the authenticated consensus-committee-history
root and required snapshots; pruning cannot discard them while their supported
proofs remain valid. The profile pins maximum header/proof/intent bytes,
committee members, signatures, trie nodes and cryptographic work. Golden vectors
cover every step above, including a valid certificate paired with a
caller-invented committee and a valid intent at the wrong storage key.

Activation dispatch orders terminal retries before the live-job guard:

1. `COMPLETED` plus the exact `IntentId`, `JobId`, attempt and `ResultDigest`
   returns the recorded activation receipt without constructing a capability or
   repeating effects;
2. `COMPLETED` with any different binding/digest rejects;
3. `EXPIRED`, `CONFLICTED` or `CANCELED` rejects;
4. only live `OFFCHAIN_PENDING` proceeds through deadline, finality,
   certificate, result and apply verification.

No post-finality background transaction is required merely to make expiry work.
A pre-finality reorg removes the intent and its expiry entry in the same reverted
state; no result from that fork is signable.

The event is only a wake-up hint. The supervisor calls a paged
`ListFinalizedJobs(after_cursor)` API and then reads the on-chain job record. On
restart it resumes from its durable finalized cursor, so a lost event or broken
subscription cannot lose a job.

### 2.3 Activation block

When correctness and, for large results, availability have been established, a
relayer submits one ordinary bounded `activateLysis` transaction. The
transaction carries all evidence needed for its profile:

- PoC/BoundedMVP: finalized-intent proof, constant-size complete-result
  commitment and q certificate;
- TargetLarge: finalized-intent proof, recursive proof, root-authoritative
  payload and availability/custody certificate.

The transaction checks:

- the exact `JobId`, attempt, protocol bundle and deadline;
- the result certificate or proof and historical OCOMP committee snapshot;
- every reserved field/version with compare-and-swap;
- the output manifest root and availability certificate;
- that the job has not already completed or expired.

The fork also fixes `MAX_OCOMP_ACTIVATIONS_PER_BLOCK`, maximum evidence bytes,
maximum signatures/receipts and maximum proof-verification cost. Valid excess
transactions revert with a typed block-capacity result and leave the job
unchanged for resubmission. Proposal policy excludes them before execution when
the same deterministic counter is already full. The counter/byte reservation is
checked before result decoding or cryptography, so an over-cap transaction costs
only bounded rejection work. No certificate/result is stored in an intermediate
`RESULT_ACCEPTED` state and no full payload is copied into EVM storage.

The consensus-visible block order is:

```text
begin-zone:
  1. apply due COMPROMISE_REVOKED key records
  2. install due PAUSE/DRAIN mode barriers
  3. evaluate upgrade cutoff/cancel/activation transitions
  4. advance bounded PAUSING cancellation cursors
  5. expire/reset remaining jobs due at this height
     (pause-marked jobs use CANCELED, not EXPIRED)
  6. process bounded custody-expiry/repair state changes
ordinary transactions (including activateLysis)
CE sealing
terminal READY inspection / request creation
commit; no later semantic writer
```

Within each numbered phase records use canonical `(effective_height, plan_id)`
order. A mode generation/snapshot change in phases 1–2 makes a stale upgrade
readiness certificate fail in phase 3. Consequently same-height key revocation
or pause beats upgrade activation, pause beats expiry, and every begin-zone
barrier beats an ordinary activation transaction.

`deadline_height = H` is exclusive: `activateLysis` must execute in a block
`< H`. At begin `H`, every still-pending job due at `H` is archived/reset before
ordinary transactions; a transaction in `H` therefore sees an expired old nonce
and rejects. If the job was marked by a same-height pause, it sees `CANCELED`
instead. This ordering has no activation-versus-expiry/pause tie.

After verification, the same transaction switches the target generation/root,
logically retires the sealed
Tribute WWD by removing its catalog pointer, applies the frozen Desis/Promis
consequences, marks Metadosis `COMPLETED` and emits one aggregate activation
event. It does not emit a billion receipts, run a billion EVM calls or physically
delete a billion-record tree in the block transition.

```text
ActiveGenerationV1 {
  JobId, program_version,
  nod_root, bucket_root, contributor_root,
  output_manifest_root, exact_counts,
  result_evidence_hash, availability_certificate_hash
}
```

This record, rather than a supervisor database, fixes which output is active.

The source collection enters `RETIRED_RETAINED`. Physical CE records are removed
later by a restart-safe, cursor-based local GC with I/O quota. GC is allowed only
after the job/source retention deadline, snapshot/custody handover and live-root
reference checks pass. The finalized CE marker never waits for physical prefix
deletion. This requires replacing today's `delete_collection_records`, which
collects and deletes every prefixed MDBX key in the finalized apply path
([persistence.rs](crates/core/compressed-entities/src/persistence.rs#L1873)).

If a live activation precondition conflicts, certified domain effects do not
apply. Instead, the
transaction commits a **typed domain outcome**, not an error that would roll the
state back. `ConflictResolved` atomically marks that job `CONFLICTED`, increments
the pending nonce and returns the day to `READY` with the same Lysis budget;
only the later terminal phase can create a new intent/snapshot. The old evidence
can never be relabelled as a retry. Unrelated chain execution continues.

Activation is one metered executor checkpoint. Evidence verification happens
before writes; the generation switch, logical retirement, frozen scalar effects,
FSM state and aggregate event commit together or all roll back. Invalid evidence
is a transaction rejection with no state change, a stale compare-and-swap is the
committed deterministic conflict outcome above, and a storage/invariant failure
is fatal to the candidate block.

## 3. Where the Tribute root comes from

The event does not create a Tribute root, and the supervisor does not invent
one. The root already exists as part of normal execution.

When a Tribute is issued, the CE layer records its canonical body commitment in
a pending block overlay. At `end_block` the current implementation:

1. derives the Tribute tree key and one of 16 shards;
2. updates the shard's CKB sparse Merkle tree;
3. aggregates the 16 shard roots;
4. derives the WWD collection root;
5. writes that collection root as a leaf in the CE catalog tree;
6. seals the catalog root;
7. writes the sealed root to the CE EVM storage slot before the block state root
   is finalized.

The current path is visible in
[`tree_service.rs`](crates/core/compressed-entities/src/tree_service.rs#L315),
[`lifecycle.rs`](crates/core/compressed-entities/src/lifecycle.rs#L45) and
[`state.rs`](crates/core/compressed-entities/src/state.rs#L118).

```text
canonical Tribute body
 -> body commitment at TreeKey
 -> one of 16 shard roots
 -> Tribute(WWD) collection root
 -> CE catalog root
 -> sealed CE root in EVM storage
 -> finalized block state root
```

"Freeze the input" therefore means versioning, not locking a live database:

- bind the job to the finalized request block/state root and CE root;
- forbid mutations to the reserved sealed WWD;
- begin retaining source bodies when the WWD is sealed, then promote that
  obligation to a job snapshot lease until the job/recovery window finishes;
- verify every transported body against its committed leaf;
- compare target versions again at activation.

MongoDB is currently a finalized body projection. It may supply bytes, but it is
not authority and its page count is not a completeness proof.

## 4. Runtime and failure boundaries

### 4.1 Production topology

```text
validator host / administrative domain

  outbe-chain.service                 protected node slice
    Reth + Commonware consensus
    finalized Job FSM
    bounded OcompControl endpoint
    OcompAttestationGate + anti-equivocation journal
    OCOMP private key (never exported)

  outbe-ocomp-supervisor.service      separate UID, cgroup and state directory
    finalized-job cursor
    local admission and immutable job binding
    deterministic planner
    scheduler/reducer/verifier
    local job journal

  outbe-ocomp-snapshot-exporter.service  read-only checkpoint UID/cgroup
    opaque finalized-checkpoint handle
    paged root-bound export cursor
    canonical witness/source bundle builder
    no live-node DB write and no validator key

  outbe-ocomp-worker@*.service        ephemeral constrained units
    one UnitId per invocation
    read-only inputs, private scratch
    no validator key, no node DB, no default network

  outbe-ocomp-launch-broker           fixed policy, no semantic computation
    admitted plan/lease ledger
    aggregate worker cap
    only holder of service-manager/namespace launch privilege

  source/artifact volume or remote CAS
    content-addressed chunks
    independent quota and retention
```

The service manager starts `outbe-chain`, the exporter, launch broker and
supervisor as sibling units under one deployment target. Ordering them after the
node is fine; `Requires=`, `BindsTo=` or `PartOf=` from the node to those services
is not. External services continually retry their bounded control connection.

Workers start only after a job passes admission. A supervisor restart kills or
expires its worker leases; the same `UnitId`s are retried and already completed
content-addressed artifacts are adopted after verification.

For a large deployment, the same supervisor may schedule workers in a separate
Kubernetes/nomad compute pool. The node remains on the protected validator host.
The process and protocol boundaries do not change.

### 4.2 Enforced isolation

The exporter, launch broker, supervisor and workers require separate:

- Unix users, mount namespaces and writable directories;
- cgroup-v2 CPU, memory, task and I/O limits;
- disk quota for journal, scratch and artifacts;
- `NoNewPrivileges`, dropped capabilities, seccomp and read-only root filesystem;
- network policy: workers have no network unless a specific source/prover adapter
  requires a job-scoped endpoint;
- logs and metrics budgets.

`MemoryMax` protects the node from a supervisor/worker OOM. CPU and I/O weights
reserve capacity for block execution and consensus. A separate process on the
same unbounded disk is not sufficient isolation.

Per-unit limits sit under a service-manager-owned aggregate boundary. systemd
places exporter, supervisor and every worker in `outbe-ocomp.slice` with total
`MemoryMax`, `TasksMax`, `CPUQuota` and device I/O limits; the node is in a
separate higher-priority slice. Kubernetes uses a dedicated namespace with
`ResourceQuota`, `LimitRange`, pod-count and ephemeral-storage quotas. The launch
broker enforces `MAX_LIVE_WORKERS_PER_VALIDATOR`, per-job live-unit limits and
one live lease per admitted `UnitId` before asking either backend to create a
unit. Exceeding any aggregate or live-unit limit rejects launch and queues the
unit; creating many individually valid templates cannot exceed the parent
budget.

The current Outbe deployment already demonstrates both sides of this lesson:

- the required Mongo projection runs in the node process and its terminal error
  requests node shutdown
  ([main.rs](bin/outbe-chain/src/main.rs#L681));
- the TEE enclave is a separate sidecar, but the main offer client connects only
  once at startup, so restart/reconnect behavior still has to be designed
  ([enclave_offer.rs](crates/core/tributefactory/src/enclave_offer.rs#L35)).

OCOMP must copy neither failure behavior. Supervisor failure changes only
`ocomp_ready`, never `consensus_ready`.

Only the bounded `OcompControl` handler and `OcompAttestationGate` remain inside
the node. Their timeouts, malformed requests, disconnects and task panics must
close the endpoint, set `ocomp_ready=false` and enter a local restart loop; their
join result is not a fatal branch of the node's top-level shutdown selection.
Messages are bounded before allocation and the handler cannot start computation.
This contains ordinary failures but does not pretend that an abort or memory
corruption in any in-process code is physically unable to terminate the node;
the safety argument is the tiny bounded surface, while all expensive and
input-dependent work remains across the process boundary.

Fork/configuration limits fix request bytes, response bytes, page length,
concurrent sessions, queue depth, requests per peer/second and handler deadline.
Overload rejects before parsing or allocation; no OCOMP queue may apply
backpressure to consensus/execution channels. The attestation journal is on a
dedicated quota/mount: quota exhaustion disables signing, not the node. Terminal
entries are compacted only after on-chain terminal finality and the evidence
window, into hash-chained read-only segments retained for the OCOMP key epoch.
Snapshot export/CAS also has its own quota. The protocol's maximum pending jobs
and retention window bound live leases; a full export volume stops OCOMP
admission/export while the protected node database continues.

The node-local task supervisor uses capped exponential backoff, a restart budget
per time window and a circuit breaker. Repeated deterministic panics open the
circuit, keep `ocomp_ready=false` and permit only infrequent health probes until
a cooldown/configuration change; they cannot hot-loop inside the protected node
CPU budget. These controls are local health policy and never alter consensus
state.

### 4.3 Start, stop and upgrade

- Cold start: the node may become consensus-ready without the supervisor. It
  reports OCOMP degraded and produces no OCOMP attestations.
- Supervisor start: handshake, compare chain/genesis/fork/protocol versions,
  replay journal, reconcile the finalized cursor, then schedule work.
- Graceful stop: stop admitting units, checkpoint the journal, terminate workers
  after their lease or grace period. Node shutdown is independent.
- Crash restart: journal records are write-before-side-effect. Uncertain units are
  rerun; content-addressed equality makes duplicate completion harmless.
- Upgrade: every job pins `ProtocolBundleHash`, including semantic program,
  codecs, verifier and planner/reducer specifications. A local release manifest
  binds the exact approved implementation artifact to those semantics; different
  conforming implementations may use different artifact digests. A release
  supports every pinned nonterminal bundle until drain and every historical
  decoder required by replay. No job downloads executable code from the network.
- Version mismatch: refuse the job and alert; never silently reinterpret bytes.

The supervisor owns scheduling intent; a tiny service-manager-owned launch broker
alone owns privileged launch/cancel/reap. On systemd the broker may start only a
polkit-restricted `outbe-ocomp-worker@<UnitId>` transient template with a fixed
binary/image and allow-listed resource class; no caller can supply a shell
command or arbitrary unit properties. On Kubernetes the broker has the
namespace-scoped service account that may create/delete only the pinned Worker
Job template. The supervisor itself has neither privilege. The worker receives
an expiring capability for the exact input/output object digests and no node
credential. Unit name and labels derive from `UnitId`; restart reconciliation
lists that namespace, kills stale leases, verifies completed artifacts, and then
reaps units. Cancel is idempotent and a worker surviving supervisor death cannot
renew its lease or publish a trusted result.

At job admission the broker records the finalized `JobId`, pinned plan manifest
root and allowed resource class. Every launch presents the bounded unit
descriptor and membership proof for that plan; the broker recomputes `UnitId`,
rejects an unknown/completed/duplicate lease and accounts it before creation.
The supervisor cannot authorize an unrelated command by inventing another unit
name.

## 5. The two interfaces

There is one real seam with two adapters: local Unix socket for a single-host
validator and mutually authenticated TLS for a remote compute control plane.
The PoC implements and demonstrates the UDS adapter; the remote mTLS adapter is
MVP work and does not change the interface.

### 5.1 Bounded control plane

`OcompControlV1` accepts only canonical, size-bounded messages:

```text
HelloV1(chain/genesis, boot/session, control_versions,
        protocol_bundle_hashes, capability_bits, limits)
HelloAckV1(selected_control_version, common_bundle_hashes,
           granted_capabilities, peer_identity, session_generation)
ListFinalizedJobs(cursor, limit)
GetJobSpec(JobId)
OpenSnapshotLease(JobId, requested_retention)
RenewSnapshotLease(LeaseId)
ListSnapshotHandoffs(cursor, limit)
GetSnapshotHandoff(JobId)
RequestAttestation(AttestationCandidateV1)
GetOcompHealth()
```

Local transport uses an owner/group-restricted UDS plus peer credentials. Remote
transport uses mTLS identities. Requests include monotonic session counters and
nonces; replays and unknown fields fail closed.

The API never accepts a private key, arbitrary signing payload, worker command,
file path, SQL query or unbounded body. `AttestationCandidateV1` is a closed,
size-bounded typed candidate/evidence locator. The caller does not supply a free
`subject`, `purpose`, `profile` or `digest`. Before signing, the node independently
reloads the finalized job and derives every single-sign key field and signed
digest from canonical candidate bytes.

For the PoC profile, the candidate includes constant-size `LysisResultV1`.
Complete catalog coverage was verified in the separate compute plane. The node
rehashes and structurally validates only the constant-size commitment and does
not read chunks or rerun Lysis. The authenticated local supervisor is part of
that validator's execution domain, and q independent domains provide the
correctness threshold.
For proof/custody candidates, the node additionally verifies the relevant proof,
opening or finalized checkpoint. Unknown, stale, mismatching or merely
caller-asserted identifiers fail closed.

Snapshot-handoff responses contain only the fixed checkpoint descriptor and an
exporter-bound opaque capability. Actual checkpoint pages and exported witnesses
use the bulk plane; they never traverse this API.

### 5.2 Bulk data plane

Source bodies, witnesses, sorted runs, proof segments and output chunks never
pass through `OcompControlV1`. They use content-addressed object references:

```text
ArtifactRef {
  purpose, codec, compression,
  encoded_bytes, decoded_bytes,
  record_count,
  semantic_digest, transport_digest,
  chunk_refs[]
}
```

Every parser enforces encoded, decoded, nesting and record limits before
allocation. Missing, duplicate, overlapping, trailing or unused chunks reject.
Transport hashes detect corruption; semantic roots establish correctness.
The PoC uses a per-validator local filesystem CAS adapter under its own quota;
remote/object-store adapters and durable replication are MVP-or-later work.

## 6. Who splits the data and how

The deterministic planner in each supervisor defines semantic units. The
scheduler only places and retries those units.

```text
UnitSpecV1 {
  protocol_bundle_hash, JobId, attempt,
  phase: ENUMERATE | FIDELITY_MAP | FIXED_REDUCE |
         AMOUNT_MAP | GRATIS_PREFIX | OUTPUT_FINALIZE |
         OWNER_SHUFFLE | BUCKET_SHUFFLE | ROOT_REDUCE,
  interval: EntityIdHalfOpenRange
          | FidelityIndexHalfOpenRange
          | CanonicalRunSpan
          | BinaryReducerNode {level,index},
  canonical_ordered_inputs[] {
    purpose, semantic_root, record_count,
    max_encoded_bytes, max_decoded_bytes
  },
  lysis_program_semantics_hash,
  planner_spec_version, reducer_spec_version
}

UnitId(spec) =
  H("OUTBE_OCOMP_UNIT_V1", canonical(UnitSpecV1))
```

The constant-size `PlanCommitmentV1` additionally fixes `wwd`,
`lysis_budget`, `logical_evaluation_time`, the authenticated manifest and the
ordered primary-unit root. These fields are covered by `PlanHash`; a worker
must decode the exact CAS bytes and match their hash and job/manifest bindings
before execution. Before signing, the node attestation gate compares the plan
context to the finalized `JobIntentV1`. For shard `j`, `AMOUNT_MAP(j)` consumes both
`FIDELITY_MAP(j)` (the per-Tribute league observations) and the fixed-reduce
root (the global fraction table). Neither artifact substitutes for the other.

Every range endpoint has its fixed-width codec, is start-inclusive/end-exclusive
and must be valid for its phase; every vector order and empty/optional form is
fixed by the bundle's object codec registry. Unknown phase/interval pair,
duplicate input purpose, non-minimal field or trailing byte rejects. Changing
worker count, host, completion order or retry count cannot change a `UnitId` or
result. Golden vectors freeze exact tag plus canonical bytes for every phase and
interval variant.

For PoC/BoundedMVP current-state jobs:

1. verify the complete current CE fold for the WWD;
2. external-sort verified records by raw 36-byte `EntityId` because that is
   current Lysis order;
3. cut fixed maximum-record/maximum-byte intervals;
4. split an owner's Fidelity history into fixed authenticated index ranges when
   it exceeds one unit; reduce the partial sums deterministically before
   deriving that owner's league;
5. use a fixed binary reduction tree.

TargetLarge may split one owner's Fidelity cohorts only into fixed authenticated
index ranges. Each range proves partial `(num, den)` sums with the current
`U256` modulo-`2^256` multiplication/addition semantics; the fixed reducer
combines partials modulo `2^256`, performs the current wrapped `num * SCALE`, and
divides exactly once, matching the current formula. Missing cohort slots are
proved and skipped as today. It is invalid to reject a wrap only in the new path,
calculate a league per segment, or average segment leagues.

For TargetLarge, the current 16-shard root is insufficient for cheap complete
parallel enumeration. A fork must add `CountedRangeTreeV1`: every node commits
both child hashes and the number of live leaves. Fixed key-prefix ranges then
carry authenticated counts. Range units cover the complete key space with no
gaps or overlap, and their roots/counts reconstruct the WWD root/count. Raw IDs
are still externally sorted before Lysis. The proof binds a multiset/permutation
commitment from the complete range cover to the sorted stream, then checks strict
raw-ID adjacency; sorting may reorder records but cannot omit, duplicate or
replace one.

This is a deliberate decision, not an optional optimization. A billion-scale
profile remains disabled without a counted/range-authenticated source index.

## 7. The deterministic Lysis job

The job is a streaming multi-stage Map/Reduce. Global FI allocation, raw-order
Gratis effects, owner grouping and bucket grouping are distinct dependencies;
none is hidden in one serial reducer.

```text
Phase A: authenticated enumeration
  Tribute ranges -> verified bodies -> raw-ID sorted runs

Phase B: map Fidelity and demand
  each Tribute/owner -> exact Fidelity reads + nominal partial by FI

Phase C: fixed reduce
  partials -> total nominal + FI fraction table + arithmetic checks

Phase D1: map output amounts in raw EntityId order
  Tribute + FI table + Oracle reads
    -> amount/conservation record

Phase D2: deterministic parallel prefix scan
  fixed segment totals + fixed prefix tree
    -> per-segment incoming Gratis, remaining Gratis, earliest failing ordinal

Phase D3: finalize raw-order outputs
  prefix result + amount record
    -> Nod body + (bucket_key, raw_ordinal, contribution)
                + optional eligible (owner, raw_ordinal, nominal)

Phase E1: owner shuffle
  bounded external sort by (owner, raw_ordinal)
    -> one canonical contributor leaf per eligible owner + nominal prefix

Phase E2: bucket shuffle
  bounded external sort by (bucket_key, raw_ordinal)
    -> stable grouped bucket leaves

Phase F: fixed root/conservation reduce
  raw, owner and bucket streams
    -> output roots, exact counts, conservation totals, event summary
```

Each external sort fixes run record/byte limits, merge fan-in, maximum open files,
spill bytes, compression ratio and tie-break order. Every shuffle carries a
permutation commitment linked to the authenticated raw stream; the proof checks
coverage, no duplicate/omission, grouping and stable source ordinal. The prefix
tree commits every segment sum and selects the lowest failing raw ordinal exactly
as the sequential implementation would. Worst cases include all unique owners,
all unique buckets and all records in one bucket. `1`, `2` and `N` workers plus
arbitrary retry/completion order must produce identical bytes with cap-bounded
RAM, file descriptors and spill.

For `K` primary runs, owner and bucket shuffle each use exactly `K` leaf units
and `K - 1` real binary merge units. Odd runs feed the next real merge directly;
there are no shuffle `CanonicalEmpty` or copy-only promotion units. Every real
merge materializes a fresh bounded page tree under its own `UnitId`; it does
not install producer roots as pages of a supposedly merged output. This keeps
per-worker memory bounded while making the binary external-merge
`Θ((N + E) log K)` I/O cost explicit.

Shuffle source coverage is composed independently from the sorted page-tree
root. A leaf binds the exact raw root/count of its `OUTPUT_FINALIZE` producer;
a real merge hashes the adjacent producer coverage roots/counts under
`OUTBE_OCOMP_SHUFFLE_SOURCE_COVERAGE_V1`. No unit synthesizes a population of
padding records merely to promote an odd run.

All logical inputs are pinned to the request snapshot: Tribute, Fidelity,
Oracle, Gratis/Metadosis scalars, time, code and arithmetic version. There is no
wall clock, floating point, host locale, random iteration order, live RPC read or
network response inside the semantic program.

Phase D3 always emits the Nod and bucket record, but emits an owner contribution
if and only if the authenticated Tribute body has
`exclude_from_intex_issuance == false`. Phase E1 and its permutation proof cover
exactly that filtered stream. `contributor_count`, `contributor_root` and
`contributor_total` therefore commit only eligible owners and the sum of only
their nominal amounts; the excluded nominal still participates everywhere the
current Lysis algorithm uses total Tribute nominal, but never creates a claim
entitlement.

The semantic program must reproduce the current Lysis algorithm, including raw
ID order, two logical Fidelity observations per Tribute, conditional Oracle
reads, each current wrapping or saturating `U256` operation, first-error ordinal,
contributor ordering and one Nod per Tribute. Native, reference and proof-guest
implementations run the same golden and adversarial corpus, including values at
`U256::MAX`. The lifecycle timing is deliberately different:
the fork pins the terminal end-of-block snapshot. Exact state/event equivalence
with today's synchronous path is expected only when no relevant same-block
mutation separates the two execution points; concurrent cases test the new
fork ordering explicitly.

Current identity is `Poseidon(owner, WWD)` and duplicates are rejected, so a
valid WWD has at most one Tribute per owner. Therefore:

```text
T = number of Tributes
U = number of distinct Tribute owners
D = number of emitted Nod items
C = number of eligible contributor owners

U = T, D = T and 0 <= C <= T on the successful branch
```

One billion Tributes means one billion owner-specific Fidelity evaluations. It
cannot be represented as one `league(owner,height)` table loaded into RAM.

## 8. What proves that the result is correct

Correctness is a chain of separate claims. None may be replaced by the next one.

### 8.1 Input authenticity and completeness

```text
finalization proof
 -> request block header/state root
 -> JobIntent inclusion and CE sealed-root slot
 -> catalog/WWD collection root
 -> complete shard/range cover
 -> each leaf commitment
 -> each canonical body and every Fidelity/Oracle state opening
```

A body hash proves one body. It does not prove that no Tribute was omitted. For
the PoC and bounded MVP, validators rebuild the entire current CKB tree. For
TargetLarge, the counted range cover and proof program establish complete
enumeration.

### 8.2 Bounded profile: independent execute-and-attest

Each participating validator's own supervisor verifies and executes the whole
job on resources controlled by that validator. Its node then signs exactly one
canonical `ResultDigest` with a separately registered OCOMP key.

```text
ResultChunkHash =
  H("OUTBE_OCOMP_RESULT_CHUNK_V1", canonical(ResultChunkV1))

ActivationPayloadV1 {
  protocol_bundle_hash, JobId, attempt,
  result_chunk_count, result_chunk_list_root,
  nod_root, bucket_root, contributor_root, output_manifest_root,
  exact_input_and_output_counts,
  conservation_totals, arithmetic_commitment, event_summary_hash
}

ResultDigest = H("OUTBE_OCOMP_RESULT_V1", canonical(ActivationPayloadV1))

ExecutionCertificateV1 {
  result_committee_snapshot_hash, signer_bitmap,
  aggregate_or_ordered_signatures, ResultDigest
}
```

For `PoC` and `BoundedMVP`, each signing validator traverses every bounded
`ResultChunkV1`, verifies gap-free complete coverage and recomputes the catalog
root, output roots, counts, totals and event summary before signing. The
activation transaction carries only the constant-size `LysisResultV1`
commitment. This is the only valid `ResultDigest` preimage; no alternate tuple
encoding is permitted. Thus a quorum signature cannot be detached from the
exact chunk catalog it authorizes. The PoC fixes small per-interface envelopes
in section 1.5 but does not cap total Tribute. For `TargetLarge`, the proof's
public output is the same `ResultDigest` and root-authoritative payload.

For the exact historical ordered unit-weight committee `C_job` pinned by the
intent:

```text
n = committee size
f = floor((n - 1) / 3)
q = floor(2n / 3) + 1 = n - f
```

`q` distinct result signatures certify the output under the assumption that at
most `f` validator administrative domains are Byzantine. One validator using
1,000 workers still contributes one signature. Shared cloud accounts, shared
worker clusters or the same buggy binary reduce real independence and are an
explicit operational/common-mode risk. Historical evidence is always checked
against `C_job` and its archived OCOMP public keys, never today's committee,
plus the key-status history at the activation height. A later compromise record
does not rewrite a block finalized earlier; at or after its effective height the
affected snapshot cannot authorize a new activation as specified in section
14.6.

### 8.3 Large profile: proof-carrying execution

Repeating a billion-record computation at `q` validators is not the target
architecture. Permissionless provers execute the deterministic phases in
segments and recursively aggregate proofs. The final public statement binds:

```text
JobId
protocol bundle, program semantics and verifier key
all input roots and exact counts
the gap-free UnitId/reduction tree
the exact ActivationPayloadV1 / ResultDigest
DA encoding commitment
```

Every validator verifies the final proof during the activation transaction and
during block replay. A supervisor may preverify it, but consensus does not trust
that precheck.

The capped proof bytes, public inputs, verifier/program identifiers,
`ActivationPayloadV1`, and result/availability certificates form
`EvidenceRecordV1` in the activation transaction; a CAS-only reference is not
enough. The terminal consensus record keeps its hash and exact identifiers.
Ordinary block-data availability retains the full record through the declared
replay horizon. Before block-body pruning, an authenticated evidence archive or
finalized checkpoint handoff must retain the record plus historical
committee/key snapshots required by the supported audit/replay profile. Nodes
replaying blocks reverify the proof; nodes bootstrapping from an accepted
finalized checkpoint verify that checkpoint and its declared evidence coverage.
No active generation points only to evidence bytes that every honest recovery
path is allowed to prune.

A proof shows that the pinned program ran correctly. It does not show that the
program implements the intended economics, that inputs were completely exposed
unless the program checks the authenticated enumeration, or that output bytes
remain available. Those are separate audit and availability obligations.

Outbe's current `zk_verify` verifies registered UltraHonkKeccak circuits and
initializes a Barretenberg CRS for circuits up to the current registry maximum
([verify.rs](crates/system/zkproof/src/verify.rs#L1)). It is useful verifier
infrastructure, not an existing billion-record recursive Lysis prover. Target
requires a benchmarked, fork-pinned recursive backend and pre-staged immutable
verifier parameters; verifier availability cannot depend on a network download.

### 8.4 Anti-equivocation

The OCOMP private key is not the consensus key and never leaves the node/HSM
boundary. The public key is bound to the validator identity in the committee
snapshot.

`OcompCommitteeSnapshotV1` is consensus state, not a supervisor configuration.
It contains the ordered validator identity/index, one unique OCOMP public key,
key scheme, a closed allowed-purpose set, proof-of-possession, validity interval
and key epoch. The bounded profile registers distinct domain-separated
`ResultSignature` and `UpgradeReadiness` purposes; custody purposes are present
only for profiles that use them. Key
registration is authorized by the validator identity but proves possession of
the new OCOMP key; duplicate public keys, duplicate indexes, the consensus key,
zero/invalid points and overlapping unauthorized epochs reject. The immutable
snapshot hash is pinned in each intent. Rotation creates a successor snapshot at
a governed height; it never edits the old snapshot. Historical OCOMP snapshots
and verification keys are retained for the full supported replay/audit horizon,
independently of any short rolling ValidatorSet cache.

PoC may use protected local files for the separate key, but BoundedMVP activation
requires the release profile's HSM/remote-signer custody, backup, rotation,
revocation and recovery gates. Old private keys remain sign-capable only until
their last pinned job terminates under orderly rotation. A compromise
revocation quarantines/destroys the affected private key immediately and
cancels its snapshot's nonterminal jobs rather than waiting for drain; public
verification keys and status history remain historical.

Before signing, `OcompAttestationGate` atomically persists:

```text
subject       = Result(JobId, attempt)
              | UpgradeReadiness(plan_id, readiness_generation)
              | JobInput(JobId)
              | JobOutput(JobId, attempt)
              | Source(SourceSubjectId)
              | StateDelta(generation_id, sequence)
              | StateSnapshot(finalized_block_hash, checkpoint_round)
purpose       = ResultSignature
              | UpgradeReadiness(target_ocomp_snapshot_hash)
              | FragmentCustody(validator_index, committee_epoch_hash, custody_round)
              | CustodyHandover(parent_certificate_hash, custody_round)
SingleSignKey = (chain_id, subject, purpose)
value         = (protocol_bundle_hash, key_epoch, digest)
```

Subjects identify a logical object without containing its manifest, bundle or
encoding root; those roots are in the signed value. Therefore two different
encodings for the same logical delta/snapshot/source collide at the same
single-sign key instead of bypassing equivocation protection. Each object has
independent custody rounds, retention and handover.

The canonical candidate mapping is part of the fork:

```text
ResultCandidate(JobId, ActivationPayloadV1, verified evidence)
  -> subject = Result(JobId, attempt)
  -> purpose = ResultSignature
  -> digest  = ResultDigest

UpgradeReadinessCandidate(canonical_readiness_core)
  -> verify plan_id/readiness_generation, PlanCoreHash,
            UpgradeEnvelopeHash and target_ocomp_snapshot_hash
            against the one current ARMING plan
  -> subject = UpgradeReadiness(plan_id, readiness_generation)
  -> purpose = UpgradeReadiness(target_ocomp_snapshot_hash)
  -> digest  = ReadinessStatementDigest(canonical_readiness_core)

UnsignedFragmentReceiptCandidate(binding, statement, identity, index,
                                 fragment_digest, fragment_opening)
  -> subject = statement.subject
  -> purpose = FragmentCustody(index, statement.custody_committee_epoch_hash,
                               statement.custody_round)
  -> digest  = FragmentReceiptDigest(canonical receipt core) from section 9

HandoverApprovalCandidate(subject, parent_certificate_hash,
                          next_statement_hash,
                          from_epoch_hash, to_epoch_hash, round)
  -> purpose = CustodyHandover(parent_certificate_hash, round)
  -> digest  = H("OUTBE_CUSTODY_HANDOVER_V1", canonical(candidate))
```

The gate derives this mapping after verification; no transport field can alias a
receipt as a handover, change its round/epoch/index, or select another journal
key. The registered OCOMP key's bundle-pinned allowed-purpose set contains
`ResultSignature` and `UpgradeReadiness` separately; the HSM/remote signer
refuses any unregistered purpose. For one `(plan_id, readiness_generation,
target_snapshot)` an identical readiness retry is idempotent and any different
core/digest is refused. Updating readiness requires a new governed plan or
generation, never journal reset or a direct key call. For handover the gate also
requires that the parent is the current finalized
custody certificate and `round = parent.round + 1`. The successor epoch and next
statement remain only in the signed digest/value, so two competing successors for
the same parent/round collide at one journal key; an identical retry is
idempotent and a different successor is rejected. Any redundant value inside a
locator must equal the derived value byte for byte or the request rejects.

The record is fsynced before the signature is released. Re-signing the same
value is allowed; a different value is rejected and is objective equivocation.
Loss or ambiguity of this journal disables OCOMP signing until recovery. It is
never reset just to restore liveness. A schema or key-epoch upgrade imports the
old append-only segments under a new journal generation, verifies the hash
chain, writes a durable migration receipt and only then enables the successor
key. The old and new generations share the same logical `SingleSignKey`
collision rules for every overlapping in-flight job.

## 9. Correctness is not availability

Availability is required on both sides of the computation. A root can prove a
body after it is supplied, but cannot recover missing bytes. Bounded validators
retain the sealed WWD bodies (or reconstruct them from retained finalized
`TributeBodyStored` data) before request. TargetLarge builds a canonical source
bundle incrementally as finalized Tributes arrive; sealing closes its manifest
and base source custody record in bounded work. `JobIntentV1` references that
record. After request finality, the pinned snapshot is exported into a
`SourceSnapshotCertificateV1` that binds `JobId`, the base Tribute manifest and
the exact Fidelity/Oracle openings used by the program. Target result evidence
is not accepted without that certificate and its custody receipts. A job is not
locally admitted if the base source is already unavailable. The post-finality
snapshot lease extends an existing retention obligation and adds
snapshot-dependent state; it is not the first request to save Tribute bytes.

The PoC profile keeps result chunks in three independently signing validator
domains and commits their catalog root in consensus. This is sufficient for the
demonstration's projection/proof-serving path, but it is not a production
availability or custody proof. BoundedMVP must harden retention/repair, and
TargetLarge adds the explicit custody construction below.

TargetLarge produces a large semantic bundle. Activating only its hash would be
unsafe. The selected target shape is:

1. canonical output chunks and a manifest root;
2. an exact historical custody committee `C_custody` with `n` identities,
   `f=floor((n-1)/3)` and `q=n-f`;
3. an erasure code with `n` identity-assigned validator fragments and
   reconstruction threshold `k=f+1`;
4. a proof/commitment that fragments encode the exact proved bundle;
5. one durable fragment receipt from `q` distinct members of `C_custody`;
6. periodic custody challenges, repair and a new valid custody certificate before
   committee-to-committee handover or old-fragment pruning.

Each custody signer first writes its assigned fragment durably, fsyncs it,
closes and reopens it, verifies the generic encoding-binding proof and its own
fragment opening, and only then signs:

```text
EncodingBindingV1 {
  chain_id, subject,
  canonical_bundle_codec, bundle_root,
  erasure_codec_id, n, k, encoding_commitment,
  encoding_binding_proof
}

CustodyStatementV1 {
  chain_id,
  subject = JobInput(JobId) | JobOutput(JobId, attempt)
          | Source(SourceSubjectId)
          | StateDelta(generation_id, sequence)
          | StateSnapshot(finalized_block_hash, checkpoint_round),
  custody_committee_epoch_hash,
  encoding_binding_hash,
  canonical_bundle_codec, bundle_root,
  erasure_codec_id, encoding_commitment,
  n, k, custody_round, parent_certificate_hash,
  retention_height
}

FragmentReceiptCoreV1 {
  statement_hash,
  validator_identity, validator_index,
  fragment_digest, fragment_opening
}

FragmentReceiptDigest(core) =
  H("OUTBE_FRAGMENT_RECEIPT_V1", canonical(core))

SignedFragmentReceiptV1 {
  canonical_core,
  fragment_receipt_digest,
  signature
}

encoding_binding_hash = H(
  "OUTBE_ENCODING_BINDING_V1", canonical(EncodingBindingV1)
)
statement_hash = H(
  "OUTBE_CUSTODY_STATEMENT_V1", canonical(CustodyStatementV1)
)
fragment_receipt_digest = FragmentReceiptDigest(FragmentReceiptCoreV1)
signature = OCOMP_SIGN(fragment_receipt_digest)
```

The fork-pinned verifier for `EncodingBindingV1` proves that the fragments under
`encoding_commitment` are the declared erasure-code encoding of the exact
canonical bytes whose domain-separated manifest digest is `bundle_root`.
Subject-specific evidence first establishes what that semantic root means:
complete source/opening verification for `Source`/`JobInput`, the Lysis proof for
`JobOutput`, finalized block data for `StateDelta`, and the transition proof for
`StateSnapshot`. A hash or two adjacent fields without this relation are not an
encoding proof.

Acceptance recomputes `encoding_binding_hash` and requires exact equality between
statement and binding for `(chain_id, subject, canonical_bundle_codec,
bundle_root, erasure_codec_id, n, k, encoding_commitment)`. Subject-specific
evidence is checked against that binding's `bundle_root`; every fragment digest/
opening is checked at its canonical index against that same binding's
`encoding_commitment`. The attestation gate recomputes and persists
`fragment_receipt_digest` as its single-sign value before returning the
signature.

Aggregation then requires one identical `CustodyStatementV1`, a valid binding
proof, `q` distinct committee identities, `q` distinct canonical indexes, the
exact identity-to-index mapping, and a valid opening/signature for every assigned
fragment. Signer-specific indexes and fragment digests are expected to differ.
The initial round uses a zero parent; every renewal/handover increments the round
and names the finalized parent certificate. For `JobOutput`, the same encoding
commitment also equals `ActivationPayloadV1.da_encoding_commitment_or_none`.

A source custody record uses the `Source` subject and cannot be manufactured for
the first time after Metadosis requests the job. Its stable pre-job identity is
defined by one canonical codec:

```text
SourceSubjectSpecV1 {
  chain_id, genesis_hash, fork_id, source_codec_version,
  WWD, seal_block_height, seal_block_hash, ce_collection_root
}

SourceSubjectId(spec) =
  H("OUTBE_OCOMP_SOURCE_SUBJECT_V1", canonical(spec))
```

`WWD`, height and every hash have fixed-width encodings in the bundle object
registry; unknown/trailing/non-minimal forms reject. Golden vectors cover byte
order and adjacent days/heights/forks. This subject is used rather than the
not-yet-existing `JobId`. The manifest/bundle root is deliberately only in the
signed receipt value: two conflicting manifests therefore hit the same
single-sign key and the second signature is refused.

Among `q` receipts at least `f+1` are honest, so at certificate time at least
`k` correct assigned fragments exist and the bundle is reconstructable under
the BFT assumption. Signatures alone do not prove future retention, hence the
custody/repair protocol.

Custody is an explicit renewable FSM, not a one-time signature:

```text
CERTIFIED(epoch, certificate_hash, retention_height)
  -> CHALLENGED -> CERTIFIED
  -> REPAIRING  -> CERTIFIED
  -> HANDOVER_PENDING(next_epoch, parent_certificate_hash)
  -> CERTIFIED(next_epoch, new_certificate_hash, new_retention_height)
```

Missed challenges trigger repair while at least `k` fragments remain. The old
committee may prune only after a finalized next-epoch certificate covers the
same encoding commitment and the snapshot/delta retention floor. Falling below
`k` blocks pruning and new dependent mutations and marks the affected generation
unavailable; it never silently advances. Receipt purposes include source/output,
fragment index and custody epoch, so renewals and handovers have distinct
persist-before-sign keys.

The availability state of every live generation is consensus-visible:

```text
AVAILABLE(valid_certificate)
  -> AT_RISK(missed_challenge, still >= k) -> AVAILABLE(repaired_certificate)
  -> UNAVAILABLE(expired_certificate | observed < k)
  -> RECOVERING(snapshot + contiguous deltas for exact current root)
  -> AVAILABLE(new q-certificate for that exact root)
```

Every mutation/claim checks certificate validity at the current height even if
the due-index transition has not yet run. Certificate expiries/challenges use a
canonical bounded due-index, not a scan of all generations. `UNAVAILABLE` halts
only dependent domain operations; unrelated consensus continues. Reactivation
requires reconstructing and proving the exact current root and a new `q` custody
certificate. If that is impossible, the generation stays unavailable; rollback
or replacement requires an explicit protocol/governance fork and is never
invented by an operator.

Large active state is root-authoritative and witness-based. A later query or
mutation supplies the body/chunk and Merkle opening against the active root;
validators need not materialize a billion records before voting. Local databases
are rebuildable caches. Without witness-based validation, a `q` availability
certificate can leave too few fully prepared honest validators for immediate
consensus liveness, while all-validator readiness lets one offline validator veto
every job. TargetLarge is therefore disabled until witness-based reads/mutations
exist for Nod and every affected primitive.

### 9.1 Mutable-state recovery bridge

The activation bundle recovers only the initial generation. Every subsequent
root-changing transaction must also produce a bounded canonical record:

```text
StateDeltaV1 {
  generation_id, block_height, tx_ordinal, sequence,
  previous_root_vector,
  ordered_set_delete_nullifier_operations,
  body_or_available_content_refs,
  new_root_vector
}

StateSnapshotCoreV1 {
  finalized_height, finalized_block_hash, checkpoint_round,
  live_generation_ids, root_vectors,
  body_and_index_manifest_roots,
  cursors_summaries_and_nullifier_roots,
  last_delta_sequence_by_generation[],
  parent_snapshot_certificate_hash
}

CertifiedStateSnapshotV1 {
  core, snapshot_manifest_digest,
  encoding_binding_hash, encoding_commitment,
  custody_certificate
}

snapshot_manifest_digest = H(
  "OUTBE_STATE_SNAPSHOT_CORE_V1", canonical(StateSnapshotCoreV1)
)
```

Execution checks each mutation witness against `previous_root_vector`, applies
the bounded operations and commits `new_root_vector`; deltas for one generation
are strictly contiguous, and the canonical sequence vector contains one entry
for every live generation. Every ordinary delta is capped by
`MAX_STATE_DELTA_BYTES` and included in block data; a larger change must use a
separate reserved intent/job and cannot bypass the cap with a CAS reference.
After block finality, custodians sign the unique finalized
`StateDelta(generation_id, sequence)` object, avoiding signatures for competing
unfinalized proposals. Block data/pins retain uncustodied deltas, pruning waits
for their receipts, and `MAX_UNCUSTODIED_DELTAS` halts further dependent
mutations before retention becomes unbounded. A periodic proof shows that
`StateSnapshotCoreV1` is the result of the previous certified snapshot plus the
complete delta interval.

`StateSnapshotCoreV1` contains only the previous certificate hash. The current
certificate is the outer `CertifiedStateSnapshotV1`, so neither the manifest
digest nor the certificate hashes itself. Validators recompute the digest from
`core`, and the aggregate custody certificate must contain receipts whose
common statement has `bundle_root` equal to that digest and whose binding hash
and encoding commitment match the outer record. Its receipt subject is the
stable finalized block hash/checkpoint round;
the signed value contains the manifest digest and encoding commitment.

At least two certified snapshots and every delta after the older one remain in
custody. Pruning requires a newer finalized snapshot certificate, `q` custody
receipts, a verified contiguous bridge and the configured recovery floor. Clean
recovery is: finalized chain checkpoint -> latest available certified snapshot
-> ordered deltas -> exact current roots. The profile fixes RPO/RTO, maximum
delta interval and repair bandwidth. `TargetLarge` remains disabled until a test
can activate, mutate/claim/delete, remove every cache and recover the exact roots
with up to `f` custody domains unavailable.

### 9.2 Contributor claims instead of a billion-record drain

`TargetLarge` replaces Intex's push drain with fork-registered
`ContributorClaimsV1`. Lysis commits leaves in canonical owner order:

```text
ContributorLeafV1 {
  series_id, index, owner, nominal,
  prefix_nominal_before, prefix_nominal_after
}
```

The proof program checks adjacency, starts at zero and ends at
`total_nominal`. For proceeds round amount `A`, a valid leaf receives:

```text
share = floor_mul_div_512(A, prefix_nominal_after, total_nominal)
      - floor_mul_div_512(A, prefix_nominal_before, total_nominal)
```

For a non-zero contributor total the shares sum exactly to `A`, are independent
of claim order and require no scan. `floor_mul_div_512` uses a full-width product
and rejects a zero denominator
or a result outside `U256`; it is not host-dependent wrapping. This deliberately
replaces today's "all floor, last contributor gets the remainder" rule; golden
tests make the fork-visible rounding change explicit.

A source route first creates an immutable protocol lot; target scheduling is not
allowed to choose its boundary:

```text
ProceedsLotV1 {
  series_id, source_domain, source_sequence,
  origin_finalized_tx_or_batch_id, amount
}
LotId   = H("OUTBE_PROCEEDS_LOT_V1", canonical(ProceedsLotV1))
RoundId = LotId
```

`SourceLotRuleV1` is fork-pinned for every source domain: at each source
finalized height it gathers, in canonical transaction/event order, all non-zero
proceeds credited to one series, checks their sum, and creates exactly one lot
whose `origin_finalized_tx_or_batch_id` contains that source block hash and
series identifier. Empty heights create no lot; `source_sequence` increments
once per non-empty height. A relayer, supervisor or target scheduler cannot
split, combine or reorder lots. Each lot retains its own `amount`; every lot for
a series with non-zero contributor total becomes exactly one claim round. Target
congestion can delay a lot but cannot change the rounding boundary used for owner
entitlements.

If the activated series has `contributor_total == 0` (including the `C = 0`
all-excluded case), a lot does not open a claim round and the formula above is
never evaluated. One bounded ownerless transition verifies the zero-total
contributor commitment, moves the exact lot amount from series escrow to the
fork-pinned reserve vault, emits
`OwnerlessProceedsSweptV1(LotId, amount, reserve_vault)`, and marks that immutable
lot accounted. This preserves today's Intex ownerless sweep instead of either
paying an excluded owner or stranding a zero-denominator round.

A closed proceeds round commits `(series_id, round_id, contributor_root,
total_nominal, A, claimed_amount, nullifier_root, round_incoming_closed)`. `A`
must equal the amount of the unique `ProceedsLotV1` whose `LotId = round_id`, and
the committed contributor count and total must both be non-zero. A
claimant supplies the leaf, Merkle opening and sparse-nullifier
non-membership/update witness. One bounded transition verifies the proof,
transfers `share`, updates the nullifier/current root and emits one claim event.
Duplicate claims fail compare-and-swap. A late cross-chain top-up is a distinct
immutable lot and therefore a distinct round over the same contributor root, so
prior claims neither block nor silently include it.

There is no begin-block loop over contributors or all active series. Completion
of fan-in or its timeout closes one round amount and opens claims; it does not by
itself prove that the series can never receive a late top-up. A credited late lot
while series ingress remains open creates its pre-identified round through a
bounded transition.
`MAX_CLAIMS_PER_BLOCK` and `MAX_ROUND_FINALIZATIONS_PER_BLOCK` cap work.

Target claim rights do **not expire** and unclaimed funds do not move to a reserve
because the chain was congested. `A - claimed_amount` remains root-accounted in
escrow until claimed. Therefore the design makes no false promise that one
billion individual balance transfers finish inside a fixed window: the physical
lower bound is `ceil(claim_count / MAX_CLAIMS_PER_BLOCK)`, but no owner forfeits
rights when that time grows. If the economic specification requires a bounded
payout deadline or reclamation, the `1B` profile remains disabled until a
proof-aggregated claim rollup also defines how its balance root becomes spendable.
Claim nullifiers, escrow accounting and round deltas use the snapshot/delta
bridge above. Target series cannot fall back to the current 200-per-block push
drain.

Indefinite rights have a finite authenticated resource owner.
`ActiveCapacityLedgerV1` caps live Target claim series, live rounds, entitlement/
nullifier leaves, canonical bytes, retained delta window, snapshot/custody bytes
and repair bandwidth. Request admission reserves the worst-case active slot in
`target_reservations`; activation atomically converts the pending job reservation
into an active-state reservation before releasing temporary prover/export units.
If conversion cannot match the exact reserved values, activation does not occur.

Every active series reservation includes bounded per-source sequence/close
markers and a fixed inbox/credit window. The conservative profile uses
`MAX_PENDING_LOTS_PER_SOURCE = 1`. Target issues an authenticated credit only for
the exact `(series_id, source_domain, next_sequence)` while that source inbox has
reserved room. A source may finalize its immutable lot before it has credit, but
keeps the value source-owned or source-parked until target-final credit is
available. A bridge transfer must quote that credit and satisfy the declared
maximum lot bytes and amount.

For a non-zero contributor total, if the live-round cap is full, the one credited
lot can wait unchanged in its pre-reserved inbox. A zero-total lot instead takes
the bounded ownerless sweep and consumes no round slot. Target issues no next
credit for that source until the prior lot is converted to its exact `RoundId`,
accounted by that ownerless sweep or provably refunded. It never merges that lot
with another amount and never splits it to fit capacity. A send with no credit,
a wrong sequence, a duplicate, an overflow or an oversized lot is rejected
before target bridge settlement and remains/refunds at source. Thus capacity
creates backpressure, not a different economic result. The source route must
enforce fork-pinned `MAX_SOURCE_UNSENT_LOTS` and
`MAX_PROCEEDS_LOT_AMOUNT`: when its reserved source queue is full, later
proceeds-producing settlement fails before accepting funds. It cannot transfer
that resource obligation to target without credit.

Series ingress has a separate terminal handshake:

```text
INBOUND_OPEN(fixed_source_domain_set, next_sequences, credit_windows,
             pending_lot_inboxes)
  -> INBOUND_CLOSING(per-source ProceedsClosed(last_sequence, cumulative_amount))
  -> INBOUND_CLOSED(InboundCloseCertificateV1)
```

Each source route finalizes its last sequence/cumulative amount, refuses or
refunds later sends, and supplies bridge-finality evidence. The target accepts
`InboundCloseCertificateV1` only after every fixed source is closed and every
immutable lot through its declared last sequence is delivered and converted to
that same `RoundId`, swept by the ownerless transition, or is individually and
provably refunded. Every issued credit is consumed or cancelled, every
pending-lot inbox is empty, and cumulative delivered-plus-swept-plus-refunded
amounts match the source close statement. Delayed
duplicates then reject; no new legitimate value can arrive. A timeout alone is
not an inbound-close proof.

A round slot is released only after every entitlement is claimed and a certified
snapshot compacts its nullifiers/accounting. The series slot and its per-source
inboxes are released only after `INBOUND_CLOSED`, all rounds are fully claimed,
all credits are closed and no pending lot remains. If a source never closes or
an owner never claims, the finite reservation remains forever; once the global
ledger is full, new Target jobs remain READY rather than invalidating old rights
or accumulating unbudgeted custody obligations.

## 10. Job state machine

```text
READY
  -> OFFCHAIN_PENDING(IntentId)

after request-block finality:
  DISCOVERED(JobId)
  -> ADMITTED
  -> RUNNING
  -> LOCALLY_VERIFIED

consensus state:
  OFFCHAIN_PENDING
    -> COMPLETED             one activation transaction verifies all evidence and applies
    -> EXPIRED               deadline and exact nonce/attempt
    -> CONFLICTED            committed activation compare-and-swap outcome
    -> CANCELED              governed profile pause before activation

  terminal job: COMPLETED | EXPIRED | CONFLICTED | CANCELED
                                                   archived forever
  corresponding day: OFFCHAIN_PENDING
    -> READY(next_pending_nonce)                  on EXPIRED/CONFLICTED/CANCELED
```

The deterministic system executor owns expiry/reset; liveness does not depend on
a supervisor or relayer. The request checkpoint inserts jobs into the consensus
expiry index by `(deadline_height, IntentId)`, because `IntentId` is known in the
request block and `JobId` depends on that block's later finality. At each
begin-zone the executor processes the canonical due prefix under
`MAX_OCOMP_EXPIRATIONS_PER_BLOCK`, which is provisioned at least as large as
`MAX_PENDING_JOBS`, so due jobs cannot build an unbounded backlog.
Expiry resolves the record by `IntentId`, removes that exact expiry entry,
archives the old intent as `EXPIRED`, releases any program-specific capacity
claim, increments the day nonce and inserts the day into the READY due-index.
Lysis preserves its frozen budget and request split for retry. It works even
when no supervisor ever ran. Successful
activation derives and records `JobId` and removes the same expiry entry; a
compare-and-swap conflict does so while performing the analogous `CONFLICTED`
transition inside the activation checkpoint. Each path has a metered system
receipt; partial removal/release/requeue is impossible, and invariant/storage
failure invalidates the candidate block rather than silently skipping cleanup.
The local pin FSM observes finality of that terminal transition and then releases
only after its export/retention gates; consensus does not directly mutate a
node-local pin journal.

There is deliberately no persistent `RESULT_ACCEPTED` or `DATA_AVAILABLE`
substate. The relayer waits until the complete profile evidence exists and then
submits it once with activation. This removes a cross-block payload handoff,
pending-result storage and one crash/replay boundary.

`RUNNING` and `LOCALLY_VERIFIED` are local supervisor states, not consensus
truth. A local journal can always be reconstructed from finalized chain state
plus verified content-addressed artifacts.

The old active state remains usable in every nonterminal job state. Expiry
preserves the Lysis budget, releases only program-specific temporary capacity
claims and leaves the day visibly unprocessed. It never invents a partial result.

Persisted tags are closed versioned enums; an unknown tag is corruption, not
READY/default. Every mutation preserves these local equivalences without a
global scan:

```text
WWD is OFFCHAIN_PENDING
  <=> exactly one live IntentId for its current pending_nonce

live intent
  <=> exactly one expiry entry
  <=> exactly one immutable activation-precondition/capacity set

terminal intent
  <=> no expiry entry
  <=> no temporary program capacity claim

COMPLETED
  <=> exactly one activation receipt and active result identity

READY
  <=> exactly one due-index entry and no live intent for the current nonce
```

Record, expiry, precondition/capacity, READY index, active result and terminal archive
writes are owned by the OCOMP aggregate command; no caller updates one side
directly.

## 11. Admission and the billion-record boundary

There are two admission layers.

Protocol admission uses authenticated counts and fork-pinned maxima. If a field
needed for a safe upper bound is unknown, stale or above the active profile, the
request does not fall back to synchronous Lysis. The WWD remains deferred and
the chain continues.

After the deterministic protocol reservation/pin, local admission checks actual
free disk, memory, CPU, I/O, ingress, proof slots, artifact retention and current
jobs before expanding the snapshot export or scheduling workers. A validator
that cannot meet its declared profile abstains and alerts; it still honors the
already-bounded protocol pin until the release gate.

Required bounds include at least:

- Tribute/body counts and decoded bytes;
- unique owners and total/per-owner Fidelity cohort visits;
- currencies and Oracle openings;
- Nod items, buckets, contributors and output bytes;
- external-sort spill, file count and merge fan-in;
- proof trace, prover RAM/disk/time and verifier limits;
- DA encoding, ingress/egress, custody and repair;
- target mutation witnesses and snapshot/restore time.
- post-activation live series/round slots, worst-case nullifiers, fixed
  per-source lot inbox/credit windows, source-side backpressure, snapshot/delta
  custody and indefinite active-state reservation.

For `T=1,000,000,000`, even the raw 36-byte Tribute IDs alone are 36 GB decimal;
one 32-byte digest per record is another 32 GB. These are arithmetic lower
bounds, not capacity measurements. Fidelity histories, bodies, sorted runs,
proof traces, Nod outputs, redundancy and indexes dominate them.

The work is at least:

```text
Omega(T + total_active_fidelity_slots + total_sold_fidelity_slots)
```

Parallel workers reduce wall time; they do not reduce total reads, bytes,
proof work or custody. A `1B` label is permitted only after a cold, reproducible
run with the exact worst-case shape, failures injected and normal consensus load
present. Until then it is a hypothesis, not a supported limit.

## 12. Failure behavior

| Failure | Job effect | Node/chain effect |
|---|---|---|
| event/subscription lost | cursor scan rediscovers job | none |
| supervisor absent at boot | no local work or attestation | node starts; OCOMP degraded |
| supervisor panic/OOM | service restarts; journal reconciles | consensus continues |
| worker panic/timeout | same `UnitId` retried | none |
| snapshot exporter crash/full volume | page cursor resumes; source certificate waits | bounded main pin remains; consensus continues |
| IPC flood/control queue full | reject/rate-limit; OCOMP may degrade | consensus queues remain independent |
| worker launcher/API unavailable | leases expire; reconcile/retry later | none |
| supervisor/artifact disk full | admission stops; job pauses/expires | node disk quota remains available |
| OCOMP signer journal quota full | no new signatures | consensus continues on separate storage budget |
| tentative-pin journal corrupt | signing off; conservative-window resync under restart budget | bounded retention; consensus continues |
| corrupt/mismatching artifact | quarantine; no reduce/sign | none |
| source body missing | fetch another provider or abstain | no false result |
| node restarts mid-job | workers may finish; signing waits | normal consensus recovery |
| request block reorg before finality | tentative observation discarded | no authority existed |
| finality violation | halt/fail closed | protocol-level incident |
| supervisor/node version mismatch | refuse pinned job version | node still validates chain |
| fewer than `q` matching complete-result commitments | job expires/remains pending | old state and base chain continue |
| invalid recursive proof | evidence rejected | no state change |
| fewer than `q` DA receipts | no Target activation | old state continues |
| custody falls below `k` | repair from holders/snapshot; halt affected generation if impossible | never claim unavailable data is valid |
| anti-equivocation journal unavailable | signing disabled | consensus key unaffected |
| common semantic bug | quorum/proof can reproduce the bug | independent spec tests, implementations and audits are mandatory |

Health is split. `consensus_ready`, `execution_ready`, `projection_ready` and
`ocomp_ready` are separately observable. OCOMP metrics include finalized cursor,
job age/state, reservations, unit retries, deterministic mismatches, proof time,
DA fragments, custody challenges, disk watermarks and signer refusals.

## 13. Trust and key boundaries

| Component | May read | May write | May sign | If compromised |
|---|---|---|---|---|
| node | canonical chain and fixed OCOMP digests | chain/node state | consensus key; separate OCOMP key | one Byzantine validator |
| snapshot exporter | one immutable checkpoint | source CAS/export journal | never | may omit/corrupt output; root rebuild/custody gate rejects it |
| supervisor | finalized job specs, source/artifact bytes | own journal/artifacts | never | bounded profile loses one validator's correctness; target invalid proofs still reject |
| launch broker | admitted plan/lease records | fixed worker lifecycle only | never | can deny work or fill only the aggregate OCOMP quota |
| worker/prover | job-scoped immutable inputs | private scratch/CAS object | never | bad artifact/proof is rejected; may waste bounded resources |
| body/CAS store | opaque chunks | stored chunks | never | may omit/corrupt/withhold, not forge roots |
| service manager | process configuration | lifecycle/cgroups | never | host administrative compromise |

The supervisor has no direct MDBX writer, Mongo writer, validator key directory,
arbitrary node RPC, shell callback or code-download capability. A remote worker
receives a job-scoped capability for exact object digests only.

Process isolation is fault containment, not a new Byzantine identity. Ten
processes controlled by one validator still constitute one validator failure
domain.

### 13.1 Nod, Gem and later primitives

The reusable part is the operational kernel in section 0. It does not know how
to enumerate Tribute, qualify a Gem, issue a Nod, prove domain completeness or
mutate domain state. Those remain program-owned.

| Layer | Stable rule |
|---|---|
| architecture seam | kernel owns lifecycle/process/evidence mechanics; the program owns semantics and effects |
| PoC source seam | concrete internal kernel modules call the concrete Lysis V1 protocol |
| PoC wire seam | all V1 intent/unit/result/activation objects remain Lysis-specific |
| future wire seam | a new fork may introduce new typed object kinds and, if two real programs justify it, a closed registry/envelope |

A second entry is not considered a program merely because it has another tag or
adapter. It qualifies only when it has all of:

1. an independent domain postcondition and fork-pinned typed intent/result;
2. authenticated complete input enumeration and canonical ordering;
3. its own admission bounds, preconditions/capacity claims and conflict rules;
4. deterministic execution plus domain-specific result verification;
5. a private typed activation capability and owner-controlled effect APIs;
6. conservation/witness/receipt rules and a complete
   finality-to-activation/expiry/replay path;
7. cross-program contention tests against Lysis.

Gem qualification is the preferred destructive design test because its
owner/global/bin indexes and state transition differ from Lysis. It is not a PoC
deliverable and no Gem schema is frozen here. Before it can be registered, its
authoritative indexes, completeness proof, ordering, preconditions/capacity,
apply owner and recovery rules require a separate domain ADR.

Only after such a second program exists may the two concrete protocols be
compared and their proven intersection extracted into `ProgramId`,
`ProgramSpec`, a closed registry or common envelopes. A future program uses new
fork-registered typed object kinds and its own signature domain; it never
reinterprets Lysis V1 bytes. No public generic `TaskAdapter`, opaque task bytes,
runtime-loaded handler, arbitrary action stream, storage-key write set or generic
activation capability is allowed.

## 14. Profiles and rollout

| Profile | Computation | Input completeness | Apply/availability | Status |
|---|---|---|---|---|
| `PoC` | `q=3/4` independent full executions of one multi-shard fixture; arbitrary `N` is count/root-addressed | full CE fold of one pinned checkpoint | constant-size result commitment + atomic root activation; three-domain chunk retention | selected devnet fork; not implemented |
| `BoundedMVP` | same q mechanism with measured per-interface and concurrency caps | CE fold + production local resource admission | same result/root contract + production retention/restore | disabled until hardening gates pass |
| `TargetLarge` | recursive proof; validators verify | counted ranges + source snapshot certificate | proof/DA + witness/delta state + claims | disabled |
| `1B` | TargetLarge exact declared shape | same | same plus O(1) activate/retire and cold recovery/repair | unclaimed |

### 14.1 What does and does not change from PoC to MVP

The PoC devnet state is disposable; it is not promoted into a production
network. “Evolutionary transition” means the implementation and protocol seams
survive, not that PoC chain history becomes production history.

These core facts are identical in PoC and BoundedMVP:

```text
finalized JobIntent
-> authenticated snapshot
-> deterministic off-chain execute_lysis
-> q independent validator-domain signatures
-> one typed activateLysis transaction
-> private CertifiedLysisActivation capability
-> atomic Nod/contributor/Tribute/Desis/Promis/Metadosis effects
```

MVP replaces local-file keys, one-job scheduling, simple process templates and
demo retention with production implementations. It does not replace the intent,
signed result meaning, activation authority or domain-effect contract. A change
to any of those is a new protocol bundle, not “MVP hardening”.

### 14.2 One immutable protocol bundle

Every consensus-visible or persisted OCOMP object is interpreted by one
fork-registered bundle:

```text
ProtocolBundleV1 {
  protocol_version, fork_id,
  intent_codec, finalized_intent_proof_codec,
  tribute_body_codec, fidelity_opening_codec, oracle_opening_codec,
  result_codec, action_codec, activation_codec, evidence_codec,
  request_semantics_version, lysis_program_semantics_hash,
  planner_spec_version, reducer_spec_version,
  activation_apply_semantics_hash,
  effect_contract_registry_hash,
  object_codec_registry_hash,
  correctness_profile_id, capacity_profile_id,
  result_signature_scheme_and_domain,
  finality_verifier_and_vote_domain_id,
  consensus_committee_history_schema_version,
  ocomp_committee_schema_version,
  proof_system_and_verifier_key_id_or_none,
  da_codec_and_binding_verifier_id_or_none,
  anti_equivocation_journal_schema_hash,
  mode_pause_revocation_semantics_hash,
  upgrade_fsm_semantics_hash,
  release_requirement_catalog_sequence,
  release_requirement_catalog_hash,
  release_requirement_catalog_parent_hash,
  release_gate_authority_envelope_hash,
  release_approval_policy_hash,
  release_validator_command_artifact_hash,
  consensus_state_schema_version,
  migration_manifest_hash,
  required_upgrade_handler_set_hash
}

ProtocolBundleHash =
  H("OUTBE_OCOMP_PROTOCOL_BUNDLE_V1", canonical(ProtocolBundleV1))
```

For every PoC or BoundedMVP job, the exact `LysisV1` wire/program binding is
derived from its pinned `ProtocolBundleV1`: intent/action/result codecs, Lysis
semantics, planner, reducer, evidence domain, apply semantics and required
handlers are already fixed there. Adding a one-entry `ProgramId` or registry
would duplicate that authority. A later multi-program design must arrive in a
new fork-pinned bundle/object codec and leave these V1 bytes historical and
unchanged.

`required_upgrade_handler_set_hash` commits the canonical ordered handler IDs,
semantic versions, owned schema transitions and work bounds—not compiler bytes.
Each release manifest separately proves which implementation artifacts provide
that semantic set. This permits independent implementations while preventing a
node from silently treating “no handler” as the same protocol.

`activation_apply_semantics_hash` commits the exact owner-call order, receipt
schemas/equations, precondition/capacity and conflict rules, logical-time mapping, error
taxonomy and terminal state/event effects. `effect_contract_registry_hash`
maps every effect owner to that transition and its private receipt constructor.
`object_codec_registry_hash` is the Merkle root of a closed canonical map:

```text
ObjectKind ->
  {codec_version, signature/hash_domain_or_none, byte/count/crypto_caps,
   persistence_scope, current_verifier_id, historical_decoder_id}
```

It contains every consensus or durable OCOMP object, including job/activation
records, finality/result/readiness/custody certificates, consensus and OCOMP
committee snapshots, manifests, deltas, pause/revocation/upgrade plans, receipts,
checkpoint/export journals and the sign-once journal. An unlisted object kind is
not persistable or consensus-valid. Registry expansion changes
`ProtocolBundleHash`; historical decoders remain reachable by their recorded ID.

The release-authority fields are copies of the independently governed
`ReleaseGateAuthorityEnvelopeV1`/core active for this network/fork. Equality is checked when
the bundle is registered, scheduled, armed and activated; a gate cannot choose
its own catalog, approval threshold or validator program.

`JobIntentV1`, `ActivationPayloadV1`, every certificate, snapshot, delta and
custody statement pins `ProtocolBundleHash`. Redundant version/profile fields
must equal that registry entry byte for byte. Correctness and capacity profiles
are immutable versioned consensus records, not strings negotiated by a
supervisor or local configuration.

Unknown bundle, object version, mandatory field or tag; a duplicate,
non-canonical or trailing field; and an over-limit byte/count value all reject.
A release may stop **producing** an old bundle after drain. It must retain every
historical decoder/verifier required by supported genesis replay and accepted
checkpoints.

The bundle pins computation and activation semantics plus test vectors, not one
vendor binary.
Different independently built implementations may participate if their signed
release manifest declares conformance to the same semantics and they pass the
same vectors. This allows implementation diversity instead of making one worker
image a consensus identity.

There is one bounded `ResultChunkV1` codec and one constant-size result
preimage, both defined normatively in section 8.2. This section does not
redefine them.

Every validator domain's separate compute process decodes the complete chunk
catalog and recomputes every root, count, conservation total and event summary
before asking its node to attest. The node's closed attestation gate verifies
only the constant-size result/job binding and sign-once subject; it never scans
bulk chunks. The activation verifier reconstructs the committed
`ActivationPayloadV1` and verifies its certified old-root-to-new-root
transition. No other tuple under the same domain is valid. Golden vectors cover
decode -> finality/certificate verification -> every typed effect transition
-> terminal state root and events; computation-only equality is insufficient.

### 14.3 Preparation, arming and activation

A supported-network rollout is a protocol FSM, not “install around height H”:

```text
DISABLED(old_bundle)
  -> PREPARING(migration_id, cursor, target_bundle)
  -> PREPARED(completion_root, target_bundle)
  -> ARMING(plan_id, readiness_cutoff, activation_height=H)
  -> ARMED(readiness_certificate, activation_height=H)
  -> ACTIVE_DUAL(new writes=new_bundle; old pinned jobs still valid)
  -> DRAINED(old_bundle)
  -> PRODUCER_RETIRED(old_bundle; historical readers retained)

PREPARING | PREPARED | ARMING | ARMED
  -> CANCELED_ENABLEMENT(reason, invalidated_completion_root)  only before H
ARMING @ cutoff without valid readiness
  -> CANCELED_NOT_ARMED; activation schedule removed
ARMED @/before H with changed authoritative snapshot/root
  -> CANCELED_NOT_ARMED; activation schedule removed
ARMED @ H
  -> ACTIVE_DUAL | ACTIVATION_FAILED(old bundle remains active)
ACTIVE_DUAL and later -> FORWARD_FIX_PENDING    never downgrade
```

PoC starts from a fresh devnet genesis already prepared for its bundle.
BoundedMVP on a network with existing state uses two governed stages:

1. a dormant preparation version adds versioned OCOMP records/indexes and runs
   bounded backfill/validation while synchronous legacy Lysis remains
   authoritative and OCOMP admission stays disabled;
2. a later enablement version switches the non-empty Metadosis branch to
   `JobIntent` only after preparation and readiness are complete.

An exhaustive Fidelity `H_max` validation, due-index construction or schema
backfill never runs as one unbounded activation handler. `PREPARING` advances a
fork-metered monotonic cursor per block, persists its source/target roots and is
restart/replay idempotent. Every relevant mutation dual-maintains or rejects
against the preparing index so the completion root cannot become stale.
`PREPARED` is written only after a final exact reconciliation and enables no
job by itself.

Canceling enablement never rolls back the already finalized preparation
protocol version. It marks the old completion root unusable, invalidates every
readiness statement over it and keeps the dormant namespace inaccessible to
active semantics. The preparation version either continues dual-maintaining the
dormant indexes, or a separately metered cursor detaches and garbage-collects
them. A later attempt always uses a new `migration_id`, runs an exact
reconciliation/full rebuild as required and produces a new completion root; it
may not reuse the canceled root. A defect in the finalized preparation protocol
itself is repaired only by a higher forward version.

Migration never fabricates an in-flight job. Existing `COMPLETED`/`FAILED`
records keep their terminal meaning; existing `READY` records receive exactly
one new due-index entry; no legacy record becomes `OFFCHAIN_PENDING`. The new
status decoder is activated only with the enablement bundle. Preparation
records live under a separate versioned namespace, so an old binary before `H`
cannot decode a dormant tag as an active Metadosis state.

The scheduled plan is not only `{version,height,info}`. Its hash dependency is
an acyclic construction:

```text
ProtocolUpgradePlanCoreV2 {
  chain_id, genesis_hash, current_fork_id, plan_id, readiness_generation,
  target_protocol_version,
  activation_height,
  readiness_cutoff_height,
  protocol_bundle_hash,
  migration_manifest_hash,
  required_upgrade_handler_set_hash,
  activation_consensus_committee_snapshot_hash,
  activation_consensus_quorum_rule_hash,
  target_ocomp_committee_snapshot_hash,
  target_result_q,
  required_live_job_bundle_set_hash,
  required_replay_dependency_set_hash,
  preparation_completion_root
}

PlanCoreHash(core) =
  H("OUTBE_OCOMP_UPGRADE_PLAN_CORE_V2",
    canonical(core))

ProtocolUpgradeEnvelopeV2 {
  canonical_plan_core,
  plan_core_hash,
  bounded_mvp_release_gate_envelope_hash
}

UpgradeEnvelopeHash(envelope) =
  H("OUTBE_OCOMP_UPGRADE_ENVELOPE_V2",
    canonical(envelope))
```

This closes a current generic Update hazard: activation with an empty handler
registry is valid today. For an OCOMP plan, missing, extra or differently hashed
required handlers fail startup/readiness before arming and fail closed at
activation. A binary installed before `H` may read/preflight dormant schema but
may not lazily mutate active consensus state under the new interpretation.

Scheduling, preparation, readiness and activation all enforce one equality
contract:

```text
plan_core.target_protocol_version
  == bundle.protocol_version
plan_core.migration_manifest_hash
  == bundle.migration_manifest_hash
plan_core.required_upgrade_handler_set_hash
  == bundle.required_upgrade_handler_set_hash
envelope.plan_core_hash
  == PlanCoreHash(envelope.canonical_plan_core)
envelope.bounded_mvp_release_gate_envelope_hash
  == GateEnvelopeHash(release_gate_envelope)
release_gate_envelope.canonical_core.protocol_upgrade_plan_core_hash
  == envelope.plan_core_hash
release_gate_envelope.canonical_core.protocol_bundle_hash
  == envelope.canonical_plan_core.protocol_bundle_hash
release_gate_envelope.canonical_core.(chain_id, genesis_hash)
  == envelope.canonical_plan_core.(chain_id, genesis_hash)
```

`PlanCoreHash(core)` means only the domain-separated function defined above;
raw `H(canonical(core))` is never valid. `GateEnvelopeHash` is the exact
domain-separated function in section 14.7.

Construction order is bytes of `ProtocolUpgradePlanCoreV2` -> `PlanCoreHash` ->
bytes/hash of `BoundedMVPReleaseGateCoreV1` -> gate approval digest/signatures ->
`GateEnvelopeHash` -> bytes/hash of `ProtocolUpgradeEnvelopeV2`. The plan core
contains no gate hash, gate approvals sign only the gate core binding, and the
gate contains no upgrade-envelope hash, so there is no mutual or self-signing
fixed point and no placeholder pass.

The referenced migration manifest is a canonical ordered list with unique
handler IDs, declared dependencies/topological order, source/target schema
versions and roots, per-step cursor/work caps and terminal root equations.
Same members in a different order, duplicates, missing dependencies or any
plan/bundle mismatch reject at every gate.

Readiness is a vector of individually signed statements, not one aggregate over
different artifact bytes:

```text
OcompReadinessStatementCoreV1 {
  common: {
    plan_id, readiness_generation, plan_core_hash, upgrade_envelope_hash,
    protocol_bundle_hash, preparation_completion_root,
    activation_consensus_committee_snapshot_hash,
    activation_consensus_quorum_rule_hash,
    target_ocomp_committee_snapshot_hash, target_result_q,
    required_live_job_bundle_set_hash,
    required_replay_dependency_set_hash,
    bounded_mvp_release_gate_envelope_hash,
    valid_from_height, valid_through_activation_height
  },
  consensus_validator_id,
  signer_local_release_artifact_manifest_hash,
  signer_local_capacity_evidence_hash,
  provided_capability_bits
}

ReadinessStatementDigest(core) =
  H("OUTBE_OCOMP_READINESS_STATEMENT_V1", canonical(core))

SignedOcompReadinessStatementV1 {
  canonical_core,
  readiness_statement_digest,
  consensus_identity_signature,
  ocomp_key_possession_signature_or_none
}

OcompReadinessCertificateV1 {
  exact_common_fields,
  canonical_ordered_signed_statements
}
```

Both signatures are over `ReadinessStatementDigest(canonical_core)`.
The embedded digest must recompute exactly; fields are never stripped from a
signed object by convention. Statements are ordered by consensus validator ID,
contain at most one entry per consensus identity and are verified under the
pinned activation-consensus snapshot.

The OCOMP identity is never caller-asserted. The verifier performs:

```text
ocomp_member =
  target_ocomp_snapshot.lookup_by_validator_id(core.consensus_validator_id)

consensus_identity_signature
  verifies under that consensus identity in activation_consensus_snapshot

provided_capability_bits contains ATTEST
  <=> ocomp_key_possession_signature verifies under ocomp_member.ocomp_key

ocomp_key_possession_signature is present
  => ocomp_member exists
```

The two snapshots must bind the same canonical validator identity; an immutable
cross-identity delegation would require a separately registered, bundle-pinned
mapping and is not supported by this version. A cyclic permutation of OCOMP
keys/identities, absent entry, `None` with `ATTEST`, duplicate derived identity
or stale snapshot rejects. The result-readiness predicate counts only these
derived, dual-signed `ATTEST` mappings. A validator whose registered OCOMP key
is temporarily unavailable may still submit consensus-only readiness without
`ATTEST` or an OCOMP signature. An aggregate is allowed only for byte-identical
digests; signer-local artifact fields normally require individual signatures.

The common fields must equal the scheduled objects exactly:

```text
common.plan_core_hash == PlanCoreHash(scheduled_envelope.canonical_plan_core)
common.upgrade_envelope_hash == UpgradeEnvelopeHash(scheduled_envelope)
common.bounded_mvp_release_gate_envelope_hash
  == scheduled_envelope.bounded_mvp_release_gate_envelope_hash
  == GateEnvelopeHash(release_gate_envelope)
common.protocol_bundle_hash
  == scheduled_envelope.canonical_plan_core.protocol_bundle_hash
```

The release gate lists every permitted independently built artifact manifest;
therefore signer-local artifact hashes may differ while the common protocol
meaning is identical. Duplicate identities/statements, an artifact outside that
set or a statement with insufficient capabilities rejects.

`ARMED` requires **both** typed predicates, never a numeric `max` across
different domains:

```text
sum consensus weight of ready identities in the pinned activation snapshot
  >= that snapshot's consensus activation quorum

count distinct ready OCOMP identities with valid keys in the pinned target
result snapshot and full execute/verify/attest/apply capability
  >= target_result_q
```

The plan pins the exact activation consensus snapshot, its weights/quorum rule,
the target OCOMP snapshot and their validity through `H`. A jail, exit,
membership/weight change, key-status change or quorum-rule change before or at
`H` makes the certificate stale. The old-version begin-zone then
deterministically enters `CANCELED_NOT_ARMED` and removes the activation schedule;
it never waits for a governance cancellation transaction.

At the cutoff, the same consensus-owned transition either records `ARMED` from a
valid certificate or records `CANCELED_NOT_ARMED`. Thus `q-1`, a censored
operator or an absent governance transaction cannot leave a plan scheduled for
`H`; only `ARMED` can reach the activation dispatcher. Readiness limits
accidental liveness failure; it is not proof that a Byzantine operator actually
provisioned its machine.

All fallible schema scans, decoder checks, handler-manifest validation and root
reconciliation finish before arming. At `H`, the old-version dispatcher first
rechecks the exact plan, snapshots, release gate, prepared root and live/replay
sets. A mismatch commits `ACTIVATION_FAILED(code, observed_roots)`, removes the
schedule and leaves the old bundle active without invoking target semantics.
Otherwise it runs the bounded target handler in a nested checkpoint:

- `Applied` commits the new schema/bundle markers and active version atomically;
- a specified deterministic handler error rolls back the inner checkpoint,
  commits `ACTIVATION_FAILED` under the old schema and keeps finalizing with the
  old bundle;
- a missing binary/artifact, panic or local storage corruption is a local node
  readiness/fatal error, never a consensus alternative.

The handler is forbidden from external I/O, unbounded scans or unspecified
errors. Installing the new binary early does not activate it early. An old
binary cannot validate/propose/sign after a successful activation at `H`.
After a finalized new-bundle block, recovery is only a higher forward-fix;
`ACTIVATION_FAILED` is not a downgrade because the new version never became
active.

### 14.4 Mixed versions, in-flight jobs and drain

The writer rule reads the committed active marker after begin-zone phase 3; a
scheduled height alone is never a semantic fork:

```text
request creation always pins current ActiveProtocolBundleHash

upgrade transition at H == Applied
  -> ActiveProtocolBundleHash = target before terminal READY inspection
  -> requests created later in H use target

CANCELED_NOT_ARMED | ACTIVATION_FAILED at/before H
  -> ActiveProtocolBundleHash remains old
  -> requests in H, H+1 and later use old until another plan Applies
```

An old nonterminal job pins its old bundle, deadline, result committee snapshot
and OCOMP key epoch. After `H` it executes, signs, verifies, activates, expires
or conflicts strictly under those old rules. It is never re-encoded,
reinterpreted, converted or relabelled as a new job. If it expires after `H`,
the new READY nonce may later create a new-bundle intent.

The bounded live-job index maintains a per-bundle nonterminal count and
`LiveJobBundleSetRoot`. Before arming, `required_live_job_bundle_set_hash`
commits a support superset containing every bundle currently nonterminal plus
the current writer bundle, because it may create intents through `H-1`.
Readiness for each member means the full planner, executor, verifier, attester,
typed apply path, capacity and required old private-key service are available;
a historical decoder alone is insufficient. At `H`, the actual live set must be
a subset of the committed support set.

`required_replay_dependency_set_hash` is separate. It commits all historical
decoders, finality/proof verifiers and parameters, public committee/key
snapshots, terminal evidence schemas and checkpoint dependencies needed for
genesis/checkpoint replay and audit. A release either supports the arbitrary
exact live set (for example `P`, `N` and `N+1`) or cannot arm until unsupported
older bundles are `DRAINED`; it may not strand a live job merely because it is
more than one release old.

`DRAINED(old_bundle)` requires all of the following:

- no nonterminal old-bundle job;
- no old expiry/reservation entry;
- no unreleased source/output pin or export;
- no pending custody handover or repair obligation;
- no old private key still required to sign an eligible job.

Drain allows old planners/producers and private signing keys to retire. It does
not allow removal of historical decoders, proof/verifier parameters, terminal
evidence or public committee/key snapshots required by replay, bootstrap or
audit. Checkpoints state their required bundle/decoder/key set and fail closed
when a node does not have it.

Before `H`, governance may cancel the scheduled update and preparation state
remains dormant under the `CANCELED_ENABLEMENT` rules in section 14.3. A
specified deterministic failure at `H` produces `ACTIVATION_FAILED` and a new
plan/migration ID is required; it is not retried automatically every block.
Once a block under the new bundle finalizes, protocol/schema downgrade is
forbidden. Recovery is a monotonically higher governed forward-fix. A
rolled-back binary may serve read-only data only if it can decode the active
chain; it cannot participate in consensus or OCOMP signing.

### 14.5 Control-plane compatibility is not consensus negotiation

The local connection performs an explicit capability exchange:

```text
HelloV1 {
  chain_id, genesis_hash, boot_id, session_nonce,
  supported_control_versions,
  supported_protocol_bundle_hashes,
  capability_bits,
  receive_byte/count_limits
}

HelloAckV1 {
  selected_control_version,
  common_protocol_bundle_hashes,
  granted_capability_bits,
  peer_identity, session_generation
}
```

Capability bits distinguish job read, snapshot export, execute, verify, attest,
custody and administration. Capability says that a component implements a
contract; readiness says that its current resources/dependencies permit use.
Every privileged request rechecks the finalized job's pinned bundle and the
session generation. No common bundle means “refuse this job”, not “reinterpret
it” and not “stop consensus”. Exact method/structure versions follow the same
principle as Ethereum Engine API capability exchange; the transport adapter may
be UDS or mTLS without changing the messages.

Minimum supported skew:

| Pair/state | Supported behavior |
|---|---|
| new node + old supervisor before `H` | old-bundle jobs only when both advertise that exact bundle |
| new node + old supervisor after `H` | old pinned jobs only; new-bundle jobs refused by the supervisor |
| old node before `H` | old consensus and OCOMP bundle only |
| old node at/after `H` | not consensus-ready and cannot sign/propose; fail fast |
| new supervisor + old node before `H` | old bundle only; no early schema or new intent |
| any pair with no common job bundle | OCOMP unavailable for that job; base consensus behavior follows node compatibility |

Every release supports at least current plus previous **nonterminal job**
bundle, but this is a minimum, not a cap. Every older bundle still present in
the committed live-job set retains full operational support until drain.
Historical read-only decoders live for the complete supported replay window.

### 14.6 Governed pause and incident recovery

Each bundle has a consensus-owned mode and generation:

```text
DISABLED -> ENABLED
ENABLED -> DRAIN_ONLY
ENABLED | DRAIN_ONLY
  -> PAUSING(mode_generation, affected_set_root, cursor)
  -> PAUSED(cancel_root, canceled_count)
DRAIN_ONLY -> DISABLED_AFTER_DRAIN
PAUSED -> ENABLED only through a new governed resume plan
```

Only the authority/quorum/delay defined by `ADR-S-GOV-003` may schedule:

```text
OcompModePlanCoreV1 {
  chain_id, genesis_hash, fork_id,
  plan_id, protocol_bundle_hash, expected_mode_generation,
  transition: DRAIN | PAUSE | RESUME | DISABLE_AFTER_DRAIN,
  reason_code, effective_height,
  affected_job_selector: ALL_PROFILE | EXACT_BUNDLE | EXACT_OCOMP_SNAPSHOT,
  affected_ocomp_snapshot_hashes,
  required_resume_bundle_hash_or_none
}

ModePlanApprovalDigest(core) =
  H("OUTBE_OCOMP_MODE_PLAN_APPROVAL_V1", canonical(core))

ScheduledOcompModePlanV1 {
  canonical_core,
  mode_plan_approval_digest,
  authorization_proof
}
```

The governance proof authorizes exactly `ModePlanApprovalDigest`; the scheduled
record recomputes it and applies the canonical duplicate/quorum/delay rules from
the active governance policy. It never signs the structure containing its own
proof.

The request/terminal paths continuously maintain a bounded ordered
`(profile,bundle,snapshot,IntentId)` live-job index with authenticated root and
counts for each closed selector above. No plan may upload executable/arbitrary
predicate logic.

At the begin-zone of `effective_height`, before expiry and ordinary
transactions, `PAUSE` atomically increments the mode generation and records the
selected pre-maintained root/count plus cursor; it does not scan live jobs or
construct a set in that block. It then enters `PAUSING`. That O(1) first write is
an immediate admission and activation barrier for every matching job; the
cursor is only cleanup, not the safety barrier. Each block then terminalizes at
most `MAX_OCOMP_PAUSE_CANCELS_PER_BLOCK` matching jobs in ascending `IntentId`,
using the same idempotent `CANCELED` helper and receipt as expiry/conflict
cleanup. The persisted cursor, remaining count and rolling cancel root must
match the live/expiry/reservation indexes after every item. Restart merely
resumes the next key.

If a pause and a deadline share height `H`, the pause barrier is installed
first. An affected due job is therefore terminalized as `CANCELED`, never
`EXPIRED`; whichever of the cancellation cursor or expiry scan encounters it
first invokes the same helper, and the other observes a terminal record. If a
pause and activation share `H`, the begin-zone barrier wins and the ordinary
activation rejects. `PAUSED` is reachable only when the affected remaining count
is zero and the exact cancel root is final. `RESUME` requires `PAUSED`, zero
affected live/reservation/expiry entries, a non-stale release gate, a safe OCOMP
committee snapshot and a new effective height/mode generation. Re-enable during
partial cancellation is invalid.

`DRAIN_ONLY` creates no new intents but permits already pinned jobs to terminate.
Requeued READY days remain deterministically deferred with the paused reason;
they do not hot-loop or fall back to Lysis. Local nodes may stop signing
immediately, but local health cannot change consensus mode.

OCOMP key status is also versioned consensus history:

```text
OcompKeyStatusV1 {
  ocomp_snapshot_hash, validator_id, key_id,
  status: ACTIVE | RETIRED_AFTER_DRAIN | COMPROMISE_REVOKED,
  effective_height, reason_code, authorizing_plan_id
}
```

Orderly rotation is non-retroactive: old jobs may use their pinned snapshot and
old private keys until drain, while new jobs use the successor. A compromise
revocation is different. At its effective begin-zone it invalidates the complete
affected result-committee snapshot for every activation not already finalized,
installs the same immediate `PAUSING` barrier and cancels/requeues its
nonterminal jobs. It never lowers `q` or silently substitutes today's committee;
new attempts are admitted only after resume under a safe successor snapshot.
An activation finalized before the revocation remains historically valid, and
replay checks the key-status record as of its activation height. One finalized
later is impossible. Public keys and status history are retained even after old
private key destruction.

The key operator may locally quarantine a suspected key before governance
finalizes revocation, sacrificing only OCOMP liveness. The consensus revocation
authority and emergency delay are exactly the governance values pinned by the
bundle; a supervisor or HSM cannot invent a chain-visible revocation.

Pause limits damage from a discovered common semantic bug; q signatures do not
protect against such a bug. It cannot undo an already finalized activation.
Repair of active state requires a higher protocol version with a deterministic,
proof-tested forward migration. The incident runbook preserves evidence,
revokes/rotates affected keys or artifacts, proves the repair against the
pre-incident root and never edits checkpoints manually.

An equal-bundle re-enable is allowed only when governance records that the pause
was operational and no semantic/output defect existed. A semantic defect always
requires a higher bundle and forward repair.

### 14.7 What “MVP complete” means

`BoundedMVP` is a supported, measured **bounded** profile. It makes no billion-
as “fast”, “hardened” or “production” are not acceptance criteria. The release
decision is the following versioned contract:

```text
BoundedMVPReleaseGateCoreV1 {
  chain_id, genesis_hash, protocol_bundle_hash,
  source_commit, protocol_upgrade_plan_core_hash,
  release_manifest_set_hash,
  network_manifest_hash,
  release_gate_authority_envelope_hash,
  requirement_catalog_sequence,
  requirement_catalog_parent_hash,
  requirement_catalog_hash,
  verification_ledger_hash,
  evidence_manifest_hash,
  valid_from_height, expires_at_height,
  validator_command_artifact_hash,
  approval_policy_hash
}

GateCoreHash(core) =
  H("OUTBE_OCOMP_BOUNDED_MVP_RELEASE_GATE_CORE_V1", canonical(core))

GateApprovalDigest(authority_envelope_hash, gate_core_hash) =
  H("OUTBE_OCOMP_BOUNDED_MVP_GATE_APPROVAL_V1",
    authority_envelope_hash, gate_core_hash)

GateApprovalV1 {
  approver_identity, gate_approval_digest, signature
}

BoundedMVPReleaseGateEnvelopeV1 {
  canonical_core,
  gate_core_hash,
  gate_approval_digest,
  canonical_ordered_approvals
}

GateEnvelopeHash(envelope) =
  H("OUTBE_OCOMP_BOUNDED_MVP_RELEASE_GATE_ENVELOPE_V1",
    canonical(envelope))

ReleaseGateAuthorityCoreV1 {
  chain_id, genesis_hash, fork_id,
  authority_sequence, parent_authority_envelope_hash,
  requirement_catalog_sequence,
  requirement_catalog_hash,
  requirement_catalog_parent_hash,
  immutable_mandatory_requirement_ids_root,
  approval_policy_hash,
  validator_command_artifact_hash,
  effective_height
}

AuthorityCoreHash(core) =
  H("OUTBE_OCOMP_RELEASE_GATE_AUTHORITY_CORE_V1", canonical(core))

AuthorityApprovalDigest(parent_authority_envelope_hash, authority_core_hash) =
  H("OUTBE_OCOMP_RELEASE_GATE_AUTHORITY_APPROVAL_V1",
    parent_authority_envelope_hash, authority_core_hash)

ReleaseGateAuthorityEnvelopeV1 {
  canonical_core,
  authority_core_hash,
  authority_approval_digest,
  governance_authorization_proof
}

AuthorityEnvelopeHash(envelope) =
  H("OUTBE_OCOMP_RELEASE_GATE_AUTHORITY_ENVELOPE_V1",
    canonical(envelope))

RequirementCatalogV1 {
  sequence, parent_catalog_hash,
  canonical_entries {
    stable_requirement_id,
    owner_ref {
      adr_or_pfs_id, canonical_repository_path,
      accepted_registry_entry_hash,
      accepted_document_hash,
      normative_section_hashes
    },
    mandatory, subject_kind, measurement_schema,
    unit, threshold_rule, expiry_rule
  }
}

NetworkManifestV1 {
  network_id, genesis_hash, minimum_hardware_class,
  validator_count_and_weights, result_n_and_q,
  every consensus/RPC/txpool/P2P/block/gas/work/job/byte/count cap,
  cgroup/namespace/volume budgets,
  SLO/RPO/RTO values with units, percentile and measurement window
}

VerificationLedgerEntryCoreV1 {
  stable_requirement_id,
  status: PASS | GAP | CONTRADICTED | EXPIRED |
          SKIPPED | QUARANTINED | ADVISORY,
  exact_subject_hashes, evidence_artifact_hashes,
  measured_value, unit, threshold, sample/window,
  produced_at_height, expires_at_height,
  verifier_identity
}

VerificationLedgerEntryDigest(core) =
  H("OUTBE_OCOMP_VERIFICATION_LEDGER_ENTRY_V1", canonical(core))

SignedVerificationLedgerEntryV1 {
  canonical_core,
  entry_digest,
  verifier_signature
}

VerificationLedgerHash(entries) =
  H("OUTBE_OCOMP_VERIFICATION_LEDGER_V1",
    canonical_ordered(SignedVerificationLedgerEntryV1[]))
```

Gate approvals sign only `GateApprovalDigest`, never a structure containing
their signatures. The envelope verifies exact core/digest recomputation, then
sorts approvals by authority-defined approver identity, rejects duplicate or
unknown identities and evaluates weights/count/delay under the approval policy
in the authenticated authority core. Changing any network, artifact, evidence,
catalog, plan, validity or policy field changes `GateCoreHash` and invalidates
every approval.

Likewise, a ledger verifier signs only
`VerificationLedgerEntryDigest(canonical_core)`. The envelope recomputes the
digest, resolves `verifier_identity` through the authority-pinned verification
policy and rejects an unknown key, duplicate stable requirement/subject,
non-canonical evidence order or signature replay onto any changed measurement,
status, subject, evidence or expiry. The gate's `verification_ledger_hash` must
equal `VerificationLedgerHash` over the complete canonical mandatory/advisory
entry set; it never hashes an implicitly stripped signature field.

Authority sequence zero is pinned directly by genesis. For a successor, the
governance proof authorizes only `AuthorityApprovalDigest` under the parent
authority's governance policy; the proposed successor policy cannot authorize
itself. The envelope recomputes core/digest, rejects duplicate/unknown
governance signers and verifies the finalized proposal/tally or canonical
signature set required by `ADR-S-GOV-002/003`. Changing sequence, parent,
catalog, policy, command or effective height invalidates the proof. Neither
authorization path uses “serialize the full signed object and strip signatures”
as an implicit convention.

Before approvals/policy evaluation, validators enforce:

```text
authority_envelope.authority_core_hash
  == AuthorityCoreHash(authority_envelope.canonical_core)
authority_envelope.authority_approval_digest
  == AuthorityApprovalDigest(
       authority_envelope.canonical_core.parent_authority_envelope_hash,
       authority_envelope.authority_core_hash)

gate_envelope.gate_core_hash
  == GateCoreHash(gate_envelope.canonical_core)
gate_envelope.canonical_core.release_gate_authority_envelope_hash
  == AuthorityEnvelopeHash(authority_envelope)
gate_envelope.gate_approval_digest
  == GateApprovalDigest(
       AuthorityEnvelopeHash(authority_envelope),
       gate_envelope.gate_core_hash)

gate_envelope.canonical_core.(
  requirement_catalog_sequence,
  requirement_catalog_hash,
  requirement_catalog_parent_hash,
  approval_policy_hash,
  validator_command_artifact_hash)
  == authority_envelope.canonical_core.(
       requirement_catalog_sequence,
       requirement_catalog_hash,
       requirement_catalog_parent_hash,
       approval_policy_hash,
       validator_command_artifact_hash)
gate_envelope.canonical_core.protocol_bundle_hash
  == registered_bundle_hash
registered bundle release-authority fields
  == authority envelope hash and authority-core fields
```

Every name on the right is the domain-separated function shown above. Raw hash,
implicit field omission and a second serialization are invalid.

`ReleaseManifestSetV1` binds every permitted independently built node,
supervisor, exporter, worker, broker, policy/template and migration artifact to
source, reproducible build/provenance and SBOM. `EvidenceManifestV1` binds the
immutable logs, reports and raw measurements. `RequirementCatalogV1` is a
versioned closed list. Its authority root is committed independently by genesis
or an accepted governed fork before a release gate may reference it. Sequence
must increase by one, `parent_catalog_hash` must name the active catalog, and
every previously mandatory stable ID must remain byte-identical or move only by
a catalog-defined machine-checkable stricter threshold rule. Deletion,
mandatory-to-optional change, unit/subject weakening or an unknown comparison is
invalid. Retiring a requirement requires a new accepted protocol profile/fork
whose authority explicitly makes the old profile unavailable; a normal
BoundedMVP release cannot shrink its catalog.

An owner ID alone is not evidence. The release validator loads the exact
repository revision bound by `source_commit`, resolves each canonical path and
ADR/PFS registry entry, requires status `Accepted`, hashes the canonical complete
document and every named normative section, and compares all values to
`owner_ref`. Changing text under the same ID, moving authority to an
unregistered file or accepting an unamended owner therefore produces `GAP`.

`ReleaseGateAuthorityEnvelopeV1` and its core are owned by the accepted
OCOMP/governance ADRs, have a
monotonic parent/sequence and pin the expected catalog root, immutable mandatory
ID root, approval policy and validator command independently of the candidate
gate. An authority successor must retain the immutable mandatory-ID root; its
approval policy must be byte-identical or pass the pinned monotonic comparator
(no lower weight/count threshold, smaller signer set or shorter delay). Its
validator command must be byte-identical unless an accepted fork first pins a
successor command and old/new validators produce identical decisions over the
complete adversarial gate corpus. The gate fields and the corresponding
`ProtocolBundleV1` fields must equal that active authority byte for byte. A
policy substitution, lower threshold, validator-command substitution or
self-selected catalog therefore rejects even when every internal hash is
consistent.

The one release command
`validate-ocomp-release --authority <trusted ReleaseGateAuthorityEnvelopeV1>
--gate <BoundedMVPReleaseGateEnvelopeV1>` first authenticates the authority from
genesis/finalized fork state, then verifies schemas, parent sequences, no-removal
rules, hashes, signatures, subjects, units, windows and expiry. It succeeds only
when every mandatory requirement has exactly one current `PASS`. Readiness
statements bind this exact gate and authority hash. Missing tools/schemas are a
`GAP`, not a manual waiver.

| Gate | Required evidence for the exact release |
|---|---|
| normative | accepted `ADR-S-OCM-001` through `ADR-S-OCM-004`, every accepted content-digest-pinned ADR owner in the exact ownership map below, and accepted OCOMP revisions of PFS-002, PFS-005 and PFS-009 |
| semantics | native/reference byte equality, frozen codecs/hashes and pre-fork golden conservation |
| authority | closed certified activation capability; no raw bypass; typed receipts from every effect owner |
| Byzantine | `q-1/q`, duplicate/stale/wrong-epoch/conflicting certificate and deterministic network simulation |
| keys | purpose-bound PoP registry, HSM/remote signer, fsync-before-sign, rotation/revocation/restore drills |
| capacity | exact maximum encoded block bill and cold benchmark on named minimum hardware with numeric finality headroom |
| isolation | deployed sibling services, aggregate cgroup/namespace/disk quotas; OOM/disk/IPC storms preserve numeric block SLO |
| recovery | crash-safe pin/export/sign journals, historical evidence/key retention, bootstrap and destructive restore with numeric RPO/RTO |
| compatibility | migration crash matrix, N/N+1 cluster, `H-1/H/H+1` jobs, old-key handover, drain and genesis/checkpoint replay |
| observability | stable failure codes, numeric job-age/availability/error SLOs, alerts, dashboards and exercised runbooks |
| supply chain | every node/supervisor/exporter/worker/broker artifact, template and policy in the signed Release/Network manifests with SBOM/provenance |
| release evidence | `BoundedMVPReleaseGateEnvelopeV1` validates and its `VerificationLedgerV1` has no non-`PASS` mandatory requirement |

The profile remains `DISABLED` if any cell fails, refers to a different commit/
artifact/network manifest or has no numeric threshold. Passing the PoC does not
waive an MVP gate. `ADR-S-OCM-001` through `ADR-S-OCM-004` are the proposed
kernel, input, evidence and activation owners. They exist but are not accepted
or implemented, so this design remains non-normative. The listed existing
ADR/PFS files retain ownership of their module/flow invariants and must be
amended and accepted, not merely linked from this proposal.

The ownership map is exact:

| Changed contract | Required accepted owner |
|---|---|
| OCOMP kernel/process/program boundary | `ADR-S-OCM-001`, `ADR-B-NOD-001`, `ADR-B-OPS-001`, `ADR-B-SUP-001` |
| authenticated job input/export/CAS authority | `ADR-S-OCM-002`, `ADR-B-OCD-004` through `ADR-B-OCD-015` |
| deterministic execution/result evidence/sign-once | `ADR-S-OCM-003`, `ADR-S-KEY-001`, `ADR-S-VAL-001`, `ADR-B-CRY-001` |
| request/job/FSM/activation/version authority | `ADR-S-OCM-004`, `ADR-S-GOV-002`, `ADR-S-GOV-003`, PFS-005 |
| Lysis result semantics and Metadosis lifecycle | `ADR-C-LYS-001`, `ADR-C-MET-001`, PFS-002, PFS-009 |
| authenticated Tribute/Fidelity/Oracle/Gratis inputs, `H_max` and emission scalars | `ADR-C-TRB-001`, `ADR-C-FID-001`, `ADR-C-GRT-001`, `ADR-S-ORC-001`, `ADR-S-EMI-001` |
| CE commitment/catalog/proof, snapshot/export, `RETIRED_RETAINED`/GC and recovery | `ADR-B-OCD-001`, `ADR-B-OCD-002`, `ADR-B-OCD-006`, `ADR-B-OCD-007`, `ADR-B-OCD-009`, `ADR-B-OCD-010`, `ADR-B-OCD-011`, `ADR-B-OCD-012`, `ADR-B-OCD-013`, `ADR-B-OCD-014`, `ADR-B-OCD-015` |
| `NodBatchReceipt` | `ADR-C-NOD-002` |
| `ContributorReceipt` | `ADR-C-INX-001`, `ADR-C-INX-002` |
| `RequestBudgetSplitReceiptV1` and strict GREEN auction dispatch | `ADR-C-DES-001`, `ADR-C-MET-001` |
| `CarryOverReceiptV1` and next-unformed-day consumption | `ADR-C-PRM-003`, `ADR-C-MET-001` |
| Commonware finality proof and historical consensus/OCOMP committee authority | `ADR-B-CNS-001`, `ADR-B-CNS-002`, `ADR-B-CNS-003`, `ADR-B-CRY-001`, `ADR-S-VAL-001` |
| genesis/fork authority, upgrade envelope and handler activation | `ADR-B-GEN-001`, `ADR-B-DEP-001`, `ADR-B-EVM-001`, `ADR-B-EVM-003`, `ADR-B-EVM-005`, `ADR-S-GOV-003` |
| activation wire/RPC/txpool/gossip and capacity | `ADR-B-WIR-001`, `ADR-B-RPC-001`, `ADR-B-TXP-001`, `ADR-B-CAP-001` |
| sibling-process lifecycle, stores, deployment and recovery operations | `ADR-B-NOD-001`, `ADR-B-OPS-001`, `ADR-B-SUP-001`, `ADR-B-OCD-014`, `ADR-B-OCD-015` |
| verification, reproducible artifacts and release evidence | `ADR-B-TST-001`, `ADR-B-RLS-001` |
| result keys, rotation and compromise revocation | `ADR-S-KEY-001` |

`RequirementCatalogV1` contains every owner above as a mandatory
content-digest-pinned entry. Missing, non-`Accepted`,
superseded-without-successor or content/section-hash-mismatched owner is a `GAP`.
In particular, OCOMP cannot pass while `ADR-C-FID-001` lacks the enforced
`H_max`/profile-readiness mutation rule, `ADR-B-OCD-011` still requires
same-transaction physical deletion, or `ADR-C-PRM-003` lacks checked carry-over
credit/take semantics.

### 14.8 Implementation and release sequence

Rollout order:

1. split current Lysis into pure `execute_lysis` and the typed
   `apply_certified_lysis` module; add the PoC consensus state/codec under an
   explicit devnet fork;
2. run four node/supervisor/exporter/worker deployments plus an untrusted relayer
   and pass the complete section 1.6 acceptance story;
3. run native/reference differential Lysis and deterministic 1/2/4-worker tests;
   completion of these first three steps is the PoC milestone;
4. accept `ADR-S-OCM-001` through `ADR-S-OCM-004`, accept every exact ADR/PFS
   amendment in section 14.7, and freeze `ProtocolBundleV1`, the single result
   digest, typed effect receipts and the production capacity/evidence
   requirement IDs;
5. activate the dormant preparation version; boundedly build/validate
   `H_max`, due indexes, schema/version markers and the preparation root while
   the old path remains authoritative;
6. harden aggregate resource isolation, HSM signing, checkpoint pins, exporter
   journals, pruning/bootstrap reconciliation, supervisor recovery,
   capabilities, monitoring and incident runbooks;
7. arm a target bundle only after the exact handler manifest, prepared root,
   live-job support set, replay dependency set, stable consensus/OCOMP snapshots,
   two readiness predicates and release-gate hash are committed;
8. run mixed-version `H-1/H/H+1`, migration crash, drain, pause and forward-fix
   tests, then activate the bundle at its governed height;
9. prove by chaos tests that killing, OOMing and filling every OCOMP resource does
   not stop block finalization; enable only a measured BoundedMVP cap after
   signer, recovery, activation and security audits;
10. design/migrate `CountedRangeTreeV1`, O(1) logical CE retirement and
   witness-based Nod state with snapshot/delta recovery;
11. select and audit one recursive proof backend and one DA encoding;
12. run destructive restore, custody handover and Byzantine tests;
13. implement and fork-test `ContributorClaimsV1`; remove the push drain and
   global active-series begin-block scan for Target series;
14. run the declared cold billion-record benchmark last.

No failed benchmark is overridden by configuration. It lowers the cap or keeps
the profile disabled.

## 15. Required closure tests

PoC closure is exactly section 1.6. Promotion beyond PoC is not complete until
the following additional MVP/Target stories pass end-to-end:

1. boot validator with no supervisor; blocks still finalize;
2. start supervisor later; it finds every missed finalized job;
3. with no relevant same-block mutation, a normal small WWD matches the offline
   reference and pre-fork golden state/events/arithmetic; no PoC block executes
   synchronous Lysis. Separate cases assert the new terminal-snapshot ordering
   under concurrent Fidelity/Oracle writes;
4. 1, 2 and N workers with reordered completion produce identical bytes;
5. kill every worker phase and supervisor journal write boundary; reconcile
   launch/cancel/reap after systemd/Kubernetes controller loss; launch
   `MAX_LIVE_WORKERS+1` and a sustained unit storm and prove the aggregate parent
   quota preserves node latency;
6. OOM/throttle/fill only the OCOMP cgroup and volume; flood IPC and fill the
   signer quota, and force a deterministic handler panic until its circuit opens,
   while ordinary blocks continue at the latency SLO;
7. corrupt, truncate, duplicate, overlap and decompress-bomb every artifact type;
8. restart/upgrade node and supervisor independently with an old job in flight;
   crash every tentative/finalized/exported pin transition and reconcile pruning;
   cancel before and after export and prove the same bounded release;
   corrupt the pin journal and prove quarantine/resync retention stays within the
   declared maximum window;
   kill the snapshot exporter on every page, corrupt its checkpoint/CAS and
   require exact root rebuild before pin release;
9. reorg before finality and attempt replay/equivocation after finality;
10. inject Byzantine result signatures, proof, DA fragment and custody receipt;
11. retain/replay evidence across block-body pruning and bootstrap from every
    supported finalized checkpoint;
12. activate, mutate, claim and delete; lose every local cache and up to `f`
    custody domains, then rebuild exact roots from two snapshots plus deltas;
13. for TargetLarge, prove activation allocation/time is independent of `T`;
    crash the cursor GC at every boundary and verify no early delete or
    block-latency regression;
14. test zero/partial/all/duplicate contributor claims, full-width rounding,
    late top-ups, arbitrarily delayed claims, concurrent series and exact
    long-lived escrow accounting with no congestion forfeiture; fill the active
    capacity ledger, hold distinct credited lots without merging and reject new
    activation; send the same source lot sequence once with empty capacity and
    once with full live-round capacity and require identical `RoundId`s and
    per-owner entitlements (including two amount-1 lots across one billion equal
    owners); attempt split, merge, reorder, duplicate, no-credit, in-flight and
    post-close transfers, and release the series slot only after all source close
    proofs, sequences, credits, pending lots, claims and certified compaction;
15. run two declared `1B` shapes with normal chain traffic: one billion unique
    owners at maximum admitted output/contributor cardinality, and the admitted
    worst Fidelity cohort distribution with exact per-owner and total visits;
    cover all-unique buckets, one hot bucket, mixed eligible/excluded contributors,
    all-excluded contributors and first failure at start/middle/end through raw
    prefix, filtered owner shuffle and bucket shuffle, with byte-identical
    native/reference/proof-guest contributor roots and claim inputs; separately
    require `T=1` excluded to produce a Nod, no contributor leaf and one exact
    ownerless reserve sweep for each proceeds lot;
16. exercise `U256::MAX` Fidelity cases and require byte equality between native,
    reference and proof guest wrapping/saturating operations;
17. prove that no ordinary or system transaction can write after the terminal
    request-snapshot phase in the same block; inject failure after every request
    and activation write and observe either the whole transition or none.
18. place permanently ineligible READY days before eligible days and prove the
    bounded due-index makes progress; expire/conflict all `MAX_PENDING_JOBS` at
    once and cancel all of them before/after export; verify atomic
    pin release, preserved Lysis budget and nonce requeue; submit
    activation at `H-1`, `H` and `H+1` and verify the exclusive-deadline rule;
    create an intent with no supervisor/activation, expire it through its
    `(deadline_height, IntentId)` entry, and verify finality-gated pin release;
19. expire a live custody certificate, reject mutations while `UNAVAILABLE`,
    repair from snapshot+deltas and reactivate only the exact prior root;
20. freeze bytes/hashes for `ProtocolBundleV1`, profiles, intent, finality proof,
    `UnitSpecV1`/`UnitId`, `SourceSubjectSpecV1`/`SourceSubjectId`, action stream,
    activation payload and certificate;
    reject every unknown phase/interval, duplicate, non-minimal, trailing and
    over-limit form, and assert that every named domain has exactly one preimage
    definition;
21. run N/N+1 validators with an old job created at `H-1`; activate the new
    bundle at `H`; finish the old job under its old codec/committee/key after
    `H`; create only new-bundle jobs from `H` onward; an old binary must stop
    participating while the upgraded quorum keeps finalizing;
22. vary the local upgrade registry under one protocol version: empty, missing,
    extra and modified handler sets must fail readiness/arming before `H` and
    never produce two accepted state roots;
23. crash `PREPARING` before/after every cursor/index/root/marker write; prove
    bounded exact resume, no lazy pre-`H` schema mutation and byte-identical
    post-activation state from genesis and `H-1/H/H+1` checkpoints;
24. rotate the validator committee and OCOMP key epoch while an old job runs;
    accept only the job-pinned snapshot, preserve sign-once history across the
    upgrade and keep historical public keys after private-key retirement;
25. run the full `Hello/HelloAck` matrix: common control with no common bundle,
    read-only versus attest capability, stale session generation, N/N+1
    supervisor and independently restarted peers; every mismatch refuses only
    OCOMP work while consensus remains ready;
26. pause at READY, OFFCHAIN_PENDING and after a finalized activation; prove
    bounded cancellation/release for pending jobs, no new activation while
    paused and deterministic higher-version forward repair for already active
    state;
27. verify `FinalizedIntentProofV1` byte vectors end to end: wrong
    chain/genesis/header/epoch/view/parent/proof-kind/bitmap/quorum/storage
    key/intent bytes reject;
    pair a valid-looking finality certificate with a caller-invented committee
    and prove it cannot replace authenticated `ConsensusCommitteeHistoryV1`;
28. swap every pair of typed effect receipts across jobs, attempts, bundles,
    result digests, preconditions and checkpoints; mutate each count/root/amount/
    event digest and fail atomically. Run independent carry-over credits in both
    transaction orders and require the declared commutative total without a
    stale-job conflict;
29. send `cap-1/cap/cap+1` activations through public JSON-RPC on all four
    validators, including decode, txpool admission/replacement, transaction
    gossip, proposer selection, block gossip, import and replay; direct executor
    injection cannot satisfy this test;
30. reach the readiness cutoff with `q-1` statements and no governance/operator
    transaction; require automatic `CANCELED_NOT_ARMED`, removal of the schedule
    and continued old-bundle finality through `H`;
31. after a valid readiness certificate but before/at `H`, rotate/jail/exit or
    change weight/quorum/key status in each pinned snapshot; require stale
    readiness cancellation. Independently meet only consensus readiness and only
    result readiness and prove neither can arm; permute A/B/C claimed OCOMP
    keys, omit an OCOMP snapshot entry, duplicate a derived identity and use a
    stale mapping; only the exact consensus-ID-derived, dual-signed `ATTEST`
    mapping can count toward result readiness. Deny the
    `UpgradeReadiness` HSM purpose, crash before/after journal fsync, restart and
    retry the identical core, then submit a different core for the same
    `(plan_id,readiness_generation)`; only the identical durable retry may sign;
32. keep a `P` job alive through `N -> N+1`; omit `P` execute/apply support or its
    HSM private key and fail readiness, then provide the full live support set and
    complete `P` under its pinned bundle. Remove only replay dependencies and
    separately prove checkpoint/genesis validation fails closed;
33. permute the same handler set, mismatch plan/bundle version or migration/
    handler hash, duplicate a dependency and exceed a handler work cap; each
    fails before arming. Inject every specified deterministic `H` handler failure
    and require one `ACTIVATION_FAILED` record, old-version block progress and no
    automatic next-block retry; place eligible READY work in `H` and `H+1` and
    require old-bundle intents with zero target-bundle intent until a later plan
    actually returns `Applied`;
34. cancel `PREPARING`, `PREPARED`, `ARMING` and `ARMED`, mutate legacy state,
    and attempt to reuse the old completion root/readiness certificate; require a
    new migration ID and exact reconciliation. Crash the detach/GC or continuing
    dual-maintenance cursor at every boundary;
35. build readiness from different conforming implementation artifact hashes:
    canonical per-validator statements pass, aggregate-as-one-message or a
    non-permitted artifact fails;
36. compromise one OCOMP key before/after certificate formation and before/after
    activation. At the effective height require the barrier to beat activation,
    cancel every affected live job without lowering `q`, retain validity only
    for activations finalized earlier, and resume new attempts only with the safe
    successor snapshot;
37. collide PAUSE with activation, expiry and upgrade at the same height; pause
    must win in the documented phase order. Crash every `PAUSING` cursor write,
    submit cap+1 cancellation work and attempt early resume; require exact
    `CANCELED` receipts/root and restart-idempotent completion;
38. run the one `validate-ocomp-release` command against the independently
    authenticated `ReleaseGateAuthorityEnvelopeV1` and missing, stale, differently
    scoped, wrongly signed and non-`PASS` ledger entries, mismatched
    Release/Network manifests and heterogeneous permitted artifacts. Attempt
    catalog shrink/removal, parent/sequence skip, mandatory-to-optional change,
    approval-policy/validator-command substitution and threshold weakening; only
    the exact unexpired gate matching the trusted authority may enable the
    profile;
39. remove or leave non-Accepted each ADR/PFS owner in the section 14.7 ownership
    map one at a time, including Fidelity, committee history, genesis, operations,
    txpool, conflicting physical-deletion and Promis contracts. Keep an Accepted
    but unamended Fidelity ADR, change owner text under the same ID, and admit an
    `H_max+1` cohort; each case must produce `GAP`/reject and keep BoundedMVP
    disabled;
40. construct independent golden bytes in the exact order
    `ReleaseGateAuthorityCoreV1 -> AuthorityApprovalDigest -> authority proof ->
    AuthorityEnvelopeHash`, then `ProtocolUpgradePlanCoreV2 -> PlanCoreHash ->
    BoundedMVPReleaseGateCoreV1 -> GateApprovalDigest -> approvals ->
    GateEnvelopeHash -> ProtocolUpgradeEnvelopeV2 -> UpgradeEnvelopeHash`;
    mutate either core or gate and reject the envelope. Replay gate approval
    across any changed field, authority proof across parent/sequence, readiness
    signature across signer-local fields, ledger signature across
    status/measurement/evidence/expiry and mode-plan authorization across a
    changed transition/height; attempt full-object self-signing, omitted-field
    or alternate strip-signatures encodings. Freeze exact domain
    tag bytes concatenated with exact canonical core/envelope bytes for every
    named hash/digest. No raw hash, placeholder, second-pass hashing or
    fixed-point iteration is permitted.

Each test has a measured CPU, RAM, disk, bytes, wall time, RPO/RTO and chain
latency result. "Completed once" is not a scale claim.

## 16. Direct answers

| Question | Answer |
|---|---|
| Who reads the Metadosis event? | The separate supervisor, through a finalized cursor exposed by the node. The event only wakes it; the on-chain job record is authority. |
| Who reads the huge historical state? | The separate read-only snapshot exporter scans one immutable checkpoint into CAS. The node creates only the O(1) checkpoint handoff and never streams bulk bytes through its control API. |
| Does the supervisor live in the node? | No. It is a sibling service/process with a separate UID, cgroup, journal and disk quota. |
| Who starts it? | systemd/Kubernetes starts it alongside the node. The node never spawns it and does not depend on its lifetime. |
| What fixes the input? | The finalized request block/state root, CE sealed root, immutable job spec and snapshot/body retention lease. Live files are not globally locked. |
| Where does the Tribute root come from? | Normal CE end-block sealing: shard roots -> WWD collection -> catalog -> sealed CE root -> EVM/block state root. |
| Who divides the work? | The deterministic planner in every supervisor. The scheduler only assigns fixed `UnitId`s. |
| Who computes? | PoC/BoundedMVP: every result-signing validator's separate workers. TargetLarge: permissionless prover workers. |
| Who checks correctness? | PoC/BoundedMVP: each signing validator independently executes, then every node verifies the `q` certificate and typed result binding. TargetLarge: every node verifies the pinned recursive proof. |
| Is OCOMP only for Lysis? | No. OCOMP defines the reusable operational kernel; Lysis V1 is its first closed typed protocol. The PoC intentionally does not pretend that one program proves generic wire abstractions. A registry/common envelopes are considered only after a second real end-to-end program, likely tested first with Gem qualification. |
| Does the PoC execute Lysis on-chain? | No. Metadosis creates `JobIntent`; validators execute Lysis off-chain. Consensus verifies the certificate/result commitment and atomically installs the typed root transition. There is no synchronous fallback and no loop over all outputs. |
| What does the PoC prove? | The real Tribute -> Metadosis -> JobIntent -> q execution -> evidence -> activation -> Nod chain on four validators. It proves arbitrary `N` is partitioned into bounded work without a total protocol cap, but exercises full bodies only on a small multi-shard fixture and makes no billion-record throughput or production-operations claim. |
| Where do result bytes live? | Bounded `ResultChunkV1` objects live in authenticated artifact storage retained by signing domains; the single `activateLysis` transaction carries their count/root in `LysisResultV1`. Consensus state retains the active roots and terminal receipt. |
| Does PoC become MVP by changing the architecture? | No. BoundedMVP keeps the same intent, q certificate, one activation transaction and typed apply seam; it replaces demo keys, scheduling, isolation, recovery and operations with production implementations and measured caps. |
| What happens to a job during an upgrade? | It finishes or expires under the exact bundle, committee and key epoch pinned when it was created. It is never converted. New request blocks use the newly active bundle. |
| What prevents a half-ready upgrade at `H`? | At the cutoff consensus evaluates separate weighted-consensus and result-committee readiness predicates over pinned snapshots. Without both, it automatically unschedules the plan; no governance transaction is needed. |
| What if an activation handler fails at `H`? | Its nested writes roll back, the old dispatcher records `ACTIVATION_FAILED`, removes the schedule and keeps the old bundle active. A new migration ID/plan is required. |
| What if an OCOMP key is compromised? | At the governed effective begin-zone the affected snapshot is invalid for new activation, its live jobs are canceled/requeued without lowering `q`, and new attempts wait for a safe successor. Earlier finalized activation remains historically valid. |
| What if pause, expiry and activation meet in one block? | The begin-zone pause barrier wins; affected jobs become `CANCELED`, then ordinary activation rejects. The bounded cleanup cursor is restart-idempotent. |
| Can the network roll back after MVP activation? | It may cancel before the activation height. After a block under the new bundle finalizes, downgrade is forbidden; repair is a higher governed forward-fix. |
| When is MVP actually complete? | Only when `validate-ocomp-release` authenticates the network/fork `ReleaseGateAuthorityEnvelopeV1`, accepts its exact unexpired `BoundedMVPReleaseGateEnvelopeV1` and every mandatory ledger requirement is `PASS`; this includes numeric capacity, SLO, RPO/RTO, mixed-version, key, recovery and incident evidence. |
| What does a signature prove? | Result signature: that one validator domain performed the bounded verification. DA/custody signature: that its exact fragment is durable. Neither means the other. |
| How are a billion owners handled? | Streamed counted ranges, external sort and deterministic Map/Reduce; never a global in-memory owner table. Feasibility still requires a cold benchmark. |
| How is a billion-record result applied? | One proved output root and availability certificate; subsequent state access is witness-based. Never a billion EVM transactions. |
| What if OCOMP dies? | The job pauses/expires, the old state remains active and consensus continues. |

## 17. Borrowed mechanisms and implementations

These are engineering precedents, not proofs that Outbe is correct.

| Mechanism borrowed | Primary source | Implementation/example | Outbe use and limit |
|---|---|---|---|
| independently run blockchain components with narrow authenticated API | [Ethereum node architecture](https://ethereum.org/developers/docs/nodes-and-clients/node-architecture), [Engine API authentication proposal](https://github.com/ethereum/execution-apis/issues/162) | [execution-apis](https://github.com/ethereum/execution-apis) | model process separation and authenticated control; OCOMP still needs its own reconnect/failure contract |
| explicit method versions and capability exchange | [Engine API common definitions](https://github.com/ethereum/execution-apis/blob/main/src/engine/common.md) | `engine_exchangeCapabilities` | model exact node/supervisor capability intersection; negotiation never changes consensus or a pinned job |
| governed handler and ordered store migrations | [Cosmos SDK x/upgrade](https://docs.cosmos.network/sdk/latest/modules/upgrade/README), [Cosmos ADR-041](https://docs.cosmos.network/sdk/latest/reference/architecture/adr-041-in-place-store-migrations) | Cosmos SDK module version map/migrators | model activation-height handlers and persisted schema versions; Outbe additionally requires a committed handler-set hash and readiness gate |
| install/approve/commit sequence before execution | [Fabric chaincode lifecycle](https://hyperledger-fabric.readthedocs.io/en/latest/chaincode_lifecycle.html) | Fabric definition sequence and commit readiness | precedent for separating artifact preparation, threshold readiness and activation; it does not define Outbe consensus |
| declared component skew and upgrade order | [Kubernetes version-skew policy](https://kubernetes.io/releases/version-skew-policy/) | Kubernetes control-plane rollout | model an explicit supported N/N+1 matrix; Kubernetes policy itself is not a blockchain safety proof |
| persist signing history before signing | [EIP-3076 slashing protection](https://eips.ethereum.org/EIPS/eip-3076), [EIP-3030 remote signer](https://eips.ethereum.org/EIPS/eip-3030) | [Web3Signer](https://github.com/Consensys/web3signer) | model single-sign journal and key isolation; OCOMP messages/rules are different |
| resource/fault isolation | [Linux cgroup v2](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html), [Landlock](https://docs.kernel.org/userspace-api/landlock.html) | [OCI runtime spec](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md), [Kubernetes resource limits](https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/) | protect consensus CPU/RAM/I/O/filesystem; containers alone are not authority |
| deterministic units, retry, external sort and fixed reduce | [MapReduce paper](https://research.google.com/archive/mapreduce-osdi04.pdf) | [Apache Hadoop MapReduce](https://github.com/apache/hadoop/tree/trunk/hadoop-mapreduce-project) | physical parallelism without semantic reorder; no Byzantine or root guarantee by itself |
| execute then endorse | [Hyperledger Fabric execute-order-validate](https://hyperledger-fabric.readthedocs.io/en/latest/whatis.html) | [Fabric endorsement policies](https://hyperledger-fabric.readthedocs.io/en/release-2.5/endorsement-policies.html) | precedent for PoC/BoundedMVP `q` execution; common bugs and input completeness remain |
| proof-carrying general computation | [RISC Zero proof system](https://dev.risczero.com/proof-system-in-detail.pdf), [Optimism Cannon](https://docs.optimism.io/op-stack/fault-proofs/cannon) | [RISC Zero](https://github.com/risc0/risc0), [SP1](https://github.com/succinctlabs/sp1), [Optimism](https://github.com/ethereum-optimism/optimism) | program/input/output binding and recursive/disputed execution; exact backend and cost require bake-off |
| hardware-attested execution, not selected as correctness authority | [Gramine SGX attestation](https://gramine.readthedocs.io/en/stable/attestation.html) | [Outbe TEE sidecar](bin/outbe-tee-enclave/src/run.rs) | useful for key confidentiality and isolation; attestation does not prove complete input, data availability or independence from hardware/vendor failure |
| authenticated tree/log size and complete ranges | [Trillian verifiable log](https://github.com/google/trillian) | [CKB sparse Merkle tree](https://github.com/nervosnetwork/sparse-merkle-tree) | current CKB codec remains bounded authority; Target adds authenticated counts/ranges rather than trusting a page count |
| availability certificates and scale-out workers | [Narwhal and Tusk paper](https://arxiv.org/abs/2105.11827) | [Sui](https://github.com/MystenLabs/sui) | separate data dissemination from consensus; an Outbe certificate must bind exact output fragments |
| erasure-coded data availability | [Celestia DA design](https://docs.celestia.org/learn/celestia-101/data-availability/) | [celestia-node](https://github.com/celestiaorg/celestia-node), [rsmt2d](https://github.com/celestiaorg/rsmt2d) | model fragment commitments/repair; sampling or coding alone does not prove Lysis correctness or permanent custody |
| optimistic alternative, deliberately not selected | [OP fault-proof specification](https://specs.optimism.io/fault-proof/index.html) | [Cannon](https://github.com/ethereum-optimism/optimism/tree/develop/cannon) | avoids always proving, but adds challenge, bond, dispute VM and long DA windows; not the simpler Outbe path |

## 18. Current-code facts and implementation gaps

- no production `JobIntent`, OCOMP state/codec, `FinalizedIntentProofV1`,
  `OcompControl`, attestation gate, supervisor/exporter/worker/broker,
  certificate verifier or certified activation module exists. This document is
  a design and implementation plan, not an implemented PoC;
- current `process_metadosis` still calls Lysis synchronously and performs
  Nod/contributor/Tribute/Desis/Promis/completion effects in that path
  ([runtime.rs](crates/core/metadosis/src/runtime.rs#L378)). PFS-002 now specifies
  the target OCOMP PoC but remains Draft and unimplemented. PFS-009 still
  describes the synchronous sequence and must be amended/accepted before an
  OCOMP BoundedMVP claim;
- the generic Update runtime has a useful atomic handler/version checkpoint, but
  the scheduled plan does not commit the handler-set or migration-manifest hash.
  Activation with an empty handler registry currently succeeds
  ([runtime.rs](crates/system/update/src/runtime.rs#L102),
  [handler test](crates/system/update/src/tests/handlers.rs#L85)). OCOMP cannot
  safely activate until `ProtocolUpgradePlanCoreV2` plus
  `ProtocolUpgradeEnvelopeV2` close that divergence. The
  current dispatcher also retries a due scheduled plan and treats handler error
  as fatal; it does not implement `CANCELED_NOT_ARMED` or
  `ACTIVATION_FAILED`, so the recoverable FSM in section 14.3 requires an Update
  protocol migration;
- current `nodfactory::api::issue_nod` has no unforgeable Lysis/activation
  authority, and current Desis dispatch reports acceptance as `bool`. The
  certified path requires the private capability and typed receipts defined in
  section 1.4;
- `outbe-chain` currently runs Reth and Commonware in one process; consensus is
  an OS thread, not a separate service
  ([main.rs](bin/outbe-chain/src/main.rs#L272)).
- a mandatory projection failure currently requests whole-node shutdown
  ([main.rs](bin/outbe-chain/src/main.rs#L681)). OCOMP must use a nonfatal
  readiness boundary instead.
- the TEE sidecar proves that external processes and authenticated UDS/TCP are
  already accepted patterns, but the offer path lacks transparent reconnect.
- the BLS consensus private key is currently loaded into the consensus process
  and available key backends are plaintext, encrypted file and OS keychain
  ([bls.rs](crates/blockchain/consensus/src/bls.rs#L39)). There is no production
  OCOMP subkey, HSM gate or anti-equivocation database yet.
- current CE has 16 shards and exact CKB roots, but no authenticated subtree
  counts/range enumeration profile.
- current Fidelity stores per-owner `u32` active/sold counts but enforces no
  profile `H_max` or authenticated pre-admission aggregate
  ([schema.rs](crates/core/fidelity/src/schema.rs#L41)).
- current Lysis materializes the complete WWD and calls Fidelity twice per
  Tribute ([runtime.rs](crates/core/lysis/src/runtime.rs#L31)).
- current CE retirement calls `delete_collection_records`, materializes every
  prefixed root/branch/leaf key and deletes them in the finalized MDBX
  transaction ([persistence.rs](crates/core/compressed-entities/src/persistence.rs#L1873));
  it is not O(1) at large `T`.
- downstream Intex currently drains only 200 contributors per series per block;
  at one billion contributors that is at least five million blocks, independently
  of how fast Lysis itself completes. Its begin-block path also materializes the
  full active-series set before paying chunks
  ([runtime.rs](crates/core/intexfactory/src/runtime.rs#L365)).
- large witness-based Nod reads/mutations, snapshot/delta recovery,
  `ContributorClaimsV1`, recursive Lysis proof and chain-native DA/custody do not
  exist;
- `ADR-S-OCM-001` through `ADR-S-OCM-004` and the OCOMP PFS-002 revision exist
  only as Proposed/Draft documents. `BoundedMVPReleaseGateEnvelopeV1`,
  `NetworkManifestV1` and a machine-validating `VerificationLedgerV1` do not
  exist. `ADR-B-TST-001` is Proposed. `ADR-B-RLS-001` is Accepted with partial
  implementation, but its broader package authorization and the OCOMP
  `NetworkManifest`/artifact slice remain open; none of these can be counted as
  a passed OCOMP MVP gate.

These are implementation tasks and protocol migrations, not details hidden by
the word "supervisor".

## 19. Final decision

The selected architecture is:

```text
finalized request-block snapshot
+ immutable per-job ProtocolBundleHash
+ separate independently supervised compute plane
+ authenticated shape pre-admission plus local resource admission
+ deterministic content-addressed Map/Reduce
+ q full execution through bounded streaming work; no total Tribute cap
+ one constant-size certificate+result-root activation transaction
+ private certified activation capability and typed effect receipts
+ prepared/armed mixed-version upgrade with old-job drain and forward-fix
+ recursive proof for large jobs
+ proved output root with erasure/custody availability
+ witness-based large active state with snapshot/delta recovery
+ O(1) logical activation/retirement and background GC
+ pull-based contributor claims
```

The design explicitly rejects:

- running the supervisor or workers inside `outbe-chain`;
- treating an event, Mongo page, count, signature or hash as input completeness;
- giving a scheduler/worker a validator key;
- claiming that more workers create more Byzantine independence;
- inlining a result proportional to `N` in an activation transaction or
  activating individual result chunks;
- activating a version whose bundle, preparation root or required handler set
  is not committed and threshold-armed;
- converting or reinterpreting an in-flight job during an upgrade;
- applying one transaction/event per output record;
- activating a large hash without data availability and future witness-based
  validation;
- calling one billion records supported before the exact cold failure/recovery
  benchmark passes.
