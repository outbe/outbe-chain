# Off-chain PoC: current-code map

Status: **resolved research asset for decision ticket #2**

Scope: repository state before OCOMP PoC implementation

Date: 2026-07-23

This map answers one question: which production seams already exist and can be
reused by the PoC, and which OCOMP boundaries are genuinely missing?

It does not select the final crate layout, wire codecs, protocol constants,
process topology or implementation tasks. Those are downstream decisions.

## 1. Current execution story

Today the relevant path is synchronous and runs inside block execution:

```text
CycleLifecycle::begin_block
  -> dispatch_triggers
  -> process_metadosis
  -> outbe_lysis::runtime::lysis
       -> read Tribute bodies
       -> query Fidelity league per owner
       -> calculate fixed-point allocation
       -> resolve Oracle entry price
       -> issue Nod
       -> record Intex contributors
       -> consume the Tribute partition
  -> dispatch Desis brief
  -> dispatch Promis
  -> mark Worldwide Day complete
  -> retire the completed Tribute partition
```

`process_metadosis` therefore blocks block execution while Lysis reads,
calculates and writes. There is no JobIntent, result certificate, activation
transaction or independent OCOMP process in this path.

Primary evidence:

- [`CycleLifecycle::begin_block`](../crates/system/cycle/src/lifecycle.rs)
- [`process_metadosis`](../crates/core/metadosis/src/runtime.rs)
- [`outbe_lysis::runtime::lysis`](../crates/core/lysis/src/runtime.rs)
- [`TributeContract::consume_lysis_partition`](../crates/core/tribute/src/runtime.rs)

## 2. Reuse and gap matrix

| Required seam | Current production asset | Safe PoC reuse | Missing OCOMP work or constraint |
|---|---|---|---|
| Deterministic trigger ordering | `CycleLifecycle::begin_block`, begin-zone `SystemTxKind` phases and phase cursor | Reuse the existing consensus execution ordering model | Add an explicitly ordered OCOMP lifecycle/expiry phase; its exact owner and position are not yet selected |
| Current Lysis semantics | `outbe_lysis::runtime::lysis` and `lysis_inner` | Use as evidence for the legacy behavior and build an independent equivalence corpus | Split pure calculation from effects; freeze intended behavior before treating current behavior as normative |
| Atomic state rollback | `StorageHandle::with_checkpoint` | Reuse for activation/apply so every typed effect commits or all effects roll back | Define the private certified activation capability and owner-specific receipts |
| Tribute body access | `ParentBodySource`, `VerifiedBody`, `AuthenticatedParentTree`, `ExecutionScope`, `load_day_tributes` | Reuse commitment/body verification concepts and canonical Tribute decoding | Existing runtime readers are not a checkpoint-pinned export API and do not prove an immutable cross-process snapshot |
| Exact parent identity | `ExactParentIdentity` binds commitment scheme, block number, block hash and root | Reuse this identity in the finalized input authority chain | Bind all exported inputs, manifests and jobs to one accepted fork-pinned identity |
| Finalized CE state | CE `FinalizedMarker` and MDBX snapshot/opening primitives | Reuse exact finalized-state identity and read-only opening behavior where its contracts are sufficient | Add bounded retention pins/capabilities and prove release/orphan behavior |
| Off-chain body persistence | `MongoStorage` with majority write concern, primary read preference, transactions and a single-writer lease | Reuse storage and canonical body decoding; do not create a second projection database for PoC | Mongo contents alone are not trusted input. Export must verify commitment binding and prevent mutable-path substitution |
| Projection checkpoint | `ProjectionCheckpoint`, `prepare_offchain_data_projection`, `validate_offchain_data_checkpoint`, `run_offchain_data_projection` | Reuse checkpoint validation and the finalized-block projection stream | Current projection is an in-node Reth ExEx, not an OCOMP exporter or sibling process |
| Consensus finality feed | `ExecutorActor` maintains finalized height/hash and supports finalized subscriptions | Reuse the authoritative finalized transition, subject to a bounded adapter | No public/protected OCOMP finality handoff, JobIntent feed or cursor exists |
| Finality proof material | `FinalizedParentCertStore`, `CertifiedParentProofKey`, `CertifiedParentProofRecord`, `FinalizedParentProofSelector` | Reuse persisted certified-parent evidence if the later authority-chain decision proves it sufficient | No `FinalizedIntentProofV1`; current selector/store is internal and parent-accounting oriented |
| Public transaction path | Normal Reth/Revm transaction execution and `outbe_ctx_dispatch` precompile dispatch | Activation must enter through this normal public transaction path | No OCOMP precompile/API, activation envelope or certificate verifier exists |
| Typed state access | `StorageHandle` typed subcalls and module APIs | Reuse closed owner APIs; preserve gas accounting and rollback | No generic write set, adapter registry or arbitrary storage mutation may be introduced |
| Existing Lysis effects | Nodfactory issue, Intex contributor record, Tribute consume/retire, Metadosis completion plus Desis/Promis dispatch | Reuse the actual owner operations behind a private typed apply boundary | Inventory exact preconditions, receipts and idempotence; remove the reachable synchronous fallback at the PoC fork |
| Local IPC precedent | TEE `EnclaveClient`/server framing over Unix sockets with bounded timeouts | Reuse small framing, timeout and connection-isolation ideas where appropriate | OCOMP needs its own protocol and trust model; it must not inherit TEE/Noise semantics accidentally |
| Key-file safety precedent | signer key loading and file-permission validation | Reuse safe permission checks and lifecycle patterns | Create a separate OCOMP identity/key epoch and durable sign-once journal; never reuse consensus, TEE or EVM keys |
| Process lifecycle testing | E2E `ChildGuard`, validator/enclave start-stop-restart controls and retained logs | Extend the existing harness to own OCOMP processes and failure injection | No supervisor/exporter/worker/CAS handles, readiness probes or UDS controls exist |
| Public verification | `World::rpc`, finalized block/root/hash queries, transaction receipts and `outbe_getCompressedEntity` point proofs | Reuse public RPC observation; acceptance must verify outcomes through public boundaries | Add only the minimum public job/terminal/active-generation views required by the protocol and PFS |
| Evidence capture | `ScenarioEvidence` records invocation, git state, outcome, duration, environment and log audit | Version and extend this evidence format | It lacks OCOMP binaries/config/profile hashes, process topology, intent/input/result/certificate identities, activation/finality references, public reads and negative-assertion inventory |

