# ADR-002: Define the Tribute and Nod off-chain body boundary

- **Status:** Proposed
- **Date:** 2026-07-15
- **Depends on:** ADR-001

> **Supersession note:** ADR-006 changes the active reset-era body wire/key contract from temporary Postcard plus 32-byte Tribute/Nod IDs to strict-canonical Protobuf plus exact 36-byte `WWD_BE4 || Poseidon_BE32` Tribute/Nod IDs. Nod bucket identity becomes `WWD_BE4 || bucket_key_BE32`. ADR-002 remains authoritative for domain ownership, typed repository boundaries, collection separation, deterministic ordering, pagination semantics, and the off-chain-versus-compact-EVM state split; ADR-006 updates the affected primary/index key suffixes, ID types, codecs, and cursors.

## Context

ADR-001 introduced a domain-neutral off-chain storage facade with equivalent in-memory and MongoDB adapters. Tribute, Nod, Lysis, NodFactory, Gratis, and the precompiles still use their existing EVM-backed records.

The next step is to define the domain layer above that facade:

- which existing records are complete off-chain bodies;
- which compact protocol state remains in EVM;
- which MongoDB collections and binary keys represent bodies and indexes;
- which typed reader/writer capabilities future consumers use.

This ADR does not switch live execution to MongoDB. It introduces and tests the typed repositories while the existing EVM path remains active. Full-body receipt events, ExEx projection, atomic block batches, and the runtime read cutover belong to later ADRs.

## Starting system

The workspace contains `outbe-offchain-storage` with:

- synchronous `StorageReader` and `StorageWriter` capabilities;
- Memory and MongoDB adapters;
- opaque bounded `Key` and `Value` types;
- exact namespace-to-Mongo-collection mapping;
- bounded lexicographic prefix scans;
- per-key atomicity only.

Tribute and Nod still store complete bodies and indexes in EVM state.

## Added capability

Domain-owned typed repositories for:

- Tribute records and owner/day indexes;
- Nod item records, bucket records, owner indexes, and global enumeration.

The repositories hide Postcard, namespace names, binary key layouts, index resolution, pagination, and repository-level corruption checks from callers.

## Decision

### Domain ownership and placement

Add repository modules to the existing domain crates:

```text
crates/core/tribute/src/repository.rs
crates/core/nod/src/repository.rs
```

The public repository types are re-exported deliberately from the corresponding crate roots.

Do not add Tribute or Nod types to `outbe-offchain-storage`. The infrastructure crate continues to know only `Namespace`, `Key`, `Value`, and backend-neutral storage errors.

### Reuse existing domain records

Use the existing records as the typed off-chain values:

- `outbe_tribute::TributeData`;
- `outbe_nod::NodItemState`;
- `outbe_nod::NodBucketState`.

Do not rename them and do not create parallel Mongo DTOs such as `TributeBody` or `NodBody`.

Add the Serde support required by Postcard directly to these existing records and their nested field types. The same semantic type is temporarily used by both the current EVM schema and the new repository. When the EVM body maps are removed in ADR-005, the domain types remain valid repository records.

### Temporary body encoding

Encode repository values with raw Postcard:

```text
postcard(TributeData)
postcard(NodItemState)
postcard(NodBucketState)
```

The codec is private to each domain repository. `outbe-offchain-storage` does not encode or decode domain values.

ADR-002 does not introduce a version envelope. There is no production state requiring backward compatibility, and MongoDB may be deleted and rebuilt after a format change. Canonical commitment encoding and real schema-version semantics belong to ADR-006.

A decoded record must be checked against the storage key that selected it. Valid Postcard with a mismatched embedded entity or bucket ID is repository corruption.

### Off-chain records

#### Tribute

The complete `TributeData` record is an off-chain body candidate:

- `token_id`;
- `owner`;
- `worldwide_day`;
- `issuance_amount_minor`;
- `issuance_currency`;
- `nominal_amount_minor`;
- `reference_currency`;
- `tribute_price_minor`;
- `exclude_from_intex_issuance`.

Owner and worldwide-day enumeration are also off-chain query indexes.

#### Nod

The complete `NodItemState` record is an off-chain body candidate:

- `nod_id`;
- `owner`;
- `gratis_load_minor`;
- `worldwide_day`;
- `league_id`;
- `floor_price_minor`;
- `bucket_key`;
- `cost_amount_minor`;
- `issuance_currency`;
- `reference_currency`;
- `issued_at`.

The complete `NodBucketState` record is also off-chain:

- `bucket_key`;
- `worldwide_day`;
- `floor_price_minor`;
- `is_qualified`;
- `total_nods`;
- `entry_price_minor`.

Owner enumeration and global Nod enumeration are off-chain query surfaces.

### Protocol state retained in EVM

