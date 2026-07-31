# ADR-B-OCD-005: Make MongoDB the Tribute and Nod body read path

- **Status:** Proposed; migrated design, current implementation evidence requires reconciliation
- **Date:** 2026-07-15
- **Depends on:** ADR-B-OCD-004

## Context

ADR-B-OCD-004 continuously materializes finalized Tribute and Nod body events into one transaction-capable MongoDB database per node. It deliberately leaves runtime and user reads on the existing EVM body maps so projector failure affects only derived storage.

The next stage removes that shadow arrangement. Tribute and Nod complete bodies and query indexes move out of active EVM storage, while the compact protocol aggregates and scheduling structures identified by ADR-B-OCD-002 remain in EVM state.

This is an explicit pre-production/testnet stage. MongoDB does not yet have an independently verifiable body commitment; ADR-B-OCD-006 adds that check. Until then, every validator and full node using this stage accepts the operational risk of its own local MongoDB materialization.

This ADR records the stricter readiness, retry, shutdown, and recovery decisions now so they are not rediscovered when the read cutover is implemented.

## Starting system

At the start of ADR-B-OCD-005:

- complete body mutations are present in canonical receipts;
- Reth ExEx projects finalized receipts into MongoDB;
- body plus secondary-index mutations are atomic per successful EVM receipt;
- the projector persists an exact finalized `{block_number, block_hash}` checkpoint;
- runtime and RPC body reads still use EVM;
- projector failure does not stop the node;
- complete EVM body maps and query indexes remain active.

## Added capability

The first complete Mongo-backed Tribute/Nod runtime: body-dependent execution and query paths read through the ADR-B-OCD-002 repositories, and the legacy complete per-entity EVM body maps/indexes cease to be an active fallback.

## Decision

### Explicit testnet-only dependency

ADR-B-OCD-005 deliberately makes each node's local MongoDB a body-read dependency before ADR-B-OCD-006 makes altered bodies independently detectable.

This exception is acceptable only for the pre-production staged rollout:

- each validator uses its own logical database;
- full nodes use their own logical database when running the cutover mode;
- databases are never shared by several active node writers;
- missing, malformed, stale, or unavailable data fails explicitly;
- there is no fallback to the removed EVM body map;
- a well-formed locally altered row remains an accepted limitation until ADR-B-OCD-006.

Production/mainnet activation must not rely on this unauthenticated intermediate profile.

### Runtime read cutover

Switch all complete-body and body-query consumers identified by ADR-B-OCD-002 to the typed repository readers, including:

- Tribute body processing and burn paths;
- Tribute owner/day body queries;
- Lysis Tribute partition/body reads;
- NodFactory Nod body reads used by mining and payment flows;
- Nod item and bucket reads;
- Nod owner/global body queries;
- Gratis inputs that consume Tribute/Nod bodies;
- metadata and user-facing point/list body reads.

Callers receive typed domain records. They do not receive `Namespace`, raw keys, Postcard bytes, BSON, MongoDB sessions, or collection handles.

The runtime receives read authority only. Receipt projection remains the sole production writer of the Mongo materialization.

### EVM state retained and removed

Retain compact protocol state identified by ADR-B-OCD-002.

For Tribute, retain at least:

- total supply;
- day totals;
- day seal state;
- monetary aggregates used directly by protocol transitions.

For Nod, retain at least:

- total supply;
- bin-tree root/mid/leaf scheduling structures;
- unqualified-bin counts and bucket-key worklists;
- other compact control state required to choose the next bucket.

Remove or disable the active complete per-entity EVM paths after the cutover:

- Tribute body map and owner/day body indexes;
- Nod item map;
- Nod bucket body map;
- Nod owner/global body indexes.

There is no long-lived dual-read mode. Tests may build fixtures through dedicated helpers, but production runtime does not silently fall back to legacy EVM bodies.

### Required projector configuration

Unlike ADR-B-OCD-004, Mongo projection is mandatory for every node running the final ADR-B-OCD-005 through ADR-B-OCD-010 implementation.

The configured database must satisfy the ADR-B-OCD-004 capability contract:

- transaction-capable replica set or sharded cluster;
- supported `storage_schema_version`;
- matching `chain_id` and `genesis_hash`;
- matching configured and persisted `start_block`;
- one logical database and one active node writer;
- canonical checkpoint block hash;
- no unmanaged body/index data.

