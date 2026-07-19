# ADR-001: Introduce the off-chain storage facade

- **Status:** Superseded; historical input only
- **Canonical mapping:** [`docs/adr/legacy-reconciliation.md`](../docs/adr/legacy-reconciliation.md)
- **Date:** 2026-07-15

## Context

Tribute and Nod bodies currently live in EVM storage. Before moving any domain data off-chain, the node needs one storage seam that can be exercised without changing existing domain behavior.

The seam must support two real adapters:

- an in-memory adapter for deterministic, isolated tests;
- a MongoDB adapter for persistent node-local storage.

The existing Lysis, NodFactory, and precompile paths are synchronous. The facade must therefore be usable from synchronous runtime code without exposing futures, MongoDB driver types, BSON, collection handles, or backend cursors.

This ADR establishes only the storage primitive. Tribute/Nod body types, receipt events, ExEx projection, block batches, projection checkpoints, commitments, and SMT state belong to later ADRs.

## Starting system

Tribute and Nod bodies are stored and read through their existing EVM contract facades. There is no MongoDB dependency and no shared abstraction for node-local entity storage.

## Added capability

A domain-neutral off-chain key/value storage facade with equivalent in-memory and MongoDB implementations.

## Decision

### Placement

Create a workspace crate:

```text
crates/blockchain/offchain-storage
```

with package name:

```text
outbe-offchain-storage
```

The crate owns the storage interface, backend-neutral value types and errors, the in-memory adapter, the MongoDB adapter, and their shared conformance suite.

The crate is node-local infrastructure. It does not belong in `outbe-primitives`, and no Tribute/Nod business type appears in its public interface.

### Capability interfaces

Separate read and write authority:

```rust
pub trait StorageReader: Send + Sync {
    fn get(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<Option<Value>, StorageError>;

    fn scan_prefix(
        &self,
        namespace: Namespace,
        request: ScanRequest<'_>,
    ) -> Result<ScanPage, StorageError>;
}

pub trait StorageWriter: Send + Sync {
    fn put(
        &self,
        namespace: Namespace,
        key: &Key,
        value: &Value,
    ) -> Result<(), StorageError>;

    fn delete(
        &self,
        namespace: Namespace,
        key: &Key,
    ) -> Result<(), StorageError>;
}
```

The composition root distributes cloneable shared handles:

```rust
pub type StorageReaderHandle = Arc<dyn StorageReader>;
pub type StorageWriterHandle = Arc<dyn StorageWriter>;
```

A handle clone shares the same adapter instance, connection pool, or in-memory map. It does not copy stored data. Adapters are thread-safe and do not use process-global mutable singletons.

Separating capabilities preserves the intended later ownership:

- ExEx receives write authority;
- Lysis, Tribute, NodFactory, Gratis, and query paths receive read authority;
- domain execution cannot write directly to the Mongo materialization merely because it can read it.

### Namespace

`Namespace` is a validated, code-defined identifier.

For MongoDB, its value is the exact collection name. The adapter does not add prefixes, pluralize names, route several namespaces through one discriminator collection, or otherwise transform the value.

The MongoDB database name belongs to adapter configuration. Namespaces select collections inside that database.

Namespaces:

- are declared by repository code, not accepted from calldata or RPC input;
- use a conservative Mongo-compatible grammar;
- are non-empty and bounded;
- reject reserved or unsafe collection names before backend access.

The in-memory adapter uses the same namespace as its top-level keyspace.

### Keys and values

The storage facade treats both keys and values as opaque bytes.

```text
Namespace + Key -> Value
```

`Key` is non-empty and bounded. `Value` is bounded but otherwise uninterpreted. Tribute/Nod serialization and semantic validation are introduced above this seam in ADR-002.

Both adapters use identical provisional bounds for:

- key bytes;
- value bytes;
- entries per scan page;
- total bytes per scan page.

This ADR requires shared limits but does not make their initial numerical values permanent architecture constants. The implementation exposes named provisional constants and boundary tests. Later performance work may change them through a testnet reset.

### Mutation semantics

`put` atomically inserts or completely replaces one value.

It is not a patch, merge, append, compare-and-swap, or domain `create/update` operation. Repeating the same `put` produces the same stored state.

`delete` is idempotent. Deleting an absent key succeeds.

The facade guarantees atomicity only for one key. It does not provide cross-key transactions, block batches, projection checkpoints, or compare-and-swap. Those capabilities are introduced only with a concrete consumer in ADR-004.

### Read semantics

`get` returns:

- `Ok(Some(value))` when the key exists;
- `Ok(None)` when it does not;
- an error when the backend cannot answer correctly.

Absence is not represented as a backend failure.

`scan_prefix` performs a bounded ordered prefix scan:

```text
scan_prefix(namespace, prefix, after, limit) -> ScanPage
```

Its contract is:

- keys are ordered lexicographically by unsigned raw key bytes;
- an empty prefix scans the namespace;
- `after` is an exclusive key cursor;
- the returned page is bounded by both entry count and total value bytes;
- `next_after`, when present, is used as the next exclusive cursor;
- one call either returns a complete page or an error;
- backend iterators and cursors never escape the adapter;
- separate calls and successive pages do not promise snapshot isolation relative to concurrent writes.

Arbitrary value predicates, query DSLs, and secondary indexes are not part of this interface. Domain repositories add owner/day/bucket access patterns later.

### Error model

The public error surface is backend-neutral.

At minimum it distinguishes:

