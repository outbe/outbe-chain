# ADR-S-OCM-003: OCOMP uses deterministic execution and independent quorum evidence

- **Status:** Accepted; full-result on-chain voting and q=3 evidence implemented
  on `feat/ocomp-poc`; final PoC closure evidence pending
- **Date:** 2026-07-26
- **Decision owners:** System Space, consensus cryptography and Lysis maintainers
- **Scope:** deterministic planning/execution/reduction, validator-domain
  independence, result digest, OCOMP signing, on-chain result voting and
  accountability evidence
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
mechanism is independent execute-and-attest by four validator domains, with
three matching on-chain votes, a separate deterministic reference corpus and
consensus-visible evidence of timely participation.

Collecting signatures only in an off-chain relay is insufficient. Consensus
cannot distinguish a validator that failed to respond from an honest validator
whose announcement was dropped or censored by the relay. Such a design cannot
support objective missed-result accountability or slashing evidence.

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
  -> one OutputManifestEntryV1 per ResultChunkV1
  -> bounded ROOT_REDUCE LEAF(entry + summary) / NODE(summary)
  -> pure LysisProgramV1 finalization over exact verified catalogs
  -> LysisResultV1(result_chunk_count, result_chunk_list_root)
  -> canonical LysisResultV1 / ResultDigest
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

`OUTPUT_FINALIZE` carries one checked
`FinalizedOutputRunV1.checked_tribute_nominal_total` per bounded primary shard.
It is computed from every input `AmountRecordV1`, including Tribute excluded
from contributor issuance. The nominal is not duplicated in every finalized
record: `ROOT_REDUCE` requires only the shard subtotal, and per-record
duplication would add 32 bytes per Tribute without adding evidence.

The subtotal becomes authoritative only through deterministic semantic replay.
Before adoption, the supervisor re-executes `OUTPUT_FINALIZE` from the exact
verified producer artifacts and requires canonical-byte equality. Each
`ROOT_REDUCE` leaf consumes that verified subtotal, internal reducers
checked-add child totals, and the typed finalizer requires the root total to
equal the authenticated `InputManifestV1.tribute_nominal_total`. It must never
infer total nominal from the optional contributor stream.

### Typed result finalization

The final `ROOT_REDUCE` worker emits a bounded closed payload:
`LEAF(RootReduceSummaryV1, OutputManifestEntryV1)` when the whole plan has one
primary shard, otherwise `NODE(RootReduceSummaryV1)`. It does not emit
`LysisResultV1` and does not receive the complete artifact catalog or canonical
`JobIntentV1` through `RunUnitV1`.

After every required unit and result chunk is durably `VERIFIED`, the supervisor
hosts one pure `LysisProgramV1` finalizer. The finalizer:

- reloads and validates the journaled canonical `FinalizedJobSpecV1`, plan and
  exported-manifest binding;
- enumerates artifacts in exact plan order and result chunks in exact chunk
  order through bounded cursors;
- reopens the exact CAS bytes and rechecks kind, digest, `UnitId`, specification
  and semantic validation;
- derives all catalog, fraction, prefix and result roots itself, and fixes the
  LYSIS_V1 pre-result semantic-event population to the canonical empty
  list-kind `5` root with count zero; and
- emits canonical `LysisResultV1` or abstains.

LYSIS_V1 defines no pre-activation `SemanticEventRecordV1` producer or codec.
Therefore `ExactCountsV1.semantic_event_count` is exactly zero and the signed
`LysisResultV1.event_summary_hash` is exactly
`H("OUTBE_OCOMP_LIST_EMPTY_V1", u16_be(5))`. Any other count/root pair causes
abstention. This pre-result commitment is distinct from the post-activation
`ApplyEventSummaryHash` over owner state-event digests and is never compared
with it. A non-empty semantic-event population requires new pinned bundle
semantics.

`RootReduceSummaryV1` carries fixed-capacity positional coverage trees: 256
slots per primary shard for each Nod/bucket/contributor list and one slot per
primary shard for both the output-manifest entry and result-chunk hash lists.
Real records occupy the shard-local prefix and frozen globally indexed pad
hashes occupy the remaining positions; canonical empty leaves are all-pad
trees. Reducers merge only equal-height adjacent carriers. These carrier roots
prove complete reducer coverage but are not canonical dense result-list roots
and receive no ordered-list root wrapper.