A node cannot enter ADR-B-OCD-005 execution using an optional or degraded projector.

### Lockstep chain and projection catch-up

Node readiness requires two aligned forms of progress:

```text
Reth execution/finalized state
Mongo finalized projection checkpoint
```

On first startup or snapshot restore:

1. start Reth networking and follower synchronization;
2. synchronize and execute chain state;
3. project each newly finalized block into MongoDB;
4. keep business readiness disabled while MongoDB is behind;
5. keep validator voting and proposing disabled while MongoDB is behind;
6. enable business readiness and validator participation only after the projection checkpoint reaches the required finalized execution point.

A full node performs the same state-plus-Mongo catch-up but has no voting/proposing transition.

Outbe has instant BFT finality. Before body-dependent execution of the next height, the node must have completely projected the finalized parent required by that execution. The integration therefore enforces a parent-projection barrier:

```text
execute/finalize block N
-> project block N and persist checkpoint N
-> permit Mongo-dependent execution at block N + 1
```

This barrier applies during live operation and historical synchronization. Reth must not execute an arbitrary batch of later Mongo-dependent blocks while their finalized predecessors remain unprojected.

#### Local projection-readiness gate

ExEx remains the sole MongoDB writer and continues to project finalized blocks asynchronously. ADR-B-OCD-005 does not delay Reth finalization or Marshal acknowledgment while waiting for ExEx:

```text
finalize/canonicalize block N
-> acknowledge Marshal normally
-> ExEx projects N asynchronously
```

A backend-neutral local readiness handle publishes ExEx health and its durable exact `BlockNumHash` checkpoint. It does not expose MongoDB sessions or become part of block, vote, certificate, or fork-choice data.

Conceptually, the interface is:

```text
ProjectionReadinessHandle
  current() -> ProjectionStatus
  wait_for(BlockNumHash, remaining_view_budget) -> WaitOutcome

WaitOutcome
  Ready
  BudgetExpired
  ProjectionAhead
  Fatal

ProjectionStatus
  Starting
  CatchingUp { checkpoint }
  Ready { checkpoint }
  MongoUnavailable { since }
  Fatal { error }
```

The handle is backed by an in-process watch/subscription, not MongoDB polling from proposal or verification handlers. MongoDB `projection_state` remains the durable source. Startup seeds the watch from the validated stored checkpoint, and ExEx publishes a newer exact checkpoint only after its MongoDB checkpoint commit succeeds.

Each wait captures the exact finalized parent required by that one propose/verify/full-node execution request. It never waits for a moving global latest-finalized target. `wait_for(required_parent)` follows these rules:

- checkpoint below the required height waits within the caller's budget;
- equal height and equal hash is `Ready`;
- equal height with another hash is `Fatal`;
- checkpoint above the required height is `ProjectionAhead`, because the unversioned MongoDB contains future state for that request;
- repeating the same request is idempotent.

`ProjectionAhead` is local unavailability for that stale request, not projector corruption: proposer forfeits, verifier withholds rather than voting `false`, and the Mongo outage timer does not start.

For local execution of block `N`, the required projection target is its exact finalized parent, not whatever newer finalized target ExEx may already be chasing.

Before local Mongo-dependent work on a successor, node wiring checks that the required finalized parent has been projected:

- a ready checkpoint is a non-blocking local check and does not alter network consensus processing;
- `handle_propose` waits for the checkpoint only within the remaining proposal/view budget, then forfeits the local proposal slot;
- `handle_verify` waits only within the remaining verification/view budget, then drops/withholds the response and never votes `false` for projection lag;
- full-node historical sync has no consensus view budget and defers local execution of the Mongo-dependent successor while projection remains healthy but behind.

Consensus waits use the Commonware runtime clock and the request's existing remaining budget. They do not create a new wall-clock deadline, reset a view timeout, or block beyond the current proposal/verification request.

The eight-second Mongo-unavailability deadline is independent. Short healthy projection lag can consume the remaining view budget and cause local abstention without starting the outage timer. An actual MongoDB availability error starts or continues the supervisor-owned eight-second window.

A lagging node may miss a proposal or vote while other validators continue by quorum. The checkpoint does not define block validity for the network. Only the local node's ability to execute with its required body materialization is gated.