## 3. Existing seams by concern

### 3.1 Trigger, calculation and effects

Relevant symbols:

- [`CycleLifecycle::begin_block`](../crates/system/cycle/src/lifecycle.rs)
- [`process_metadosis`](../crates/core/metadosis/src/runtime.rs)
- [`lysis` and `lysis_inner`](../crates/core/lysis/src/runtime.rs)
- [`TributeContract::consume_lysis_partition` and `retire_completed_partition`](../crates/core/tribute/src/runtime.rs)
- [`StorageHandle::with_checkpoint`](../crates/blockchain/primitives/src/storage/handle.rs)
- [`SystemTxKind`](../crates/blockchain/primitives/src/system_tx.rs)
- [`OutbeBlockExecutor`](../crates/blockchain/evm/src/executor.rs)

What exists:

1. Metadosis obtains the Worldwide Day totals and invokes Lysis directly.
2. Lysis interleaves reads, arithmetic and state mutation:
   - loads the day's Tribute bodies;
   - obtains each owner's Fidelity league;
   - computes fixed-point fractions;
   - resolves Oracle entry price;
   - issues Nod;
   - records contributors in Intex;
   - consumes the Lysis partition.
3. Metadosis then dispatches Desis and Promis work, marks the day complete and
   retires the partition.
4. The storage checkpoint provides rollback if any nested step fails.
5. Begin-zone system transactions already have deterministic phase ordering.

What does not exist:

- a pure Lysis planner/unit/reducer/result codec;
- an immutable input manifest;
- a job state machine, budget split or activation precondition set;
- a certified activation entry point;
- typed activation receipts;
- an OCOMP expiry phase;
- protection against re-entering the legacy synchronous path after the PoC
  fork.

Planning consequence: keep the Lysis semantic owner and owner-specific effect
APIs, but do not wrap the existing `lysis_inner` call in a worker. Its reads,
calculation and writes must first become explicit boundaries.

### 3.2 Finality, CE state and body reads

Relevant symbols:

