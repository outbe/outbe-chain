# Outbe off-chain computation — PoC specification

Date: 2026-07-26

Status: implemented on `feat/ocomp-poc`; exact public/E2E/isolation closure
evidence pending. The superseded relay, digest-only-vote and separate-activation
paths are not part of the runtime protocol. Section 22 values are generated
from the checked-in registries and must pass the freeze checks before PoC
closure.

Source: [`off-chain-computation.md`](off-chain-computation.md), SHA-256
`7c56e04c3e42d8803b09af5329f3a052ca2f15b0fcddbaf341d3f50647f9b9d9`

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
  -> Metadosis creates JobIntent for lysis_budget, marks the day
     OFFCHAIN_PENDING and stores job AWAITING_FINALITY
  -> the request block finalizes
  -> four additional blocks elapse
  -> consensus records VOTING_OPEN(open_height, deadline_height)
  -> four validator domains independently read and execute Lysis off-chain
  -> every Supervisor submits one signed full LysisResultV1 transaction on-chain
     through the validator-only result-vote ZeroFee path
  -> the third matching vote atomically records quorum, stores the canonical
     result once and verifies it without executing Lysis or iterating outputs
  -> Nod/contributor roots and Tribute/carry-over/Metadosis effects commit in
     that same transaction checkpoint
  -> Metadosis is COMPLETED and the Tribute partition is logically retired
  -> the fourth vote slot remains open until the response deadline
     for 4/4, missing, divergent and equivocation evidence
```

There is no synchronous fallback. If fewer than three validator domains produce
the same result before the deadline, the attempt expires and the day returns to
`READY` with the same Lysis budget and request-phase auction effect. No Nod is
created and the auction is not repeated.

The central architectural seam is:

```text
off-chain:
  execute_lysis(JobSpec, AuthenticatedInputBundle)
    -> ResultChunkV1 catalog + RootReduceSummaryV1
  finalize_lysis(verified authority, exact bounded cursors, summary)
    -> LysisResultV1

on-chain:
  open_voting(finalized JobIntentV1, finality_recorded_height + 4)
    -> VOTING_OPEN(open_height, deadline_height)
  submit_result_vote(VOTING_OPEN, ResultVoteV1 { result: LysisResultV1, ... })
    -> first/second match: bounded four-slot accountability state
    -> third match: quorum + one stored result + atomic typed root transition
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
has exactly one public result entrypoint, `submitLysisResult`, whose
`ResultVoteV1` carries the canonical `LysisResultV1`. `JobIntentV1`,
`UnitSpecV1`, `LysisResultV1`, `ResultVoteV1` and `ProtocolBundleV1` are
Lysis-specific even where the names look generic. There is no public
`activateLysis`, activator or post-quorum result delivery. A consensus program
registry, common program envelopes, public adapter and second domain program
are not PoC deliverables.

### 1.1 What the PoC proves

- real consensus `JobIntent`, lifecycle, finality binding and expiry;
- real separation between node, supervisor, exporter and workers;
- authenticated input reconstruction from a finalized checkpoint;
- deterministic parallel execution independent of worker count and order;
- four independent validator domains, with a `3-of-4` on-chain result threshold;
- separate OCOMP keys and durable sign-once behavior;
- consensus-visible response timing, fourth-vote and equivocation evidence;
- q-forming atomic result apply with no trusted relay or separate activator;
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
| chain state | `JobIntentV1`, Metadosis `OFFCHAIN_PENDING`, OCOMP `AWAITING_FINALITY`/`VOTING_OPEN`, expiry and terminal receipt are consensus state |
| input | exact finalized request block, state root, CE root and sealed WWD root |
| processes | node, supervisor, snapshot exporter and workers are separate OS processes |
| execution | each of four validator domains executes the complete job through bounded work |
| parallelism | deterministic units and fixed reduction, exercised with 1, 2 and 4 workers |
| correctness | exactly three distinct eligible on-chain vote slots over one canonical `ResultDigest` |
| keys | separate OCOMP key per validator; the supervisor never receives OCOMP or validator EVM private keys |
| evidence | four bounded vote slots plus consensus-owned finality binding and one canonical full result stored at quorum |
| vote submission | Supervisor owns prepare/submit/inclusion/finality/reorg workflow; it uses canonical `latest` nonce plus the frozen gas envelope, never `eth_estimateGas`/pending execution, and rebroadcasts exact journaled bytes; exact-selector validator ZeroFee waives fee, while OCOMP alone validates protocol |
| quorum apply | the third matching public full-result vote applies inside its normal block execution |
| apply | private capability, closed root-transition APIs, typed receipts and one outer checkpoint |
| failure | no quorum means deterministic expiry, preserved budget, no repeated auction and no Nod |
| output | the request split and all observable Nod/contributor/Tribute/carry-over/Metadosis effects are real |

### 2.2 Permitted weak implementations

These are PoC-SCAFFOLD, not architectural shortcuts:

- a fresh disposable base genesis followed by one canonical chain manifest
  prepared for exactly one fork profile and bundle;
- at most two simultaneously live `LYSIS_V1` Jobs, with one live attempt per
  WorldwideDay and independent FSM progress;
- a static four-member result committee;
- protected local OCOMP key files;
- a simple durable local sign-once journal;
- one local pinned checkpoint whose export may be recomputed after restart;
- a per-validator filesystem content-addressed store;
- a fixed unprivileged local worker template and basic cgroup limits;
- direct public result-vote submission by each validator-domain Supervisor
  through the closed validator ZeroFee hook;
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