`ConsensusExecutionBridge` status and ExEx `FinishedHeight` are not substituted for the exact Mongo checkpoint. `FinishedHeight` retains its Reth pruning/backpressure meaning. The readiness handle reports the projector's own durable checkpoint and health.

Historical and live paths must be verified against the pinned Reth revision so no path executes a Mongo-dependent successor using an unprojected finalized parent. This is local execution/readiness wiring, not a new consensus protocol acknowledgment.

### Implementation and deployment sequence

Implement ADR-B-OCD-005 through ADR-B-OCD-010 consecutively on the same development branch before deploying or starting a network with the new compressed-entity path. A temporary same-block read fence would duplicate checkpoint, rollback, scope, error, and test machinery that ADR-B-OCD-007 immediately provides permanently.

Therefore ADR-B-OCD-005 does not add `outbe-fencing`, `BlockBodyReadFence`, guarded temporary readers, a temporary provider decorator, temporary same-block revert rules, or an intermediate CLI/Cargo/chain-spec activation switch.

```text
implement ADR-B-OCD-005 Mongo execution reads
-> implement ADR-B-OCD-006 commitments and verified reads
-> implement ADR-B-OCD-007 permanent journaled body overlay
-> implement ADR-B-OCD-008 unsharded CKB reference stage
-> benchmark and implement ADR-B-OCD-009 sharding
-> implement ADR-B-OCD-010 collections and Root Catalog
-> run the combined acceptance/integration suite
-> one coordinated testnet reset and first CES1 deployment
```

No intermediate ADR-B-OCD-005 binary is deployed. ADR-B-OCD-005 may have focused compile/unit checks while being developed, but its network-level acceptance is evaluated only after ADR-B-OCD-010 completes the first deployed CES1 path.

### Read semantics

Repository semantics remain explicit:

- before ADR-B-OCD-006, `Some(body)` means a valid decodable body exists under the requested identity;
- before ADR-B-OCD-006, `None` produces the normal domain `NotFound`/revert behavior for that local execution and does not start recovery;
- `StorageError::Unavailable` aborts the current local propose/verify execution as infrastructure unavailable, never emits a `false` vote, and publishes `MongoUnavailable` to the shared projection supervisor;
- each synchronous execution read is bounded by the lesser of the request's remaining view budget and a one-second Mongo operation timeout;
- the individual EVM call never owns or waits through the complete eight-second recovery window;
- corruption, dangling indexes, identity mismatches, and checkpoint conflicts publish `Fatal` rather than absence;
- list pages are all-or-error and never silently omit malformed entries;
- no caller falls back to an EVM body after a repository error.

ADR-B-OCD-005 by itself has no body commitment or authenticated non-membership proof. Therefore a locally omitted row is indistinguishable from canonical absence during focused ADR-B-OCD-005-only development. If other validators have the body, the incomplete validator may reject or fail to reproduce their execution and will not contribute a matching positive vote.

Focused ADR-B-OCD-006 and ADR-B-OCD-007 tests use direct EVM commitment mappings: mapping zero is point absence and non-zero plus MongoDB `None` is fatal `CommittedBodyMissing`. No direct-map stage is deployed. The first combined testnet path after ADR-B-OCD-010 uses the CES1 sharded collection/Root Catalog tree for the same authenticated absence/body check.

### Cross-receipt visibility

ADR-B-OCD-004 intentionally does not add block-wide MVCC or historical versions.

Execution/projector Mongo access uses a fixed consistency contract:

```text
readPreference = primary
readConcern = majority
writeConcern = majority
```

Receipt transactions and checkpoint commits require majority acknowledgment. Runtime body/index reads use the primary and majority-committed data; lagging secondary reads are forbidden in execution. Replica-set and sharded-cluster operators may choose topology but cannot downgrade these guarantees. A read/write concern failure is `Unavailable`. A single-node replica set remains supported, with majority determined by its voting topology.

- One EVM receipt's body/index changes are atomic.
- Different receipts in a finalized block may appear progressively while projection runs.
- Mongo reads are not globally blocked during Apply.
- The projection checkpoint is written only after all receipts in the block commit.

Consensus execution does not cross the parent-projection barrier until the complete checkpoint is durable, so it never consumes a partially applied parent block.