`OutputManifestEntryV1` binds one chunk ordinal and semantic `ResultChunkHash`
to one typed content-addressed reference containing only exact length,
transport digest and `ResultChunkV1` OCB1 kind. It contains no path or
validator-local namespace. A leaf stages the chunk and includes the entry in
its durable semantic artifact; the supervisor must reopen and verify those
exact bytes and durably admit both artifact and chunk before the leaf becomes
`VERIFIED`. No inbox scan or control-only descendant reference is accepted.
The finalizer independently streams the exact admitted entries/records to
derive `output_manifest_root`, `result_chunk_list_root` and every canonical
result root, and cross-checks all summary carriers, counts and totals. The
manifest proves addressing integrity, not data availability.

The finalizer accepts no caller-built `LysisResultV1` and no precomputed result
root. Its semantic author is the pinned Lysis program; the supervisor is only
the invocation/admission host. The finalizer is not a schedulable unit, new
program, signer or generic framework. The final `ROOT_REDUCE` artifact remains
excluded from `unit_artifact_root` to prevent self-reference, while its summary
is bound through the semantic result and `ResultDigest`.

The result identity is exactly:

```text
ResultDigest =
  H("OUTBE_OCOMP_RESULT_V1", canonical(LysisResultV1))
```

No activation payload, submitter identity, signature ordering or fourth-slot
accountability field participates in that semantic digest.

### PoC evidence profile

The first devnet fixes:

```text
n = 4 result validator domains
f = 1 faulty or unavailable domain
q = 3 distinct matching on-chain votes
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

### On-chain result votes

After its attestation gate signs the closed result subject, the validator
domain's existing `OffchainLysis Supervisor` owns submission. It replaces its
superseded relay publication edge with one signed
`submitLysisResult` EVM transaction through the public RPC, txpool,
gossip, proposal, import and replay path:

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

`ResultVoteV1` is a signed full-result submission, not a digest-only
announcement. `LysisResultV1` is a constant-size typed summary containing the
complete job bindings, roots, counts, totals, carry-over action and arithmetic
commitment; it never contains the `N` Nod/contributor records or result-chunk
bodies.

The executor bounded-decodes and structurally verifies the canonical result,
then reconstructs `ResultDigest`. The inner OCOMP signature binds the
chain/genesis/fork, bundle, job, attempt, committee, key epoch, result purpose
and that exact digest. The outer EVM
transaction uses the validator's EVM identity through a restricted node-owned
signing seam; the Supervisor never receives either private key. A dedicated
ZeroFee hook, constrained like Oracle's existing hook to the exact selector,
zero value, bounded envelope and eligible validator, waives the native fee.
ZeroFee performs no JobIntent, finality, window, digest, quorum or slashing
validation. Those checks belong exclusively to the OCOMP on-chain module.

The Supervisor persists `prepared -> submitted -> included -> finalized`,
rebroadcasts the same logical vote after an orphaned inclusion while the slot
is empty and the window is open, and stops at finality or window close.
For the PoC it reads the validator account nonce from canonical `latest` state,
uses the frozen bounded gas envelope and never calls `eth_estimateGas` or asks
RPC to execute a `pending` block. Its single-writer vote journal preserves the
exact node-signed transaction bytes and nonce for rebroadcast; an exact retry
does not reconstruct or re-sign the envelope.
Consensus verifies protocol eligibility and the OCOMP signature before
recording the vote. No distinguished relay, collector or off-chain certificate
builder is required.

Consensus stores one bounded `ResultVoteSlotV1` for each of the four validator
indexes. A slot records the first valid digest, key epoch, signature and
consensus-assigned inclusion height; it does not retain four copies of
`LysisResultV1`. Exact resubmission is idempotent. A second valid signature by
the same validator for a different digest never replaces or adds to the first
tally; the first such conflict records bounded `EquivocationEvidenceV1`
containing both signed vote identities. Further distinct conflicts cannot grow
state and reject.

After each accepted vote, consensus scans exactly four slots and groups their
first digests by byte equality. When one digest first reaches `q=3`, consensus
immutably records:

```text
quorum_digest
quorum_height
quorum_signer_bitmap
quorum_evidence_hash
```

The q-forming transaction already carries the canonical `LysisResultV1`.
Inside the same outer checkpoint, consensus stores that result exactly once,
constructs the private Lysis-scoped apply capability and executes the
constant-size typed root/scalar transition. There is no durable
`QUORUM_READY` waiting state, public `activateLysis` call, relay or activator.
The transition is:

```text
VOTING_OPEN
  -- third matching ResultVoteV1 --> COMPLETED
  -- third matching result with stale target preconditions --> CONFLICTED
