# ADR-004: Project finalized body events into MongoDB through Reth ExEx

- **Status:** Proposed
- **Date:** 2026-07-15
- **Depends on:** ADR-003

> **ADR-006 integration note:** the coordinated reset replaces ADR-004's explicit-field event decoder and Postcard value construction with strict-canonical Protobuf payload verification. ADR-004's finalized-only source, full-block prepare, per-receipt atomicity, checkpoint-last rule, and crash replay remain authoritative. Because Mongo may contain partially applied receipt batches above its durable block checkpoint after a crash, ADR-006 ExEx validation does not compare the first event's `previousCommitment` with the current Mongo body; execution and independent ordered replay validate that pre-state, while ExEx validates the new leaf and same-identity event continuity within the block.

## Context

ADR-001 introduced backend-neutral Memory and MongoDB storage adapters. ADR-002 added typed Tribute and Nod repositories over six body/index namespaces. ADR-003 added complete `Stored` and identity-only `Deleted` events for every successful mutation of:

- `TributeData`;
- `NodItemState`;
- `NodBucketState`.

The live node still stores and reads complete bodies through EVM state. MongoDB can be populated manually in tests, but no canonical consumer keeps it synchronized with the chain. Repository writers also update primary bodies and derived indexes through separate single-key operations, so they do not yet provide the atomicity required by a real projector.

This ADR adds the first live receipt-to-Mongo materialization. It does not switch execution or business reads to MongoDB. That cutover and its stricter node-availability policy belong to ADR-005.

## Starting system

The starting system has:

- six code-defined Tribute/Nod repository namespaces;
- complete explicit-field projection events in normal user and `HookEvents` receipts;
- idempotent per-key `put` and `delete` operations;
- no cross-key transaction interface;
- no ExEx consumer;
- no projection checkpoint;
- no runtime dependency on MongoDB.

## Added capability

A mandatory Reth ExEx on validator and full-node modes that consumes finalized projection events and maintains a durable, restartable MongoDB materialization of Tribute and Nod bodies and indexes.

## Decision

### Stage boundary

ADR-004 is a materialization stage only.

- Tribute, Nod, Lysis, NodFactory, Gratis, and RPC body reads continue to use their existing EVM path.
- A projector failure degrades or stalls only the Mongo materialization.
- A projector failure does not stop validator consensus, full-node synchronization, execution, or existing RPC behavior in this ADR.
- The eight-second reconnect deadline, startup readiness gate, validator-participation gate, and whole-node shutdown policy are recorded in ADR-005, where MongoDB becomes an execution dependency.

The projector never runs consensus-critical business logic. It only reconstructs records already described by accepted receipts.

### Installation and node modes

Install the projector through Reth's normal `install_exex` node-builder seam.

The projector is required on both:

- validator nodes; and
- full nodes.

A full node executes and stores accepted blocks even though it does not vote or propose, so it has the same finalized receipt source required by the projector.

A missing projector or MongoDB configuration is a startup configuration error and stops node startup. After a successful startup, ExEx projects asynchronously: consensus, execution, and existing EVM-backed reads do not wait for each MongoDB block checkpoint during ADR-004.

### One node, one logical database, one writer

Each projector owns one logical MongoDB database.

```text
one node instance -> one projection database -> one active writer
```

Several validators or full nodes must not actively write the same logical projection database. A snapshot may be copied into separate databases for several nodes, but those restored databases diverge into independently owned local materializations after startup.

The MongoDB operator may choose the physical topology. Different nodes may use different topologies without changing projection semantics.

### Transaction-capable MongoDB topology

The enabled projector requires a MongoDB topology that supports multi-document transactions:

- a replica set, including a single-node replica set; or
- a sharded cluster whose shards support transactions.

Standalone MongoDB is not a supported projector topology because it cannot atomically update one body and its secondary-index documents.

The node performs a startup capability check. It does not infer correctness merely from a successful TCP connection. A configured topology that cannot provide sessions and transactions is a startup configuration error and stops node startup.

Replica-set versus sharded-cluster selection remains an operator decision. Shard keys, balancing, replication factor, capacity, backup topology, and failover policy are outside the projector interface. A sharded deployment may execute the same receipt transaction as a distributed transaction and may therefore have different performance, but not different observable storage semantics.

### Atomic storage batch seam

ADR-001 deliberately deferred cross-key transactions until a real consumer existed. ADR-004 adds that capability to `outbe-offchain-storage` rather than exposing MongoDB sessions to ExEx or domain repositories.

Conceptually, the storage seam gains:

```text
AtomicWriteBatch
  Put(namespace, key, value, optional_metadata)
  Delete(namespace, key)

StorageWriter::apply_atomic(batch)
```

