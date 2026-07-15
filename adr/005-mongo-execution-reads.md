# ADR-005: Make MongoDB the Tribute and Nod body read path

- **Status:** Proposed
- **Date:** 2026-07-15
- **Depends on:** ADR-004

## Context

ADR-004 continuously materializes finalized Tribute and Nod body events into one transaction-capable MongoDB database per node. It deliberately leaves runtime and user reads on the existing EVM body maps so projector failure affects only derived storage.

The next stage removes that shadow arrangement. Tribute and Nod complete bodies and query indexes move out of active EVM storage, while the compact protocol aggregates and scheduling structures identified by ADR-002 remain in EVM state.

This is an explicit pre-production/testnet stage. MongoDB does not yet have an independently verifiable body commitment; ADR-006 adds that check. Until then, every validator and full node using this stage accepts the operational risk of its own local MongoDB materialization.

This ADR records the stricter readiness, retry, shutdown, and recovery decisions now so they are not rediscovered when the read cutover is implemented.

## Starting system

At the start of ADR-005:

- complete body mutations are present in canonical receipts;
- Reth ExEx projects finalized receipts into MongoDB;
- body plus secondary-index mutations are atomic per successful EVM receipt;
- the projector persists an exact finalized `{block_number, block_hash}` checkpoint;
- runtime and RPC body reads still use EVM;
- projector failure does not stop the node;
- complete EVM body maps and query indexes remain active.

## Added capability

The first complete Mongo-backed Tribute/Nod runtime: body-dependent execution and query paths read through the ADR-002 repositories, and the legacy complete per-entity EVM body maps/indexes cease to be an active fallback.

## Decision

### Explicit testnet-only dependency

ADR-005 deliberately makes each node's local MongoDB a body-read dependency before ADR-006 makes altered bodies independently detectable.

This exception is acceptable only for the pre-production staged rollout:

- each validator uses its own logical database;
- full nodes use their own logical database when running the cutover mode;
- databases are never shared by several active node writers;
- missing, malformed, stale, or unavailable data fails explicitly;
- there is no fallback to the removed EVM body map;
- a well-formed locally altered row remains an accepted limitation until ADR-006.

Production/mainnet activation must not rely on this unauthenticated intermediate profile.

### Runtime read cutover

Switch all complete-body and body-query consumers identified by ADR-002 to the typed repository readers, including:

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

Retain compact protocol state identified by ADR-002.

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

Unlike ADR-004, Mongo projection is mandatory for every node running the ADR-005 fork profile.

The configured database must satisfy the ADR-004 capability contract:

- transaction-capable replica set or sharded cluster;
- supported `storage_schema_version`;
- matching `chain_id` and `genesis_hash`;
- matching configured and persisted `start_block`;
- one logical database and one active node writer;
- canonical checkpoint block hash;
- no unmanaged body/index data.

A node cannot enter ADR-005 execution using an optional or degraded projector.

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

#### Required non-ExEx coordinator seam

Standard Reth ExEx delivery alone cannot implement this barrier. ExEx observes executed/canonicalized blocks, and `FinishedHeight` controls retention rather than permission to execute the next block. Using only ExEx would allow historical execution to run ahead or create a circular wait in which projection needs a committed block while execution waits for projection.

ADR-005 therefore remains **Proposed and blocked for implementation** until the pinned Reth integration has a verified non-ExEx coordinator seam that:

- gates live next-height execution on the finalized parent's durable Mongo checkpoint;
- forces historical sync through a sequence that lets each required predecessor execute, finalize, project, and checkpoint before a Mongo-dependent successor executes;
- does not treat an execution-valid block as invalid merely because local MongoDB is temporarily behind;
- works identically for proposer, validator, and full-node paths;
- integrates the eight-second availability/shutdown policy without `block_on` or process-local consensus state.

The concrete seam may be an engine/finalization coordinator owned by Outbe's single-binary wiring, but it must be verified against the pinned Reth revision before this ADR is accepted. `ConsensusExecutionBridge` status and ExEx `FinishedHeight` are not substitutes.

The barrier is operational wiring around execution readiness. Consensus-visible state transitions still do not read process-local consensus memory as a substitute for the repository.

### No same-block body overlay in this stage

ADR-005 does not pull the generic journaled body overlay from ADR-007 forward.