User-facing reads may observe the last available per-document state while the current finalized block is being applied. Status surfaces expose the current projection checkpoint so operators and clients can determine materialization freshness. A direct MongoDB reader receives no stronger block-wide snapshot guarantee.

### Eight-second Mongo reconnect deadline

MongoDB connection recovery uses one fixed total deadline:

```text
mongo_reconnect_deadline = 8 seconds
```

The deadline applies both:

- during initial startup; and
- after a runtime connection failure.

It is eight seconds total from the first connection/unavailability error, not eight seconds per retry attempt. Recovery requires an acknowledged MongoDB transaction-capability operation or successful retry of the failed transaction; a TCP connection or ping alone does not end the outage window.

The retry schedule is fixed:

```text
retry_interval = 1 second
total_deadline = 8 seconds
initial_retry = immediate
```

The first recovery attempt starts immediately. Later attempts start at one-second intervals while the total deadline remains open. Attempts never overlap, and each MongoDB connect/server-selection/operation timeout is capped by the remaining total deadline. The node does not wait eight seconds before making its first attempt.

After one acknowledged operation ends an outage, a later independent availability failure starts a new eight-second window. A failure encountered while replay/catch-up is still trying to establish that first acknowledged operation remains inside the original window.

On a runtime connection failure:

1. mark business readiness false immediately;
2. suspend new validator voting/proposing that requires the missing projection;
3. keep the projection checkpoint and Reth `FinishedHeight` unchanged;
4. retry MongoDB connection/transaction capability for at most eight seconds;
5. if connection recovers, replay any uncertain receipt transaction idempotently;
6. catch MongoDB up to the required finalized execution point;
7. restore readiness and validator participation only after catch-up;
8. if connection does not recover by the deadline, initiate graceful whole-node shutdown.

Catch-up time after a successful reconnection is not counted inside the eight-second connection deadline. While catch-up is healthy but incomplete, readiness and validator participation remain gated.

The maximum protocol consequence of a local outage is that the validator misses one or more votes/views. The node must not guess body state merely to remain online.

### Long-lived ExEx runner and projection supervisor

A transient MongoDB failure does not terminate or dynamically reinstall ExEx. Reth expects one long-lived ExEx future, and delivered notification lifecycle must remain owned by that runner.

Conceptually:

```text
ExExRunner                 // process-lifetime task
  ProjectorSession         // replaceable Mongo client/session

ProjectionSupervisor       // deadline and node-lifecycle owner
```

On `StorageError::Unavailable`, `ExExRunner`:

1. catches the error instead of returning it through Reth's critical-task wrapper;
2. publishes `MongoUnavailable` through `ProjectionReadinessHandle`;
3. continues draining Reth canonical notifications to avoid backpressure;
4. keeps `FinishedHeight` at the last durable checkpoint;
5. recreates or reconnects only `ProjectorSession` on the retry schedule;
6. after recovery, reloads and verifies the durable checkpoint;
7. reads the current provider finalized target;
8. replays `checkpoint + 1 ..= finalized` and publishes `CatchingUp`, then `Ready`.

While MongoDB is unavailable, ExEx coalesces finalized notifications into one latest exact target instead of retaining an unbounded target queue. Recovery replays every provider block in `checkpoint + 1 ..= latest_target`; coalescing never skips intermediate blocks. A lower finalized target or a different hash at the same finalized height is `Fatal`. Repeating the identical target is an idempotent wake-up.

`ProjectionSupervisor` owns the eight-second monotonic deadline and sends a structured `ProjectionExit` to the top-level node supervisor on deadline expiry, `Fatal`, unexpected ExEx exit, or readiness-channel closure. Node main initiates the common graceful shutdown. After reporting fatal state, ExEx waits for the common cancellation token rather than returning early and triggering Reth's "ExEx finished unexpectedly" panic path.

Only the replaceable Mongo session is soft-restarted. The ExEx task is never automatically restarted.

### Deterministic failures

The eight-second retry window is for technical MongoDB availability failures, not for deterministic data defects or projector lifecycle failure.

`ProjectionStatus::Fatal`, unexpected ExEx task exit, or closure of the mandatory readiness watch initiates immediate structured graceful shutdown. The node does not wait eight seconds for a subsystem that is no longer running.