The exact Rust representation may use dedicated structs and enums. The storage seam also gains a metadata-aware record result, conceptually:

```text
StorageMetadata = validated map<string, string>
StoredValue { value, optional_metadata }
StorageReader::get_record(namespace, key) -> optional StoredValue
```

The existing value-only `get` may remain as a convenience for domain readers that do not need provenance. Prefix-scan entries carry the same optional metadata so Memory and MongoDB preserve one observable interface. `outbe-offchain-storage` understands only validated metadata keys and values; conversion to the typed `ProjectionSource` remains owned by the projection module.

The interface contract is fixed:

- one batch is all-or-nothing;
- operation order inside the batch is preserved;
- a repeated batch produces the same final state;
- Memory and MongoDB adapters expose equivalent observable results;
- backend session, transaction, retry, and commit-result types never cross the seam.

The Memory adapter applies a batch under one exclusive adapter lock. The MongoDB adapter applies it in one database transaction.

Existing single-key `put` and `delete` operations may remain for setup, isolated repository tests, and simple callers. Production receipt projection uses `apply_atomic`.

### Domain-owned mutation planning

ExEx must not derive Tribute/Nod storage keys or index maintenance itself.

The Tribute and Nod modules gain internal planning capabilities that:

- accept the old body, if present;
- accept a complete new body or a delete identity;
- validate key/body identity;
- encode the Postcard body;
- derive new secondary-index keys;
- derive stale secondary-index keys to delete;
- return backend-neutral storage mutations.

This keeps owner/day/index rules inside the repositories that own them while allowing ExEx to combine all mutations from one receipt into one atomic storage batch.

### Primary document metadata

The MongoDB primary document representation is extended from:

```javascript
{
  _id: "<lowercase hex domain key>",
  value: BinData(...)
}
```

to:

```javascript
{
  _id: "<lowercase hex domain key>",
  value: BinData(...),
  _projection: {
    block_number: "...",
    block_hash: "...",
    tx_hash: "...",
    transaction_index: "...",
    log_index: "...",
    emitter: "...",
    event_signature: "..."
  }
}
```

`_id` remains the canonical lowercase hexadecimal encoding of the existing domain key:

- Tribute ID for `tributes`;
- Nod ID for `nods`;
- bucket key for `nod_buckets`.

The storage representation may use a validated string map internally, but callers use a typed `ProjectionSource`. Conversion between the typed representation and map is centralized. The allowed keys are fixed; missing, unknown, duplicate, or malformed values are corruption.

`_projection` is stored only on primary body documents. Identity-only secondary-index documents retain their exact empty value and do not duplicate provenance.

The storage reader accepts exactly `_id`, `value`, and optional `_projection`. `get_record` returns `_projection` through backend-neutral `StorageMetadata`; ordinary `get` returns the same `Value` while intentionally discarding metadata. Other unexpected fields remain corruption. Memory storage retains the same optional metadata so shared conformance tests do not hide Mongo-only behavior.

ADR-003 does not provide an event version, hash version, body commitment, or `body_hash`. ADR-004 therefore stores the emitter and exact event signature but does not invent `event_schema_version` or `body_hash` fields. Those concepts remain deferred to ADR-006.

### Local projection schema version

The projector stores a local schema identifier:

```text
storage_schema_version = 1
```

This version describes only the local projection representation:

- primary document shape;
- `_projection` shape;
- collection names and key encodings;
- checkpoint shape;
- failure-record shape.

It is not an on-chain storage version, receipt-event version, body commitment version, or hard-fork selector.

A binary must explicitly support the stored local schema version. ADR-004 does not perform in-place schema migration. An incompatible database stalls the projector and requires a compatible snapshot or complete rebuild.

### Projection state

Store one singleton projector state document in the same logical MongoDB database as the domain collections. It contains at least:

```text
chain_id
genesis_hash
storage_schema_version
start_block
checkpoint.block_number
checkpoint.block_hash
```

No hostname, validator address, consensus key, node ID, filesystem path, or permanent machine identity is stored. This makes a valid snapshot portable between nodes on the same chain.

The checkpoint means that every projection event through that finalized block has been successfully applied. It advances only at a block boundary.

No `projection_blocks` record is written for every successful block. Successful contiguous history is represented by the checkpoint. Exceptional diagnostics live in `projection_failures`.

### Empty, managed, and unmanaged databases

Startup distinguishes three states:

1. An empty logical database has no `projection_state` and no body/index data. The projector initializes it and starts from configured `start_block`.
2. A managed database has `projection_state`. The projector validates its identity, schema, and checkpoint before writing.
3. A database with body/index data but no `projection_state` is `unmanaged_projection_data`. The projector does not adopt, delete, or overwrite those documents automatically.