- [`ExactParentIdentity`, `FinalizedMarker`](../crates/core/compressed-entities/src/persistence.rs)
- [`ParentBodySource`, `VerifiedBody`, `AuthenticatedParentTree`, `ExecutionScope`](../crates/core/compressed-entities/src/api.rs)
- [`RuntimeBodyReaders`](../crates/system/offchain-data/src/runtime_readers.rs)
- [`MongoStorage`](../crates/blockchain/offchain-storage/src/mongo.rs)
- [`prepare_offchain_data_projection`, `validate_offchain_data_checkpoint`, `run_offchain_data_projection`](../crates/blockchain/node/src/projection.rs)
- [`ExecutorActor`](../crates/blockchain/consensus/src/executor/actor.rs)
- [`FinalizedParentCertStore`, `CertifiedParentProofKey`, `CertifiedParentProofRecord`](../crates/blockchain/consensus/src/finalization/parent_cert_store.rs)
- [`FinalizedParentProofSelector`](../crates/blockchain/consensus/src/finalization/selection.rs)

What exists:

1. The consensus executor tracks the last finalized height/hash and can notify
   internal subscribers.
2. Certified-parent proof records persist block identity, committee identity,
   signer bitmap and encoded proof under an epoch/view/block key.
3. CE persistence represents an exact parent identity and finalized marker.
4. Authenticated parent-tree APIs verify a leaf against a selected parent root.
5. Runtime body readers retrieve typed bodies from Mongo and bind decoded bodies
   to commitments.
6. The off-chain projection consumes finalized blocks and checks its persisted
   checkpoint against Reth's finalized canonical block before serving.

What does not follow from those facts:

- a Mongo document is not trustworthy merely because it is on local disk;
- `RuntimeBodyReaders` do not provide an immutable cross-process snapshot;
- the projection checkpoint does not pin every dependency needed by a job;
- a certified parent proof is not yet a finalized JobIntent proof;
- internal finality subscriptions are not yet a bounded, authenticated OCOMP
  protocol.

Planning consequence: the exporter should be a narrow adapter over these
authorities, not a new source of truth. Downstream ticket #5 must prove the exact
chain:

```text
consensus-finalized identity
  -> accepted CE/checkpoint identity
  -> authenticated commitment opening
  -> verified typed body bytes
  -> content-addressed immutable artifact
  -> manifest/root bound to the JobIntent
```

### 3.3 Transaction, activation and apply

Relevant symbols:

- [`outbe_ctx_dispatch`](../crates/blockchain/evm/src/precompiles.rs)
- [`StorageHandle`](../crates/blockchain/primitives/src/storage/handle.rs)
- [`OutbeBlockExecutor`](../crates/blockchain/evm/src/executor.rs)
- [`process_metadosis`](../crates/core/metadosis/src/runtime.rs)
- [`lysis_inner`](../crates/core/lysis/src/runtime.rs)

What exists:

1. A public transaction executes through the normal proposer/import/replay path.
2. Typed precompile dispatch receives caller, value, static-call state, gas and
   the guarded execution context.
3. Nested module calls can share a rollback checkpoint.
4. The complete current effect sequence is observable in Metadosis/Lysis code.

What is absent:

- `JobIntentV1`;
- `ActiveGenerationV1`;
- `ExecutionCertificateV1`;
- a public `activateLysis` envelope;
- `CertifiedLysisActivation`;
- one closed verifier/apply interface;
- replay, stale-generation, expiry and single-activation guards;
- public terminal job/activation receipts.

Planning consequence: the normal public transaction path and checkpoint are the
reuse seams. The PoC must add a closed typed Lysis activation path, not an
arbitrary digest signer or generic state-write executor.

### 3.4 Process, IPC and key isolation

Relevant symbols:

- [`EnclaveClient`](../crates/system/tee/src/client.rs)
- [`serve`](../bin/outbe-tee-enclave/src/transport.rs)
- [signer key handling](../crates/blockchain/primitives/src/signer.rs)
- [`ChildGuard`](../crates/testing/e2e-harness/src/internal/proc.rs)
- [localnet committee process controls](../crates/testing/e2e-harness/src/world/localnet/committee.rs)

What exists:

1. The repository already knows how to run a bounded request/response protocol
   over a Unix socket, apply timeouts and isolate per-connection failures.
2. It validates sensitive key-file permissions.
3. The E2E harness owns subprocess lifetime, log capture and validator restart.