The following fail immediately and initiate graceful whole-node shutdown:

- malformed recognized projection event;
- unsupported projection fork/binary combination;
- Postcard decode or encode invariant failure;
- primary key/body identity mismatch;
- dangling or mismatched secondary index;
- unsupported local storage schema;
- wrong chain/genesis identity;
- conflicting checkpoint hash;
- unmanaged existing projection data;
- unavailable required historical receipt;
- unexpected ExEx task termination;
- closed projection-readiness channel.

The node records structured diagnostics without interpreting the defective value as absence. Raw event payload diagnostics remain available through ADR-B-OCD-004's `projection_failures` representation when the failure originates during projection.

### Receipt prepare and transaction failures

ADR-B-OCD-004's two-phase block processing remains active:

1. validate and simulate the entire finalized block before domain writes;
2. apply one atomic storage batch per successful EVM receipt;
3. persist the block checkpoint last;
4. emit Reth `FinishedHeight` only after the durable checkpoint.

A deterministic prepare error causes zero domain writes for that block and shuts down the ADR-B-OCD-005 node.

A technical MongoDB failure during Apply may leave earlier receipt transactions from the block committed, but never a torn individual receipt. If MongoDB recovers within eight seconds, replay from the durable prior block checkpoint converges through idempotent complete-body operations.

### Startup and restored snapshots

Snapshot tooling remains an operator responsibility. The node does not implement MongoDB backup transport.

A restored database is accepted only after ADR-B-OCD-004 validation of:

- network identity;
- local schema support;
- start block;
- canonical checkpoint hash.

The node then synchronizes Reth and MongoDB together. It does not become ready or participate as a validator until both are aligned.

Because ADR-B-OCD-005 deliberately has no historical body versions, startup enforces:

```text
Mongo checkpoint <= local Reth finalized/executed checkpoint
```

- equal heights require equal hashes and can resume directly;
- Mongo behind Reth is safe only when retained receipts let ExEx project the missing range before new Mongo-dependent execution;
- Mongo ahead of Reth is `projection_ahead_of_execution` and stops startup.

A fresh node that bootstraps from a Mongo snapshot at height `H` must also restore or already possess a Reth datadir/state snapshot at the same finalized height/hash (or later compatible Reth state). It cannot execute earlier blocks against future Mongo bodies. Automatic Reth catch-up from below the Mongo checkpoint is forbidden until a later versioned-body design exists.

If the restored checkpoint cannot be connected to locally available chain history, recovery requires another valid snapshot pair or an archive source. The node does not skip history or initialize from the current finalized head.

### Runtime and projector ownership

MongoDB remains node-local. It is not shared by validators and is not read over a remote execution/consensus protocol.

The composition root creates the Mongo adapter and concrete typed readers once, then injects one explicit bundle through `OutbeEvmConfig` into the block executor, conceptually:

```rust
struct RuntimeBodyReaders {
    tribute: TributeRepositoryReader,
    nod: NodRepositoryReader,
}
```

The composition root distributes:

- projector write capability to ExEx;
- `RuntimeBodyReaders` read capability to body-dependent execution;
- typed reader clones to RPC/query modules where needed;
- no write capability to runtime business modules.

Runtime and domain code do not construct Mongo clients, select collection names, perform ad hoc BSON queries, use process globals, or carry a legacy EVM body fallback inside the bundle. MongoDB types remain inside the storage adapter; domain consumers see only typed repository results.

#### Required architecture-contract update

The current repository rule says Reth ExEx is observability/indexing only and consensus-critical logic must not run inside it. ADR-B-OCD-005 deliberately approves one narrow testnet-only exception: ExEx still performs only finalized receipt materialization, but its Mongo output becomes a local input to Tribute/Nod body-dependent execution.

This does not make ExEx checkpoint data part of the consensus protocol. A locally broken projection affects that node's ability to produce or positively verify the same result; the remaining correctly materialized validators continue by quorum.

The ADR-B-OCD-005 implementation change must record this exception consistently in the source architecture rules, README/debt documentation, and regenerated agent instructions through the repository's `ruler apply` workflow. The exception must include a hard production disable and must not be generalized to validator settlement or other consensus-critical ExEx work.

### Node modes

The final combined body-storage implementation applies to both validator and full-node modes.

