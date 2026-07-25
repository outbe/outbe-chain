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

A single prover implementation is premature for this PoC. The selected
mechanism is independent execute-and-attest by four validator domains, with a
three-signature threshold and a separate deterministic reference corpus.

## Decision

### Deterministic plan and execution

Every supervisor independently derives the same canonical plan from the same
finalized `JobIntentV1`, authenticated input manifest and
`ProtocolBundleV1`.

```text
JobId + authenticated input
  -> canonical partition of all N Tribute
  -> X = ceil(N / max_tributes_per_work_shard) primary work shards
  -> PlanCommitmentV1(X, primary_work_unit_root)
  -> canonical UnitSpecV1 derived lazily by ordinal
  -> stable UnitId for every derived unit
  -> retryable pure unit artifacts
  -> bounded ShuffleRunArtifactV1 trees for owner/bucket runs
  -> fixed streaming reduction tree/order
  -> bounded ResultChunkV1 objects
  -> bounded RootReduceSummaryV1
  -> pure LysisProgramV1 finalization over exact verified catalogs
  -> LysisResultV1(result_chunk_count, result_chunk_list_root)
  -> ActivationPayloadV1 / ResultDigest
```

The planner, Lysis phase/range rules, codecs, arithmetic, logical time, sort
keys, reducer and hash domains are consensus semantics. Scheduler order,
worker identity, host count, completion order, retry count, wall time, locale,
filesystem order, network response and local configuration are excluded.

A full primary range never terminates the plan. The next authenticated Tribute
starts the next range, and the planner proves that the ordered ranges are
adjacent, non-overlapping and cover the manifest count exactly once. The
generated PoC profile bounds an individual work shard and the demonstrated
chunk/concurrency envelope. It never bounds the parent Tribute population.

`PlanCommitmentV1` is constant-size and also commits the finalized `wwd`,
`lysis_budget` and `logical_evaluation_time`. A supervisor does not materialize
all `UnitSpecV1` values in memory or persist them inside the plan hash: it
derives a unit from `(PlanCommitment, ordinal)`, verifies membership against the
committed root and advances bounded cursors. Before execution, the worker
decodes the exact commitment bytes from CAS and checks `PlanHash`, job, manifest
and planner/reducer-version bindings. Before signing, the node attestation gate
independently reloads the finalized intent and checks the plan's `wwd`, budget
and logical time. The deterministic reduction hierarchy is derived from the
committed unit count and is likewise scheduled lazily.

`AMOUNT_MAP(j)` has two distinct Fidelity dependencies:
`FIDELITY_MAP(j)` supplies the per-Tribute league observation and the fixed
reducer root supplies the global league fraction table. Omitting the matching
leaf would leave the Tribute-to-league relation unproved.

Raw Tribute coverage is reduced through constant-size globally indexed Merkle
subtree carriers. A leaf carrier commits its exact raw ordinal interval;
canonical empty leaves commit the corresponding all-pad subtree. A reducer
accepts only equal-height left/right siblings with adjacent coverage and hashes
them once into their parent carrier. Only the root carrier receives the frozen
ordered-list root wrapper using the manifest Tribute count, and that root must
equal the canonical complete raw coverage commitment. Reducers therefore never
materialize or re-read the complete Tribute population.

Owner and bucket shuffle outputs use the Lysis-specific
`ShuffleRunArtifactV1`; this is not a generic artifact framework. Its root
object is embedded as the bounded `canonical_output_bytes` of the producing
`UnitArtifactV1`. A leaf carries at most 256 canonically ordered owner or bucket
records. A node carries exactly two content-addressed child references plus
their adjacent page/record summaries. The split for every non-leaf page span is
canonical and never creates a unary page-tree node, which makes the tree
independent of worker count and completion order.
Descendants are individually bounded OCB1 objects in validator-local CAS.
Consumers verify every referenced object's digest, OCB1 kind, job/unit/run
binding, canonical split, page and record adjacency, exact count, order,
ordered-record root and source coverage before using the stream. Leaf roots
use the frozen ordered-list construction; every internal root is recomputed
with `OUTBE_OCOMP_SHUFFLE_RUN_NODE_V1` from its kind, interval/count summary and
two child roots. The tree commitment is therefore composable and verifiable in
one bounded-memory traversal rather than a trusted field. No unit
contains a population-sized page-reference vector or merged run.

Raw source coverage uses the distinct frozen domain
`OUTBE_OCOMP_SHUFFLE_SOURCE_COVERAGE_V1`. A shuffle leaf binds its exact
`OUTPUT_FINALIZE` raw-coverage root/count and run span. A real merge hashes the
two adjacent producer coverage roots/counts with its parent run span. This
composition needs neither padding records nor promotion units and is checked
independently from the ordered output-page commitment.

A shuffle execution DAG is distinct from the page tree stored by one unit.
For `K` primary runs, each owner/bucket phase contains exactly `K` leaf units
and `K - 1` real two-input merge units. An odd run is consumed directly by the
next real merge; no shuffle `CanonicalEmpty`, unary alias or copy-only
promotion unit is scheduled. Every real merge reads exactly two verified,
materialized producer runs and writes a new canonical page tree whose root and
descendants carry the current merge `UnitId`. Producer roots remain UnitSpec
inputs and are never reused as output-tree children: concatenating two sorted
runs would not prove their merged order.