There is no required relay service. Each validator domain submits its signed
result vote through the normal public transaction path. Any client may
rebroadcast identical signed bytes, but no off-chain collector chooses the
result, forms consensus evidence or performs a later activation.

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
| public submitter | canonical signed vote/typed result bytes | ordinary public transaction submission | never | may delay/drop its own delivery, but cannot choose quorum or mutate another validator's slot |
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
| required matching on-chain votes | `q=3` |
| `MAX_TRIBUTES_PER_WORK_SHARD` | `256` |
| workers per validator | `1..4` |
| `MAX_RECORDS_PER_INPUT_CHUNK` | `768` |
| candidate `MAX_INPUT_CHUNK_BYTES` | `1 MiB`, generated before fork |
| candidate `MAX_RESULT_CHUNK_BYTES` | `512 KiB`, generated before fork |
| candidate `MAX_RESULT_SUMMARY_BYTES` | `1 MiB`, generated before fork |
| `MAX_PENDING_JOBS` | `2` |
| intents per block | `1` |
| result-vote slots per job | `4` |
| q-forming result applies per block | `1` |
| distinct reference currencies | `8` |
| Fidelity cohorts per owner | `64` |
| result deadline | `64 blocks`, exclusive |

The table is a starting envelope, not a capacity claim. Before the fork is
enabled, a generator must construct the maximum-shaped:

- input and result chunks;
- constant-size `LysisResultV1`;
- bounded full-result `ResultVoteV1`, four-slot state and accountability summary;
- finalized-job binding already retained in consensus state;
- non-q-forming and q-forming result-vote transactions;
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

Generated caps bound one chunk/work invocation, the live worker pool and each
constant-size full-result vote transaction. The selected values must fit transaction
bytes, the complete RLP block, gas/internal-work, root-transition work and
finality budgets with measured headroom on the declared minimum devnet machine.
Local configuration may never raise these per-interface bounds or reinterpret
them as a total population ceiling.

An immutable PoC build/deployment manifest records the tested node, supervisor,
exporter and worker artifacts. A PoC network manifest records every
compatible RPC, transaction-input, txpool, P2P, block-body, gas,
internal-work/job/byte/count limit and the minimum devnet hardware class. Its
consensus-bearing `OcompForkInstallV1` binding contains the classification,
`AtBlock(H)`, complete request profile, exact protocol bundle and complete
result committee. It is generated after the base genesis hash exists, loaded
once before node startup and used unchanged by proposer, importer, historical
replay, consensus and txpool. CLI/environment overrides and runtime reload are
forbidden. These manifests are reproducibility artifacts, not the signed
BoundedMVP release gate; the smallest recorded layer limit is the protocol cap.

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
4. freezes the split, exact Lysis scalars and live apply preconditions;
5. stores `JobIntentV1` for exactly `lysis_budget`;
6. stores OCOMP job state `AWAITING_FINALITY`;
7. changes the Metadosis day to `OFFCHAIN_PENDING`;
8. emits `OffchainJobRequested(IntentId)`;
9. returns without creating a response deadline or executing Lysis.

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
  apply_preconditions,
  result_committee_snapshot_hash,
  custody_committee_epoch_hash
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
a fixed backoff so it cannot starve later eligible work. The PoC permits two
simultaneously live Jobs, still creates at most one intent per block and uses
this bounded index rather than scanning all days.

`PreAdmissionEnvelopeV1` is authenticated consensus state, not a value supplied
by the supervisor. For PoC it commits at least the sealed Tribute count,
canonical-body bytes, applicable profile and conservative upper bounds for
Fidelity/Oracle openings, outputs, full-result vote bytes and retention. The fresh
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

The consensus finality-ingestion and attestation paths must:

1. verify chain, genesis, fork, bundle and canonical header hash;
2. verify height/hash, proof kind and empty missed-proposer list;
3. resolve committee and cryptographic material from authenticated historical
   committee state, never from caller bytes alone;
4. verify the exact Commonware finalization subject, certificate, bitmap and
   quorum using the bundle-pinned verifier;
5. verify the fixed OCOMP account/storage path against the request state root;
6. decode the exact canonical intent and recompute `IntentId` and `JobId`;
7. compare nonce, attempt, deadline and apply preconditions with live
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
Metadosis READY
  -> OFFCHAIN_PENDING(IntentId)

OCOMP AWAITING_FINALITY(IntentId, intent_height)
  -> VOTING_OPEN(
       JobId,
       finality_recorded_height,
       open_height = checked_add(finality_recorded_height, 4),
       deadline_height = checked_add(open_height, response_window_blocks)
     )

VOTING_OPEN
  -> COMPLETED(ResultDigest, quorum_height)
  -> EXPIRED
  -> CONFLICTED
  -> CANCELED

EXPIRED | CONFLICTED | CANCELED
  -> READY(next_pending_nonce)
