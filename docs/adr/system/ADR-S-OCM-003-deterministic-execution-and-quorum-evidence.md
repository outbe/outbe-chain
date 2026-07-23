# ADR-S-OCM-003: OCOMP uses deterministic execution and independent quorum evidence

- **Status:** Proposed; PoC not implemented
- **Date:** 2026-07-23
- **Decision owners:** System Space, consensus cryptography and Lysis maintainers
- **Scope:** deterministic planning/execution/reduction, validator-domain
  independence, result digest, OCOMP signing and untrusted relay
- **Depends on:** ADR-S-OCM-001, ADR-S-OCM-002, ADR-B-CRY-001,
  ADR-B-CAP-001, ADR-S-VAL-001, ADR-S-KEY-001
- **Related:** ADR-B-TST-001, ADR-S-OCM-004, ADR-C-LYS-001, PFS-002
- **Supersedes:** None

## Context

Moving computation off-chain removes the implicit guarantee that every block
executor ran the same function. The PoC must replace that guarantee without
running Lysis on-chain and without equating worker count with Byzantine
independence.

A single prover implementation is premature for the bounded PoC. The selected
mechanism is independent execute-and-attest by four validator domains, with a
three-signature threshold and a separate deterministic reference corpus.

## Decision

### Deterministic plan and execution

Every supervisor independently derives the same canonical plan from the same
finalized `JobIntentV1`, authenticated input manifest and
`ProtocolBundleV1`.

```text
JobId + authenticated input
  -> canonical ordered UnitSpecV1 set
  -> stable UnitId for every unit
  -> retryable pure unit artifacts
  -> fixed reduction tree/order
  -> BoundedLysisResultV1
  -> ActivationPayloadV1 / ResultDigest
```

The planner, Lysis phase/range rules, codecs, arithmetic, logical time, sort
keys, reducer and hash domains are consensus semantics. Scheduler order,
worker identity, host count, completion order, retry count, wall time, locale,
filesystem order, network response and local configuration are excluded.

A unit may run zero or many times. Only a digest-valid artifact for its exact
`UnitId` and plan membership participates in reduction. One, two and four
workers plus randomized completion/retry order must produce byte-identical plan,
result and digest.

### PoC evidence profile

The first devnet fixes:

```text
n = 4 result validator domains
f = 1 faulty or unavailable domain
q = 3 distinct matching signatures
```

Each domain independently owns its node, supervisor, exporter, CAS and workers.
Several processes or workers controlled by one validator contribute one
validator index and one signature at most.

The node owns a separate OCOMP signing key/epoch and an
`OcompAttestationGate`. The supervisor and worker never receive the key or an
arbitrary signing endpoint. Before signing, the gate reloads the finalized job,
checks the pinned bundle/committee, reconstructs the canonical result digest,
checks caps and program structure, and durably commits the sign-once record.

The sign-once subject binds at least:

```text
(chain/genesis/fork, OCOMP key epoch, JobId, attempt, result purpose)
```

An exact retry returns the recorded signature. A different digest for the same
subject is refused after restart as well as in one process.

### Certificate and relay

Any relay may collect announcements and submit the complete typed result plus
`ExecutionCertificateV1`. The relay has no signing key, exclusivity or trusted
ordering role. Every node verifies:

- exact `ResultDigest` reconstruction;
- distinct eligible committee indexes from the job-pinned snapshot;
- exactly the required threshold under the pinned signature domain;
- no duplicate, unknown, wrong-epoch or malformed signer;
- exact `JobId`, attempt, bundle and typed result binding.

Three signatures over different result bytes do not form evidence.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| plan/unit/result semantics | pinned Lysis program bundle |
| local scheduling and retry | supervisor journal; non-consensus |
| artifact equality | canonical digest and plan membership |
| OCOMP key custody | node attestation gate plus ADR-S-KEY-001 backend |
| signer eligibility/weight | job-pinned result committee snapshot |
| certificate construction | untrusted replaceable relay |
| certificate validity | every node during activation |
| semantic reference | independent golden/reference implementation |

## Invariants

- Equal finalized inputs and bundle produce byte-identical units and result.
- Worker/scheduler count cannot change result bytes or evidence weight.
- One validator index contributes at most one matching signature.
- The OCOMP key is distinct from the consensus key and unavailable to compute
  processes.
- Sign-once history is durable before signature release.
- A certificate binds one exact typed result and one exact job attempt.
- The relay cannot turn mismatched or duplicate signatures into authority.
- Quorum evidence never proves data availability, domain-spec correctness or
  implementation diversity by itself.

## Atomicity, replay and failure

Local unit/reducer work is content-addressed and freely replayable. The
sign-once journal update is write-before-sign and crash-safe: an uncertain write
disables signing until reconciled. A worker/supervisor crash cannot corrupt
consensus state.

One unavailable domain still permits `q=3`. Two unavailable domains produce no
fallback or lower threshold; the job reaches its on-chain expiry path. A
deterministic mismatch is retained as evidence and no local “majority choice”
rewrites the job.

## Determinism and bounds

All unit/result/certificate counts, bytes, signatures and cryptographic work are
checked against the generated PoC capacity profile before large allocation or
verification. Unit artifacts do not enter activation state individually. The
complete bounded typed result is carried once in the activation transaction.

## Compatibility and migration

The result digest and signature domain pin the exact protocol bundle. Capability
negotiation may cause a validator to abstain but cannot choose consensus
semantics. A changed planner, reducer, result meaning, signature domain or
committee schema requires a new bundle and golden vectors. Live jobs finish or
expire under the bundle and committee pinned at creation.

BoundedMVP may harden keys, scheduling and retention while preserving this
execute-and-attest meaning. A proof-carrying TargetLarge profile is separate
evidence semantics and cannot be presented as PoC completion.

## Production-interface verification evidence

No OCOMP planner, workers, signer, sign-once journal, certificate or relay path
exists. Required evidence includes independent full executions by four real
validator domains, 1/2/4-worker equality, randomized order/retry, restart-safe
sign-once refusal, wrong/duplicate signer rejection, one-byte/ordering/JobId
mutation rejection and comparison with a separate reference corpus.

## Consequences

The PoC establishes decentralised bounded correctness without on-chain Lysis,
but accepts that three implementations can share one semantic bug. The reference
corpus, adversarial vectors and later implementation diversity remain required;
quorum is not an excuse to skip specification testing.

## Rejected alternatives

- **One central calculator:** creates a trusted sequencer/oracle for Lysis.
- **Count many workers as many voters:** they share one validator failure domain.
- **Sign an arbitrary digest supplied by the supervisor:** key authority can be
  redirected to unrelated statements.
- **Let the relay select “close enough” results:** result equality must be exact.
- **Rerun Lysis on-chain:** defeats the purpose and scale boundary of OCOMP.
- **Lower `q` on timeout:** changes the fault model exactly when availability is
  weakest.

## Open questions and technical debt

1. Freeze exact OCOMP key type, proof-of-possession registration, epoch history
   and compromise/revocation interaction.
2. Freeze the sign-once journal schema, fsync contract and recovery procedure.
3. Produce an independent Lysis reference implementation and adversarial golden
   corpus before allowing signatures.
4. Generate all `UnitSpecV1`, `UnitId`, result and certificate golden vectors.
5. Prove maximum result/signature verification work through the public
   RPC/txpool/P2P/import/replay path.
6. Define durable mismatch diagnostics without leaking raw user data.