ADR-002 distinguishes complete/query records from compact protocol control state.

#### Tribute retained state

Retain in EVM:

- `total_supply`;
- day totals;
- day seal state;
- monetary aggregates used directly by protocol transitions.

The existing Tribute body map and owner/day indexes remain active only until the later runtime cutover. Their future repository replacements are defined here.

#### Nod retained state

Retain in EVM:

- `total_supply`;
- the bin-tree root/mid/leaf structures;
- unqualified-bin counts and bucket-key worklists;
- other compact scheduling state required to find the next bucket to process.

`NodBucketState` itself is not retained after the cutover. Lifecycle code will use the EVM bin tree/worklists to select a `bucket_key`, then load the bucket record through the typed repository.

The existing Nod item map, bucket map, owner indexes, and global enumeration remain active until ADR-005.

### MongoDB namespaces

Fix six code-defined namespaces. Because ADR-001 maps namespace values directly to MongoDB collection names, these are also the exact collection names:

```text
tributes
tributes_by_owner
tributes_by_day

nods
nod_buckets
nods_by_owner
```

The namespace constants are private implementation details of their owning domain repositories. Callers cannot select arbitrary collections.

### Binary key layouts

All entity identifiers use fixed-width unsigned big-endian bytes. This makes raw-byte lexicographic ordering equal to unsigned numeric ID ordering.

```text
collection             key
---------------------  -----------------------------------------
tributes               tribute_id_be32
tributes_by_owner      owner20 || tribute_id_be32
tributes_by_day        worldwide_day_be4 || tribute_id_be32

nods                    nod_id_be32
nod_buckets             bucket_key32
nods_by_owner           owner20 || nod_id_be32
```

Primary collection values contain Postcard records.

Secondary index values are exactly empty. They never duplicate a body or entity ID. The entity ID is decoded from the fixed-width suffix of the index key.

A non-empty secondary-index value, malformed key length, wrong prefix, invalid entity ID, or key/body mismatch is repository corruption. Readers do not silently skip such records.

### Ordering

All repository lists use ascending canonical entity ID order.

Do not introduce insertion sequence numbers, counters, reverse sequence maps, or swap-and-pop ordering in the off-chain repositories.

Consequences:

- Tribute owner/day results are deterministic by `token_id`;
- Nod owner results are deterministic by `nod_id`;
- global Nod enumeration scans the primary `nods` collection directly in `nod_id` order;
- deleting a record preserves the relative order of all remaining IDs;
- exact current Nod swap-and-pop enumeration order is intentionally not preserved.

Production state does not yet exist, and ERC-721 Enumerable does not promise a particular enumeration order. The changed deterministic order is accepted for the later testnet cutover.

### Typed pages and cursors

Domain callers receive typed record pages, not raw index IDs or storage entries.

Conceptually:

```rust
pub struct TributePageRequest {
    pub after: Option<U256>,
    pub limit: usize,
}

pub struct TributePage {
    pub records: Vec<TributeData>,
    pub next_after: Option<U256>,
}

pub struct NodPageRequest {
    pub after: Option<U256>,
    pub limit: usize,
}

pub struct NodPage {
    pub records: Vec<NodItemState>,
    pub next_after: Option<U256>,
}
```

The cursor is the last entity ID and is exclusive.

For an indexed query, the repository converts `after_id` to the complete internal storage cursor:

```text
owner query: owner20 || after_id_be32
day query:   day_be4 || after_id_be32
```

Raw `Key`, hex-encoded MongoDB `_id`, and `ScanPage` do not escape the repository.

Repository pages are all-or-error. When an identity index points to an absent, malformed, or mismatched primary body, the repository returns corruption instead of a partial page.

### Concrete reader capabilities

Use concrete domain-owned structs rather than another public trait layer or a generic entity-repository framework.

Conceptually:

```rust
pub struct TributeRepositoryReader {
    storage: StorageReaderHandle,
}

impl TributeRepositoryReader {
    pub fn new(storage: StorageReaderHandle) -> Self;

    pub fn get(
        &self,
        token_id: U256,
    ) -> Result<Option<TributeData>, TributeRepositoryError>;

    pub fn list_by_owner(
        &self,
        owner: Address,
        request: TributePageRequest,
    ) -> Result<TributePage, TributeRepositoryError>;

    pub fn list_by_day(
        &self,
        day: WorldwideDay,
        request: TributePageRequest,
    ) -> Result<TributePage, TributeRepositoryError>;
}
```

Tribute does not need a global enumeration method in this ADR.