Consequently, after a Tribute/Nod body or relevant partition is mutated in a block, a later body-dependent operation on that same entity or partition in the same block is forbidden by a deterministic runtime guard. It reverts rather than reading a pre-mutation Mongo body or an unavailable post-mutation body.

The next block may consume the body after the previous block is finalized and projected.

System-phase ordering that consumes existing bodies must remain explicit and hard-fork governed. In particular, body-consuming Lysis work executes before user transactions and before later same-block work that could mutate the same affected records/partitions.

ADR-007 may later replace this temporary restriction with a deterministic in-block journaled overlay.

### Read semantics

Repository semantics remain explicit:

- `Some(body)` means a valid decodable body exists under the requested identity;
- `None` means the repository authoritatively has no row for that identity at its complete checkpoint;
- corruption, dangling indexes, identity mismatches, backend unavailability, and checkpoint lag are errors, not absence;
- list pages are all-or-error and never silently omit malformed entries;
- no caller falls back to an EVM body after a repository error.

ADR-005 still has no body commitment. A body that is well-formed, has the expected identity, and is consistently indexed but has altered semantic fields cannot yet be detected. ADR-006 closes that integrity gap.

### Cross-receipt visibility

ADR-004 intentionally does not add block-wide MVCC or historical versions.

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

### Deterministic failures

The eight-second retry window is for technical MongoDB availability failures, not for deterministic data defects.

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
- a required body missing at a complete checkpoint.

The node records structured diagnostics without interpreting the defective value as absence. Raw event payload diagnostics remain available through ADR-004's `projection_failures` representation when the failure originates during projection.

### Receipt prepare and transaction failures

ADR-004's two-phase block processing remains active:

1. validate and simulate the entire finalized block before domain writes;
2. apply one atomic storage batch per successful EVM receipt;
3. persist the block checkpoint last;
4. emit Reth `FinishedHeight` only after the durable checkpoint.

A deterministic prepare error causes zero domain writes for that block and shuts down the ADR-005 node.

A technical MongoDB failure during Apply may leave earlier receipt transactions from the block committed, but never a torn individual receipt. If MongoDB recovers within eight seconds, replay from the durable prior block checkpoint converges through idempotent complete-body operations.

### Startup and restored snapshots

Snapshot tooling remains an operator responsibility. The node does not implement MongoDB backup transport.

A restored database is accepted only after ADR-004 validation of:

- network identity;
- local schema support;
- start block;
- canonical checkpoint hash.

The node then synchronizes Reth and MongoDB together. It does not become ready or participate as a validator until both are aligned.

If the restored checkpoint cannot be connected to locally available chain history, recovery requires another valid snapshot or an archive source. The node does not skip history or initialize from the current finalized head.

### Runtime and projector ownership

MongoDB remains node-local. It is not shared by validators and is not read over a remote execution/consensus protocol.

The composition root distributes:

- projector write capability to ExEx;
- repository read capability to body-dependent runtime/query modules;
- no write capability to runtime business modules.

Runtime code does not construct Mongo clients, select collection names, or perform ad hoc BSON queries.

#### Required architecture-contract update

The current repository rule says Reth ExEx is observability/indexing only and consensus-critical logic must not run inside it. ADR-005 intentionally makes ExEx-produced Mongo bodies an intermediate testnet execution input, so implementation would otherwise contradict that rule even though ExEx still does not execute domain business logic.

Before ADR-005 can move from Proposed to Accepted, the narrow testnet-only exception must be recorded consistently in the source architecture rules, README/debt documentation, and regenerated agent instructions through the repository's `ruler apply` workflow. The exception must include a hard production disable and must not be generalized to validator settlement or other consensus-critical ExEx work.

Until both this contract update and the non-ExEx coordinator seam above are approved, ADR-005 must not be implemented.

### Node modes

The ADR-005 profile applies to both validator and full-node modes.

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

ADR-005 does not design or implement:

- off-chain computation;
- compute-result delivery;
- proof generation or verification;
- SMT construction or sharding;
- authenticated secondary-index completeness;
- peer-to-peer body recovery;
- automatic snapshot distribution;
- body commitments or canonical commitment encoding;
- production/mainnet Mongo trust policy.

ADR-006 adds deterministic body commitments and verifies real Mongo point reads. Later ADRs add generic lifecycle, journaled overlay, SMT roots, proofs, persistence, and recovery.

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