```

Invalid result evidence rejects before filling a slot. An expected target
precondition conflict commits the quorum evidence and deterministic
`CONFLICTED`/retry outcome without owner effects. Any unexpected verifier or
owner error reverts the whole q-forming transaction, including its new slot and
quorum, so it can be retried without partial state.

No later vote can change the selected digest or terminal result. The fourth
validator may still submit until the exclusive response deadline. Its vote
updates only a separate bounded `OcompVoteAccountabilityV1` record keyed by
`JobId`:

- matching produces observable `4/4`;
- a different digest is retained as a minority result but is not automatically
  slashable;
- no timely vote produces a missing-response bit;
- two different signed digests produce objective equivocation evidence.

At the deadline consensus closes `OcompAccountabilitySummaryV1` inside that
record. The immutable `LysisTerminalV1`, apply receipt, active-generation
hash, applied domain state and exact-retry identity bind only the already-fixed
quorum evidence and never change because of the fourth vote or deadline close.
The PoC records this evidence but does not introduce monetary slashing policy
or call `SlashIndicator`. Missing-response and equivocation policy, penalty
size, appeals and operator exceptions require a separate ADR. Any later
slashing mechanism must consume only canonical on-chain evidence; relay logs,
mempool observations and supervisor journals are never authority.

### Quorum-applied result evidence

Every validator submission carries one constant-size `LysisResultV1` and no
`ExecutionCertificateV1`. The executor reconstructs `ResultDigest` before
recording the validator slot. The q-forming submission supplies the canonical
result that is stored once and applied atomically; no later transaction repeats
result transport or selects voters.

Result chunks remain authenticated content-addressed artifacts. Before
requesting its node's signature, every validator's separate compute plane must
have validated complete chunk coverage and reduction. The node verifies only
the closed constant-size signing subject; no chunk has independent evidence
weight. A faulty compute plane may consume its own sign-once slot, but
contributes at most that validator domain's first on-chain vote.

Three votes over different result bytes do not form quorum evidence.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| plan/unit/result semantics | pinned Lysis program bundle |
| local scheduling and retry | supervisor journal; non-consensus |
| artifact equality | canonical digest and plan membership |
| OCOMP key custody | node attestation gate plus ADR-S-KEY-001 backend |
| signer eligibility/weight | job-pinned result committee snapshot |
| vote submission/rebroadcast | validator-domain `OffchainLysis Supervisor` |
| EVM transport signature | restricted node-owned validator EVM signing seam |
| vote fee waiver | exact-selector validator-only ZeroFee hook |
| vote inclusion | ordinary public transaction path |
| vote eligibility/signature validity | every node while executing the vote transaction |
| quorum selection | bounded consensus vote state |
| result-apply trigger | the q-forming full-result submission |
| terminal result identity | immutable `LysisTerminalV1` |
| post-quorum fourth-slot evidence | separate bounded `OcompVoteAccountabilityV1` |
| semantic reference | independent golden/reference implementation |

## Invariants

- Equal finalized inputs and bundle produce byte-identical units and result.
- `N` authenticated Tribute produce exactly the canonical shard/range set; a
  shard boundary never drops or rejects a valid next Tribute.
- Worker/scheduler count cannot change result bytes or evidence weight.
- One validator index contributes at most one first vote to the tally.
- The OCOMP key is distinct from the consensus key and unavailable to compute
  processes.
- Sign-once history is durable before signature release.
- A vote binds one exact result digest and one exact job attempt.
- The quorum digest is set only by three matching eligible on-chain vote slots.
- The fourth slot remains writable until the response deadline even after
  quorum application.
- A fourth vote or deadline close cannot change terminal/result,
  active-generation or exact-retry identity.
- A conflicting second vote records equivocation evidence and never replaces
  the first tally vote.
- Absence and timeliness are derived only from canonical inclusion heights.
- Quorum evidence never proves data availability, domain-spec correctness or
  implementation diversity by itself.

## Atomicity, replay and failure

Local unit/reducer work is content-addressed and freely replayable. Completion
of a proper subset of work shards is not a result and is never signable or
applicable. The
sign-once journal update is write-before-sign and crash-safe: an uncertain write
disables signing until reconciled. A worker/supervisor crash cannot corrupt
consensus state.

Each non-q-forming vote transaction changes only its bounded accountability
state. The q-forming vote executes the constant-size certified apply inside the
same outer checkpoint. Exact vote retry is idempotent. A conflicting retry
records bounded equivocation evidence without replacing the first vote or
contributing twice. ZeroFee classification only waives native fee debit; all
execution still consumes consensus-accounted gas/work, and a protocol-invalid
vote is rejected by OCOMP with no vote-state change.

One unavailable domain still permits `q=3` and immediate atomic application.
There is no post-quorum liveness dependency on a separate submitter. Two
unavailable domains produce no fallback or lower threshold; the
job reaches its on-chain expiry path. A deterministic mismatch is retained as
evidence and no local “majority choice” rewrites the job.

## Determinism and bounds

Per-shard/per-chunk counts and bytes, live workers, vote bytes/signatures and
q-forming apply work are checked against the generated PoC capacity profile
before allocation or verification. The consensus vote state is fixed at four
bounded slots plus one terminal `LysisResultV1` and therefore independent of
total Tribute count. Total Tribute, unit and result-chunk counts
are checked for arithmetic validity and exact committed coverage, not capped by
the PoC. Unit and result-chunk artifacts do not enter activation state
individually. Exactly four bounded full-result vote transactions are carried
through the normal public path; the q-forming one also performs the bounded
root/scalar apply. `ShuffleRunArtifactV1` bounds each leaf to 256 records
and each internal node to two child references; the number of leaves and nodes
is derived from the uncapped Tribute population and is never a consensus cap.

## Compatibility and migration

The result digest and signature domain pin the exact protocol bundle. Capability
negotiation may cause a validator to abstain but cannot choose consensus
semantics. A changed planner, reducer, result meaning, signature domain or
committee schema requires a new bundle and golden vectors. Live jobs finish or
expire under the bundle and committee pinned at creation.

BoundedMVP may harden keys, scheduling and retention while preserving this
execute, full-result on-chain vote and quorum-apply meaning. A proof-carrying
TargetLarge profile is separate evidence semantics and cannot be presented as
PoC completion.

## Production-interface verification evidence

No OCOMP planner, workers, signer, sign-once journal or on-chain vote path
exists in the baseline inspected by this ADR. Required evidence includes a boundary fixture whose Tribute count is
one greater than a full work shard, synthetic plan derivations for 10,000 and
1,000,000,000 Tribute without proportional plan allocation, independent full
executions of the bounded fixture by four real validator domains, a healthy
`4/4` matching run, a one-domain-unavailable `3/4` application, a two-domain
unavailable expiry, 1/2/4-worker equality, randomized order/retry, restart-safe
sign-once refusal, public vote inclusion and replay, duplicate/wrong/late voter
rejection, conflicting-vote evidence, one-byte/ordering/JobId mutation
rejection and comparison with a separate reference corpus.

## Consequences

The PoC establishes decentralised correctness and consensus-visible
accountability through bounded work without on-chain Lysis or a total Tribute
cap, but accepts four additional public transactions per job and that three
implementations can share one semantic bug. The reference corpus, adversarial
vectors and later implementation diversity remain required; quorum is not an
excuse to skip specification testing.

## Rejected alternatives

- **One central calculator:** creates a trusted sequencer/oracle for Lysis.
- **Count many workers as many voters:** they share one validator failure domain.
- **Sign an arbitrary digest supplied by the supervisor:** key authority can be
  redirected to unrelated statements.
- **Collect votes only in an off-chain relay:** canonical consensus cannot prove
  which validator missed its response window or whether the relay censored it.
- **Let any submitter select “close enough” results:** result equality must be
  exact and the four-slot tally is consensus-derived.
- **Require all four votes for application:** loses the selected one-domain
  unavailable liveness property; the fourth is retained for accountability,
  not made a veto.
- **Rerun Lysis on-chain:** defeats the purpose and scale boundary of OCOMP.
- **Lower `q` on timeout:** changes the fault model exactly when availability is
  weakest.

## Open questions and technical debt

1. Freeze exact OCOMP key type, proof-of-possession registration, epoch history
   and compromise/revocation interaction.
2. Freeze the sign-once journal schema, fsync contract and recovery procedure.
3. Produce an independent Lysis reference implementation and adversarial golden
   corpus before allowing signatures.
4. Generate all `UnitSpecV1`, `UnitId`, result, vote-slot, quorum and
   accountability golden vectors.
5. Prove maximum result/vote-signature verification work through the public
   RPC/txpool/P2P/import/replay path.
6. Define durable mismatch diagnostics without leaking raw user data.
7. Define monetary missed-response/equivocation policy in a separate slashing
   ADR; the PoC supplies evidence only.
8. Define BoundedMVP cleanup/retention for closed vote slots and terminal result
   records.