```

`DISCOVERED`, `ADMITTED`, `RUNNING` and `LOCALLY_VERIFIED` are local supervisor
states only.

The existing consensus-certified finalization path is the only authority that
can bind `JobId` and record `finality_recorded_height`. Exactly four blocks
later, begin-zone atomically installs `VOTING_OPEN` and inserts the
response-deadline index ordered by `(deadline_height, JobId)`. Finality,
open-height and deadline arithmetic use checked addition.

`MAX_OCOMP_EXPIRATIONS_PER_BLOCK` is at least `MAX_PENDING_JOBS`; for PoC it can
therefore close both concurrently pending vote windows at their deadline.

`deadline_height = H` is exclusive:

- result votes are timely only in blocks `< H`;
- begin-block `H` closes the four-slot accountability summary before ordinary
  transactions;
- a `VOTING_OPEN` job without quorum expires and requeues;
- a `COMPLETED` or `CONFLICTED` job retains its terminal quorum outcome;
- a result vote included in `H` is late and cannot change the closed summary.

When a third matching first vote is included before `H`, that transaction
already carries the canonical `LysisResultV1`. Inside one outer checkpoint the
job records the immutable quorum digest/height/bitmap/evidence, stores the
canonical result once, verifies it and commits the certified effects. It becomes
`COMPLETED`, or records the defined `CONFLICTED`/retry outcome with no owner
effects when target preconditions changed. The fourth validator's slot remains
open until `H`; deadline close removes only the response index and closes
accountability.

Persisted state must maintain:

```text
Metadosis WWD OFFCHAIN_PENDING
  <=> exactly one live IntentId for current pending_nonce

AWAITING_FINALITY
  <=> no response-deadline entry and no admissible signature/vote

open response window
  <=> exactly one response-deadline entry until its window closes
  <=> exactly one immutable budget split and apply-precondition set

closed response window
  <=> exactly one closed OcompVoteAccountabilityV1 and no response-deadline entry

COMPLETED
  <=> exactly one immutable quorum/result, LysisTerminalV1, apply receipt and active result identity

READY
  <=> exactly one due-index entry and no live current-nonce intent
```

Consensus stores the full constant-size `LysisResultV1` once only when q=3 is
formed; four vote slots retain only digest/signature/height accountability.
Result chunks remain outside consensus state. There is no persistent
`QUORUM_READY` or `DATA_AVAILABLE` state.
`CANCELED` is reserved in the closed codec for the MVP pause/revocation path;
PoC need not implement governed pause.

### 5.5 Block ordering preserved for evolution

The PoC implements these stable phase slots:

```text
pre-execution:
  0. at H, activate protocol v1 and initialize owner pre-admission profiles
begin-zone:
  1. at H only, atomically install the chain-manifest-bound request profile,
     protocol bundle and complete result committee
  2. reserved future pause/revocation barrier
  3. consume consensus-recorded finality; open jobs whose
     finality_recorded_height+4 is due; then close response windows and expire
     only no-quorum jobs due at this height
ordinary transactions, including submitLysisResult
CE sealing
terminal READY inspection/request creation
commit; no later semantic writer
```

The install reuses the existing empty-body `OcompLifecycleBegin` envelope; it
does not add a SystemTx or change its ABI. Exact replay is idempotent, while a
partial or different authority is fatal. The existing protocol-version-1
Update handler initializes Tribute/Fidelity/Oracle/Metadosis pre-admission
profiles at the same height in deterministic pre-execution hooks. The
receipt-visible `OcompLifecycleBegin` install/expiry phase and `CycleTick`
follow; owner mutations are not duplicated in the install slot.

MVP may populate the reserved revocation and pause barrier without moving
installation, response-window close, quorum apply or request creation. This is how the PoC core
evolves without changing its lifecycle ordering.

Result-vote dispatch handles terminal retries before the live-job guard:

1. exact retry of an already `COMPLETED` binding/digest returns its recorded
   receipt without effects;
2. `COMPLETED` with different binding or digest rejects;
3. `EXPIRED` or `CANCELED` rejects;
4. a live `VOTING_OPEN` submission can enter result verification/apply only
   when recording it creates the third matching slot.

Result-vote dispatch remains available in `COMPLETED` and `CONFLICTED` until
the response deadline. It may fill the missing fourth first-vote slot or record
the first conflicting signed vote for any already filled slot in the separate
bounded `OcompVoteAccountabilityV1`. It never re-enters apply, replaces a
first vote, changes the selected result, mutates `LysisTerminalV1`, changes the
apply receipt/active-generation hash or changes exact-retry identity.

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
- sealed bindings and live apply preconditions reject conflicting writes;
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

For each PoC Job, a validator domain uses one immutable checkpoint:

```text
(checkpoint_id, finalized_block_hash, state_root, ce_root, schema)
```

Before a node advertises or votes for a candidate block containing an intent,
and before pruning may pass that height, it must durably record the corresponding
`TENTATIVE` Job entry and Authenticated Input Lease reference. This is retention
bookkeeping only; semantic computation still waits for finality. A pre-finality
reorg releases that exact Job reference and makes every result from that fork
non-signable.

When the exact request becomes finalized, the node's asynchronous finality
worker promotes the durable pin and pre-arms the O(1) opaque read-only
capability before the live CE finalized marker advances. The consensus
finalization callback only enqueues this work; CE lease creation, proving and
disk I/O never run inline in the consensus actor. The same already armed
handoff remains available if the Supervisor starts or restarts later. A late
Supervisor must consume that exact handoff and must not recreate an old
snapshot from current live state.

The capability is available only to the exporter UID. The node never exposes a
live MDBX/Reth writer or accepts an arbitrary filesystem path from the
supervisor.

The exporter:

1. resumes a bounded page cursor;
2. full-folds the complete current 16-shard CKB WWD tree;
3. verifies every canonical Tribute body;
4. exports and verifies every required Fidelity and Oracle state opening;
5. verifies exact count/nominal and rebuilt collection root;
6. writes a canonical authenticated input bundle to CAS.

Opening transport has no total-owner ceiling. Sorted owners are first divided
into consecutive groups of at most 256. If one canonical proof exceeds the
bundle-pinned local-control body cap, the node returns typed `LimitExceeded`
without dropping the session and the exporter deterministically bisects that
group, left half first, until each response fits. The settlement ISO set is
unchanged in every sub-request. A single owner that still cannot fit causes
that validator domain to abstain; the limit is not increased or bypassed
dynamically.

PoC requires a bounded durable multi-job registry and a separately addressed
Authenticated Input Lease Registry. A terminal predecessor, its retry and
unrelated Jobs must coexist and progress independently. Retry creates a new
exact `JobId` but may reuse an existing lease only when every authenticated
source commitment is identical. Full production-scale repair and operational
recovery remain DEFERRED to MVP. If a Job entry, lease, checkpoint or journal is
ambiguous, the validator abstains only for affected work; it must never guess,
use live state or globally replace another Job.
Insufficient local disk or compute may change that validator's admission and
attestation decision, but it cannot change consensus job eligibility or the
authenticated job contents.

The minimum local lifecycle is:

```text
TENTATIVE(IntentId, candidate block/state root, InputLeaseId)
-> FINALIZED(JobId, finality_recorded_height, InputLeaseId)
-> VOTING_OPEN(open_height = finality_recorded_height + 4,
               deadline_height = open_height + response_window_blocks)