This binary external merge has `Θ((N + E) log K)` aggregate record I/O for
`N` bucket and `E` eligible-owner records, while each worker retains only two
bounded input pages, one bounded output page and logarithmic tree frontiers.
That write amplification is an explicit PoC performance assumption, not hidden
behind an unbounded lazy-merge frontier. A fixed higher fan-in is a later
versioned capacity decision.

A unit may run zero or many times. Only a digest-valid artifact for its exact
`UnitId` and plan membership participates in reduction. One, two and four
workers plus randomized completion/retry order must produce byte-identical plan,
result and digest.

### Typed result finalization

The final `ROOT_REDUCE` worker emits a bounded
`RootReduceSummaryV1`; it does not emit `LysisResultV1` and does not receive the
complete artifact catalog or canonical `JobIntentV1` through `RunUnitV1`.

After every required unit and result chunk is durably `VERIFIED`, the supervisor
hosts one pure `LysisProgramV1` finalizer. The finalizer:

- reloads and validates the journaled canonical `FinalizedJobSpecV1`, plan and
  exported-manifest binding;
- enumerates artifacts in exact plan order and result chunks in exact chunk
  order through bounded cursors;
- reopens the exact CAS bytes and rechecks kind, digest, `UnitId`, specification
  and semantic validation;
- derives all catalog, fraction, prefix, result and event roots itself; and
- emits canonical `LysisResultV1` or abstains.

`RootReduceSummaryV1` carries fixed-capacity positional coverage trees: 256
slots per primary shard for each action/output list and one slot per primary
shard for the result-chunk hash list. Real records occupy the shard-local
prefix and frozen globally indexed pad hashes occupy the remaining positions;
canonical empty leaves are all-pad trees. Reducers merge only equal-height
adjacent carriers. These carrier roots prove complete reducer coverage but are
not canonical dense result-list roots and receive no ordered-list root wrapper.
The finalizer independently streams the exact verified records to derive every
canonical result root and cross-checks all summary carriers, counts and totals.

The finalizer accepts no caller-built `LysisResultV1` and no precomputed result
root. Its semantic author is the pinned Lysis program; the supervisor is only
the invocation/admission host. The finalizer is not a schedulable unit, new
program, signer or generic framework. The final `ROOT_REDUCE` artifact remains
excluded from `unit_artifact_root` to prevent self-reference, while its summary
is bound through the semantic result and `ResultDigest`.

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
checks constant-size caps, bindings, equations and program structure, and
durably commits the sign-once record. It does not traverse bulk result chunks or
repeat Lysis finalization.

The sign-once subject binds at least:

```text
(chain/genesis/fork, OCOMP key epoch, JobId, attempt, result purpose)
```

An exact retry returns the recorded signature. A different digest for the same
subject is refused after restart as well as in one process.

### Certificate and relay

Any relay may collect announcements and submit the constant-size
`LysisResultV1` commitment plus `ExecutionCertificateV1`. Result chunks remain
authenticated content-addressed artifacts. Before requesting its node's
signature, every validator's separate compute plane must have validated
complete chunk coverage and reduction. The node verifies only the closed
constant-size signing subject; no chunk has independent evidence weight. A
faulty compute plane may consume its own sign-once slot, but contributes at most
that validator domain's one signature. The relay has no signing key,
exclusivity or trusted ordering role. Every activation verifier verifies:

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
- `N` authenticated Tribute produce exactly the canonical shard/range set; a
  shard boundary never drops or rejects a valid next Tribute.
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

Local unit/reducer work is content-addressed and freely replayable. Completion
of a proper subset of work shards is not a result and is never signable or
activatable. The
sign-once journal update is write-before-sign and crash-safe: an uncertain write
disables signing until reconciled. A worker/supervisor crash cannot corrupt
consensus state.

One unavailable domain still permits `q=3`. Two unavailable domains produce no
fallback or lower threshold; the job reaches its on-chain expiry path. A
deterministic mismatch is retained as evidence and no local “majority choice”
rewrites the job.

## Determinism and bounds

Per-shard/per-chunk counts and bytes, live workers, signatures and activation
cryptographic work are checked against the generated PoC capacity profile
before allocation or verification. Total Tribute, unit and result-chunk counts
are checked for arithmetic validity and exact committed coverage, not capped by
the PoC. Unit and result-chunk artifacts do not enter activation state
individually. One constant-size complete-result commitment is carried in the
activation transaction. `ShuffleRunArtifactV1` bounds each leaf to 256 records
and each internal node to two child references; the number of leaves and nodes
is derived from the uncapped Tribute population and is never a consensus cap.

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
exists. Required evidence includes a boundary fixture whose Tribute count is
one greater than a full work shard, synthetic plan derivations for 10,000 and
1,000,000,000 Tribute without proportional plan allocation, independent full
executions of the bounded fixture by four real validator domains, 1/2/4-worker
equality, randomized order/retry, restart-safe sign-once refusal,
wrong/duplicate signer rejection, one-byte/ordering/JobId mutation rejection
and comparison with a separate reference corpus.

## Consequences

The PoC establishes decentralised correctness through bounded work without
on-chain Lysis or a total Tribute cap, but accepts that three implementations
can share one semantic bug. The reference corpus, adversarial vectors and later
implementation diversity remain required; quorum is not an excuse to skip
specification testing.

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
