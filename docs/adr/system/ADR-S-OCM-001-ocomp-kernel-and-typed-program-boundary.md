# ADR-S-OCM-001: OCOMP is an operational kernel for closed typed programs

- **Status:** Accepted; full-result quorum apply implemented on
  `feat/ocomp-poc`; final PoC closure evidence pending
- **Date:** 2026-07-26
- **Decision owners:** System Space, consensus execution and Core program maintainers
- **Scope:** OCOMP lifecycle/process boundary and the contract between shared
  computation infrastructure and a domain program
- **Depends on:** ADR-B-NOD-001, ADR-B-CNS-003, ADR-B-EVM-001,
  ADR-B-EVM-005, ADR-B-SUP-001, ADR-S-GOV-003
- **Related:** ADR-B-TST-001, ADR-S-OCM-002 through ADR-S-OCM-004,
  ADR-C-MET-001, ADR-C-LYS-001, PFS-002
- **Supersedes:** None

## Context

Lysis is the first computation that must leave synchronous block execution, but
Tribute, Nod and Gem volumes show that it cannot become a one-off sidecar. At the
same time, one Lysis implementation is insufficient evidence for a public plugin
framework, generic task bytes or generic state writes. Those abstractions would
hide domain completeness, conservation and mutation authority behind a shallow
interface.

The durable decision is therefore the ownership boundary: shared operational
mechanics are reusable, while each computation remains a closed, fork-pinned,
typed protocol.

## Decision

OCOMP is a System Space operational kernel:

```text
OCOMP operational kernel
  finality + job lifecycle + process isolation + artifacts
  + evidence + bounded certified dispatch
        |
        +-- Lysis V1 typed protocol       PoC and BoundedMVP
        |
        +-- future typed protocol         separate ADR and fork
```

The kernel and program own different truths:

| OCOMP kernel owns | Typed program owns |
|---|---|
| finalized discovery and job identity | trigger and domain subject |
| pending, expiry and terminal lifecycle shell | program-specific preconditions, capacity claims and cleanup |
| process/control-plane boundaries | authenticated input schema and completeness |
| artifact addressing, leases and retry mechanics | planner, units, reducer and semantic ordering |
| verified-admission journal and bounded catalog cursors | pure typed finalizer and every result-field derivation |
| committee snapshot, sign-once and evidence envelope | typed result, equations and result verifier |
| outer quorum-apply checkpoint and closed dispatch | private effect capability, owner calls and receipts |
| protocol compatibility and readiness | domain-visible output and recovery contract |

For the PoC the kernel is implemented as internal deep modules wired directly to
one concrete `LysisProgramV1`. The persisted and wire-visible
`JobIntentV1`, `UnitSpecV1`, `LysisResultV1`, `ResultVoteV1`,
`ProtocolBundleV1` and the private `CertifiedLysisActivation` capability remain
Lysis-specific even where a historical name looks generic. There is no public
post-quorum activation call.

No PoC consensus `ProgramRegistry`, public `TaskAdapter`, dynamic handler table,
uploaded program, `execute(program_id, bytes)`, opaque result, generic action
stream, arbitrary call list, storage-key write set or program-neutral activation
capability exists.

### Process boundary

One validator domain contains:

- `outbe-chain`, which owns consensus, finalized job state, OCOMP attestation
  authority and q-forming result verification/apply;
- a separate OCOMP supervisor process, which discovers work, plans,
  schedules and journals, then hosts the closed program's pure finalizer after
  complete verified admission, and owns only the role-delegated EVM key used to
  submit the public result-vote transaction;
- a separate read-only snapshot exporter;
- retryable one-unit worker child processes launched by the Supervisor's fixed
  PoC adapter; and
- untrusted content-addressed artifact storage.

The PoC proves these process and protocol boundaries, not protection from root
or a compromised host. Distinct service identities, host sandboxing and
service-manager policy are MVP deployment hardening.

The Rust E2E harness starts node, Supervisor and SnapshotExporter independently
and records their child processes. The Supervisor's bounded PoC launcher starts
one production worker child per immutable unit; the harness may launch that
same entrypoint directly only in a narrow process-boundary test. The node never
spawns workers or depends on Supervisor lifetime for consensus progress. Many
workers under one Supervisor remain one Byzantine validator domain.

### Logical job and worker-shard boundary

`JobIntentV1` names the complete Lysis computation for one WWD. It is not sized
to one worker invocation. The Lysis planner deterministically partitions its
complete authenticated Tribute stream into bounded `UnitSpecV1` work shards:

```text
one finalized JobIntent / JobId over N Tribute
  -> InputManifestV1 commits N and the input-chunk catalog root
  -> PlanCommitmentV1 commits X = ceil(N / S) and the work-unit root
  -> bounded local worker pool lazily derives and executes every shard
  -> fixed streaming reducer commits bounded ResultChunkV1 objects
  -> final ROOT_REDUCE worker emits bounded RootReduceSummaryV1
  -> LysisProgramV1 pure finalizer streams the verified catalogs
  -> LysisResultV1 commits the complete result-chunk catalog root
```