- This is a testnet-only operational trust stage.
- A well-formed but altered Mongo body is not detected until ADR-006.
- Every validator/full node must operate an independent complete MongoDB projection.
- Local Mongo failure can remove a validator from voting and reduce network liveness until quorum/operator recovery.
- Same-block body-dependent reuse after mutation is forbidden until ADR-007 adds an overlay.
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
- The temporary same-block mutation/read restriction is stricter than the eventual overlay design.
- Different locally corrupted but well-formed bodies can cause nodes to disagree until ADR-006.
- Operators must provision transaction-capable MongoDB and maintain snapshots/archive recovery.

## Alternatives considered

### Keep permanent EVM body fallback

Rejected because it would preserve two sources of truth and allow MongoDB defects to remain hidden indefinitely. ADR-005 is specifically the cutover stage.

### Continue voting while MongoDB is behind

Rejected because body-dependent execution would read a stale parent materialization and could diverge from nodes whose projection is current.

### Treat missing/corrupt data as absence

Rejected because it can turn an infrastructure defect into a valid-looking but different business transition.

### Retry MongoDB forever

Rejected because the node would remain superficially alive while unable to provide complete business behavior. The fixed eight-second deadline leads either to bounded recovery or explicit shutdown.

### Shut down after the first connection error

Rejected because short replica elections, network interruptions, and container startup ordering should recover without operator intervention.

### Block all reads during every block Apply

Rejected because frequent multi-second read pauses would be worse than the accepted per-receipt eventual visibility. Consensus execution is protected by the parent-checkpoint barrier instead.

### Add block-wide body versions

Rejected because users primarily consume current records and receipt-level atomicity plus parent-checkpoint gating is sufficient for this stage.

### Read unfinalized canonical projection

Rejected because ADR-004 intentionally projects only exact finalized targets. Outbe's finalized-parent execution sequence and the ADR-005 parent-projection barrier provide the required next-height state.

### Pull the generic journaled overlay into ADR-005

Rejected to keep the staged implementation bounded. ADR-005 uses a deterministic same-block reuse restriction; ADR-007 introduces the general overlay.

## Verification

### Read cutover coverage

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
- restored snapshot validates and catches up before readiness;
- validator does not vote/propose while Mongo is behind;
- full node does not advertise business readiness while Mongo is behind;
- state and Mongo process finalized blocks in lockstep during historical sync;
- unsupported topology/schema/network identity cannot enter ADR-005 mode.

### Eight-second recovery

Use controlled time and injected backend failures to verify:

- startup connection recovery before eight seconds succeeds;
- startup failure at the deadline aborts startup;
- runtime recovery before eight seconds replays and catches up;
- runtime failure at the deadline triggers graceful shutdown;
- catch-up time is not incorrectly charged to the connection deadline;
- validator participation returns only after catch-up;
- no panic, busy loop, or hidden fallback occurs.

### Data failure behavior

Verify that missing, malformed, wrong-identity, dangling-index, stale-checkpoint, and unavailable values:

- produce structured errors;
- never become `None` unless the row is genuinely absent at a complete checkpoint;
- never fall back to EVM bodies;
- prevent invalid body-dependent execution;
- lead to the documented shutdown/recovery path.

### Determinism and same-block guard

Run proposer/validator execution with independent MongoDB instances populated from the same receipts and assert equal:

- transaction validity/results;
- emitted logs;
- balance deltas;
- post-block state roots.

Exercise attempts to consume a body or partition after same-block mutation and assert the deterministic guard rejects them identically.

Inject a well-formed altered row and document the expected pre-ADR-006 failure/risk explicitly rather than claiming authenticated integrity.

## Reset policy

ADR-005 changes consensus-visible runtime behavior and removes active EVM body paths. Activate it through a coordinated hard fork and complete testnet reset.

No migration of legacy per-entity EVM bodies is required before production. Start the new testnet from a clean or deliberately seeded MongoDB state consistent with genesis and the activated receipt format.

MongoDB may be rebuilt from retained receipts or an operator-provided compatible snapshot. The node does not automatically weaken the read contract during recovery.

## Next unlocked step

ADR-006 can define canonical body encoding and commitments, store the commitment beside compact EVM state, and verify every MongoDB point read before a body influences runtime behavior.