The operator must provide a clean database or a complete managed snapshot for the third case.

### Configurable start block

When no checkpoint exists, projection starts from configured `start_block`. Its safe default is the first executable block of the chain.

The projector never silently starts at the current finalized height. Doing so would produce a superficially healthy but incomplete database.

`start_block` is persisted in `projection_state`. A restored database whose configured and persisted start blocks conflict is rejected rather than reinterpreted.

### Finality source

Use Reth's provider finality stream as the sole finalized target source:

```text
ctx.provider().finalized_block_stream()
```

Each target is an exact `BlockNumHash`. The projector does not poll `ConsensusExecutionBridge`, does not add a second consensus-to-projector channel, and does not use the bridge's status-only finalized height.

For a verified checkpoint `H` and a new finalized target `F`, process:

```text
H + 1 ..= F
```

in ascending block order. For every height, load the exact canonical block and receipts through the Reth provider and verify the expected block hash before projection.

Normal ExEx canonical notifications must still be drained according to the pinned Reth notification contract so the extension does not create notification backpressure. They are not permission to write unfinalized data. Canonical commit/reorg/revert notifications may inform buffering or wakeups, but only the provider's finalized stream advances projection.

Outbe uses instant BFT finality, but the projector still treats exact finalized number/hash pairs as its durable authority.

### Receipt source and filtering

Read both user-transaction and receipt-visible `HookEvents` system-transaction receipts.

Process logs in deterministic order:

```text
block number
-> transaction index
-> log index
```

Filter only the six exact `(owning emitter address, event signature)` pairs defined by ADR-003:

- Tribute body Stored/Deleted at `TRIBUTE_ADDRESS`;
- Nod item body Stored/Deleted at `NOD_ADDRESS`;
- Nod bucket body Stored/Deleted at `NOD_ADDRESS`.

Product events and unrelated logs are ignored. ExEx does not inspect calldata and does not rerun Tribute, Nod, Lysis, pricing, payment, lifecycle, or authorization rules.

ADR-003 intentionally has no version envelope. A later ABI change therefore requires coordinated hard-fork/binary rollout or a pre-production reset; ADR-004 does not invent runtime version negotiation.

### Full-block prepare phase

Before any domain write for a finalized block, build a complete in-memory `ProjectionPlan`.

The prepare phase:

1. loads all required receipts;
2. decodes every recognized projection event;
3. reconstructs complete Rust domain records;
4. validates event identity against record identity;
5. collects all affected primary domain keys;
6. batch-loads current bodies from MongoDB;
7. builds an in-memory overlay for those bodies;
8. simulates every Stored and Deleted event in receipt/log order;
9. derives exact primary/index mutations for every receipt;
10. performs Postcard encoding, key, metadata, and configured-size validation.

The overlay is necessary when several receipts in one block mutate the same record. Later simulated events observe the result of earlier simulated events even though MongoDB has not yet been written.

A deterministic prepare failure writes no domain or index mutation from that block. The checkpoint and Reth `FinishedHeight` remain on the previous block.

If MongoDB is available, diagnostic details and the raw failing log payload may be written to `projection_failures`. Failure diagnostics include source block/transaction/log coordinates, emitter, event signature, raw topics/data, and a structured error class. This technical identifier never replaces the domain `_id` of Tribute or Nod documents.

### Receipt transaction boundary

After the whole block prepares successfully, apply one `AtomicWriteBatch` per successful EVM receipt containing projection events.

One receipt transaction may update:

- a primary body;
- its previous and new secondary indexes;
- a Nod item and its bucket;
- primary `_projection` metadata;
- any combination of the above emitted by that one accepted EVM transaction.

This makes one business operation atomic while avoiding one large MongoDB transaction for the entire block.

Different receipts in a block commit sequentially and may become visible progressively. ADR-004 deliberately does not add block-wide MVCC, historical body versions, a block-wide read barrier, or one transaction spanning the full block.

The accepted read-consistency model is:

- every primary document replacement is atomic;
- every body plus its derived indexes is transactionally atomic;
- every receipt's related projection mutations are transactionally atomic;
- different receipts in the same block may become visible at different times;
- the durable checkpoint continues to identify the last fully applied block.

Runtime does not read MongoDB in ADR-004, so progressive visibility cannot alter execution. ADR-005 gates execution on a complete parent checkpoint before making Mongo reads authoritative.

### Checkpoint commit and replay

After all receipt batches for a block commit, update the singleton block checkpoint as the final MongoDB write for that block. Then emit:

```text
ExExEvent::FinishedHeight(block_num_hash)
```

A crash may occur after some or all receipt transactions but before the checkpoint update. On restart, replay the entire block from the previous checkpoint. Complete-body replacement, idempotent delete, and deterministic index mutations make this replay safe.

A MongoDB `UnknownTransactionCommitResult` or transient transaction failure is resolved behind the adapter seam according to MongoDB transaction retry rules. If a process restart loses the in-memory transaction outcome, replay from the durable block checkpoint remains authoritative.

The projector never emits `FinishedHeight` above its durable MongoDB checkpoint. This retains the Reth ExEx WAL/history required for later replay instead of allowing premature pruning.

### Failure policy in ADR-004

ADR-004 distinguishes projection health from node health.

- A transient MongoDB error leaves the checkpoint unchanged and retries or parks the projector in `degraded` state.
- A malformed recognized event leaves the block unapplied and parks the projector with diagnostics.
- Missing required historical receipts produce `historical_receipts_unavailable`.
- A local schema mismatch produces `projection_schema_mismatch`.
- A checkpoint hash mismatch produces `checkpoint_mismatch`.
- Existing data without state produces `unmanaged_projection_data`.

None of these failures is interpreted as entity absence or silently skipped. The projector does not advance around a bad block.

Because runtime reads remain EVM-backed in ADR-004, these failures do not stop the whole node. The ExEx task must remain alive in a stalled/degraded state rather than return unexpectedly through Reth's critical-task wrapper.

ADR-005 promotes MongoDB to an execution dependency and therefore strengthens these failures into readiness, participation, retry-deadline, and shutdown behavior.

### Snapshot restore

Snapshot creation, transfer, signing, backup, and restore are MongoDB operator responsibilities. ADR-004 does not add a custom `outbe-chain` snapshot import/export command.

After an operator restores a database, the projector validates:

- `chain_id`;
- `genesis_hash`;
- local storage schema support;
- configured/persisted `start_block` compatibility;
- canonical Reth block hash at the stored checkpoint.

If Reth has not yet synchronized through the snapshot checkpoint, the projector waits until that block can be verified. After verification it may report the restored checkpoint as `FinishedHeight` and process later finalized blocks.

A checkpoint restored from another machine on the same chain is valid because projector state contains no machine or validator identity.

A point-in-time backup may contain some receipt transactions from the block after its stored checkpoint. Replay of that block remains safe. Transaction-capable MongoDB guarantees that each captured receipt is internally atomic.

### Historical receipt availability

A clean rebuild requires every block and receipt from `start_block` through the finalized target. If any required receipt is unavailable because of pruning or incomplete storage, the projector does not skip the gap or move `start_block` forward.

Recovery requires one of:

- a complete managed MongoDB snapshot with a verifiable checkpoint;
- a Reth archive containing the required receipts;
- another operator-managed source capable of restoring those receipts.

Portable snapshot protocols, peer body recovery, authenticated snapshot manifests, and automatic recovery belong to later storage/recovery ADRs.

## Working result

After implementation:

- the mandatory ExEx on validator and full-node modes populates all six Tribute/Nod repository namespaces from finalized receipts;
- body and derived-index mutations from one EVM receipt commit atomically;
- primary documents carry exact receipt provenance;
- restart resumes from a durable exact block checkpoint;
- duplicate delivery and crash replay produce the same final materialization;
- a restored compatible database resumes on another machine after chain/hash validation;
- runtime execution and existing user reads remain EVM-backed.

## Accepted limitations

- MongoDB is not yet an execution or business-read dependency.
- Projection failures do not stop the whole node in this stage.
- Different receipts from one block may be visible progressively.
- Direct external MongoDB readers receive no block-wide snapshot guarantee.
- There is no event version envelope, body commitment, authenticated body read, SMT, proof service, or off-chain computation.
- There is no automatic in-place storage migration.
- There is no built-in snapshot export/import or peer recovery.
- A well-formed but altered MongoDB body is not detectable yet.
- Complete rebuild depends on retained receipts or an operator-provided snapshot.
- MongoDB capacity and sharding choices remain operator responsibilities.

## Consequences

### Positive

- Finalized chain receipts become the sole input to a real MongoDB materialization.
- ExEx remains a projection mechanism rather than a second implementation of domain business rules.
- Exact block checkpoints and idempotent receipt transactions make restart behavior explicit.
- Body/index atomicity is enforced at the storage seam for Memory and MongoDB.
- Replica-set and sharded-cluster deployments share one application interface.
- Full-node and validator projections use the same Reth finality source.
- Snapshot portability does not require embedding machine identity in the database.