- `InvalidArgument` — invalid namespace, key, prefix, cursor, or limit;
- `Unavailable` — the backend cannot currently serve the operation;
- `Corruption` — stored data violates the adapter representation or invariant;
- `Backend` — another backend failure that does not fit the stable classes.

Invalid wrapper values are rejected before backend access. MongoDB errors, error codes, BSON values, server topology, and driver result types do not cross the public interface. Backend-specific diagnostics may remain as safe error sources or logs.

### In-memory adapter

The in-memory implementation uses instance-owned shared state with ordered maps, conceptually:

```text
RwLock<BTreeMap<Namespace, BTreeMap<Key, Value>>>
```

It provides raw-byte lexicographic ordering naturally and enforces the same limits and semantics as MongoDB. Every test creates its own adapter instance.

### MongoDB adapter

Use the official Rust MongoDB driver's synchronous API.

Each namespace maps directly to one collection. Store one document per key:

```javascript
{
  "_id": "<lowercase hex encoding of raw key bytes>",
  "value": BinData(...)
}
```

The collection uses simple binary collation.

Lowercase hex is used for `_id` because it preserves raw-byte lexicographic ordering and supports prefix range scans. The adapter does not rely on BSON binary comparison to satisfy the interface ordering contract.

Mongo operations map as follows:

- `put` uses whole-document replacement with upsert;
- `get` reads one `_id`;
- `delete` deletes one `_id` and ignores a zero deletion count;
- `scan_prefix` uses the encoded prefix range, ascending `_id` order, and an internal bounded cursor to construct one complete `ScanPage`.

Malformed documents, invalid key encoding, unexpected fields required by the adapter, or oversized stored values are reported as `Corruption` rather than silently skipped.

## Working result

After implementation:

- the new crate builds as part of the workspace;
- Memory and Mongo adapters expose the same reader/writer capabilities;
- callers can put, get, replace, delete, and page through opaque records;
- both adapters produce equivalent ordered results for the same valid operation sequence;
- the node's existing Tribute/Nod EVM behavior remains unchanged.

This is a working storage module, not yet a domain migration.

## Accepted limitations

- Tribute and Nod do not use the facade yet.
- There are no domain body types or codecs.
- There are no receipt body events.
- ExEx does not write MongoDB.
- Runtime execution does not read MongoDB.
- There are no block batches, checkpoints, cross-key transactions, or snapshot scans.
- There are no commitments, SMT roots, proofs, or recovery guarantees.
- Synchronous Mongo calls block their calling thread; worker isolation is deferred unless measurement proves it necessary.
- Provisional resource limits are not production capacity claims.

## Consequences

### Positive

- Memory and Mongo become interchangeable at one real seam.
- Domain readers and ExEx writers can later receive only the authority they require.
- MongoDB details remain local to one adapter.
- Typed Tribute/Nod repositories can be added without changing adapter semantics.
- The conformance suite becomes the executable contract for future adapters.

### Negative

- Two capability handles require slightly more composition-root wiring than one universal trait.
- Lowercase hex doubles the encoded key size in MongoDB.
- Prefix scans are not transactional snapshots.
- Synchronous storage calls may consume runtime threads while MongoDB is slow.
- Memory can model semantics and ordering but cannot reproduce every MongoDB timeout, collation, or corruption failure.

## Alternatives considered

### One combined `OffchainStorage` trait

Rejected because every holder would receive both read and write authority. The split interface directly expresses the later architecture in which ExEx writes the materialization and domain execution only reads it.

### Generic `Store<A: StorageAdapter>` throughout callers

Rejected because adapter types would propagate through repositories, runtime services, and node wiring. Trait-object capability handles keep the backend choice at the composition root and simplify process-wide distribution.

### Async storage interface

Rejected because current Lysis, NodFactory, and precompile execution paths are synchronous. Making the public seam async would require a broader execution-model change before the storage abstraction could be used.

### Worker thread around an async Mongo client

Deferred. It introduces queueing, shutdown, backpressure, and request-correlation behavior before measurements show that the official synchronous driver is insufficient.

### Structured generic entity documents

Rejected because ADR-001 does not yet own Tribute/Nod schemas or indexes. The base storage stores opaque bytes; domain repositories add structure later.

### Mongo `BinData` as the directly sorted key

Rejected because the interface requires explicit raw-byte lexicographic ordering. A lowercase hexadecimal `_id` gives a simple, portable ordering contract under binary collation.

### Block batches and projection checkpoints

Deferred to ADR-004. No real ExEx consumer exists in ADR-001, and introducing projection semantics now would make the first storage interface wider than its current use case.

## Verification

Implement one shared conformance suite and run it unchanged against Memory and MongoDB.

The suite covers:

- insert and complete replacement;
- repeated idempotent `put`;
- existing and missing `get`;
- existing and missing `delete`;
- namespace isolation;
- raw-byte lexicographic ordering;
- empty, ordinary, and all-`0xff` prefixes;
- exclusive cursors and multi-page traversal;
- key/value/page boundaries;
- invalid argument rejection before backend access;
- no torn values under concurrent read/write stress;
- shared-handle clone semantics;
- Mongo document corruption detection;
- stable backend-neutral error classification.

Mongo integration tests use an isolated database and remove their collections after completion. Tests do not share mutable global state.

## Reset policy

No chain reset or hard fork is required because Tribute/Nod execution remains unchanged.

Test Mongo collections may be deleted at any time. ADR-001 data is non-authoritative and can be recreated by its tests or callers.

## Next unlocked step

ADR-002 can define complete typed Tribute and Nod bodies, repository interfaces, key encodings, namespaces, and the boundary between off-chain bodies and protocol-owned EVM state without changing the underlying adapters.
