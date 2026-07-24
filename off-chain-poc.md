# Outbe off-chain computation — PoC specification

Date: 2026-07-23

Status: ready for implementation planning and task breakdown; not implemented.
The protocol values in section 22 must be frozen by explicit tasks before the
corresponding consensus codecs or state transitions are implemented.

Source: [`off-chain-computation.md`](off-chain-computation.md), SHA-256
`21f0664c80f1e32afda83ca749a0ce2811668af47c21f2c04e7db80c99b89a99`

Visual summary: [`off-chain-poc-one-pager.html`](off-chain-poc-one-pager.html)

Scope/continuity audit:
[`outbe-plan/off-chain-poc-scope-audit.md`](outbe-plan/off-chain-poc-scope-audit.md)

Proposed ADR owners:
[ADR-S-OCM-001](docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
through
[ADR-S-OCM-004](docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md).

Canonical protocol/test flow:
[PFS-002](docs/flows/002-off-chain-poc-protocol-flow.md).

## 0. Purpose of this document

This document extracts the complete Proof of Concept from the wider off-chain
computation design. It is the baseline for:

1. checking that the PoC has no missing architectural dependency;
2. agreeing on the exact PoC boundary;
3. preparing an implementation plan and independently assignable tasks;
4. verifying the finished PoC against one acceptance story.

This is not an implementation plan and does not claim that the described
components exist. Section 18 lists the known current-code gaps.

The source document remains the authority for BoundedMVP, TargetLarge, supported
network rollout and billion-record scale. This document is authoritative only
for the PoC scope extracted from the source revision above.

### 0.1 Requirement labels

Every PoC concern has one of three dispositions:

| Label | Meaning |
|---|---|
| **PoC-MUST** | must be real in the final four-validator demonstration |
| **PoC-SCAFFOLD** | codec/interface/state seam must be compatible with MVP, but production hardening is not required |
| **DEFERRED** | deliberately not part of PoC acceptance |

The final demonstration may use weak operational implementations. It may not
replace a PoC-MUST protocol seam with a mock, direct state injection or
synchronous on-chain calculation.

### 0.2 Readiness contract

This document is:

- complete enough to build the implementation dependency graph;
- complete enough to split discovery, protocol, runtime and verification work
  into tasks;
- the PoC scope and acceptance authority for those tasks.

It is not permission for an implementation task to invent an unresolved codec,
hash domain, cap, checkpoint primitive or wire format. Such a task must first
resolve and record the applicable section 22 decision, update the canonical
bundle/vector artifact and then implement against that frozen result.

### 0.3 Relationship to the parent design

This document is a profile extraction and implementation-planning continuation
of the pinned parent revision, not a competing architecture:

```text
off-chain-computation.md
  system architecture + PoC/MVP/Target profile decisions
        |
        | select only PoC behavior and its required dependencies
        v
off-chain-poc.md
  PoC boundary + implementation surfaces + PoC evidence
        |
        v
future implementation plan and task graph
```

Rules of inheritance:

1. copied protocol types, domains, formulas and lifecycle decisions keep the
   parent meaning; this document may explain or organize them but not redefine
   them;
2. MVP and TargetLarge behavior is referenced only to draw the exclusion and
   evolutionary-interface boundary;
3. implementation inventories, requirement labels, test IDs, decision tasks and
   readiness notes are planning metadata, not additional runtime behavior;
4. if an accidental conflict exists, the parent file at the pinned SHA-256 wins
   and this extraction must be corrected;
5. a changed parent SHA requires a new completeness/scope audit before this
   document is used for further planning.

## 1. PoC outcome

The PoC proves one complete protocol path:

```text
Tribute is issued
  -> the WWD is sealed
  -> Metadosis splits day_limit into lysis_budget and auction_base
  -> GREEN dispatches auction_base to Desis; RED credits it to carry-over
  -> Metadosis creates JobIntent for lysis_budget and OFFCHAIN_PENDING
  -> the request block finalizes
  -> four validator domains independently read and execute Lysis off-chain
  -> any relayer collects q=3 matching result signatures
  -> one ordinary activation transaction carries proof of finality,
     certificate and constant-size complete-result commitment
  -> consensus verifies evidence but does not execute Lysis or iterate outputs
  -> Nod/contributor roots and Tribute/carry-over/Metadosis effects commit atomically
  -> Metadosis is COMPLETED and the Tribute partition is logically retired
```

There is no synchronous fallback. If fewer than three validator domains produce
the same result before the deadline, the attempt expires and the day returns to
`READY` with the same Lysis budget and request-phase auction effect. No Nod is
created and the auction is not repeated.

The central architectural seam is:

```text
off-chain:
  execute_lysis(JobSpec, AuthenticatedInputBundle)
    -> ResultChunkV1 catalog + LysisResultV1

on-chain:
  apply_certified_lysis(JobIntentV1, LysisResultV1,
                        ExecutionCertificateV1)
    -> one atomic typed root transition
```

`apply_certified_lysis` is not a generic transaction interpreter. It accepts
only one closed Lysis result type. It cannot contain arbitrary calls, storage
addresses, keys, opcodes or uploaded code.

This is the first typed protocol carried by an internal OCOMP operational
kernel, not a claim that V1 is a generic computation wire format:

```text
OcompLifecycle
  finality / JobId / pending-expiry-terminal FSM / evidence / sign-once
        |
        v
LysisProgramV1
  authenticated input / planner / units / execute / reduce / typed verifier
        |
        v
CertifiedLysisApply
  private capability / owner receipts / conservation / atomic completion
```

The implementation must preserve those module/authority boundaries. The PoC
still has exactly one activation entrypoint, `activateLysis`, and its
`JobIntentV1`, `UnitSpecV1`, `ActivationPayloadV1`,
`LysisResultV1` and `ProtocolBundleV1` are Lysis-specific even where the
names look generic. A consensus program registry, common program envelopes,
public adapter and second domain program are not PoC deliverables.

### 1.1 What the PoC proves

- real consensus `JobIntent`, lifecycle, finality binding and expiry;
- real separation between node, supervisor, exporter and workers;
- authenticated input reconstruction from a finalized checkpoint;
- deterministic parallel execution independent of worker count and order;
- four independent validator domains, with a `3-of-4` result threshold;
- separate OCOMP keys and durable sign-once behavior;
- an untrusted, replaceable relayer;
- result verification through normal transaction and block execution;
- one atomic real apply of every existing Lysis/Metadosis domain effect;
- failure without taking consensus down and without on-chain fallback;
- an evolutionary core interface that BoundedMVP can harden in place.

### 1.2 What the PoC does not prove

- billion-record or production throughput;
- production queueing, fairness, SLOs, RPO/RTO or operational readiness;
- safe rollout onto an already supported network;
- recursive proof-carrying execution;
- erasure-coded data availability or custody handover;
- witness-based large Nod state or contributor claims;
- HSM-grade key recovery and compromise response;
- exhaustive crash, Byzantine, disk, OOM, upgrade and recovery behavior.

## 2. Non-negotiable PoC boundaries

### 2.1 Must be real

| Area | PoC requirement |
|---|---|
| chain state | `JobIntentV1`, `OFFCHAIN_PENDING`, expiry and terminal receipt are consensus state |
| input | exact finalized request block, state root, CE root and sealed WWD root |
| processes | node, supervisor, snapshot exporter and workers are separate OS processes |
| execution | each of four validator domains executes the complete job through bounded work |
| parallelism | deterministic units and fixed reduction, exercised with 1, 2 and 4 workers |
| correctness | exactly three distinct signatures over one canonical `ResultDigest` |
| keys | separate OCOMP key per validator; the supervisor never receives it |
| evidence | finality proof, constant-size complete-result commitment and certificate in one activation transaction |
| activation | submitted through public RPC, txpool, gossip, proposal, import and replay |
| apply | private capability, closed root-transition APIs, typed receipts and one outer checkpoint |
| failure | no quorum means deterministic expiry, preserved budget, no repeated auction and no Nod |
| output | the request split and all observable Nod/contributor/Tribute/carry-over/Metadosis effects are real |

### 2.2 Permitted weak implementations

These are PoC-SCAFFOLD, not architectural shortcuts:

- a fresh disposable devnet genesis prepared for exactly one bundle;
- one pending job;
- a static four-member result committee;
- protected local OCOMP key files;
- a simple durable local sign-once journal;
- one local pinned checkpoint whose export may be recomputed after restart;
- a per-validator filesystem content-addressed store;
- a fixed unprivileged local worker template and basic cgroup limits;
- a trivial HTTP announcement relay;
- structured logs and demo metrics instead of production observability;
- static configuration values frozen into the devnet fork.

### 2.3 Forbidden shortcuts

The following do not count as the PoC:

- running Lysis on-chain, including a “temporary” comparison path;
- invoking legacy synchronous Lysis when the job times out;
- one central computation copied to four signers;
- counting multiple workers under one supervisor as independent validators;
- letting the node start or supervise worker processes;
- running supervisor or exporter inside `outbe-chain`;
- reading a live database as the job snapshot;
- treating an event, Mongo page count or supplied list as input completeness;
- letting a supervisor or worker access consensus/OCOMP private keys;
- sending an arbitrary digest to a generic signing endpoint;
- submitting result bytes directly to the executor or storage in the system test;
- using a privileged internal test path instead of normal RPC/txpool/P2P/block
  processing;
- applying arbitrary storage writes or one transaction per output;
- storing a full result in an intermediate consensus state;
- declaring PoC completion using module mocks only.

## 3. Actors and deployment

Each of the four validator administrative domains contains:

```text
outbe-chain.service
  consensus and finality
  JobIntent/FSM/expiry
  bounded OcompControl UDS endpoint
  OcompAttestationGate and sign-once journal
  separate OCOMP private key

outbe-ocomp-supervisor.service
  finalized-job cursor
  local admission and immutable job binding
  deterministic planner/scheduler/reducer
  local job journal

outbe-ocomp-snapshot-exporter.service
  read-only immutable checkpoint
  paged root-bound input export
  input-root/count reconstruction

outbe-ocomp-worker@.service
  one immutable UnitId per invocation
  read-only inputs and private scratch
  no node DB, validator key or default network

local filesystem CAS
  content-addressed source, intermediate and result artifacts
  independent quota
```

An untrusted relay runs outside these domains. It collects announcements,
groups them by digest and submits a transaction. It owns no protocol key and is
not unique; any client can perform the same submission.

The service manager starts node, exporter and supervisor as sibling services.
Starting them after the node is allowed, but the node must not have
`Requires=`, `BindsTo=` or `PartOf=` lifecycle dependence on OCOMP services.
The node never spawns the supervisor.

### 3.1 Failure boundary

- the node can become consensus-ready with no supervisor;
- a supervisor crash cannot select node shutdown;
- a worker failure only retries the same `UnitId`;
- exporter/CAS/sign-journal pressure disables local OCOMP work or signing, not
  consensus;
- all control messages are bounded before allocation;
- the in-node OCOMP handler cannot start computation;
- `consensus_ready`, `execution_ready` and `ocomp_ready` are distinct health
  signals.

PoC uses separate UIDs/directories and basic cgroup CPU, memory, task and I/O
limits. A production launch broker, hardened namespaces, aggregate lease
accounting and remote compute policy are DEFERRED to MVP.

### 3.2 Trust and access matrix

| Component | May read | May write | May sign | Compromise within PoC |
|---|---|---|---|---|
| node | canonical chain and verified candidate bytes | chain/node state and own journals | consensus key and separate OCOMP key through separate gates | one Byzantine validator domain |
| snapshot exporter | one opaque immutable checkpoint | source CAS and export journal | never | can omit/corrupt bytes; root/count/opening verification rejects |
| supervisor | finalized specs and CAS artifacts | own journal/artifacts | never | may compute or schedule incorrectly; loses one validator result |
| worker | exact job-scoped inputs | private scratch and one CAS result | never | produces a rejected artifact or wastes bounded resources |
| CAS/body transport | opaque chunks | its own stored chunks | never | may omit/corrupt/withhold, but cannot forge authenticated roots |
| relay | public candidate announcements | public transaction submission | never | can delay/reorder/drop, but cannot construct a false certificate |
| service manager | fixed unit configuration | process lifecycle and cgroups | never | host-administrator compromise, outside the in-protocol fault boundary |

The supervisor has no MDBX/Mongo writer, validator-key directory, arbitrary node
RPC, shell callback or code-download capability. Process separation is fault
containment, not extra Byzantine identity: all processes in one validator
administrative domain still count as one signer.

## 4. Frozen PoC profile

These are the proposed fork constants:

| Limit | PoC value |
|---|---:|
| result validators | `n=4` |
| tolerated faulty/offline validator domains | `f=1` |
| required matching signatures | `q=3` |
| `MAX_TRIBUTES_PER_WORK_SHARD` | `256` |
| workers per validator | `1..4` |
| `MAX_RECORDS_PER_INPUT_CHUNK` | `768` |
| candidate `MAX_INPUT_CHUNK_BYTES` | `1 MiB`, generated before fork |
| candidate `MAX_RESULT_CHUNK_BYTES` | `512 KiB`, generated before fork |
| candidate `MAX_RESULT_SUMMARY_BYTES` | `1 MiB`, generated before fork |
| `MAX_PENDING_JOBS` | `1` |
| intents per block | `1` |
| activations per block | `1` |
| distinct reference currencies | `8` |
| Fidelity cohorts per owner | `64` |
| result deadline | `64 blocks`, exclusive |

The table is a starting envelope, not a capacity claim. Before the fork is
enabled, a generator must construct the maximum-shaped:

- input and result chunks;
- constant-size activation payload and `LysisResultV1`;
- three-signature certificate;
- finality and storage proof;
- activation transaction;
- typed receipts, logs and all mandatory block artifacts.

The generator must exercise `cap-1`, `cap` and `cap+1` through:

```text
public JSON-RPC
-> transaction decode
-> txpool admission/replacement
-> four-node transaction gossip
-> proposer selection
-> block gossip
-> validation/import
-> replay
```

There is no batch Tribute cap. A parent `JobIntent` covers all `N` records; the
planner commits `ceil(N / MAX_TRIBUTES_PER_WORK_SHARD)` ordered worker shards
without materializing their complete vector. The 257th Tribute starts shard 2,
the 10,000th belongs to one of 40 shards, and one billion Tribute imply
3,906,250 shards. No record is rejected because the parent crosses a shard,
chunk or fixture boundary.

Generated caps bound one chunk/work invocation, the live worker pool and the
constant-size activation transaction. The selected values must fit transaction
bytes, the complete RLP block, gas/internal-work, root-transition work and
finality budgets with measured headroom on the declared minimum devnet machine.
Local configuration may never raise these per-interface bounds or reinterpret
them as a total population ceiling.

An immutable PoC build/deployment manifest records the tested node, supervisor,
exporter, worker and relay artifacts. A PoC network manifest records every
compatible RPC, transaction-input, txpool, P2P, block-body, gas,
internal-work/job/byte/count limit and the minimum devnet hardware class. These
manifests are reproducibility artifacts, not the signed BoundedMVP release gate;
the smallest recorded layer limit is the protocol cap.

An ineligible or over-limit WWD stays `READY` or the created job expires. It
never falls back to on-chain Lysis.

The new path is gated by the explicit disposable-devnet fork/profile. Before
that fork the current synchronous path remains authoritative. Once the PoC
profile is active, an eligible non-empty WWD has no synchronous escape path.
Empty and protocol-ineligible WWD behavior remains outside the off-chain job and
must be covered by a regression task so the fork does not accidentally change
it.

## 5. Consensus lifecycle

### 5.1 Request block

The request is created in a terminal deterministic system phase:

```text
ordinary transactions
-> CE sealing
-> bounded READY inspection and request creation
-> commit; no later semantic writer
```

Creating the intent in begin-block is incorrect because later transactions
could change Fidelity or Oracle after the supposed snapshot.

For one eligible non-empty sealed WWD, terminal Metadosis:

1. validates the day, sealed totals and authenticated pre-admission envelope,
   including per-interface shape bounds but no total Tribute cap;
2. derives a checked split:

   `day_limit = lysis_budget + auction_base`;
3. for GREEN, dispatches exactly `auction_base` to Desis; for RED, credits
   exactly `auction_base` to Promis carry-over;
4. freezes the split, exact Lysis scalars and live activation preconditions;
5. stores `JobIntentV1` for exactly `lysis_budget`;
6. inserts `(deadline_height, IntentId)` in the expiry index;
7. changes the day to `OFFCHAIN_PENDING`;
8. emits `OffchainJobRequested(IntentId)`;
9. returns without executing Lysis.

It must not issue Nod, record contributors, consume Tribute or complete
Metadosis in the request block. The request-phase Desis/carry-over effect and
the JobIntent commit atomically and are never repeated by retries.

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

IntentId =
  H("OUTBE_OCOMP_INTENT_V1", canonical(JobIntentV1))
```

The PoC does not use custody, but it keeps the canonical optional/none form of
`custody_committee_epoch_hash`; it does not define a second intent codec.

All request writes form one outer checkpoint with a typed outcome:

```text
Deferred(reason, next_check_height)
  -> only due-index/READY metadata and receipt change

IntentCreated(IntentId)
  -> budget split, request-phase effect, intent, expiry, state and event commit

execution/storage/invariant failure
  -> everything rolls back; candidate block is invalid
```

The READY index is ordered by `(next_check_height, WWD, pending_nonce)`.
Inspection and intent creation are bounded. A deferred item is reinserted using
a fixed backoff so it cannot starve later eligible work. PoC permits one pending
job, but it uses this bounded index rather than scanning all days.

`PreAdmissionEnvelopeV1` is authenticated consensus state, not a value supplied
by the supervisor. For PoC it commits at least the sealed Tribute count,
canonical-body bytes, applicable profile and conservative upper bounds for
Fidelity/Oracle openings, outputs, activation bytes and retention. The fresh
genesis starts with the bounded Fidelity profile prepared; every subsequent
Fidelity mutation enforces the fork cap of 64 cohorts per owner. Unknown or
overflowing bounds leave the day `READY` and create no intent.

### 5.2 Finality and JobId

No authoritative semantic computation begins before request-block finality.

```text
JobId = H(
  "OUTBE_OCOMP_JOB_V1",
  IntentId,
  finalized_request_block_hash,
  finalized_request_state_root
)
```

`IntentId` is the lifecycle/expiry key. `JobId` binds the work to the actual
finalized block and state root.

`FinalizedIntentProofV1` is a strict bounded codec:

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

The activation verifier must:

1. verify chain, genesis, fork, bundle and canonical header hash;
2. verify height/hash, proof kind and empty missed-proposer list;
3. resolve committee and cryptographic material from authenticated historical
   committee state, never from caller bytes alone;
4. verify the exact Commonware finalization subject, certificate, bitmap and
   quorum using the bundle-pinned verifier;
5. verify the fixed OCOMP account/storage path against the request state root;
6. decode the exact canonical intent and recompute `IntentId` and `JobId`;
7. compare nonce, attempt, deadline and activation preconditions with live
   state;
8. reject duplicate, missing, non-minimal, trailing or over-limit fields.

The PoC must include positive vectors plus wrong chain, header, committee,
bitmap, quorum, storage key and intent vectors.

### 5.3 Discovery

`OffchainJobRequested` is only a wake-up hint. Each supervisor calls:

```text
ListFinalizedJobs(after_cursor, limit)
-> GetJobSpec(JobId)
```

It stores a durable finalized cursor and reconciles it after restart. A lost
event or subscription must not lose a job.

### 5.4 Expiry and terminal states

```text
READY
  -> OFFCHAIN_PENDING(IntentId)

OFFCHAIN_PENDING
  -> COMPLETED
  -> EXPIRED
  -> CONFLICTED
  -> CANCELED

EXPIRED | CONFLICTED | CANCELED
  -> READY(next_pending_nonce)
```

`DISCOVERED`, `ADMITTED`, `RUNNING` and `LOCALLY_VERIFIED` are local supervisor
states only.

The expiry index is ordered by `(deadline_height, IntentId)`.
`MAX_OCOMP_EXPIRATIONS_PER_BLOCK` is at least `MAX_PENDING_JOBS`; for PoC it can
therefore clear the only pending job at its deadline.

`deadline_height = H` is exclusive:

- activation is allowed only in blocks `< H`;
- begin-block `H` expires and requeues the old intent before transactions;
- an activation transaction in `H` sees the expired nonce and rejects.

Successful activation removes the expiry entry and commits the certified
effects. Expiry or conflict resolution removes the old expiry entry, increments
the attempt and requeues the same budget. It works when no supervisor or
relayer ever ran.

Persisted state must maintain:

```text
WWD OFFCHAIN_PENDING
  <=> exactly one live IntentId for current pending_nonce

live intent
  <=> exactly one expiry entry
  <=> exactly one immutable budget split and activation-precondition set

terminal intent
  <=> no expiry entry

COMPLETED
  <=> exactly one activation receipt and active result identity

READY
  <=> exactly one due-index entry and no live current-nonce intent
```

There is no persistent `RESULT_ACCEPTED` or `DATA_AVAILABLE` state.
`CANCELED` is reserved in the closed codec for the MVP pause/revocation path;
PoC need not implement governed pause.

### 5.5 Block ordering preserved for evolution

The PoC implements these stable phase slots:

```text
begin-zone:
  1. reserved protocol/mode barrier slot (no-op in PoC)
  2. expire/reset jobs due at this height
ordinary transactions, including activateLysis
CE sealing
terminal READY inspection/request creation
commit; no later semantic writer
```

MVP may populate the earlier revocation, pause and upgrade barriers without
moving expiry, activation or request creation. This is how the PoC core evolves
without changing its lifecycle ordering.

Activation dispatch handles terminal retries before the live-job guard:

1. exact retry of an already `COMPLETED` binding/digest returns its recorded
   receipt without effects;
2. `COMPLETED` with different binding or digest rejects;
3. `EXPIRED`, `CONFLICTED` or `CANCELED` rejects;
4. only live `OFFCHAIN_PENDING` continues to evidence verification.

## 6. How the input is fixed and read

### 6.1 Origin of the Tribute root

The event and supervisor do not create the root. Normal CE sealing does:

```text
canonical Tribute body
-> body commitment at TreeKey
-> one of 16 shard roots
-> Tribute(WWD) collection root
-> CE catalog root
-> sealed CE root in EVM storage
-> finalized block state root
```

“Freeze” means binding and retention, not locking a live database:

- the intent commits the CE sealed root and sealed WWD collection root;
- the finalized request header commits the state root;
- sealed bindings and live activation preconditions reject conflicting writes;
- source bodies and historical openings remain retained until terminal/recovery
  gates permit release;
- each transported body/opening is verified against these commitments.

MongoDB or another body store may transport bytes. It is not authority, and its
page count is not proof that no Tribute was omitted.

The current sealing path to be wrapped and tested is implemented in
[`tree_service.rs`](crates/core/compressed-entities/src/tree_service.rs#L315),
[`lifecycle.rs`](crates/core/compressed-entities/src/lifecycle.rs#L45) and
[`state.rs`](crates/core/compressed-entities/src/state.rs#L118).

### 6.2 PoC checkpoint and exporter

For the PoC each validator domain uses one immutable checkpoint:

```text
(checkpoint_id, finalized_block_hash, state_root, ce_root, schema)
```

Before a node advertises or votes for a candidate block containing an intent,
and before pruning may pass that height, it must durably record the corresponding
`TENTATIVE` pin. This pin is retention bookkeeping only; semantic computation
still waits for finality. A pre-finality reorg releases the tentative pin and
makes every result from that fork non-signable.

The node performs an O(1) handoff of an opaque read-only capability to the
exporter UID. It never exposes a live MDBX/Reth writer or accepts an arbitrary
filesystem path from the supervisor.

The exporter:

1. resumes a bounded page cursor;
2. full-folds the complete current 16-shard CKB WWD tree;
3. verifies every canonical Tribute body;
4. exports and verifies every required Fidelity and Oracle state opening;
5. verifies exact count/nominal and rebuilt collection root;
6. writes a canonical authenticated input bundle to CAS.

PoC requires a real retention pin for the demonstrated job. A one-entry durable
implementation is sufficient. Full crash-safe pin/export/pruning recovery is
DEFERRED to MVP. If the pin, checkpoint or journal is ambiguous, the validator
must abstain from signing; it must never guess or use live state.
Insufficient local disk or compute may change that validator's admission and
attestation decision, but it cannot change consensus job eligibility or the
authenticated job contents.

The minimum local lifecycle is:

```text
TENTATIVE(IntentId, candidate block/state root)
-> FINALIZED(JobId, deadline)
-> EXPORTED(authenticated input bundle)
-> RELEASED only after terminal finality and retention rule

TENTATIVE -> RELEASED when candidate is orphaned
```

## 7. Node/supervisor interfaces and artifacts

### 7.1 `OcompControlV1`

The PoC demonstrates the local UDS adapter:

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

The UDS is owner/group restricted and checks peer credentials. Requests use
bounded canonical messages, session generations, counters and nonces.
No common control version, bundle or required capability refuses only OCOMP
work and sets `ocomp_ready=false`; it must not affect consensus readiness.

The API never accepts:

- a private key;
- an arbitrary signing payload, subject, purpose or digest;
- a worker command or executable path;
- a database/path/query;
- an unbounded body.

For PoC, `AttestationCandidateV1` includes constant-size `LysisResultV1`. The
separate compute plane has already validated complete result-chunk coverage.
The node independently reloads the finalized job, derives the closed signing
purpose and sign-once key, rehashes the result commitment and validates its
constant-size structure/equations. It neither reads result chunks nor reruns
Lysis, so bulk work cannot enter the consensus process failure boundary.

Remote mTLS is DEFERRED to MVP and must implement the same logical interface.

### 7.2 Bulk plane

Bulk data never crosses `OcompControlV1`:

```text
ArtifactRef {
  purpose, codec, compression,
  encoded_bytes, decoded_bytes,
  record_count,
  semantic_digest, transport_digest,
  chunk_refs[]
}
```

PoC uses a per-validator local filesystem CAS. Every parser checks encoded and
decoded size, nesting, count and compression ratio before allocation. Missing,
duplicated, overlapping, trailing or unused chunks reject. Transport hashes
detect corruption; semantic roots establish authority.

## 8. Deterministic planning and parallel execution

Each supervisor independently derives the same semantic work graph. The
scheduler only assigns and retries already-defined units.

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

Ranges are fixed-width, start-inclusive and end-exclusive. The bundle fixes
vector order, optional/empty encoding and valid phase/interval combinations.
Unknown combinations, duplicates, non-minimal fields and trailing bytes reject.

For the PoC, every invocation is bounded while the parent population is not:

1. verify the complete CE fold;
2. external-sort verified records by raw 36-byte `EntityId`, matching current
   Lysis order;
3. cut fixed intervals of at most `max_tributes_per_work_shard` Tribute and
   bounded bytes;
4. keep one owner’s Fidelity history in one unit;
5. use a fixed binary reduction tree.

Changing worker count, host, completion order or retry count cannot change a
`UnitId`, intermediate semantic digest or result byte.

### 8.1 Execution graph

```text
Phase A — authenticated enumeration
  Tribute ranges -> verified bodies -> raw-ID sorted runs

Phase B — Fidelity and demand map
  Tribute/owner -> exact Fidelity reads + nominal partial by FI

Phase C — fixed reduce
  partials -> total nominal + FI fraction table + arithmetic checks

Phase D1 — output amount map in raw EntityId order
  Tribute + FI table + Oracle openings
    -> amount/conservation record

Phase D2 — deterministic prefix scan
  fixed segment totals + fixed prefix tree
    -> incoming/remaining Gratis and earliest failing ordinal

Phase D3 — output finalize
  prefix + amount record
    -> Nod body, bucket record and optional eligible owner contribution

Phase E1 — owner shuffle
  stable sort by (owner, raw_ordinal)
    -> contributor leaves and eligible nominal total

Phase E2 — bucket shuffle
  stable sort by (bucket_key, raw_ordinal)
    -> grouped bucket leaves

Phase F — fixed root/conservation reduce
  -> roots, exact counts, totals and event summary
```

Sort run limits, merge fan-in, file count, spill bytes, compression ratio and
tie-breaking are fixed. Even though the PoC is small, no result may depend on a
language hash-map iteration order or worker completion timing.

Every sort/shuffle artifact carries a permutation/coverage commitment linked to
the authenticated raw stream. The local validator-domain verifier checks that
the complete raw ordinal set appears exactly once, with no omission,
duplication or replacement, and that owner/bucket grouping preserves the fixed
tie-break order. The prefix tree commits every segment total and selects the
lowest failing raw ordinal exactly as sequential Lysis would.

All semantic inputs are pinned to the request:

- Tribute bodies and ordering;
- Fidelity and Oracle state openings;
- Gratis and Metadosis scalars;
- logical evaluation height/time;
- Lysis, arithmetic, planner, reducer and codec versions.

The computation uses no wall clock, floating point, locale, randomness, live RPC
read or network response.

### 8.2 Required Lysis equivalence

`execute_lysis` must reproduce the current semantic program:

- raw Tribute ID order;
- two logical Fidelity observations per Tribute;
- conditional Oracle reads;
- current wrapping/saturating `U256` operations;
- wrapped multiplication/addition and exact division points;
- first-error ordinal;
- contributor ordering;
- exactly one Nod per Tribute;
- existing conservation relationships.

An owner contribution is emitted only when
`exclude_from_intex_issuance == false`. Excluded Tribute still participates in
the calculations where legacy Lysis uses total Tribute nominal, but it creates
no contributor entitlement.

Current Tribute identity makes owners unique within a WWD:

```text
T = Tribute count
U = distinct Tribute owner count
D = emitted Nod count
C = eligible contributor owner count

U = T
D = T
0 <= C <= T
```

Native execution and an independent offline reference/golden corpus must match,
including adversarial values at `U256::MAX`. Consensus never obtains this
comparison by calling on-chain Lysis.

## 9. Result, certificate and sign-once rule

Each validator domain produces:

```text
ResultChunkV1 {
  protocol_bundle_hash, JobId, attempt,
  chunk_ordinal, first_nod_ordinal,
  bounded ordered_nod_actions,
  bounded ordered_eligible_contributors
}

LysisResultV1 {
  protocol_bundle_hash, JobId, attempt,
  result_chunk_count, result_chunk_list_root,
  tribute_count, tribute_nominal_total,
  unused_lysis,
  exact roots,
  conservation totals,
  arithmetic commitment,
  carry_over_credit,
  metadosis_completion_summary
}
```

The canonical signed preimage is:

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

ResultDigest =
  H("OUTBE_OCOMP_RESULT_V1", canonical(ActivationPayloadV1))

ExecutionCertificateV1 {
  result_committee_snapshot_hash,
  signer_bitmap,
  aggregate_or_ordered_signatures,
  ResultDigest
}
```

Every validator domain's separate compute process independently reads every
authenticated result chunk, proves gap-free complete catalog coverage, and
recomputes roots, counts, totals, arithmetic commitment and event summary
before requesting its node's attestation. The node verifies the constant-size
closed signing subject and never scans those chunks. The activation carries
only `LysisResultV1`; result-chunk bodies remain content-addressed artifacts for
projection, availability and proof serving. No chunk is separately signable or
activatable.

### 9.1 Committee and threshold

`OcompCommitteeSnapshotV1` is static consensus state for the PoC. It contains
ordered validator identities/indexes, unique OCOMP public keys, scheme, allowed
purpose, validity interval, proof of possession and key epoch.

The PoC uses canonical low-`s` `secp256k1` signatures. The certificate contains:

- the pinned committee snapshot hash;
- a canonical four-bit signer bitmap;
- exactly three ordered `(validator_index, signature)` entries;
- one `ResultDigest`.

A validator may contribute at most one signature. Aggregate signatures are not
required.

For `n=4, q=3`, any two quorums intersect in at least two validators. With at
most one faulty domain and honest sign-once behavior, conflicting certificates
cannot both form. This does not protect against a common implementation bug, so
reference and differential tests remain mandatory.

The result committee and its certificate are not the consensus finality
committee/certificate. The same four validator identities may appear in both on
the PoC devnet, but the snapshots, keys, signature domains and verified
statements remain distinct.

### 9.2 Sign-once journal

Before releasing a signature, the node durably stores:

```text
subject = Result(JobId, attempt)
purpose = ResultSignature

SingleSignKey =
  (chain_id, subject, purpose)

value =
  (protocol_bundle_hash, key_epoch, ResultDigest)
```

The record is fsynced before signature release. Retrying the exact same value is
idempotent. Any different value for the same key is rejected as equivocation.
Journal loss or ambiguity disables OCOMP signing; it does not reset the journal
and does not affect the consensus key.

The key is separate from the consensus private key, never exported to the
supervisor and only signs the closed `ResultSignature` purpose derived by the
node from a verified candidate.

### 9.3 Relay

The PoC relay receives:

```text
CandidateAnnouncement(
  JobId,
  LysisResultV1,
  validator_index,
  signature
)
```

It groups exact digests, deduplicates validator indexes, constructs one
`PoCActivationV1` at three matching signatures and submits `activateLysis`.
Dropping, mixing, reordering or mutating announcements can delay/reject a job
but cannot authorize a different result.

The relay recomputes `ActivationPayloadV1` from the canonical result and builds
or obtains `FinalizedIntentProofV1` from authenticated public finalized-chain
data. It does not trust a proof merely because one announcement supplied it.
The implementation plan must decide whether existing public data is sufficient
or a bounded read-only proof adapter is needed. PoC does not require a new
consensus wire type or public RPC when the proof can be constructed from
existing data.

There is no separate “result accepted” transaction.

## 10. Activation: verification without Lysis

The ordinary activation transaction contains:

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

1. derives `JobId` from the finality proof;
2. verifies the exact live intent, attempt, bundle and exclusive deadline;
3. loads the pinned historical OCOMP committee snapshot;
4. verifies three distinct signatures over one `ResultDigest`;
5. decodes the constant-size result and binds its result-chunk count/root;
6. verifies committed roots, counts, conservation totals, arithmetic commitment
   and event summary;
7. verifies activation byte/crypto caps and live old-root/generation
   preconditions;
8. constructs a private `CertifiedLysisActivation`;
9. installs the certified root transition and scalar effects in the same
   transaction checkpoint.

It does not:

- enumerate Tribute or Fidelity;
- call `fidelity::league`;
- read Oracle;
- calculate FI fractions, prices, Gratis distribution or Nod bodies;
- iterate over `N` Nod/contributor actions;
- rerun any phase of Lysis.

Hashing, decoding, signature verification, constant-size structural checks and
installing precomputed authenticated roots are verification/apply work, not
Lysis computation.

The complete action bytes do not occur in the activation transaction.
Consensus state retains the active roots/generation, terminal hashes, counts,
certificate hash and receipt. Result chunks supply bodies to projection and
proof-serving paths but never become consensus authority by location alone.

### 10.1 Capacity admission

The fork fixes one activation per block and maximum evidence bytes,
signatures, receipts and verification work. A valid transaction above a block
counter/byte limit receives a typed rejection and leaves the job live for later
resubmission. The limit is checked before large decode or cryptography.

### 10.2 Private activation authority

`CertifiedLysisActivation` has no public constructor. Only the production
executor verifier can create it:

```text
ActivationCallCoreV1 {
  IntentId, JobId, attempt, protocol_bundle_hash,
  ResultDigest, activation_preconditions_hash, terminal_pending_nonce
}

ActivationCallId(core) =
  H("OUTBE_OCOMP_ACTIVATION_CALL_V1", canonical(core))

EffectBindingV1 {
  IntentId, JobId, attempt, protocol_bundle_hash, ResultDigest,
  activation_preconditions_hash, activation_call_id
}
```

The capability is move-only, non-cloneable, non-serializable and valid only
inside one outer executor checkpoint.

The activation module calls only:

```text
NodFactory::install_certified_generation(
  capability, old_generation, nod_root, bucket_root, counts, totals)
  -> NodBatchReceipt

Intex::install_certified_contributor_root(
  capability, old_generation, contributor_root, count, total)
  -> ContributorReceipt

Tribute::retire_certified_partition(
  capability, sealed_generation, root, count, totals)
  -> TributeReceipt

PromisLimit::credit_certified_carry_over(capability, unused_lysis)
  -> CarryOverReceipt
```

Despite its historical name, `NodBatchReceipt` acknowledges one constant-size
generation/root installation; activation does not submit or loop over a Nod
batch proportional to the Tribute population.

Each effect owner is the only constructor of its receipt:

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

Before commit the aggregate module consumes all four activation receipts and
the request budget-split receipt and checks:

```text
all bindings equal the capability binding

Nod count == consumed Tribute count == payload Nod count
Nod root == payload Nod root

Nod Gratis consumed + unused Lysis
  == frozen lysis_budget

frozen day_limit
  == frozen lysis_budget + frozen auction_base

Tribute root/count/nominal
  == intent sealed root/authenticated count/authenticated nominal

contributor root/count/eligible nominal
  == decoded eligible actions and payload

request split receipt
  == exact GREEN Desis auction_base or RED carry-over auction_base

carry-over credited == signed unused_lysis
carry-over after == checked_bundle_add(before, unused_lysis)

every precondition identity/version and state-event digest
  == the owning module's decoded action, pre-state and post-state
```

Only after these checks may the same checkpoint:

- mark Metadosis `COMPLETED`;
- remove the expiry;
- logically retire the exact WWD;
- emit `LysisActivated` and canonical aggregate/domain events.

Any missing or mismatching receipt or write failure rolls back the entire
activation.

The terminal transition records the active result identity in consensus state:

```text
ActiveGenerationV1 {
  JobId, program_version,
  nod_root, bucket_root, contributor_root,
  output_manifest_root, exact_counts,
  result_evidence_hash,
  availability_certificate_hash
}
```

For PoC, `result_evidence_hash` binds the transaction-carried result commitment
and certificate. `availability_certificate_hash` uses the bundle's canonical
`none` value: the PoC relies on all three signing domains having verified and
retained the authenticated result chunks, but does not claim an independent
data-availability protocol. The active roots remain consensus authority; a
supervisor database or artifact location can never select the active output.

### 10.3 Carry-over

`PromisLimit.total_unallocated` is the explicit carry-over accumulator. Request
credits RED `auction_base`; successful activation credits `unused_lysis`.

Limit formation atomically consumes the available carry-over into the next
not-yet-formed day limit. A credit arriving after a day limit was formed waits
for the following unformed day.

Retry preserves `lysis_budget` without a second request-phase effect. A terminal
no-retry outcome credits the whole `lysis_budget` exactly once.

### 10.4 Time semantics

| Field/effect | Authoritative time |
|---|---|
| Fidelity/Oracle input, Metadosis scalars, Nod `issued_at` and semantic event fields | request `logical_evaluation_height/time` |
| certificate | no wall-clock field |
| activation receipt location and `activated_at` | actual activation block |

Delaying an otherwise valid activation may change only explicit activation
metadata. It must not change Nod economics, repeat the Desis brief or change the
signed carry-over credit.

### 10.5 Conflict and retirement

If a live activation precondition conflicts, certified effects do not apply.
The activation commits `ConflictResolved`:

- mark the old job `CONFLICTED`;
- remove the old expiry entry;
- increment the pending nonce;
- return the day to `READY` with the same `lysis_budget`.

The old evidence cannot be relabelled as a retry.

Invalid evidence rejects with no state change. A storage/invariant failure makes
the candidate block invalid.

The source WWD is logically moved to `RETIRED_RETAINED`; activation does not
synchronously enumerate and delete every physical CE record. A minimal
generation/catalog-pointer retirement is PoC-MUST. Production restart-safe
cursor GC and retention/handover policy are DEFERRED to MVP.

## 11. Protocol bundle and evolutionary transition

Every PoC consensus object pins one `ProtocolBundleHash`:

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

The PoC bundle uses canonical `none` identifiers for proof and DA, fixed/no-op
versions for pause/upgrade behavior, and canonical genesis placeholders for
supported-network release authority and migration fields. It must not invent a
smaller alternate codec. Changing those placeholders for MVP creates a new
bundle; it never reinterprets PoC bytes.

The bundle fixes:

- every codec and hash/signature domain;
- request and Lysis semantics;
- planner and reducer rules;
- activation call order, receipt equations and error taxonomy;
- effect-owner registry;
- object byte/count/cryptographic caps;
- historical decoder/verifier identities.

Unknown bundle, object kind, version or tag; non-canonical, duplicate or trailing
fields; and over-limit values reject.

The evolutionary commitment at this milestone is the internal kernel/domain
boundary, not a hypothetical multi-program wire abstraction. The PoC does not
add a one-entry consensus registry or wrap Lysis V1 objects in generic
envelopes: that would add bytes and lifecycle obligations without proving a new
capability. A future program must introduce new fork-pinned typed object kinds
and signature domains; old Lysis V1 bytes are never reinterpreted. Common
`ProgramSpec`, registry or envelope types may be frozen only after a second
end-to-end program proves the shared invariants.

### 11.1 Core unchanged from PoC to BoundedMVP

```text
finalized JobIntent
-> authenticated snapshot
-> deterministic execute_lysis
-> q independent validator-domain signatures
-> one typed activateLysis transaction
-> private CertifiedLysisActivation
-> atomic Nod/contributor/Tribute/carry-over/Metadosis effects
```

MVP replaces the operational implementations around these interfaces:

| Concern | PoC | BoundedMVP |
|---|---|---|
| network | disposable prepared devnet | governed supported-network rollout |
| committee | static four members | historical epoch changes and handover |
| keys | protected local key and journal | HSM/remote signer, rotation/recovery/revocation |
| checkpoint | one pinned export; recompute allowed | crash-safe pin/export/pruning/restore FSM |
| scheduling | one parent job, deterministic multi-shard queue and shard retry | bounded fair multi-job queues and resumable journals |
| worker launch | fixed unprivileged template | audited broker and aggregate lease accounting |
| isolation | separate processes/basic cgroups | hardened identities, policies and quotas |
| storage | local CAS | production retention/bootstrap policy |
| reliability | selected failures | exhaustive crash/disk/OOM/Byzantine chaos |
| operations | logs/demo metrics | SLOs, alerts, dashboards and runbooks |
| capacity | no claim beyond the generated bounded multi-shard PoC envelope | cold benchmark and frozen measured cap |

The PoC chain history is disposable. “Evolutionary” means reusing the protocol
and implementation seams, not promoting PoC state into a production network.

## 12. Failure behavior required by the PoC

| Failure | Required result |
|---|---|
| request event lost | finalized cursor discovers the job |
| supervisor absent at node boot | blocks finalize; OCOMP reports degraded |
| one supervisor/domain stopped | remaining three can form `q=3` |
| two domains stopped | no fallback; job expires and requeues |
| supervisor crash | node continues; supervisor can reconcile cursor/artifacts |
| worker crash/timeout | retry identical `UnitId` |
| exporter/CAS full or corrupt | local domain abstains; node continues |
| source body/opening unavailable | fetch another retained copy or abstain; never sign an incomplete input |
| checkpoint/root mismatch | quarantine input; no computation/signature |
| artifact corruption/truncation/duplicate chunk | reject or recompute |
| request block reorgs before finality | release tentative pin; discard local work; no result is signable |
| node restarts during a job | consensus recovers normally; signing waits for finalized-state/journal reconciliation |
| node/supervisor bundle mismatch | refuse local OCOMP work; node continues validating the chain |
| relay unavailable | another client may submit; consensus unchanged |
| relay mixes signatures/results | certificate/result verification rejects |
| wrong result byte/order/count/root | activation rejects with no state change |
| wrong JobId/intent/finality proof | activation rejects with no state change |
| second digest for same sign-once key | local attestation gate refuses |
| activation write failure | all effects roll back |
| activation at deadline or later | old intent is already expired |
| cap exceeded | bounded typed rejection; job/state unchanged |
| no supervisor ever runs | begin-zone expiry still releases/requeues |
| finality proof cannot be verified | fail closed; no job authority or signature |
| common semantic bug | quorum may reproduce it; independent reference and adversarial corpus must detect it |

PoC must record an execution trace proving that no on-chain call reaches Lysis,
Fidelity league calculation or Oracle calculation.

## 13. Acceptance demonstration

The PoC is complete only when this exact story passes from public Tribute
issuance through public Nod reads on a four-validator devnet:

1. issue a bounded WWD with different Fidelity leagues, currencies and at least
   one `exclude_from_intex_issuance` Tribute;
2. seal the WWD and reach terminal Metadosis;
3. inspect the finalized split and `JobIntent`; prove the request-phase
   Desis/RED carry-over effect happened once and there are zero new Nod,
   contributor, Tribute-consume or unused-Lysis carry-over effects;
4. stop one validator’s supervisor;
5. show the other three domains independently rebuild the same input root and
   produce the same `ResultDigest`;
6. submit their certificate and exact `LysisResultV1` commitment in one
   activation transaction through the untrusted relay;
7. finalize it and query every expected Nod, contributor total, Metadosis state,
   request-phase Desis brief, carry-over credit and retired Tribute partition;
8. compare the result with an offline reference/golden corpus, never an on-chain
   Lysis execution;
9. repeat with 1, 2 and 4 workers and randomized completion order;
10. request a second digest signature for the same job and observe sign-once
    refusal; mutate one result byte, signer, JobId and ordering field and observe
    consensus rejection;
11. delay otherwise identical activations by different block counts and prove
    byte-identical Nod/contributor/Tribute/carry-over results, with no repeated
    request-phase effect and only explicit activation metadata changed;
12. run another job with two validators unavailable and observe expiry,
    preserved budget, no repeated auction and no Nod;
13. record a trace proving no on-chain call to Lysis, Fidelity or Oracle
    calculation paths.

Direct executor invocation, direct storage injection or a central calculator
does not satisfy this acceptance story.

## 14. PoC verification matrix

PoC closure remains exactly the section 13 system story, as required by the
parent document. This matrix only decomposes evidence for already stated parent
requirements:

- `DEMO` is evidence observed in that same story;
- `CORE` is a focused contract/unit check for a PoC-MUST seam;
- `FORK-GATE` is the pre-activation capacity check required by parent section
  1.5;
- `COMPAT` proves that the explicit fork did not change an out-of-scope branch.

These classes add no protocol feature or additional end-to-end scenario.
Exhaustive crash-boundary, cross-product receipt, H-1/H/H+1, fuzz and mixed-
version matrices remain BoundedMVP work under parent section 15.

| ID | Class | Verification | Required evidence |
|---|---|---|---|
| POC-01 | DEMO | pure Lysis equivalence | native/reference golden corpus, including the parent-required arithmetic edge corpus |
| POC-02 | DEMO | request has no effects | state/event diff before and after finalized request |
| POC-03 | DEMO | no on-chain Lysis | call trace/static boundary test plus system trace |
| POC-04 | CORE | finality authority | positive and parent-required adversarial `FinalizedIntentProofV1` vectors |
| POC-05 | CORE | event is not authority | one lost subscription followed by cursor discovery |
| POC-06 | DEMO | input completeness | full-fold root/count/nominal and state-opening verification |
| POC-07 | CORE | deterministic plan | frozen bytes/hashes for every PoC `UnitSpecV1` variant; 10,000 and 1,000,000,000 counts derive exact `ceil(N/S)` unit counts without proportional plan allocation |
| POC-08 | DEMO | worker independence | 1/2/4 workers and randomized retries/orders yield identical bytes |
| POC-09 | DEMO | validator independence | four separate node/supervisor/exporter/CAS domains |
| POC-10 | DEMO | one domain unavailable | remaining three form exactly one valid certificate |
| POC-11 | DEMO | two domains unavailable | exclusive-deadline expiry, release and requeue |
| POC-12 | DEMO | sign once | fsync-before-sign, exact retry allowed, different digest refused |
| POC-13 | DEMO | certificate binding | duplicate/wrong signer plus the section 13 signer mutation reject |
| POC-14 | DEMO | result binding | the exact section 13 result-byte, JobId and ordering mutations reject |
| POC-15 | CORE | atomic apply | one representative injected owner-write failure yields the whole transition or none |
| POC-16 | CORE | receipt binding | one wrong-job binding and one mutated receipt reject atomically |
| POC-17 | DEMO | logical time | activation delay changes only explicit activation metadata |
| POC-18 | CORE | exclusive deadline | activation before the deadline succeeds and activation at the deadline sees expiry |
| POC-19 | DEMO | process isolation | stop the supervisors required by section 13 without stopping block finality |
| POC-20 | CORE | bounded interfaces | one over-limit UDS message/chunk rejects before unbounded allocation; crossing a work-shard boundary creates another unit rather than rejecting the parent |
| POC-21 | FORK-GATE | public wire path | activation-byte cap-1/cap/cap+1 through RPC/txpool/P2P/import/replay |
| POC-22 | DEMO | public output | Nod and all domain effects verified through public read interfaces |
| POC-23 | CORE | tentative pin | pin is durable before vote/prune; one orphan releases it and cannot be signed |
| POC-24 | CORE | version isolation | one incompatible node/supervisor handshake refuses OCOMP while blocks continue |
| POC-25 | DEMO | active generation authority | finalized state and replay select the same `ActiveGenerationV1`, independent of supervisor storage |
| POC-26 | COMPAT | fork boundary | pre-fork, active-PoC, empty and ineligible WWD cases follow their parent-defined paths |

## 15. Implementation surfaces to plan

This is an inventory for the next planning step, not a task breakdown.

### 15.1 Protocol and state

- PoC fork/profile constants;
- internal `OcompLifecycle` module owning finality binding, OCOMP job-state
  transitions, expiry timing and terminal commit ordering, without domain write
  authority;
- `ProtocolBundleV1` registration and canonical object registry;
- `JobIntentV1`, IDs, state records, due/expiry indexes, budget split and
  activation preconditions;
- `ActiveGenerationV1` and terminal receipt identity;
- terminal Metadosis request command and event;
- historical committee and finality proof verification;
- static OCOMP committee/key registry;
- activation transaction codec and terminal receipt.
- reproducible PoC build/deployment and network/capacity manifests.

### 15.2 Pure semantics

- concrete `LysisProgramV1` module owning input completeness, planning,
  execution, reduction and typed result verification;
- storage-independent `execute_lysis`;
- canonical authenticated input bundle;
- `PlanCommitmentV1`, bounded `ResultChunkV1` and constant-size
  `LysisResultV1`;
- offline independent reference implementation/golden corpus;
- conservation and arithmetic checks.

### 15.3 Compute plane

- UDS `OcompControlV1` client/server;
- finalized cursor and local supervisor journal;
- immutable checkpoint handoff and exporter;
- local CAS and bounded artifact codecs;
- deterministic planner, worker unit runner and fixed reducers;
- fixed unprivileged worker service template.

### 15.4 Attestation and relay

- separate PoC OCOMP keys and proof-of-possession registration;
- `OcompAttestationGate`;
- durable sign-once journal;
- candidate announcement/relay adapter;
- certificate builder and activation submission.

### 15.5 Certified activation

- concrete `CertifiedLysisApply` module; no generic program/write dispatcher;
- verifier and private `CertifiedLysisActivation`;
- `NodBatchReceipt`;
- `ContributorReceipt`;
- `TributeReceipt` plus logical retirement;
- request-phase `RequestBudgetSplitReceiptV1`;
- activation `CarryOverReceiptV1`;
- aggregate receipt equations, conflict outcome and terminal events.

### 15.6 System harness

- four independent validator deployments;
- input-shape and maximum-block generator;
- fault/tamper controls;
- trace proving no on-chain Lysis calls;
- public state query and golden comparison;
- repeatable acceptance report for section 13.

## 16. Recommended vertical slices

The source design defines six independently testable slices:

1. **Pure semantics** — extract `execute_lysis` and match existing golden
   fixtures.
2. **Real request** — terminal Metadosis creates/expires `JobIntent` and removes
   synchronous Lysis for the PoC profile.
3. **One validator domain** — exporter, supervisor and workers discover,
   authenticate and compute a candidate with no direct state write.
4. **Certificate** — four domains use separate OCOMP keys; the untrusted relay
   can form evidence only from three identical digests.
5. **Real activation** — verify and atomically apply every observable domain
   effect through the closed capability/receipt path.
6. **System demonstration** — pass section 13 including one offline domain,
   tampering, worker-count determinism and expiry without fallback.

Each slice is tested at the external seam used by the next. In-memory adapters
are allowed in module tests; the final slice must use real UDS, processes,
consensus blocks and public APIs.

## 17. Deliverables and PoC completion checklist

### 17.1 Protocol artifacts

- [ ] canonical PoC bundle and frozen hash;
- [ ] canonical bytes and hash vectors for every PoC object;
- [ ] consensus request/FSM/expiry/finality/activation state;
- [ ] static committee and OCOMP key registry;
- [ ] normal RPC transaction and public read schemas.
- [ ] immutable PoC build/deployment and network/capacity manifests.

### 17.2 Runtime artifacts

- [ ] node control/attestation endpoint;
- [ ] standalone supervisor;
- [ ] standalone checkpoint exporter;
- [ ] standalone deterministic worker;
- [ ] local CAS;
- [ ] replaceable untrusted relay;
- [ ] four-domain devnet deployment.

### 17.3 Semantic artifacts

- [ ] pure `execute_lysis`;
- [ ] independent reference/golden implementation;
- [ ] bounded result chunks plus constant-size typed result commitment;
- [ ] certified activation module;
- [ ] one request split receipt and four typed activation receipts;
- [ ] logical Tribute retirement.

### 17.4 Evidence

- [ ] every POC-01..POC-26 matrix row passes;
- [ ] exact 13-step acceptance story passes;
- [ ] cap generator fixes the actual fork constants;
- [ ] negative trace proves no on-chain Lysis/Fidelity/Oracle calculation;
- [ ] report records software revision, bundle hash, genesis, validator
      identities, test seed, machine class and produced artifact hashes.

PoC is not complete while any checkbox is replaced by a direct executor hook,
mocked validator domain or undocumented manual step.

## 18. Current-code baseline and known gaps

At the source revision:

- no production `JobIntent`, OCOMP state/codec, finality proof,
  `OcompControl`, supervisor, exporter, worker, attestation gate, certificate
  verifier or certified activation module exists;
- current terminal Metadosis still calls Lysis synchronously and performs
  Nod/contributor/Tribute/Desis/Promis/completion effects
  ([runtime.rs](crates/core/metadosis/src/runtime.rs#L378));
- current `nodfactory::api::issue_nod` has no unforgeable certified-Lysis
  authority, and Desis acceptance is a `bool`;
- `outbe-chain` runs Reth and Commonware in one process
  ([main.rs](bin/outbe-chain/src/main.rs#L272));
- one mandatory projection failure currently requests node shutdown
  ([main.rs](bin/outbe-chain/src/main.rs#L681)); OCOMP must not copy this failure
  boundary;
- an external TEE sidecar shows that external processes and authenticated
  UDS/TCP are accepted patterns, but its reconnect behavior is not sufficient
  for OCOMP;
- there is no separate OCOMP key registry or anti-equivocation store;
- current CE has exact 16-shard CKB roots but no counted range tree; the latter
  is not needed for the PoC full-fold over bounded pages;
- Fidelity has per-owner counts but no enforced profile cohort ceiling;
- current Lysis materializes the complete WWD and reads Fidelity twice per
  Tribute ([runtime.rs](crates/core/lysis/src/runtime.rs#L31));
- current CE physical retirement enumerates and deletes every prefixed record
  ([persistence.rs](crates/core/compressed-entities/src/persistence.rs#L1873));
  the PoC certified path needs logical retirement;
- large-state proof, DA/custody, witness state, snapshot/delta recovery and
  contributor-claims mechanisms do not exist and are outside PoC.

The PoC starts from a fresh devnet genesis already prepared for its bundle.
Supported-network migration and the full upgrade FSM are not prerequisites for
the PoC milestone.

## 19. Explicitly deferred work

### 19.1 BoundedMVP

- governed preparation/arming/activation and mixed-version operation;
- complete ADR/PFS ownership and release-gate evidence;
- changing/historical result committees and key handover;
- HSM/remote signer and operational key lifecycle;
- remote mTLS control adapter;
- launch broker and aggregate worker leases;
- full checkpoint pin/export/pruning/bootstrap recovery;
- fair multi-job queues and resumable phase journals;
- production CAS retention/replication;
- complete isolation, chaos testing, SLOs, dashboards and runbooks;
- signed release/network manifests and production supply-chain evidence;
- cold benchmark defining a supported bounded cap.

### 19.2 TargetLarge and the billion-record profile

- `CountedRangeTreeV1`;
- recursive proof-carrying Lysis;
- DA encoding, custody certificates, repair and handover;
- root-authoritative output without full action bytes in the block;
- witness-based active Nod state;
- snapshot/delta recovery;
- pull-based contributor claims and long-lived capacity accounting;
- billion-record cold failure/recovery benchmark.

None of these deferred mechanisms may be simulated and presented as a PoC
success claim.

### 19.3 Multi-program generalization

The following are also DEFERRED and are not prerequisites for the Lysis PoC:

- a consensus `ProgramId`, `ProgramSpec` or program registry;
- generic intent, unit, result, certificate or activation envelopes;
- a heterogeneous dispatcher or public `TaskAdapter`;
- cross-program scheduling, precondition and capacity policy;
- Nod/Gem program codecs, handlers, effects or acceptance tests.

Gem qualification is the preferred future litmus test, not a promised design.
It counts as a second program only after a separate domain ADR defines its
authenticated complete indexes, ordering, preconditions, typed result, private
apply authority, conservation/recovery rules and full end-to-end lifecycle.
Only the demonstrated intersection of Lysis and that program may become shared
wire or source interfaces.

## 20. PoC implementation precedents

These links are engineering examples, not evidence that the Outbe design is
correct:

| PoC mechanism | Primary reference | Implementation example | What is reused |
|---|---|---|---|
| node/compute process split and a narrow authenticated API | [Ethereum node architecture](https://ethereum.org/developers/docs/nodes-and-clients/node-architecture), [Engine API authentication proposal](https://github.com/ethereum/execution-apis/issues/162) | [Ethereum execution APIs](https://github.com/ethereum/execution-apis) | independent processes, authenticated/versioned control boundary |
| explicit capability negotiation | [Engine API common definitions](https://github.com/ethereum/execution-apis/blob/main/src/engine/common.md) | `engine_exchangeCapabilities` | exact interface/bundle intersection without changing consensus |
| write-before-sign anti-equivocation | [EIP-3076 slashing protection](https://eips.ethereum.org/EIPS/eip-3076), [EIP-3030 remote signer](https://eips.ethereum.org/EIPS/eip-3030) | [Web3Signer](https://github.com/Consensys/web3signer) | durable signing history and key isolation; OCOMP subjects remain different |
| process/resource isolation | [Linux cgroup v2](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html), [Landlock](https://docs.kernel.org/userspace-api/landlock.html) | [OCI runtime spec](https://github.com/opencontainers/runtime-spec/blob/main/config-linux.md), [Kubernetes resource limits](https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/) | protect consensus resources from compute failures |
| deterministic unit retry and fixed reduction | [MapReduce paper](https://research.google.com/archive/mapreduce-osdi04.pdf) | [Apache Hadoop MapReduce](https://github.com/apache/hadoop/tree/trunk/hadoop-mapreduce-project) | stable semantic units, external sort and retry without result reorder |
| independent execute then endorse | [Hyperledger Fabric execute-order-validate](https://hyperledger-fabric.readthedocs.io/en/latest/whatis.html) | [Fabric endorsement policies](https://hyperledger-fabric.readthedocs.io/en/release-2.5/endorsement-policies.html) | multiple domains execute before a certificate authorizes apply |
| authenticated collection commitments | [Trillian verifiable log](https://github.com/google/trillian) | [CKB sparse Merkle tree](https://github.com/nervosnetwork/sparse-merkle-tree) | distinguish an authenticated root from untrusted body transport |

The PoC does not adopt any reference wholesale. Its codecs, finality proof,
input-completeness rule, result digest, threshold and typed domain effects remain
Outbe protocol definitions.

## 21. Traceability to the source design

| Source section | PoC disposition | Covered here |
|---|---|---|
| 1, 1.1–1.4 | PoC-MUST | 1, 3, 5, 9, 10 |
| 1.5 PoC envelope | PoC-MUST | 4 |
| 1.6 demonstration | PoC-MUST | 13 |
| 1.7 PoC versus MVP | PoC-MUST boundary | 2, 11, 19 |
| 1.8 implementation slices | planning input | 16 |
| 2.1 request block | PoC-MUST | 5.1 |
| 2.2 finality/discovery | PoC-MUST; recovery hardening deferred | 5.2–5.3, 6.2 |
| 2.3 activation block | PoC-MUST; pause/upgrade/custody deferred | 5.4–5.5, 10 |
| 3 Tribute root | PoC-MUST | 6.1 |
| 4 runtime boundaries | process split PoC-MUST; hardening MVP | 3, 12 |
| 5 interfaces | UDS/local CAS PoC-MUST; mTLS deferred | 7 |
| 6 planner/units | bounded rules PoC-MUST; counted ranges deferred | 8 |
| 7 Lysis Map/Reduce | PoC-MUST | 8 |
| 8.1 input authenticity | PoC-MUST full fold | 6 |
| 8.2 independent correctness through bounded work | PoC-MUST | 9 |
| 8.3 proof execution | DEFERRED TargetLarge | 19.2 |
| 8.4 anti-equivocation | result-signature subset PoC-MUST | 9.2 |
| 9 availability | bounded block/source retention subset | 6, 10 |
| 10 state machine | PoC-MUST; governed cancel is scaffold | 5 |
| 11 admission | PoC per-interface-bounds subset; no population cap | 4, 5.1 |
| 12 failure behavior | selected PoC failures | 12, 14 |
| 13 trust boundaries | PoC-MUST | 3, 7, 9 |
| 14 profiles | boundary | 11, 19 |
| 14.1 transition to MVP | PoC-SCAFFOLD | 11.1 |
| 14.2 protocol bundle | PoC-MUST subset | 11 |
| 14.3–14.7 supported rollout/release gate | DEFERRED MVP | 19.1 |
| 14.8 steps 1–3 | PoC milestone | 15, 16 |
| 15 closure tests | section 1.6 is PoC; rest promotion work | 13, 14, 19 |
| 16 direct answers | incorporated throughout | 1, 3, 5–10 |
| 17 precedents | planning input, not acceptance evidence | 20 |
| 18 current gaps | planning input | 18 |

## 22. Decisions to resolve as the first planning work

These are bounded implementation parameters, not unresolved core architecture.
The implementation plan must create an explicit decision/freeze task for each
applicable item before any dependent consensus or runtime task starts:

1. exact PoC fork ID, protocol version, bundle hash and genesis;
2. generated final per-shard/per-chunk/evidence/activation/block byte and work
   caps, with no total Tribute cap;
3. exact canonical fields and precondition/budget formulas for
   `PreAdmissionEnvelopeV1`, activation preconditions, `PlanCommitmentV1`,
   `ResultChunkV1`, `LysisResultV1`, `ActiveGenerationV1` and terminal
   receipts;
4. canonical codec library, hash/signature preimages and golden-vector format;
5. exact legacy Lysis arithmetic/state/event semantics and authoritative golden
   corpus;
6. finalized-intent proof production and historical committee data source,
   including whether existing public data suffices or a bounded read adapter is
   needed;
7. checkpoint API supported by the current Reth/MDBX integration;
8. local CAS layout, quota and cleanup rule;
9. exact systemd/container topology, UIDs, UDS paths and cgroup budget;
10. OCOMP key storage format and sign-journal durability primitive;
11. relay HTTP schema and public activation transaction encoding;
12. minimal logical-retirement representation and deferred GC boundary;
13. independent reference implementation technology;
14. trace mechanism proving no on-chain Lysis/Fidelity/Oracle calculation;
15. minimum devnet hardware class and measured headroom rule.

Sections 15–17 can therefore be converted into an implementation dependency
graph now: the section 22 decisions become its first blocking nodes, followed by
the dependent implementation slices. No core architecture choice needs to be
reopened during that decomposition.

## 23. Readiness review

Review date: 2026-07-23

Verdict: **READY_FOR_IMPLEMENTATION_PLANNING**

The review checked:

- every PoC-specific source section and every required dependency in sections
  1–18 of the source design;
- the complete system path from public Tribute issuance to public Nod reads;
- request/finality/input/compute/certificate/activation/apply/expiry boundaries;
- PoC versus BoundedMVP/TargetLarge classification;
- current source-code entry points and implementation gaps;
- presence of all named PoC protocol objects and all local code links;
- balanced Markdown structure and the full POC-01..POC-26 evidence matrix.

No missing core architecture decision remains. Section 22 contains concrete
schema/integration/cap choices that must become the first blocking tasks in the
implementation plan. Therefore:

- the document may be used now to create the dependency graph and task set;
- pure-semantics, discovery and golden-corpus work may start once their own
  inputs are frozen;
- consensus codec/state/apply tasks may not start by inventing section 22
  values inside implementation;
- PoC completion still requires the exact section 13 demonstration and every
  section 14 evidence row.

The source design remains authoritative for anything explicitly deferred in
section 19.