```rust
pub struct NodRepositoryReader {
    storage: StorageReaderHandle,
}

impl NodRepositoryReader {
    pub fn new(storage: StorageReaderHandle) -> Self;

    pub fn get(
        &self,
        nod_id: U256,
    ) -> Result<Option<NodItemState>, NodRepositoryError>;

    pub fn get_bucket(
        &self,
        bucket_key: B256,
    ) -> Result<Option<NodBucketState>, NodRepositoryError>;

    pub fn list_all(
        &self,
        request: NodPageRequest,
    ) -> Result<NodPage, NodRepositoryError>;

    pub fn list_by_owner(
        &self,
        owner: Address,
        request: NodPageRequest,
    ) -> Result<NodPage, NodRepositoryError>;
}
```

`list_all` scans the `nods` primary collection. It does not maintain a redundant global index.

The concrete repository structs are cloneable handles over the ADR-001 reader capability. Backend substitution already exists below them, so an additional public repository trait would be a shallow seam with only one implementation.

### Concrete writer capabilities

Keep write authority separate from read authority at the domain level:

```rust
pub struct TributeRepositoryWriter {
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
}

impl TributeRepositoryWriter {
    pub fn new(
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
    ) -> Self;

    pub fn put(
        &self,
        tribute: &TributeData,
    ) -> Result<(), TributeRepositoryError>;

    pub fn delete(
        &self,
        token_id: U256,
    ) -> Result<(), TributeRepositoryError>;
}
```

```rust
pub struct NodRepositoryWriter {
    reader: StorageReaderHandle,
    writer: StorageWriterHandle,
}

impl NodRepositoryWriter {
    pub fn new(
        reader: StorageReaderHandle,
        writer: StorageWriterHandle,
    ) -> Self;

    pub fn put_nod(
        &self,
        nod: &NodItemState,
    ) -> Result<(), NodRepositoryError>;

    pub fn delete_nod(
        &self,
        nod_id: U256,
    ) -> Result<(), NodRepositoryError>;

    pub fn put_bucket(
        &self,
        bucket: &NodBucketState,
    ) -> Result<(), NodRepositoryError>;

    pub fn delete_bucket(
        &self,
        bucket_key: B256,
    ) -> Result<(), NodRepositoryError>;
}
```

A writer receives a storage reader internally because replacement and deletion must load the previous primary body to derive stale index keys.

`delete(id)` hides index maintenance from its caller:

1. load the current primary body;
2. if absent, return success;
3. derive index keys from the old body;
4. delete those index entries;
5. delete the primary body.

A replacement loads the old body, writes the new primary body and new indexes, and removes old indexes whose owner/day fields changed.

Bucket writes are single-key operations. Nod item and Tribute writes span a primary body and secondary indexes.

### Non-atomic repository writes

ADR-001 guarantees only per-key atomicity. ADR-002 does not pretend that a body plus its indexes is an atomic transaction.

Until ADR-004:

- typed writers are exercised by repository integration tests;
- they are not wired into live node execution;
- an operation returns the first storage failure;
- it does not claim rollback;
- a partial write may leave missing or stale indexes;
- a repository reader reports observed inconsistency as corruption;
- the affected test database may be cleared and rebuilt.

Atomic body/index batches and durable projection checkpoints are introduced with the real ExEx consumer in ADR-004.

### Repository errors

Each domain owns a structured, evolvable error type:

```text
TributeRepositoryError
NodRepositoryError
```

Both are `#[non_exhaustive]` and distinguish at least:

- wrapped `StorageError`;
- Postcard encode failure;
- Postcard decode failure;
- invalid page limit;
- malformed primary or index key;
- non-empty secondary-index value;
- dangling index pointing to a missing body;
- primary key/body identity mismatch;
- indexed owner/day not matching the loaded body;
- bucket key/body identity mismatch.

MongoDB and BSON types do not cross this boundary. Repository corruption is not translated to entity absence and is not silently skipped.

## Working result

After implementation:

- existing Tribute/Nod records serialize and deserialize through Postcard without DTO copies;
- both domain crates expose concrete typed reader/writer capabilities;
- exact body and index layouts work over Memory and MongoDB;
- owner/day/global pagination returns typed records in entity-ID order;
- Nod bucket records round-trip through their own primary collection;
- corrupt values, malformed indexes, dangling indexes, and key/body mismatches fail explicitly;
- the current EVM-backed node behavior remains unchanged.

The result is a working typed repository layer that can be populated in tests and is ready for receipt-driven projection. It is not yet the live source of domain state.

## Accepted limitations

- Tribute and Nod still use their EVM body maps in live execution.
- EVM body/query indexes are not removed yet.
- Repository multi-key writes are not atomic.
- There are no body receipt events or ExEx writer.
- Postcard bytes are temporary storage encoding, not commitment-canonical bytes.
- Identity-only indexes require one primary body lookup per returned record.
- Separate scan and body reads do not form a database snapshot.
- A partial test write may require clearing the repository.
- There are no commitments, SMTs, proofs, or production migration guarantees.

