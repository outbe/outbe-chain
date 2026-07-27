# Off-chain PoC implementation-planning decision map

Status: **HISTORICAL DECISION MAP — SUPERSEDED ITEMS MUST NOT BE IMPLEMENTED**

Decision #7 and every dependent certificate/relay/digest-only-vote/separate
activation conclusion are historical. The authoritative replacement is a
signed full-result vote from each validator and atomic application by the
q-forming transaction, with no durable `QUORUM_READY` or public activator, as
defined by ADR-S-OCM-003/004, PFS-002, `off-chain-poc.md` and the implementation
plan.

Normative inputs:

- [`off-chain-computation.md`](../off-chain-computation.md)
- [`off-chain-poc.md`](../off-chain-poc.md)
- [`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
  through
  [`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md)
- [`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md)

The map resolves implementation parameters only. It cannot reopen the selected
architecture or promote deferred BoundedMVP/TargetLarge work into the PoC.

## #1: What is the exact planning and completion boundary?

Blocked by: none

Type: Discuss

### Question

What must the implementation plan produce, and what must it explicitly exclude?

### Answer

Resolved by the source documents and goal:

- plan one fresh-devnet Lysis V1 vertical slice over bounded work units;
- preserve the OCOMP-kernel/typed-program boundary and PoC-to-BoundedMVP core;
- require `POC-01..POC-26`, non-deferred `PFS-002` scenarios and the exact
  thirteen-step demonstration;
- make tests, reproducible commands and retained evidence part of every task's
  Definition of Done;
- exclude implementation in this planning goal, a generic registry/adapter,
  second program, supported-network rollout, production hardening and
  TargetLarge;
- keep `PFS-002-07` and `PFS-002-08` deferred.

## #2: Which current production seams can the PoC reuse?

Blocked by: #1

Type: Research

### Question

What exact files, symbols, state owners, checkpoints, finality data, body readers,
process harnesses and public paths exist today, and where must new boundaries be
introduced?

### Answer

Resolved. See the
[`current-code map`](off-chain-poc-current-code-map.md).

The synchronous extraction point is `process_metadosis` ->
`outbe_lysis::runtime::lysis`. Consensus finality, exact CE parent identity,
authenticated body reads, normal public transaction execution, typed
checkpointed storage and the existing process-aware E2E harness are reusable
anchors. Mongo/projection readers are not an immutable input proof, and the
OCOMP job, export, process, key, artifact, certificate, activation and lifecycle
surfaces do not yet exist. Exact crate/process placement remains deliberately
deferred to the tickets that first freeze semantics and protocol bytes.

## #3: What is the frozen legacy Lysis semantic baseline?

Blocked by: #2

Type: Research

### Question

Which exact current arithmetic, ordering, Fidelity/Oracle inputs, events and
effects must Lysis V1 preserve, and what independent corpus proves equivalence?

### Answer

Resolved. See the
[`Lysis V1 semantic baseline`](off-chain-poc-lysis-v1-semantics.md).

PoC freezes the current successful economic transformation and deterministic
first-failure order, but not synchronous storage calls, activation-time inputs
or the reverted `FAILED` diagnostic. Runtime `f=allocation/nominal` and
`fmax=2f` are authoritative; stale 8%/16% comments and unused test constants are
not. Zero/over-budget allocation fails the whole local execution, valid input
has unique owner/day identity, and Fidelity/Oracle observations are reconstructed
from one request-pinned authenticated snapshot. A separate test-only Rust
reference crate with arbitrary-precision arithmetic and no production
dependencies, a versioned JSONL semantic corpus, mandatory edge classes and a
non-self-referential freeze gate are selected. Consensus bytes and hashes
remain ticket #4.

## #4: Which fork-pinned bytes and bounds must be frozen first?

Blocked by: #3

Type: Discuss

### Question

What are the exact fork/profile identity, canonical codecs, hash/signature
domains, IDs, intent/split/precondition/result/chunk/receipt schemas, deadlines and
generated cap values?

### Answer

Resolved. See the
[`protocol-byte and capacity freeze`](off-chain-poc-protocol-freeze.md).

PoC now has one exact fork/profile identity, OCB1 canonical grammar and object
registry, public `activateLysis(bytes)` ABI, hash/root/signature contracts,
closed intent/input/unit/result-chunk/result/certificate/precondition/receipt schemas,
exclusive 64-block deadline and fixed phase behavior. Candidate limits are
upper bounds. Ticket #10 removed a dependency cycle by splitting the gate:
`P0-PROTOCOL-SHAPE-FREEZE` freezes schemas/domains/registries and non-armable
candidate ceilings before dependent runtime work; after the real public path
exists, `P1-POC-CAPACITY-AND-ARMING` measures cap-1/cap/cap+1, emits the final
genesis/bundle/profile/committee manifests and is the only task allowed to arm
the canonical fresh-devnet fork. Provisional fixture bytes may exist only in a
harness-owned disposable measurement chain and can never become final PoC
history or closure evidence.
This avoids both inventing capacity numbers and blocking their measurement on a
runtime that is forbidden to exist.

Ticket #5 found two current Oracle loops whose ranges were not represented in
the first cap profile. The freeze is amended with candidate 256-entry WWD and
active S-curve bounds plus the already-required 64-finalized-block terminal
evidence window. These are bounded-profile closure, not new behavior; generation
may only reduce the data-dependent caps.

The 2026-07-24 correction separated the complete parent job from worker-sized
work: one `JobIntentV1` covers arbitrary `N`, while constant-size
`PlanCommitmentV1` commits
`ceil(N / max_tributes_per_work_shard)` ordered work shards by count/root.
There is no complete-job Tribute ceiling. The candidate
`max_tributes_per_work_shard=256` bounds one invocation only; the 257th Tribute
starts shard 2, 10,000 produce 40 shards and 1,000,000,000 produce 3,906,250.
Final generation may lower the per-shard value but cannot create a total
population cap.

The 2026-07-24 execution-binding amendment also makes three request-context
values explicit in `PlanCommitmentV1`: `wwd`, `lysis_budget` and
`logical_evaluation_time`. They are covered by `PlanHash`; workers authenticate
the exact commitment and its job/manifest bindings, while the node attestation
gate compares the values to the finalized intent before signing.
It also fixes the `AMOUNT_MAP(j)` dependency grammar: the unit consumes both
`FIDELITY_MAP(j)` for per-Tribute league observations and the fixed-reduce root
for the global fraction table. This is a correction of missing commitments, not
a new phase or a wider PoC scope.

## #5: How is finalized authenticated input retained and exported?

Blocked by: #2, #4

Type: Research

### Question

Which existing finality proof/data, Reth/CE/MDBX checkpoint primitive, Mongo body
reader and Fidelity/Oracle opening mechanism can implement tentative/final pins,
full-fold verification and a bounded read-only exporter without trusting local
storage?

### Answer

Resolved. See the
[`finalized input retention and authenticated export decision`](off-chain-poc-finalized-input-export.md).

The exact authority is finalization certificate/header/state root -> intent and
historical committee proofs -> exact CE MDBX read transaction -> rebuilt sealed
WWD root/count/nominal -> commitment-verified Mongo bodies plus raw
state-root-verified Fidelity/Oracle openings. Mongo is discovery/transport, not
completeness. A one-entry journal is durable before a positive vote; an orphan
releases it, terminal data remains for 64 finalized blocks, and ambiguity makes
only that validator abstain.

The exporter opens the exact CE marker in a bounded next-apply gate, while Reth
uses exact block-hash history and Mongo may be ahead only after canonical
containment. Current code needs narrow additions for a true CE read-only open,
typed historical proof source, retained Tribute namespace, pin coordinator and
CAS; it does not need a historical CE query service, generic framework or second
projection database.

## #6: What is the smallest safe local process and artifact topology?

Blocked by: #5

Type: Discuss

### Question

Which binaries or modes, UIDs, UDS messages, CAS layout, quotas, cgroups and
supervision hooks demonstrate separate validator-domain failure boundaries
without creating unnecessary crates or deployment machinery?

### Answer

Resolved. See the
[`process and artifact topology`](off-chain-poc-process-and-artifact-topology.md).

The minimal shape is one narrow shared protocol crate plus one `outbe-ocomp`
package/executable with fixed supervisor, snapshot-exporter, worker and relay
modes. Each validator uses sibling node/exporter/supervisor services, a separate
quota-controlled filesystem CAS and systemd per-connection worker activation:
`Accept=yes` creates one process for one immutable `UnitId`, while
`MaxConnections=4` enforces the PoC parallelism without a launch broker.

Stable UIDs, `SO_PEERCRED` method ACLs, bounded versioned local frames,
digest-only CAS paths, atomic publish/same-descriptor verification, read-only
worker CAS mounts and generated cgroup/disk budgets fix the authority and fault
boundaries. A required systemd/cgroup-v2 evidence lane proves real UID, mount,
socket, quota and lifecycle separation; an unprivileged harness lane may test
the same process entrypoints but cannot claim OS isolation.

The review exposed one omitted byte formula in ticket #4:
`TransportDigestV1 = keccak256(exact stored bytes)`. The protocol freeze now
states it explicitly together with input-manifest/chunk/unit-artifact hash
derivations; this closes an existing `transport_digest` field rather than
expanding scope.

## #7: How are deterministic execution and q=3/4 evidence implemented?

Blocked by: #3, #4, #6

Type: Research

### Question

What exact planner/unit/reducer seams, OCOMP key epoch, durable sign-once record,
certificate verifier, relay message and mutation vectors provide independent
byte-identical execution and one vote per validator domain?

### Answer

Resolved. See the
[`deterministic execution and quorum decision`](off-chain-poc-deterministic-execution-and-quorum.md).

Every domain now derives one constant-size producer-bound plan commitment before
execution. Work units and reducer nodes are derived lazily by ordinal from
fixed profile-bounded work-shard ranges; external sort, prefix, shuffle and
reduction use bounded runs/cursors for arbitrary unit count. One, two or four
socket-activated workers schedule immutable `UnitId`s; they never alter plan
bytes or evidence weight. Phase verifiers recompute outputs and exact
no-gap/no-overlap coverage before CAS adoption, and an independent corpus
remains mandatory. The `S+1` fixture must place its last Tribute in shard 2; a
missing shard or result chunk prevents reduction and signing.

The validator domain's separate compute plane validates every bounded
`ResultChunkV1`, reduces to `RootReduceSummaryV1` and invokes the pure typed
`LysisProgramV1` finalizer over exact durable verified catalogs. The node
reloads finalized job/export state, validates only the constant-size closed
`LysisResultV1` bindings/equations and derives `ResultDigest` itself; it never
scans bulk chunks or reruns Lysis. A node-only epoch-1 OCOMP key writes an immutable
OCB1 sign-once record with file fsync, no-clobber hard-link publication and
directory fsync before releasing its deterministic low-`s` signature. Existing
best-effort JSON journals are explicitly not reused. The relay accepts bounded
OCB1 announcements, groups exact result commitments, verifies indexed signatures,
selects exactly three distinct indexes and builds finality/state proof from
existing public exact-block RPC.

This review also closed two protocol contradictions before implementation:
derived unit inputs now bind producer `UnitId`s instead of unknown future
semantic roots, and `unit_artifact_root` excludes only the final
`ROOT_REDUCE` carrier to avoid self-reference. New interval/input/output,
coverage, validator-identity, committee and sign-once hash formulas are recorded
in the protocol freeze.

The later A′ review closes the final-result authority gap: bounded
`ROOT_REDUCE` emits `RootReduceSummaryV1`, not `LysisResultV1`; the supervisor
hosts but cannot parameterize the closed program finalizer; and the finalizer
derives unit/chunk/fraction/prefix roots from bounded cursors over exact durable
admission. Passing the complete catalog through `RunUnitV1` was rejected because
it expands the worker/control seam without adding Byzantine evidence.

The consistency pass additionally fixes raw-ordinal coverage as an exact
list-kind root, gives Fidelity its own canonical opening-index range, and pads
prefix/root input vectors to the same fixed tree width. Static committee
generation is two-stage: base genesis and bundle first, registrations/PoPs
second, then a final chain manifest. This removes the otherwise circular
requirement to embed a PoP over `genesis_hash` inside the genesis being hashed,
without adding a runtime registration API.

The 2026-07-26 activation-binding amendment closes the production request gap.
The final chain manifest carries one canonical immutable
`OcompForkInstallV1` containing classification, `AtBlock(H)`, the complete
request profile, exact bundle and complete result committee. Every node loads
and validates it before startup and supplies the same typed value to proposal,
import, replay, consensus and txpool. At `H`, the existing empty-body
`OcompLifecycleBegin` atomically installs request and activation authority
before expiry. No new SystemTx, local height/profile override, hot reload,
state injection or registration API is introduced. The existing
protocol-version-1 Update handler remains the sole owner of Tribute, Fidelity,
Oracle and Metadosis pre-admission initialization at the same height.

## #8: How is the result activated and applied atomically?

Blocked by: #3, #4, #5, #7

Type: Research

### Question

Where do JobIntent/FSM/expiry live, how does the normal public activation
transaction enter execution, and which private typed capability and owner
receipts replace synchronous Lysis while preserving complete rollback?

### Answer

Resolved. See the
[`activation and atomic-apply decision`](off-chain-poc-activation-and-atomic-apply.md).

The implementation uses two mandatory SystemTx V2 lifecycle envelopes:
`OcompLifecycleBegin` expires/releases before `CycleTick`, while
`OcompTerminalRequest` runs as the sole end-zone kind after ordinary
transactions and actual compressed-entity sealing. This corrects the earlier
assumption that request creation could remain inside begin-zone `CycleTick`;
only the end-zone can bind the intent to the request block's final CE root.

The concrete job/FSM and bounded due/expiry indexes live under Metadosis.
`activateLysis(bytes)` remains one normal paid transaction to the existing
Metadosis address, with three bounded OCB1 views and no custom RPC or ZeroFee
change. The public handler reaches current consensus storage through the normal
execution scope and one outer checkpoint.

After complete finality, q=3/4 certificate and Lysis-result verification, one
runtime-only `CertifiedLysisActivation` permits exactly four owner calls in
order: NodFactory, Intex, Tribute and PromisLimit. The stored request split
receipt, activation receipts, state-event digests, conservation equations and a
terminal permit
close the boundary; there is no generic write set. Any owner failure or receipt
mismatch rolls back the complete activation. Certified CAS conflict releases
and requeues only after valid evidence, while exact completed retry returns the
stored receipt without effects or events.

The protocol freeze now records the job schema, nonce/attempt rule, capacity
limits, request/expiry ordering, selectors, errors, event topics, receipt/hash
equations, active generation and public reads. Current owner helpers are reused
only behind certified methods: timestamp-sensitive Nod creation takes request
logical time, Desis bypasses the existing best-effort wrapper, and Intex uses a
strict checked batch path.

## #9: What evidence proves each requirement through a real boundary?

Blocked by: #4, #5, #6, #7, #8

Type: Discuss

### Question

What test IDs, commands, allowed oracles, retained artifacts and CI gates prove
every ADR invariant, `POC-01..POC-26`, non-deferred `PFS-002` row and each step
of the thirteen-step story?

### Answer

Resolved. See the
[`test and evidence decision`](off-chain-poc-test-and-evidence.md) and its
machine-readable
[`planning ledger`](off-chain-poc-evidence-ledger.yaml).

PoC proof is split into byte/pure/model, production-seam integration,
public fork/execution, four-domain E2E and privileged isolation layers. The
ledger defines 37 stable planned test IDs and maps all 34 invariants from
ADR-S-OCM-001..004, `POC-01..POC-26`, `PFS-002-01..25` and story steps 1..13.
Only `PFS-002-07/-08` are DEFERRED.

The existing Rust/Cucumber harness remains the single orchestration owner. It
gains OCOMP process/CAS handles, public OCOMP reads/transactions, declared fault
controls, structured block-boundary traces and a run-level evidence manifest.
The current per-scenario evidence is insufficient by itself, so a small
independent verifier in the same test package recomputes artifact/coverage
closure and fails on missing, skipped, todo, quarantined or retried-away claims.

The final story uses four real node/OCOMP domains, real UDS/Mongo/CE/checkpoints,
mock Gramine only for the existing encrypted Tribute interface, an untrusted
relay and normal RPC/txpool/P2P/proposal/import/replay. A separate systemd/
cgroup-v2 lane proves UID, mount, socket, quota and failure isolation. Mongo,
CAS, supervisor state, direct handlers, a central calculator and on-chain Lysis
are forbidden outcome oracles.

Stable planned commands separate fast PR, integration, public-path, E2E,
isolation and evidence-verification lanes. Automatic test retries are zero;
full closure consumes one exact source/binary/genesis/fork/bundle/profile
identity and a retained hash-indexed evidence bundle.

## #10: What is the dependency-ordered implementation task graph?

Blocked by: #2, #3, #4, #5, #6, #7, #8, #9

Type: Discuss

### Question

How should the frozen decisions and implementation work be split into small
reviewable tracer-bullet vertical slices that can be taken directly into work?

### Answer

Resolved. See the canonical
[`implementation plan`](off-chain-poc-implementation-plan.md).

The plan contains 28 dependency-ordered tasks (`OCM-00..OCM-27`) behind nine
observable gates. Each task names dependencies, concrete files/symbols,
interface and state changes, invariants and failures, fork impact, reuse and
non-goals, test-first work, retained evidence, CI ownership, risks and an
observable Definition of Done.

The graph starts with the evidence contract and pure semantic/byte foundations,
then joins finalized-input export, independent execution, q=3 evidence and the
four typed activation owners plus the stored request split receipt at one normal
public atomic activation. The existing
four-node harness is extended only after production seams exist. The final
capacity task depends on a public measurement suite and is the sole owner of
fork arming; final E2E runs only against those exact generated artifacts.

The companion
[`planning ledger`](off-chain-poc-evidence-ledger.yaml) now gives every one of
the 37 stable test IDs exactly one closing task owner. Contributing tasks keep
task-local tests but cannot claim a system requirement closed. This makes
requirement -> test -> evidence -> lane -> task and task -> acceptance/evidence
traversable in both directions.

The dependency review found and removed the only structural cycle: capacity
needed the real public path, while the earlier freeze wording required final
capacity before that path could merge. The two-gate `P0` shape freeze / `P1`
capacity-and-arming design keeps dependent code inert until measured artifacts
exist without allowing a provisional network to become closure evidence.

## #11: Is the resulting plan complete, minimal and independently verifiable?

Blocked by: #10

Type: Research

### Question

Does a reverse traceability and dependency audit find any missing owner,
unverified requirement, circular dependency, duplicated authority, scope
expansion, code-bloat seam or untestable completion claim?

### Answer

Resolved. See the
[`implementation-plan audit`](off-chain-poc-implementation-audit.md).

Reverse checks cover the 34 source-derived ADR invariants, all 26 POC rows, all
24 PFS identities, the thirteen-step story, section 17 deliverables, all
section 22 decisions and the eleven planning-goal requirements. The
machine-readable ledger has 37 stable tests; every test is required somewhere
and has exactly one closing task. All 28 tasks are represented, including tasks
whose merge evidence is local and whose system evidence closes downstream.

The audit found and fixed real planning defects: runtime capability placement
would have required a public factory/dependency cycle; incremental and closure
CI claims were ambiguous; exact configuration hashes, private-key file format
and the minimum-machine/headroom rule were incomplete; and six direct task
dependencies were absent from the diagram. The corrected 72-edge graph exactly
matches every card and is acyclic.

Authority remains singular and the implementation shape remains minimal:
exactly one pure protocol package and one fixed-mode compute binary are new.
No generic registry/adapter, second program, ZeroFee change, TargetLarge work,
production rollout or deferred PFS behavior is required by a Definition of
Done; the closure card names the two deferred IDs only as exclusions.

Verdict: the **implementation plan** is complete and may start at `OCM-00`.
The OCOMP PoC itself remains unimplemented; only `OCM-27` and a fail-closed
exact-artifact evidence report can later claim PoC completion.