- Validator mode requires complete Mongo readiness before voting/proposing.
- Full-node mode requires complete Mongo readiness before advertising business/RPC readiness.
- Both modes continue to use the same in-process Reth execution integration.
- Full-node mode still does not require consensus private keys and does not vote or propose.

### Status and observability

Expose at least the following operational state through the node's established status/metrics surfaces:

```text
projection status
projection checkpoint number/hash
Reth finalized number/hash
projection lag
Mongo topology capability
Mongo reconnect deadline state
last structured projection/storage failure
readiness
validator participation gate
```

Do not expose MongoDB credentials, raw authentication errors containing secrets, or private validator material.

The exact public RPC/metric names are implementation details unless separately added to the normative README surface.

### Explicit non-goals

ADR-B-OCD-005 does not design or implement:

- off-chain computation;
- compute-result delivery;
- proof generation or verification;
- SMT construction or sharding;
- authenticated secondary-index completeness;
- peer-to-peer body recovery;
- automatic snapshot distribution;
- body commitments or canonical commitment encoding;
- production/mainnet Mongo trust policy.

ADR-B-OCD-006 adds deterministic body commitments and verifies real Mongo point reads. Later ADRs add generic lifecycle, journaled overlay, SMT roots, proofs, persistence, and recovery.

## Working result

After implementation:

- a Tribute or Nod mutation executes and emits a complete receipt event;
- finalized ExEx projection atomically updates the corresponding body and indexes;
- the next eligible block reads the body through MongoDB;
- Tribute, Nod, Lysis, NodFactory, Gratis, metadata, and query flows no longer use active complete EVM body maps;
- validator/full-node startup aligns chain and Mongo state before enabling its operational role;
- transient Mongo outage recovers within eight seconds and catches up, or the node shuts down gracefully;
- no hidden EVM fallback masks missing or corrupt Mongo data.

## Accepted limitations

- No intermediate ADR-B-OCD-005 binary is deployed or treated as a network profile.
- Before ADR-B-OCD-006 is implemented on the branch, focused ADR-B-OCD-005 tests cannot detect a well-formed altered Mongo body.
- Every validator/full node must operate an independent MongoDB projection.
- The testnet assumes a protocol quorum has complete, correct local materializations.
- During focused pre-ADR-B-OCD-006 development, a locally omitted or altered body may make one node disagree with the correctly materialized quorum; the deployed combined path uses ADR-B-OCD-006 leaf commitments inside ADR-B-OCD-010's final scheme-1 tree for point integrity and absence.
- After the final ADR-B-OCD-005 through ADR-B-OCD-010 deployment, local Mongo failure can remove a validator from voting and reduce network liveness until quorum/operator recovery.
- Same-block body correctness is supplied by ADR-B-OCD-007 rather than a temporary ADR-B-OCD-005 mechanism.
- Different receipt transactions are not block-snapshot-visible to user reads.
- Secondary-index list completeness is not authenticated.
- Snapshot creation/distribution and full recovery remain operator runbooks.
- There is no off-chain computation, SMT, proof service, or production security claim.

## Consequences

### Positive

- Complete per-entity bodies and query indexes leave active EVM storage.
- Runtime callers use one typed repository seam rather than MongoDB-specific code.
- Missing and corrupt local state fail explicitly instead of producing silent defaults.
- State/Mongo lockstep prevents execution from reading a partially projected finalized parent.
- The eight-second policy makes temporary availability handling finite and testable.
- Removing legacy fallback exposes projection defects before commitments and production rollout.

### Negative

- MongoDB becomes a local execution dependency before it is cryptographically authenticated.
- A database outage may make a validator miss consensus votes or shut down.
- Lockstep projection can limit block-to-block throughput if MongoDB cannot keep pace.
- End-to-end network validation waits until ADR-B-OCD-006 and ADR-B-OCD-007 complete the branch.
- Different locally corrupted but well-formed bodies can cause focused pre-ADR-B-OCD-006 tests to disagree.
- Operators must provision transaction-capable MongoDB and maintain snapshots/archive recovery.

## Alternatives considered

### Keep permanent EVM body fallback

Rejected because it would preserve two sources of truth and allow MongoDB defects to remain hidden indefinitely. ADR-B-OCD-005 is specifically the cutover stage.

### Continue voting while MongoDB is behind