-> EXPORTED(authenticated input bundle)
-> RELEASED only after terminal finality and retention rule

TENTATIVE -> RELEASED when candidate is orphaned
```

Lease GC is separate: retained source bytes are deleted only after every Job
referencing the same `InputLeaseId` has released and the maximum retention
height has finalized.

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
  -> roots, exact counts, totals and the frozen empty pre-result event root
```

Both shuffle phases emit a Lysis-specific `ShuffleRunArtifactV1` tree. A leaf
contains at most 256 ordered records; a node contains exactly two verified CAS
child references. The bounded root is carried by the producing
`UnitArtifactV1`, while descendants stay as bounded content-addressed objects.
Canonical page spans and splits make the same input produce byte-identical
trees under any worker count. The tree grows with the number of Tribute; the
protocol does not reject or truncate a 257th, 10,000th or billionth Tribute.

For `K` primary runs, each shuffle execution DAG contains exactly `K` leaf
units and `K - 1` real binary merge units. An odd run is consumed directly by
the next real merge; no shuffle `CanonicalEmpty` or copy-only promotion unit
exists. Each real merge reads two verified materialized producer runs and
writes a fresh local page tree under its own `UnitId`; producer roots are not
reused as pages of the merged result. The valid all-excluded case produces one
canonical empty owner leaf with complete raw source coverage.

Source coverage and output order are separate commitments. A shuffle leaf
binds the exact raw-coverage root/count recomputed from its
`OUTPUT_FINALIZE` producer. A real merge hashes the two adjacent producer
coverage commitments under `OUTBE_OCOMP_SHUFFLE_SOURCE_COVERAGE_V1`. The
materialized page tree independently commits the sorted owner/bucket output.
This avoids both population-sized padding work and a fictitious promotion job.

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

Each bounded `OUTPUT_FINALIZE` shard therefore carries one checked
`tribute_nominal_total` over all of its `AmountRecordV1` inputs, including
excluded Tribute. The value is stored once per shard, not once per finalized
record. The supervisor deterministically re-executes the phase before
admission; `ROOT_REDUCE` checked-adds the verified shard subtotals and the typed
finalizer requires the result to equal
`InputManifestV1.tribute_nominal_total`.

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

## 9. Result, on-chain votes and sign-once rule

Workers never manufacture the final signing subject. The final
`ROOT_REDUCE` worker emits bounded `RootReduceSummaryV1`. Once every planned
artifact and result chunk has been durably verified, the supervisor hosts the
closed pure `LysisProgramV1` finalizer. It streams artifacts in exact plan order
and chunks in exact chunk order, reloads the canonical finalized intent/export
binding, derives every result root and either emits `LysisResultV1` or abstains.
The supervisor cannot pass a ready-made root or result into this interface.
In particular, optional contributor records cannot be used to infer
`tribute_nominal_total`, because they intentionally omit excluded Tribute.

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

ResultDigest =
  H("OUTBE_OCOMP_RESULT_V1", canonical(LysisResultV1))