What is absent:

- a separately supervised OCOMP process;
- exporter/worker control messages and readiness protocol;
- a content-addressed artifact store with quotas;
- an OCOMP-only key and epoch identity;
- durable sign-once state;
- process failure rules proving node finality is independent.

Planning consequence: reuse the engineering patterns, not the TEE trust
boundary. The later process-topology decision should prefer the fewest deep
processes/modes that still demonstrate failure isolation.

### 3.5 E2E and retained evidence

Relevant symbols:

- [`World`](../crates/testing/e2e-harness/src/world/mod.rs)
- [`World::rpc`](../crates/testing/e2e-harness/src/world/rpc.rs)
- [`ScenarioEvidence`](../crates/testing/e2e-harness/src/evidence.rs)
- [committee lifecycle controls](../crates/testing/e2e-harness/src/world/localnet/committee.rs)
- [historical synchronous Lysis E2E](../crates/core/e2e/tests/wwd_lysis_nod_gratis.rs)
- [Tribute projection feature](../crates/testing/e2e-harness/features/tribute_projection.feature)

What exists:

1. A real localnet with validators, Mongo, RPC, fixtures and process ownership.
2. Validator stop/restart with persistent datadirs and retained logs.
3. Public receipt, finalized-height, state-root, block-hash and compressed-entity
   proof queries.
4. A versioned per-scenario evidence JSON file.
5. Historical tests for the current synchronous Tribute-to-Lysis behavior and
   public Tribute projection.

What is absent:

- a PFS-002 OCOMP feature/scenario implementation;
- four independent OCOMP validator domains;
- OCOMP process controls and fault/mutation injection;
- a certificate/result/artifact evidence collector;
- a manifest binding the source revision, built binaries, fork profile, process
  topology, finality, inputs, outputs, certificate, activation and public
  effects;
- a gate that rejects skipped/todo scenarios and mocked/direct-injection
  shortcuts.

Planning consequence: extend the existing harness and evidence mechanism rather
than creating a parallel test runner. Existing synchronous tests are reference
evidence, not proof that OCOMP works.

## 4. Confirmed absent protocol surface

A repository-wide exact-name survey found no implementation definitions for:

- `JobIntentV1`;
- `InputManifestV1`;
- `UnitSpecV1`;
- `BoundedLysisResultV1`;
- `ExecutionCertificateV1`;
- `CertifiedLysisActivation`;
- `ActiveGenerationV1`;
- `OcompControlV1`;
- `activateLysis`.

The workspace contains crates named `outbe-lysis`, `outbe-offchain-storage` and
`outbe-offchain-data`, but no OCOMP crate or binary. Similar words must not be
treated as implementation evidence.

## 5. Minimal placement implications for later tickets

These are constraints derived from the map, not final placement decisions:

1. The pure Lysis semantics should remain owned by `outbe-lysis`; the current
   runtime function is the extraction source, not the worker API.
2. Job lifecycle, activation verification and apply must live in consensus
   execution state, behind one closed typed boundary.
3. Finalized input export must adapt existing finality, CE and body authorities;
   it must not create another authoritative database.
4. At least one process outside the node must own off-chain calculation so its
   failure cannot take down node finality. The exact binary/mode split remains
   ticket #6.
5. The existing E2E harness and evidence format should be extended, not
   duplicated.
6. Generic program registries, generic adapters, generic write sets and a second
   program are outside PoC scope.

## 6. Decision #2 result

Ticket #2 is resolved:

- the synchronous extraction point is `process_metadosis` ->
  `outbe_lysis::runtime::lysis`;
- the reusable correctness anchors are consensus finality, exact CE parent
  identity, authenticated typed body reads, normal public transaction
  execution, typed checkpointed storage and public RPC verification;
- Mongo/projection/runtime readers are data-access components, not standalone
  input-integrity proof;
- the existing TEE service is only an IPC/process precedent;
- the existing E2E harness is the correct place to demonstrate four independent
  validator domains and retain evidence;
- the OCOMP protocol, process, key, artifact, certificate, activation and
  lifecycle surfaces are new work.

No user choice is required to close this map. The next frontier is ticket #3:
freeze the intended legacy Lysis semantics and select an independent equivalence
corpus before defining protocol bytes or tasks.