Crossing a shard boundary never rejects or drops a Tribute: the next canonical
Tribute starts the next shard. Shards are local retryable artifacts, not
consensus jobs, validator votes or independently applicable results. Every
validator domain derives and executes the complete shard set independently.
`N` has no artificial PoC or protocol ceiling. Only one work shard, one
input/result chunk, the live worker pool and constant-size protocol summaries
are bounded.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| consensus job state and finality binding | OCOMP kernel inside block execution |
| local job discovery/lease/attestation control | bounded versioned `OcompControlV1` |
| bulk input/result bytes | authenticated CAS objects, never control messages |
| exact admitted artifact/chunk order | plan-derived, durable supervisor catalog cursors |
| Lysis semantics and result meaning | ADR-C-LYS-001 and the pinned Lysis bundle |
| result construction | pure `LysisProgramV1` finalizer; supervisor is only its host |
| result signature | node-owned `OcompAttestationGate`; never the finalizer host |
| public vote construction/submission/reorg retry | validator-domain `OffchainLysis Supervisor` using its role-delegated EVM key after node attestation |
| result-vote fee waiver | exact-selector validator-only ZeroFee hook; no protocol authority |
| vote-slot/quorum/accountability state | OCOMP kernel inside block execution |
| immutable terminal vs late accountability | separate `LysisTerminalV1` and bounded `OcompVoteAccountabilityV1` |
| Metadosis trigger/status | ADR-C-MET-001 |
| effect mutations | domain-owner certified methods, never kernel raw storage |
| deployment/readiness/failure reporting | ADR-B-SUP-001 and ADR-B-OPS-001 |

## Invariants

- OCOMP failure cannot stop consensus or mutate domain state.
- A supervisor, exporter, worker, CAS or public transaction submitter owns no
  consensus authority; quorum is derived from bounded on-chain vote state.
- The supervisor may invoke the Lysis finalizer but cannot supply precomputed
  result fields or roots; the finalizer derives them from typed finalized
  authority and exact durable verified catalogs.
- A final `ROOT_REDUCE` worker emits a bounded summary, not
  `LysisResultV1`, and receives no population-sized catalog through the control
  plane.
- Worker count never increases evidence weight.
- A worker-shard capacity limits one invocation, not the Tribute population
  covered by the parent Job Intent.
- `JobIntentV1`, `InputManifestV1`, `PlanCommitmentV1` and `LysisResultV1`
  commit arbitrary-size populations by count and root; they never inline a
  vector proportional to `N`.
- Every authenticated Tribute belongs to exactly one canonical work shard and
  every shard belongs to exactly one parent Job Intent.
- The kernel cannot enumerate or interpret Tribute, Fidelity, Oracle, Nod or Gem
  business state.
- A typed program cannot choose consensus semantics through local configuration
  or capability negotiation.
- Only a fork-pinned handler set can verify and apply a program result.
- Domain effects remain unreachable through a generic OCOMP write interface.
- Lysis V1 bytes are never reinterpreted as a future common envelope.

## Atomicity, replay and failure

The kernel owns the outer q-forming job-state/apply checkpoint.
Program verification and typed effect receipts either complete inside that
checkpoint or leave job and domain state unchanged. Local process retries are
not consensus transitions and cannot invent `RUNNING` chain state.

Supervisor absence, crash or version mismatch sets only `ocomp_ready=false` for
that validator. Invalid/corrupt artifacts cause local abstention. Consensus
continues and on-chain expiry remains available without any compute process.

## Determinism and bounds

Every control message, individual chunk/work artifact and result summary
has fork-pinned byte/count/work caps. Total job size is a committed `u64`
population, not an admission cap. Enumeration, scheduling, reduction and GC are
cursor/page bounded so resident memory and per-step work do not grow with `N`.
The control API accepts no database query, path, command, executable, private
key or arbitrary signing payload. Scheduling order and worker identity are
operational facts, never semantic inputs.

## Compatibility and extension

A future program qualifies only when its own ADRs define:

1. an independent domain postcondition and typed intent/result;
2. authenticated complete input enumeration and canonical ordering;
3. program-specific caps, preconditions/capacity claims and conflict rules;
4. deterministic execution and result verification;
5. private typed apply authority and owner-controlled effects;
6. conservation/witness/receipt/recovery rules; and
7. a complete finality, full-result vote, quorum-apply, expiry and replay flow
   with contention tests.

Only after two real programs exist may their demonstrated intersection be
extracted into a common registry, envelope or source interface. Such extraction
requires a new fork-pinned protocol decision; it cannot reinterpret Lysis V1.
Gem qualification is the preferred future stress test, not a PoC deliverable.

## Production-interface verification evidence

No complete OCOMP process, control API, job state, attestation gate or
quorum-apply module
exists in production code. PFS-002 and `off-chain-poc.md` define the required
four-validator demonstration. Evidence must use separate released processes,
real UDS, consensus blocks, public RPC and public state/proof reads. Direct
executor calls, shared calculator output or injected state cannot satisfy it.

## Consequences

The first implementation cannot collapse back into a Lysis-specific daemon, but
the protocol also avoids pretending that arbitrary computations are safe.
Future programs reuse proven lifecycle machinery while preserving their own
completeness and mutation authority.

## Rejected alternatives

- **Keep Lysis entirely special-cased:** the next large computation would need
  another lifecycle, signer, recovery and result-apply mechanism.
- **Freeze a one-entry consensus registry:** it duplicates
  `ProtocolBundleV1` without proving heterogeneous dispatch.
- **Expose generic task/result bytes:** type erasure hides domain authority and
  creates an arbitrary execution surface.
- **Run the supervisor inside the node:** compute crashes and resource pressure
  would share the consensus failure boundary.
- **Let the node spawn arbitrary workers:** it turns a bounded protocol into a
  privileged command-execution API.

## Open questions and technical debt

1. Choose exact crate/binary ownership and names without weakening the three
   deep internal boundaries: lifecycle, program semantics and certified apply.
2. Register every new process/task with ADR-B-SUP-001 and its deployment profile
   with ADR-B-OPS-001.
3. Freeze the bounded control protocol and capability handshake before coding.
4. Prove no existing public/runtime entrypoint bypasses the certified Lysis path
   after the PoC fork.
5. A second program ADR is intentionally absent; do not create placeholder
   adapters or registry entries to satisfy this decision.