## Consequences

### Positive

- Tribute/Nod layout and query semantics are no longer infrastructure concerns.
- Callers receive typed records rather than raw bytes or storage keys.
- Body bytes have one primary source; secondary indexes do not duplicate bodies.
- Deterministic ID ordering removes sequence counters and reverse sequence maps.
- Nod bucket bodies can move off-chain while the compact EVM bin tree remains the scheduling index.
- Future ExEx and runtime code can receive only domain-level read or write capability.

### Negative

- Identity-only indexes create N+1 primary reads for filtered pages.
- The future Lysis order changes from current insertion order to token-ID order.
- Exact current Nod swap-and-pop enumeration order is not preserved.
- Replacement and delete need a read before their writes.
- Two domain repositories contain similar mechanics.
- Raw Postcard persistence is tied to current Rust field layout until the test database is rebuilt.
- Before ADR-004, multi-key failures can leave inconsistent test materialization.

## Alternatives considered

### Parallel `TributeBody`, `NodBody`, and bucket DTOs

Rejected because they duplicate the existing semantic records and require conversions that can silently omit or reinterpret fields.

### Rename existing records to body-specific names

Rejected because the current names remain meaningful and a broad mechanical rename produces no functional improvement.

### Public repository traits

Rejected because backend substitution already occurs through ADR-001 reader/writer handles. Concrete repository structs have one implementation and provide a smaller interface.

### Public generic entity repository or macro DSL

Rejected because two domains do not justify a framework for dynamic schemas or index registration. Small private helpers may remove mechanical duplication without exposing a generic extension mechanism.

### BSON or JSON bodies

Rejected because BSON would couple domain repositories to MongoDB and JSON is larger and weaker for typed binary round trips. Postcard is already used in the workspace and fits the opaque `Value` seam.

### Postcard version envelope

Deferred because no production state requires multi-version compatibility. ADR-006 introduces real canonical/schema version semantics.

### Sequence-based insertion ordering

Rejected because it requires counters, reverse mappings, and atomic metadata maintenance. Canonical entity-ID ordering is simpler and deterministic.

### Full body copies in secondary indexes

Rejected because duplication creates additional consistency obligations. Indexes contain identity only and repositories resolve the primary body.

### Keep `NodBucketState` in EVM

Rejected. The EVM bin tree and worklists retain compact scheduling keys, while complete bucket records are read through the off-chain repository.

### Move the Nod bin tree and worklists off-chain now

Rejected because they are compact protocol scheduling state. Moving ordered lifecycle selection into MongoDB would expand ADR-002 beyond the body/repository boundary.

### Add repository transactions or manual rollback

Deferred to ADR-004. ADR-002 has no live multi-key writer and should not pre-design ExEx block atomicity.

## Verification

Run the repository test suites against both ADR-001 adapters.

### Shared record tests

- Postcard round-trip for `TributeData`, `NodItemState`, and `NodBucketState`;
- all minimum/maximum integer and boolean field values;
- malformed Postcard rejection;
- exact primary key bytes;
- embedded ID/key mismatch rejection;
- bucket key/body mismatch rejection.

### Tribute repository tests

- exact namespace names;
- put/get/replace/delete;
- absent delete idempotency;
- owner and day key bytes;
- owner/day prefix isolation;
- typed owner/day pages in token-ID order;
- exclusive ID cursors and multiple pages;
- empty secondary-index values;
- non-empty index value rejection;
- dangling index rejection;
- replacement cleanup when owner/day changes;
- injected failure after every multi-key step documenting possible partial state.

### Nod repository tests

- exact namespace names;
- Nod item and bucket put/get/replace/delete;
- absent delete idempotency;
- owner key bytes and prefix isolation;
- typed owner pages in Nod-ID order;
- global pages from the primary `nods` collection;
- exclusive ID cursors and multiple pages;
- stable relative order after deletion;
- empty secondary-index values;
- non-empty index value rejection;
- dangling index rejection;
- replacement cleanup when owner changes;
- bucket corruption rejection;
- injected failure after every multi-key step documenting possible partial state.

### Existing runtime regression

Run the existing Tribute, Nod, NodFactory, and Lysis tests to prove that adding repositories and Serde support did not change the active EVM path.

Mongo integration tests use isolated databases and delete them after completion. Tests do not share mutable global state.

## Reset policy

ADR-002 does not change live node execution, so no hard fork or chain reset is required.

Repository test databases may be deleted at any time. Postcard compatibility is not preserved across field-layout changes before the production versioning ADR.

## Next unlocked step

ADR-003 can define complete receipt-visible events for Tribute, Nod item, and Nod bucket mutations. Those events will carry enough typed data for ADR-004 ExEx projection to populate the repositories without re-executing domain business logic.