ResultVoteV1 {
  protocol_bundle_hash, JobId, attempt,
  result_committee_snapshot_hash,
  validator_index, key_epoch,
  result: LysisResultV1,
  signature_over_ResultDigest
}
```

For LYSIS_V1, `exact_input_and_output_counts.semantic_event_count` is exactly
zero and `event_summary_hash` is the canonical empty list-kind
`SemanticEventRecords` root. This signed pre-result commitment is distinct from
the post-apply receipt summary: `APPLIED` hashes four validated owner
state-event digests in the fixed order Nod, Contributor, Tribute, CarryOver;
`CONFLICT_RESOLVED` hashes an empty payload under the apply-event-summary
domain. The values are never compared.

Every validator domain's separate compute process independently reads every
authenticated result chunk through that typed finalizer, proves gap-free
complete catalog coverage, and recomputes roots, counts, totals, arithmetic
commitment plus the frozen empty pre-result event root before requesting its
node's attestation. The node
reloads finalized authority, verifies the constant-size closed signing subject
and never scans those chunks or reruns Lysis. Every `ResultVoteV1` carries the
same canonical `LysisResultV1`; result-chunk bodies remain content-addressed artifacts for
projection, availability and proof serving. No chunk is separately signable or
applicable.

### 9.1 Committee and threshold

`OcompCommitteeSnapshotV1` is static consensus state for the PoC. It contains
ordered validator identities/indexes, unique OCOMP public keys, scheme, allowed
purpose, validity interval, proof of possession and key epoch.

The PoC uses canonical low-`s` `secp256k1` signatures. Every result-vote
transaction contains the pinned committee snapshot hash, one validator index,
its key epoch, one `ResultDigest` and one signature. A validator's first valid
vote contributes at most once.

For `n=4, q=3`, consensus scans exactly four first-vote slots. Once one digest
first appears in three slots it records an immutable quorum. Because a slot
never changes its tally digest, a later vote cannot create a conflicting
quorum. This does not protect against a common implementation bug, so reference
and differential tests remain mandatory.

The result committee and its votes are not consensus-finality votes. The same
four validator identities may appear in both on
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

### 9.3 On-chain vote slots and accountability

Each validator domain's `OffchainLysis Supervisor` sends its signed
`ResultVoteV1`, including the canonical full `LysisResultV1`, through normal
RPC, txpool, gossip, proposal, import and replay.
This replaces the superseded `publish_candidate(relay)` edge. The Supervisor
persists `prepared -> submitted -> included -> finalized` and rebroadcasts
after an orphaned inclusion while the slot remains empty and the window open.
A third party may rebroadcast the same signed bytes, but there is no required
relay or collector.

The PoC Supervisor reads the validator account nonce from canonical `latest`
state and uses the frozen bounded gas envelope. It does not call
`eth_estimateGas` or request a `pending` block build. Its single-writer journal
retains the exact node-signed raw transaction and nonce, so retry/reorg
rebroadcast does not reconstruct or re-sign the transaction.

The inner OCOMP signature and outer EVM transaction signature remain
node-owned; the Supervisor calls restricted signing seams and never receives
private keys. The outer transaction is fee-free for the validator through a
dedicated exact-selector, zero-value, bounded-envelope ZeroFee hook modelled on
Oracle. That hook decides only the fee waiver. The OCOMP on-chain module alone
checks `VOTING_OPEN`, JobId, height, committee, key epoch, bounded canonical
result, signature, slot and equivocation rules.

Consensus owns a separate bounded `OcompVoteAccountabilityV1` keyed by `JobId`
with four fixed `ResultVoteSlotV1` records:

```text
ResultVoteSlotV1 {
  validator_index,
  first_result_digest,
  key_epoch,
  first_signature,
  submitted_height,
  optional EquivocationEvidenceV1
}
```

The first eligible timely vote fills the slot. An exact retry is idempotent. A
different second valid signature does not replace the first digest and does not
count toward another tally; consensus records the first conflicting pair as
bounded equivocation evidence. Further distinct conflicting votes cannot grow
state and reject after signature verification.

After every accepted first vote, consensus groups the four slots by exact
digest. The first group reaching `q=3` atomically fixes:

```text
quorum_digest
quorum_height
quorum_signer_bitmap
quorum_evidence_hash
```

The submission that creates the first q=3 group supplies the canonical result
for the atomic apply. Consensus stores it once and transitions directly to
`COMPLETED`, or to the defined `CONFLICTED` outcome without owner effects. The
fourth slot remains open until the response deadline. At close,
`OcompAccountabilitySummaryV1` records timely, matching, divergent, missing and
equivocating bitmaps.

`LysisTerminalV1`, the apply receipt, active-generation hash, applied
domain state and exact retry identity exclude the mutable/closing
accountability fields. Late fourth-slot or deadline-close writes can change
only `OcompVoteAccountabilityV1`.

The PoC does not apply a monetary penalty. Missing response and equivocation
become objective inputs for a separate slashing policy. A minority digest is
retained but is not automatically slashable: `q=3` can still share a common
software defect.

### 9.4 Why there is no result relay

An off-chain relay that collects signatures before consensus would make
validator non-response indistinguishable from relay censorship. Relay logs and
mempool observations are not consensus evidence. Therefore the PoC has no
required relay process and no `ExecutionCertificateV1`.

The q-forming verifier uses the finalized JobIntent/JobId/attempt already owned
by consensus state. No activator resubmits a finalized-intent proof or chooses a
result.

## 10. Q-forming verification and apply without Lysis

Every validator transaction contains:

```text
ResultVoteV1 {
  protocol_bundle_hash,
  JobId,
  attempt,
  result_committee_snapshot_hash,
  validator_index,
  key_epoch,
  result: LysisResultV1,
  signature
}
```

For every submission, every node bounded-decodes `LysisResultV1`, binds it to
the live finalized job, reconstructs `ResultDigest` over the complete canonical
result and verifies the validator signature. When the submission forms q=3, the
same execution frame:

1. records the q-forming slot and immutable quorum evidence;
2. stores the canonical `LysisResultV1` once;
3. verifies committed roots, counts, conservation totals, arithmetic
   commitment, the exact LYSIS_V1 empty semantic-event commitment and every
   completion field against the finalized intent;
4. verifies vote byte/crypto/work caps and live old-root/generation
   preconditions;
5. constructs a private `CertifiedLysisActivation`;
6. installs the certified root transition and scalar effects in the same
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

The complete action bytes do not occur in the result-vote transaction.
Consensus state retains the active roots/generation, terminal hashes, counts,
quorum evidence hash and receipt. Result chunks supply bodies to projection and
proof-serving paths but never become consensus authority by location alone.

### 10.1 Capacity admission

The fork fixes one q-forming apply per block and maximum full-result vote
bytes, signatures, receipts and verification work. A valid transaction above a
block counter/byte limit receives a typed rejection and leaves the vote slot or
job live for later resubmission. The limit is checked before large decode or
cryptography.

### 10.2 Private apply authority

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

The q-forming apply module calls only:

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
generation/root installation; quorum apply does not submit or loop over a Nod
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

Before commit the aggregate module consumes all four apply receipts and
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
- logically retire the exact WWD;
- emit `LysisResultApplied` and canonical aggregate/domain events.

The response-deadline entry and separate `OcompVoteAccountabilityV1` remain
until the fourth-slot window closes; quorum apply does not erase participation
evidence or make the mutable accountability record part of terminal identity.

Any missing or mismatching receipt or write failure rolls back the entire
q-forming transition.

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
and immutable on-chain quorum evidence. `availability_certificate_hash` uses
the bundle's canonical `none` value: the PoC relies on the three quorum domains
having verified and retained the authenticated result chunks, but does not
claim an independent data-availability protocol. The active roots remain
consensus authority; a supervisor database or artifact location can never
select the active output.

### 10.3 Carry-over

`PromisLimit.total_unallocated` is the explicit carry-over accumulator. Request
credits RED `auction_base`; successful quorum apply credits `unused_lysis`.

Limit formation atomically consumes the available carry-over into the next
not-yet-formed day limit. A credit arriving after a day limit was formed waits
for the following unformed day.

Retry preserves `lysis_budget` without a second request-phase effect. A terminal
no-retry outcome credits the whole `lysis_budget` exactly once.

### 10.4 Time semantics

| Field/effect | Authoritative time |
|---|---|
| Fidelity/Oracle input, Metadosis scalars, Nod `issued_at` and semantic event fields | request `logical_evaluation_height/time` |
| result-vote timeliness and quorum | canonical vote inclusion/quorum block heights |
| apply receipt location and `applied_at` | actual q-forming block |

Delaying an otherwise valid q-forming vote may change only explicit inclusion
metadata. It must not change Nod economics, repeat the Desis brief or change the
signed carry-over credit.

### 10.5 Conflict and retirement

If a live apply precondition conflicts, certified effects do not apply.
The q-forming transition commits `ConflictResolved`:

- mark the old job `CONFLICTED`;
- remove the old expiry entry;
- increment the pending nonce;
- return the day to `READY` with the same `lysis_budget`.

The old evidence cannot be relabelled as a retry.

Invalid evidence rejects with no state change. A storage/invariant failure makes
the candidate block invalid.

The source WWD is logically moved to `RETIRED_RETAINED`; quorum apply does not
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
- q-forming apply order, receipt equations and error taxonomy;
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
-> typed deterministic finalization
-> q independent validator-domain signatures
-> q full-result validator transactions; the third matching one applies
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
| one supervisor/domain stopped | remaining three on-chain votes form `q=3`; close records one missing bit |
| two domains stopped | no fallback; response-window close expires and requeues |
| supervisor crash | node continues; supervisor can reconcile cursor/artifacts |
| worker crash/timeout | retry identical `UnitId` |
| exporter/CAS full or corrupt | local domain does not vote; node continues; deadline summary records absence |
| source body/opening unavailable | fetch another retained copy or do not vote; never sign an incomplete input |
| checkpoint/root mismatch | quarantine input; no computation/signature |
| artifact corruption/truncation/duplicate chunk | reject or recompute |
| request block reorgs before finality | release tentative pin; discard local work; no result is signable |
| vote before `finality_recorded_height + 4` | OCOMP module rejects; ZeroFee only controls fee debit and grants no protocol authority |
| node restarts during a job | consensus recovers normally; signing waits for finalized-state/journal reconciliation |
| node/supervisor bundle mismatch | refuse local OCOMP work; node continues validating the chain |
| Supervisor vote submission unavailable/censored | only canonical inclusion counts; Supervisor retries and another client may rebroadcast identical signed bytes |
| duplicate/wrong/late vote | idempotent or rejected without changing the first slot/tally |
| second conflicting signed vote | first tally vote remains; bounded equivocation evidence is recorded |
| fourth vote differs from quorum | minority digest is retained; no automatic slashing |
| fourth vote arrives after completion but before deadline | only separate accountability state changes; terminal/receipt/generation/retry identity stays byte-identical |
| wrong result byte/order/count/root | full-result vote rejects with no state change |
| wrong JobId/intent/finality binding | full-result vote rejects with no state change |
| second digest for same sign-once key | local attestation gate refuses |
| q-forming owner write failure | q-forming slot, quorum and all effects roll back |
| vote at deadline or later | late; cannot change the closed accountability summary |
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
   contributor, Tribute-consume or unused-Lysis carry-over effects; observe
   `VOTING_OPEN` exactly at `finality_recorded_height + 4` and prove earlier
   signing/voting rejects in OCOMP;
4. in the healthy workflow show all four domains independently produce and
   submit the same canonical `LysisResultV1`, yielding `4/4` evidence; prove the
   third matching submission atomically applies the result and the fourth
   changes only accountability, then finalize and verify the public result;
5. start a separately initialized workflow with a fresh WWD and `JobIntent`,
   repeat the request-only checks from steps 1–3, then stop one validator’s
   supervisor before execution; show the other three domains independently
   rebuild that workflow's input root and include three matching result-vote
   transactions;
6. observe the third matching full-result vote atomically record q=3 and apply,
   then later close the fourth slot as missing;
7. finalize the degraded workflow's q-forming block and query every expected Nod,
   contributor total, Metadosis state, request-phase Desis brief, carry-over
   credit and retired Tribute partition;
8. compare the result with an offline reference/golden corpus, never an on-chain
   Lysis execution;
9. repeat with 1, 2 and 4 workers and randomized completion order;
10. request a second digest signature for the same job and observe sign-once
    refusal; test exact vote retry, wrong signer/key epoch, late vote and a
    conflicting signed vote; prove only the first counts and equivocation is
    recorded;
11. include otherwise identical q-forming votes at different valid heights and
    prove byte-identical Nod/contributor/Tribute/carry-over results, with no
    repeated request-phase effect and only explicit inclusion metadata changed;
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
| POC-04 | CORE | finality/open authority | positive/adversarial `FinalizedIntentProofV1` vectors; `open_height = finality_recorded_height + 4`; pre-open sign/vote rejection and checked-overflow failure |
| POC-05 | CORE | event is not authority | one lost subscription followed by cursor discovery |
| POC-06 | DEMO | input completeness | full-fold root/count/nominal and state-opening verification |
| POC-07 | CORE | deterministic plan | frozen bytes/hashes for every PoC `UnitSpecV1` variant; 10,000 and 1,000,000,000 counts derive exact `ceil(N/S)` unit counts without proportional plan allocation |
| POC-08 | DEMO | worker independence | 1/2/4 workers and randomized retries/orders yield identical bytes |
| POC-09 | DEMO | validator independence | four separate node/supervisor/exporter/CAS domains |
| POC-10 | DEMO | quorum and fourth-domain evidence | healthy run closes `4/4`; one-domain-unavailable run reaches q=3 and closes exactly one missing bit |
| POC-11 | DEMO | two domains unavailable | response-deadline no-quorum expiry, release and requeue |
| POC-12 | DEMO | sign once | fsync-before-sign, exact retry allowed, different digest refused |
| POC-13 | DEMO | public vote/accountability binding | Supervisor submits the node-signed result through validator-only ZeroFee RPC/txpool/P2P/import/replay; no native fee is debited; OCOMP rejects pre-open/wrong/unknown/late votes; conflicting second signed vote records equivocation and never replaces the first tally vote |
| POC-14 | DEMO | result binding | the exact section 13 result-byte, JobId and ordering mutations reject |
| POC-15 | CORE | atomic apply | one representative injected owner-write failure yields the whole transition or none |
| POC-16 | CORE | receipt binding | one wrong-job binding and one mutated receipt reject atomically |
| POC-17 | DEMO | logical time | q-forming vote delay changes only explicit inclusion/apply metadata |
| POC-18 | CORE | voting window/deadline | voting opens exactly four blocks after finality; a pre-open vote rejects, a vote before the deadline counts, a vote at the deadline is late, no-quorum expires, and timely q=3 applies atomically |
| POC-19 | DEMO | process isolation | stop the supervisors required by section 13 without stopping block finality |
| POC-20 | CORE | bounded interfaces | one over-limit UDS message/chunk rejects before unbounded allocation; crossing a work-shard boundary creates another unit rather than rejecting the parent |
| POC-21 | FORK-GATE | public wire path | non-q-forming and q-forming full-result-vote cap-1/cap/cap+1 plus proposer/import/replay parity through RPC/txpool/P2P |
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
  apply preconditions;
- `VOTING_OPEN` finality+4 transition and response deadline;
- immutable `ActiveGenerationV1`/`LysisTerminalV1` plus separate bounded
  `OcompVoteAccountabilityV1`;
- terminal Metadosis request command and event;
- historical committee and finality proof verification;
- static OCOMP committee/key registry;
- result-vote/quorum/accountability state and public transaction codec;
- full-result vote codec, q-forming apply and terminal receipt.
- reproducible PoC build/deployment and network/capacity manifests.

### 15.2 Pure semantics

- concrete `LysisProgramV1` module owning input completeness, planning,
  execution, reduction and typed result verification;
- storage-independent `execute_lysis`;
- canonical authenticated input bundle;
- `PlanCommitmentV1`, bounded `ResultChunkV1`/`RootReduceSummaryV1`, pure typed
  finalizer and constant-size `LysisResultV1`;
- offline independent reference implementation/golden corpus;
- conservation and arithmetic checks.

### 15.3 Compute plane

- UDS `OcompControlV1` client/server;
- finalized cursor and local supervisor journal;
- immutable checkpoint handoff and exporter;
- local CAS and bounded artifact codecs;
- deterministic planner, worker unit runner and fixed reducers;
- fixed unprivileged worker service template.

### 15.4 Attestation and on-chain voting

- separate PoC OCOMP keys and proof-of-possession registration;
- `OcompAttestationGate`;
- durable sign-once journal;
- Supervisor-owned `ResultVoteV1` submission/reorg journal, restricted
  node-owned outer EVM signing seam and exact-selector validator ZeroFee hook;
- four fixed consensus slots in separate accountability state;
- deterministic q=3 tally, fourth-vote window and accountability summary;
- q-forming full-result application; no separate activator.

### 15.5 Certified quorum apply

- concrete `CertifiedLysisApply` module; no generic program/write dispatcher;
- verifier and private `CertifiedLysisActivation`;
- `NodBatchReceipt`;
- `ContributorReceipt`;
- `TributeReceipt` plus logical retirement;
- request-phase `RequestBudgetSplitReceiptV1`;
- apply `CarryOverReceiptV1`;
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
4. **On-chain quorum** — four domains use separate OCOMP keys and public vote
   transactions; consensus fixes quorum only from three identical first-vote
   slots and retains the fourth for accountability.
5. **Real quorum apply** — make the third matching full-result vote verify and
   atomically apply every observable domain effect through the closed
   capability/receipt path.
6. **System demonstration** — pass section 13 including one offline domain,
   tampering, worker-count determinism and expiry without fallback.

Each slice is tested at the external seam used by the next. In-memory adapters
are allowed in module tests; the final slice must use real UDS, processes,
consensus blocks and public APIs.

## 17. Deliverables and PoC completion checklist

### 17.1 Protocol artifacts

- [ ] canonical PoC bundle and frozen hash;
- [ ] canonical bytes and hash vectors for every PoC object;
- [ ] consensus request/FSM/finality+4 open/response-window/vote/quorum-apply state;
- [ ] static committee and OCOMP key registry;
- [ ] validator-only full-result-vote ZeroFee hook, q-forming atomic apply and
      public read schemas.
- [ ] immutable PoC build/deployment and network/capacity manifests.

### 17.2 Runtime artifacts

- [ ] node control/attestation endpoint;
- [ ] standalone supervisor;
- [ ] standalone checkpoint exporter;
- [ ] standalone deterministic worker;
- [ ] local CAS;
- [ ] direct four-domain Supervisor-to-chain result-vote path;
- [ ] four-domain devnet deployment.

### 17.3 Semantic artifacts

- [ ] pure `execute_lysis`;
- [ ] independent reference/golden implementation;
- [ ] bounded result chunks/reduction summary plus pure typed finalizer and
  constant-size typed result commitment;
- [ ] certified quorum-apply module;
- [ ] one request split receipt and four typed apply receipts;
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
  `OcompControl`, supervisor, exporter, worker, attestation gate, complete
  on-chain full-result vote/quorum/accountability path or certified apply module exists
  in the baseline from which the PoC was specified;
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

The PoC starts from a reproducible fresh base genesis. After its exact hash is
known, the generator creates the bundle, committee/PoPs and one canonical
chain-manifest-bound `OcompForkInstallV1` before any node starts. This avoids
embedding a genesis-hash-dependent object back into the genesis being hashed.
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
| independent execute then record bounded votes | [Hyperledger Fabric execute-order-validate](https://hyperledger-fabric.readthedocs.io/en/latest/whatis.html) | [Fabric endorsement policies](https://hyperledger-fabric.readthedocs.io/en/release-2.5/endorsement-policies.html) | multiple domains execute before threshold evidence authorizes apply; Outbe records each vote on-chain for accountability |
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
| 2.3 q-forming block | PoC-MUST; pause/upgrade/custody deferred | 5.4–5.5, 10 |
| 3 Tribute root | PoC-MUST | 6.1 |
| 4 runtime boundaries | process split PoC-MUST; hardening MVP | 3, 12 |
| 5 interfaces | UDS/local CAS PoC-MUST; mTLS deferred | 7 |
| 6 planner/units | bounded rules PoC-MUST; counted ranges deferred | 8 |
| 7 Lysis Map/Reduce | PoC-MUST | 8 |
| 8.1 input authenticity | PoC-MUST full fold | 6 |
| 8.2 independent correctness through bounded work | PoC-MUST | 9 |
| 8.3 proof execution | DEFERRED TargetLarge | 19.2 |
| 8.4 anti-equivocation | sign-once plus canonical first-vote/equivocation evidence PoC-MUST | 9.2–9.3 |
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
2. generated final per-shard/per-chunk/full-result-vote/evidence/apply/block byte and work
   caps, with no total Tribute cap;
3. exact canonical fields and precondition/budget formulas for
   `PreAdmissionEnvelopeV1`, apply preconditions, `PlanCommitmentV1`,
   `ResultChunkV1`, `LysisResultV1`, `ActiveGenerationV1` and terminal
   receipts;
4. canonical codec library, hash/signature preimages and golden-vector format;
5. exact legacy Lysis arithmetic/state/event semantics and authoritative golden
   corpus;
6. consensus-owned finalized JobIntent/JobId binding and historical committee
   data source used by full-result vote verification;
7. checkpoint API supported by the current Reth/MDBX integration;
8. local CAS layout, quota and cleanup rule;
9. exact systemd/container topology, UIDs, UDS paths and cgroup budget;
10. OCOMP key storage format and sign-journal durability primitive;
11. full-result `ResultVoteV1`, four-slot/quorum/accountability schemas and
    public vote encoding;
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
- request/finality/input/compute/full-result-vote/quorum-apply/expiry boundaries;
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