### Negative

- Projector-enabled MongoDB must support transactions; standalone MongoDB is insufficient.
- Full-block preflight requires bounded memory and batch reads proportional to affected records.
- Cross-receipt progressive visibility is observable to direct database readers.
- Holding `FinishedHeight` on a stalled projection retains ExEx history and may increase disk usage.
- The storage facade and conformance suite become wider than ADR-001's original per-key contract.
- Sharded deployments may pay distributed-transaction costs.

## Alternatives considered

### Poll `ConsensusExecutionBridge`

Rejected because the bridge exposes a status height rather than the provider's exact finalized number/hash stream and is not the uniform Reth seam for validator and full-node modes.

### Project canonical but unfinalized notifications

Rejected for ADR-004. The accepted materialization is finalized-only and therefore does not require a MongoDB reorg journal. ADR-005 coordinates execution with the complete finalized-parent projection before Mongo becomes authoritative.

### One MongoDB transaction per block

Rejected because a gas-saturated block can create a large, long-lived transaction. One successful EVM receipt is the natural atomic business boundary.

### One transaction per entity

Rejected because one EVM transaction can atomically mutate a Nod item and its bucket. Exposing those related records separately would weaken the business operation's receipt semantics.

### Standalone MongoDB with application locks

Rejected because a process crash between primary and index writes can persist an inconsistent repository. An in-process mutex is not a database transaction.

### Block-wide read barrier

Rejected because it would repeatedly suspend user reads while a complete block is applied. ADR-004 accepts cross-receipt eventual visibility and uses a final checkpoint as the durable completeness marker.

### Historical versions and MVCC

Rejected because users primarily consume current per-entity data and the additional storage/query complexity is not needed for this stage.

### Separate provenance collection

Rejected after adding transactional metadata support. Keeping `_projection` beside the primary body provides one read and physical body/provenance atomicity without duplicating metadata in secondary indexes.

### Projector-owned snapshot implementation

Rejected because replica-set and sharded-cluster backup mechanics are database-operator concerns. The node validates restored state but does not replace MongoDB backup tooling.

## Verification

### Storage conformance

Extend the shared Memory/Mongo conformance suite to cover:

- atomic multi-key batch commit;
- rollback after injected failure at every operation;
- operation ordering;
- duplicate batch application;
- primary metadata round trip;
- strict rejection of malformed `_projection` data;
- rejection of unsupported transaction topology;
- parity between Memory and MongoDB observable state.

### Projection decoding and prepare

For all six projection events, verify:

- exact emitter plus signature filtering;
- user and `HookEvents` receipt consumption;
- block/transaction/log ordering;
- complete record reconstruction;
- full-block batch loading and overlay simulation;
- multiple mutations of the same entity in one block;
- malformed event causes zero domain writes for that block;
- delete and replacement derive correct stale-index deletions.

### Receipt atomicity

Inject failures around every primary/index mutation for:

- Tribute Stored and Deleted;
- Nod item Stored and Deleted;
- Nod bucket Stored and Deleted;
- Nod issue item-plus-bucket receipt;
- Nod mining item-plus-bucket receipt.

Assert that one receipt is either fully visible or fully absent.

### Finality and checkpoint

Verify:

- unfinalized canonical notifications never reach MongoDB;
- finalized target jumps replay every intermediate block;
- exact checkpoint number/hash persistence;
- checkpoint is written after all receipt transactions;
- `FinishedHeight` never exceeds checkpoint;
- crash before/after each receipt and before/after checkpoint converges after restart;
- duplicate finalized delivery is idempotent;
- checkpoint hash mismatch stalls projection;
- missing historical receipt stalls projection;
- a verified restored checkpoint resumes at the next block.

### Node-stage behavior

Verify in ADR-004 specifically that:

- validator and full-node modes must have projection;
- a node without projector configuration is stopped during startup;
- a projector failure reports degraded status but does not stop EVM-backed node operation;
- no runtime Tribute/Nod read is switched to MongoDB.

## Reset policy

ADR-004 changes only node-local derived storage and node wiring. It does not change block execution, receipts, or consensus state beyond the already activated ADR-003 event contract.

MongoDB may be dropped and rebuilt from retained compatible receipts. If the ADR-003 event ABI changes simultaneously, use its coordinated hard fork or complete testnet reset.

Pre-production incompatible local schema changes may use a complete MongoDB rebuild rather than an in-place migration.

## Next unlocked step

ADR-005 can switch body-dependent execution and query paths to the populated repositories. At that point MongoDB becomes an explicit testnet execution dependency, and projection readiness must gate validator participation and whole-node availability.