Rejected because body-dependent execution would read a stale parent materialization and could diverge from nodes whose projection is current.

### Treat backend or corruption errors as absence

Rejected because it can turn an infrastructure defect into a valid-looking but different business transition. In ADR-B-OCD-005-only semantics, a genuine `None` is normal `NotFound`; ADR-B-OCD-006 supersedes this in focused direct-map tests, and the deployed ADR-B-OCD-010 tree performs the equivalent commitment check and treating non-zero plus `None` as `CommittedBodyMissing`.

### Retry MongoDB forever

Rejected because the node would remain superficially alive while unable to provide complete business behavior. The fixed eight-second deadline leads either to bounded recovery or explicit shutdown.

### Shut down after the first connection error

Rejected because short replica elections, network interruptions, and container startup ordering should recover without operator intervention.

### Block all reads during every block Apply

Rejected because frequent multi-second read pauses would be worse than the accepted per-receipt eventual visibility. Consensus execution is protected by the parent-checkpoint barrier instead.

### Add block-wide body versions

Rejected because users primarily consume current records and receipt-level atomicity plus parent-checkpoint gating is sufficient for this stage.

### Read unfinalized canonical projection

Rejected because ADR-B-OCD-004 intentionally projects only exact finalized targets. Outbe's finalized-parent execution sequence and the ADR-B-OCD-005 parent-projection barrier provide the required next-height state.

### Deploy an intermediate ADR-B-OCD-005 binary

Rejected because it would require a temporary same-block fence with its own scopes, rollback journal, provider integration, errors, and tests. Implement ADR-B-OCD-006 through ADR-B-OCD-010 on the same branch and deploy only the completed CES1 path.

## Verification

### Read cutover coverage

Verify composition-root wiring constructs one Mongo adapter, injects `RuntimeBodyReaders` through `OutbeEvmConfig`, gives ExEx separate write authority, and exposes no runtime write or legacy fallback capability.

Add a completeness guard proving every production body/query read in:

- Tribute;
- Nod;
- NodFactory;
- Lysis;
- Gratis;
- metadata/query adapters

uses the typed repository and that production code no longer reads the retired EVM body maps/indexes.

Do not use tests that inspect source text for string presence. Exercise real interfaces and runtime behavior.

### End-to-end flows

Run real node scenarios covering:

- clean genesis has no Tribute/Nod per-entity bodies and starts projection at the first executable block;
- Tribute issue -> finalized receipt -> Mongo -> later Tribute/Lysis read;
- Tribute burn and owner/day index removal;
- Nod issue -> item plus bucket receipt transaction -> later mining/payment read;
- Nod mining with bucket update and final bucket deletion;
- qualification through `HookEvents` -> projected bucket -> later lifecycle/query read;
- Nod-to-Gratis and Tribute-to-Gratis flows;
- validator and full-node parity.

### Startup gating

Verify:

- empty database replays from `start_block` before readiness;
- restored Reth/Mongo snapshot pair validates the same finalized height/hash before readiness;
- Mongo behind Reth catches up from retained receipts before new Mongo-dependent execution;
- Mongo ahead of local Reth state fails startup with `projection_ahead_of_execution`;
- startup seeds `ProjectionReadinessHandle` from the validated durable checkpoint;
- ExEx publishes readiness only after Mongo checkpoint commit;
- each wait remains bound to its request's exact finalized parent while global finality advances;
- same-height hash conflict is `Fatal`;
- checkpoint ahead returns local `ProjectionAhead`, never reads future Mongo state, never votes `false`, and does not start the outage timer;
- proposal and verification perform no MongoDB polling on their hot paths;
- a ready parent checkpoint adds no proposal/verification delay;
- proposal and verification wait only within their existing remaining view budgets;
- budget expiry forfeits/withholds locally and never produces a `false` vote for projection lag;
- healthy projection lag does not start the Mongo-unavailability deadline;
- full node does not advertise business readiness while Mongo is behind;
- state and Mongo process finalized blocks in lockstep during historical sync;
- unsupported topology/schema/network identity cannot enter ADR-B-OCD-005 mode;
- execution/projector access enforces primary read preference and majority read/write concern;
- attempted consistency downgrade is rejected as configuration error.

### Eight-second recovery

Use controlled time and injected backend failures to verify:

- runtime read timeout is `min(remaining view budget, 1 second)`;
- read-side `Unavailable` aborts only the local request, produces no `false` vote, and enters the shared supervisor recovery state;
- before ADR-B-OCD-006, `None` does not start recovery; after ADR-B-OCD-006, mapping zero is `NotFound`, non-zero plus `None` is fatal, and read-side corruption immediately publishes `Fatal`;
- startup and runtime perform an immediate first recovery attempt;
- later attempts occur at one-second intervals without overlap;
- each attempt is bounded by the remaining total deadline;
- startup connection recovery before eight seconds succeeds;
- startup failure at the deadline aborts startup;
- runtime recovery soft-restarts only `ProjectorSession`, then replays and catches up;
- `ExExRunner` continues draining notifications while MongoDB is unavailable;
- multiple finalized notifications coalesce to the latest target and recovery still replays every intermediate block;
- finalized-height regression and same-height hash conflict are `Fatal`;
- runtime failure at the deadline sends structured `ProjectionExit` and triggers graceful shutdown;
- catch-up time is not incorrectly charged to the connection deadline;
- validator participation returns only after catch-up;
- no panic, busy loop, or hidden fallback occurs;
- `Fatal`, ExEx task exit, and readiness-channel closure bypass the Mongo retry window and shut down immediately.

### Data failure behavior

Verify that:

- focused ADR-B-OCD-005 tests preserve the temporary `None -> NotFound` result;
- the combined ADR-B-OCD-006 suite proves mapping zero -> `NotFound` and non-zero plus `None` -> `CommittedBodyMissing`;
- malformed, wrong-identity, dangling-index, stale-checkpoint, and unavailable values produce structured non-absence errors;
- no failure falls back to EVM bodies;
- a validator with an injected local omission does not contribute a matching positive vote for execution produced by the correct quorum;
- MongoDB unavailability follows the eight-second retry/shutdown path;
- deterministic corruption follows its documented immediate failure path.

### Combined cutover determinism

After ADR-B-OCD-007, exercise Mongo read cutover with independent databases and assert equal transaction results, logs, balances, and state roots when all inputs are identical.

Do not add temporary same-block fencing tests. ADR-B-OCD-007 owns mutation-sequence, nested-revert, same-key, same-block, and overlay parity coverage in the final combined suite.

Before ADR-B-OCD-006 lands on the branch, focused tests may inject a well-formed altered row only to document the temporary implementation risk; they must not claim authenticated integrity.

## Reset policy

ADR-B-OCD-005 changes consensus-visible runtime behavior and removes active EVM body paths. Implement ADR-B-OCD-005 through ADR-B-OCD-010 consecutively on one branch without an intermediate runtime activation mechanism or deployment.

After ADR-B-OCD-010 and the combined test/benchmark suite, deploy ADR-B-OCD-003 through ADR-B-OCD-010 together through one complete coordinated testnet reset rather than an in-place migration fork. The new genesis contains no Tribute/Nod per-entity bodies. MongoDB starts empty, and projection begins at the first executable block.

No legacy per-entity EVM body migration, dual-read period, dual-write period, hidden fallback, or temporary fence is implemented. If a future genesis must contain Tribute/Nod entities, that is a separate design requiring a matching genesis Mongo snapshot and explicit validation.

MongoDB may be rebuilt from retained receipts or an operator-provided compatible snapshot. The node does not automatically weaken the read contract during recovery.

## Next unlocked step

Implement ADR-B-OCD-006 next, then continue through ADR-B-OCD-010 on the same branch. Direct-map and unsharded stages remain focused test/reference milestones; only the completed sharded collection/Root Catalog path is deployed.

## Open questions and technical debt

- **Critical:** every `eth_call`, estimate, trace and simulation route must install the
  same compressed-entity execution scope as block execution; current failures outside
  the active block lifecycle show this boundary is not closed.
- Define the exact snapshot for pending/latest/safe/finalized reads and prevent one call
  from mixing Mongo bodies with a different EVM/CE root.
- Move the historical eight-second reconnect value into a versioned operational profile.
- Prove Mongo availability cannot become consensus nondeterminism; participation must
  remain gated until verified bodies are available.
- Remove or formally specify every EVM-body fallback and test corruption, schema mismatch
  and projection lag through real RPC.
