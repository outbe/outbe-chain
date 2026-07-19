# ADR-B-OCD-007: Centralize compressed-body lifecycle with a journaled block overlay

- **Status:** Proposed; migrated design, current implementation evidence requires reconciliation
- **Date:** 2026-07-15
- **Depends on:** ADR-B-OCD-006

## Context

ADR-B-OCD-006 gives Tribute, Nod item, and Nod bucket bodies exact 36-byte identities, strict-canonical Protobuf envelopes, direct typed EVM commitment mappings, verified MongoDB point reads, and public commitment-transition events.

MongoDB remains a finalized-parent materialization. After a successful body mutation in block `B`, a later call in the same block must observe the new body or deletion even though ExEx cannot project block `B` until finality. Per-domain ad hoc handling would duplicate existence transitions, same-block rules, rollback behavior, and cleanup.

ADR-B-OCD-007 introduces one generic mutation lifecycle and the permanent block-scoped body overlay used by the later SMT stages. It does not build an SMT, global root, proof service, sharding, or partition retirement.

## Starting system

The starting system has:

- three typed domain-owned commitment mappings;
- zero as canonical absence and non-zero Poseidon leaves for present bodies;
- finalized-parent MongoDB bodies verified against those mappings;
- canonical Stored/Delete events with previous/new commitments;
- no generic existence transition layer;
- no authoritative body source for read-after-write in the same block;
- finalized-parent Mongo secondary indexes cannot represent same-block owner/day/global membership changes.

## Added capability

One generic `mint/update/delete` lifecycle owns current existence checks, pending Set/Delete state, read-your-write behavior for point and list queries, repeated mutation rules, rollback, unique touched identities/index deltas, and deterministic end-block cleanup while preserving domain-owned authorization and business rules.

## Decision

### Journaled full-body block overlay

The overlay uses ordinary EVM storage through the current scoped `StorageHandle`. It is block-scoped by lifecycle and cleanup, not EIP-1153 transient storage and not process-local memory.

Conceptually:

```text
pending[typed_collection, identity_f] =
    Untouched
  | Set {
        commitment,
        stored_body
    }
  | Deleted

touched = unique identities in deterministic first-touch order
```

`stored_body` is the exact strict-canonical ADR-B-OCD-006 `StoredBody` envelope containing per-body schema version and canonical payload. A commitment-only overlay is insufficient because same-block domain reads need the complete new semantic body before MongoDB can project it.

Mutation behavior is:

```text
mint/update:
  validate generic transition
  write the persistent typed commitment mapping
  write pending Set with the same non-zero commitment and StoredBody
  append the identity on first touch only
  emit the ADR-B-OCD-006 typed Stored event

delete:
  validate generic transition
  clear the persistent typed commitment mapping to ZERO
  write pending Deleted
  append the identity on first touch only
  emit the ADR-B-OCD-006 typed Delete event
```

In ADR-B-OCD-007 storage schema v1, mutation-map writes, overlay writes, touched-list changes, domain writes, and event emission use one EVM journal/revert scope. ADR-B-OCD-008 schema v2 explicitly supersedes the direct-map portions of this pseudocode: mint/update/delete write only journaled overlay/index/event state during transactions, slots 1–3 are never mutated per entity, and the sole persistent root changes once in end-block sealing. Existing REVM checkpoints therefore revert nested-call failures, failed transactions, out-of-gas execution, and atomic system-handler failures without a second process-memory undo journal.

Consensus execution reads use overlay-first semantics:

```text
Set       -> return and verify the pending StoredBody
Deleted   -> canonical absence
Untouched -> read the exact finalized-parent MongoDB body and verify it
```

The same interface is used by proposer and validator execution. RPC/proof reads of finalized state do not consult an in-progress execution overlay.

### Fixed storage and pending encoding

The `0xEE0D` storage layout v1 is:

```text
slot 0  storage_schema_version = 1
slot 1  tribute_commitments[identity_f] -> leaf
slot 2  nod_commitments[identity_f]     -> leaf
slot 3  bucket_commitments[identity_f]  -> leaf
slot 4  pending_word[locator]                 -> U256
slot 5  pending_body[locator]                 -> StorageBytes
slot 6  touched                               -> List<B256>
slot 7   index_delta_word[delta_key]          -> U256
slot 8   index_delta_record[delta_key]        -> StorageBytes
slot 9   touched_index_deltas                 -> List<B256>
slot 10  body_identity_record[locator]        -> StorageBytes
```

The internal fixed collection IDs are:

```text
1 = Tribute
2 = NodItem
3 = NodBucket
```

They select a typed namespace only. They are not caller-controlled, do not alter EntityId36, and are not added to the ADR-B-OCD-006 leaf preimage.

The shared overlay locator is:

```text
locator = keccak256(
  "OUTBE_CE_OVERLAY_V1"
  || collection_id_u8
  || identity_f_be32
)
```

All components have fixed unambiguous lengths. Changing the prefix or collection assignments is a storage-schema/protocol change.

Because the hashed locator is not reversible, first touch also stores:

```text
body_identity_record[locator] =
  record_version_u8 = 1
  || collection_id_u8
  || EntityId36
```

The 38-byte record must recompute the same `identity_f` and locator on every access. It lets cleanup validate the touched entry and lets ADR-B-OCD-008 consume the final pending set with its original collection/identity instead of redesigning the permanent overlay. Later touches require exact record equality.

`pending_word` uses one slot:

```text
0          = Untouched
1 .. p-1   = Set(non-zero leaf)
U256::MAX  = Deleted
p .. U256::MAX-1 = invalid
```

Here `p` is the BN254 scalar-field modulus. Every valid ADR-B-OCD-006 leaf is canonical, non-zero, and below `p`, so it cannot collide with `Deleted`. An invalid word is fatal rather than normalized.

A `Set` entry must have the exact canonical `StoredBody` in `pending_body[locator]`. A `Deleted` entry must have empty pending bytes. Updating to a shorter body clears obsolete `StorageBytes` tail slots. Delete clears body bytes immediately before writing `Deleted`; end-block cleanup clears them again idempotently.

First touch is detected before mutation:

```text
if pending_word[locator] == Untouched:
    store canonical body_identity_record
    touched.push(locator)
```

Repeated transitions never return the pending word to `Untouched`, so one identity is appended once even for `Set -> Deleted -> Set`. A reverted checkpoint restores the prior word, identity record, dynamic body bytes, touched element, and touched length together.

### End-block cleanup

After all receipt-producing system and user transactions and after the last body mutation, but before final state-root calculation, the executor invokes the explicit ADR-B-OCD-007 lifecycle cleanup.

Cleanup:

1. walks every unique body locator in deterministic first-touch order;
2. validates its canonical body identity record/hash and that `pending_word` is a non-zero canonical leaf or `Deleted` with the correct body-presence invariant;
3. clears all dynamic `pending_body` and body-identity record data/tail slots, then clears `pending_word`;
4. walks every unique index `delta_key`, validates its status and canonical record/hash, then clears record bytes and delta word;
5. clears all body/index touched-list elements and finally both lengths;
6. verifies that every visited pending body/identity/word and index record/word is empty;
7. leaves the persistent typed commitment mappings unchanged.

At begin block, the touched list must be empty. A non-empty initial overlay, malformed pending encoding, missing dynamic-body cleanup, duplicate touched entry, or failed cleanup is a deterministic fatal block-execution error. A successful block reaches state-root calculation with no block-overlay body bytes remaining in persistent EVM state.

### Executor lifecycle ordering

Use a zero-sized marker implementing the repository lifecycle contract:

```rust
pub struct CompressedEntitiesLifecycle;

impl BlockLifecycle for CompressedEntitiesLifecycle {
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()>;
    fn end_block(ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult>;
}
```

The executor calls `begin_block` before begin-zone system transactions. It requires both touched-list lengths to be zero and no previous-block body/index pending state reachable from them. Dirty state is fatal; begin-block never repairs or silently clears it.

The hard-fork-governed order is:

```text
begin-zone system transactions
-> user transactions
-> receipt-visible body-mutating hooks/system transactions
-> CompressedEntitiesLifecycle::end_block
-> flush post-block EVM changes
-> notify Reth OnStateHook / parallel state-root task
-> finalize state root
```

`end_block` runs only after the final compressed-body-dependent execution read, list, or mutation. Any later execution module attempting `read`, `list`, `mint`, `update`, or `delete` is an executor-ordering bug and deterministic fatal error; after cleanup an untouched fallback would refer to the finalized parent rather than the just-executed block. The ordering is explicit in the executor, not runtime plugin registration.

Cleanup uses the same scoped `BlockRuntimeContext.storage`. It emits no receipt or body event because every canonical mutation event was already emitted by the operation that produced it. Cleanup changes are included in the buffered post-block EVM changes and delivered to the state-root task before root finalization. The final root includes persistent commitment mappings and excludes overlay words, dynamic body slots, and touched-list entries.

Proposer and validator paths invoke the same begin/end lifecycle around the same transaction list and starting state. Cleanup failure fails block execution; it never runs in a background task or after root calculation. ADR-B-OCD-007 adopts the associated-output lifecycle shape with `EndBlockResult = ()`; ADR-B-OCD-008 changes only this implementation's output to typed `SealOutput`, while all ordinary lifecycle modules remain `()`.

No extra arbitrary mutation-count limit is introduced in ADR-B-OCD-007. Instead, overlay work and its deferred cleanup are prepaid inside the transaction that creates them.

On the first body locator touch, the module deducts deterministic gas for clearing its pending word, canonical body-identity record, touched-list element, and the active schema's maximum `StorageBytes` footprint. The fixed-size v1 messages derive this footprint directly from their normative schemas rather than adding a separate arbitrary payload limit. A future variable-size schema must publish its maximum committed footprint before activation. On the first index delta touch, the module prepays clearing the delta word, canonical record slots, and index touched-list element. Repeated changes of an already touched body/index do not pay a duplicate cleanup reserve.

This ADR-B-OCD-007 charge is ordinary EVM transaction gas: it contributes to receipt cumulative gas and header `gas_used`. Gas is deducted before the corresponding mutation/event writes. Insufficient gas reverts without committed module state or logs. End-block cleanup issues no refund and credits no arbitrary transaction. Reverted transactions consume work gas under ordinary EVM rules while their journaled overlay/reservation writes disappear. ADR-B-OCD-008 separately adds non-header CE work units for deferred tree/persistence liveness; those units never replace or alter this EVM gas accounting.

Runtime point/list reads charge deterministic work proportional to delta records scanned, parent IDs consumed, verified bodies, and canonical Protobuf/Poseidon bytes. Exact coefficients use the active EVM storage schedule and implementation benchmarks and are pinned by golden gas tests before deployment. Proposer and validator recompute the same charge.

This prepayment makes successful body/index touches and repeated list-merge work block-gas-bounded without a second count cap. Future variable-size schemas still define their own semantic length/cardinality rules and gas coefficients when introduced.

The commitment mappings remain the ADR-B-OCD-006 backend in this stage. ADR-B-OCD-008 replaces that backend with an SMT while retaining overlay-first reads and the generic lifecycle interface.

### Journaled query-index deltas

A point-body overlay alone is insufficient: MongoDB owner/day/global indexes describe the finalized parent and cannot include a body minted earlier in the current block or remove one deleted earlier in the block. ADR-B-OCD-007 therefore overlays the four active query surfaces:

```text
TributeByOwner(owner)
TributeByDay(worldwide_day)
NodByOwner(owner)
NodAll
```

Conceptually:

```text
index_delta[index_kind, partition, EntityId36] =
    NeverTouched
  | Added
  | Removed
  | NoChangeTouched
```

`NoChangeTouched` represents a first-touched membership whose ordered mutations cancel back to the parent membership. It remains non-zero until end-block so a later mutation does not append a duplicate touched record.

Mutation derives membership changes from the verified current body and the new typed body:

```text
mint:   add every new membership
update: remove changed old memberships, add changed new memberships
delete: remove every old membership
```

For one membership, ordered net-delta transitions are:

```text
add:    Removed -> NoChangeTouched
        NeverTouched/NoChangeTouched -> Added

remove: Added -> NoChangeTouched
        NeverTouched/NoChangeTouched -> Removed
```

Calling add for an already-current member or remove for a non-current member is an internal invariant error. Body mutation events remain one-per-operation and ordered; only the final query-membership delta is compacted.

A list read:

1. selects all touched delta records matching the typed query/partition;
2. forms deterministic sorted Added and Removed EntityId36 sets;
3. reads finalized-parent Mongo IDs after the exclusive cursor, fetching additional bounded pages when removals reduce the candidate count;
4. removes `Removed`, unions `Added`, sorts/deduplicates by canonical EntityId36 byte order, and takes the requested page plus lookahead;
5. resolves every selected body through the same overlay-first verified point read;
6. returns the normal typed page and exclusive EntityId36 cursor.

No same-block list query is fenced or allowed to observe stale Mongo membership. Index deltas use ordinary journaled EVM storage, revert with the surrounding mutation, and are cleared by the same end-block lifecycle. They are derived state and do not introduce separate canonical events.

Secondary-index completeness of the finalized-parent Mongo input remains the explicit ADR-B-OCD-005 and ADR-B-OCD-006 testnet limitation. The delta overlay makes same-block changes correct relative to that parent; it does not prove the parent list complete.

#### Index-delta wire/storage encoding

The fixed query IDs and partitions are:

```text
1 = TributeByOwner  partition = Address20
2 = TributeByDay    partition = WWD_BE4
3 = NodByOwner      partition = Address20
4 = NodAll          partition = empty
```

A canonical record is:

```text
record_version_u8 = 1
|| index_kind_u8
|| partition_len_u8
|| partition_bytes
|| EntityId36
```

The partition length must be exactly 20, 4, 20, or 0 for the selected kind. `delta_key` is:

```text
keccak256(
  "OUTBE_CE_INDEX_DELTA_V1"
  || canonical_delta_record
)
```

`index_delta_word` uses:

```text
0 = NeverTouched
1 = Added
2 = Removed
3 = NoChangeTouched
other = invalid/fatal
```

On the first transition from `NeverTouched`, the module stores the exact record and appends `delta_key` once. Later access recomputes the hash and requires record equality. `NoChangeTouched` remains non-zero until cleanup so a later change never appends a duplicate touched key.

#### Parent page seam and deterministic merge

`ParentBodySource` also exposes an internal ID-only page:

```rust
fn list(
    &self,
    query: QueryRef,
    request: IdPageRequest,
) -> Result<IdPage>;
```

```rust
enum QueryRef {
    TributeByOwner(Address),
    TributeByDay(WorldwideDay),
    NodByOwner(Address),
    NodAll,
}
```

For an exclusive `{ after, limit }` request, the module:

1. validates non-zero repository-bounded `limit`, exact EntityId36 cursor length, and WWD-prefix equality for `TributeByDay`;
2. scans the journaled index-delta touched list, validates every stored record/hash/status, and selects the requested kind/partition;
3. forms deterministic `BTreeSet<EntityId36>` Added and Removed sets after the cursor;
4. reads strictly ascending, duplicate-free parent pages and repeatedly computes `(parent_seen - Removed) union Added`;
5. stops only when the parent is exhausted, or when a `limit + 1` merged lookahead exists and the last consumed parent ID is greater than or equal to that lookahead; strict parent ordering then proves every unseen parent ID sorts after the candidate page;
6. sorts/deduplicates by raw EntityId36 byte order and retains one lookahead;
7. returns at most `limit` IDs with `next_after = last_returned` only when the lookahead proves more data;
8. resolves every selected body through overlay-first verified `read` and requires its current typed payload to satisfy the requested owner/day/global predicate.

A parent ID not strictly after its requested cursor, a repeated/out-of-order ID, a wrong Tribute day prefix, or an Added ID encountered in the parent index is structured local projection/internal-delta corruption. As the ordered parent stream reaches or passes each relevant Removed ID, that ID must have been observed; parent exhaustion likewise requires every remaining relevant Removed ID to have been seen. Validation never scans an unrelated tail after the lookahead proves the requested page. `NoChangeTouched` has no result effect.

The repository's existing page bound remains authoritative. ADR-B-OCD-007 adds no second arbitrary limit. Loop work is bounded by the requested page, matching same-block removals, and the block-gas-bounded touched list.

### Same-block transition matrix

Generic existence checks use the overlay-aware current state:

```text
Untouched + persistent ZERO     -> absent
Untouched + persistent non-zero -> present
Set                              -> present
Deleted                          -> absent
```

The complete generic matrix is:

```text
current absent:
  mint   -> allowed
  update -> reject
  delete -> reject

current present:
  mint   -> reject
  update -> allowed
  delete -> allowed
```

Therefore ordered same-block sequences such as `mint -> update`, `mint -> delete`, `mint -> delete -> mint`, `update -> update`, `update -> delete`, and `delete -> mint` are valid when domain rules allow them. `mint -> mint`, `delete -> update`, and `delete -> delete` fail at the operation that violates current existence.

Every successful operation:

- observes the current overlay-aware commitment as `previousCommitment`;
- under ADR-B-OCD-007 schema v1 immediately updates the direct commitment mapping and final pending state; under ADR-B-OCD-008 schema v2 updates only final pending state;
- emits its own canonical ADR-B-OCD-006 mutation event in execution order;
- does not append a second touched entry for the identity.

Events are not collapsed even when the final block state equals the parent state. An update that recomputes the same leaf is a successful ordered update and emits `previousCommitment == newCommitment`, unless the domain rejects the no-op before calling the generic lifecycle.

The generic module stores no permanent historical-used-ID tombstone. It enforces current existence only. Any lifetime non-reuse requirement belongs to the domain generator/lifecycle and must be checked before `mint`; this keeps future domains free to define a different delete/recreate policy.

### Module seam and physical ownership

Add one internal runtime module:

```text
crates/core/compressed-entities
package: outbe-compressed-entities
state address: 0x000000000000000000000000000000000000EE0D
```

`0xEE0D` follows the existing `UPDATE_ADDRESS` (`0xEE0B`) and `VOTE_ADDRESS` (`0xEE0C`) allocations. It is a protocol-critical address recorded in the root README/address registry and included in the EIP-161 marker allowlist with `0xef` marker bytecode.

The module physically owns:

- three logically distinct direct commitment namespaces for Tribute, Nod item, and Nod bucket;
- the shared pending full-body overlay;
- the shared query-index delta overlay;
- unique first-touch lists for body identities and index records;
- the begin/end-block overlay lifecycle and cleanup.

Tribute and Nod continue to own authorization, business transitions, compact aggregates/scheduling state, canonical semantic record construction, and product events. Moving the direct maps to `outbe-compressed-entities` does not merge the three logical collections or allow cross-collection lookup.

There is no public mutating precompile dispatch at `0xEE0D`. Only fixed trusted Rust call paths can select a typed collection and mutate compressed state. User calldata cannot provide an arbitrary collection identifier, schema version, commitment, pending state, or event emitter.

Canonical ADR-B-OCD-006 body events remain at `TRIBUTE_ADDRESS` or `NOD_ADDRESS` with their distinct signatures. The internal module owns construction/emission of the canonical mutation record for the selected typed collection so domain callers cannot omit or forge previous/new commitments, versions, IDs, or payload bytes. Product-specific events remain domain-owned.

The module follows the complex runtime-module layout:

```text
src/
  schema.rs       # direct maps, body/index overlays, touched lists
  state.rs        # local typed storage operations
  runtime.rs      # mint/update/delete/read lifecycle
  api.rs          # narrow trusted cross-module interface
  lifecycle.rs    # begin/end block validation and cleanup
  errors.rs
  tests/
```

No MongoDB client, repository adapter, `StorageHandle` global, background task, dynamic plugin registry, or SMT implementation lives at this seam. ADR-B-OCD-008 deepens the same module internally by replacing direct-map fallback/sealing without changing domain callers.

### Trusted typed interface

The module does not expose a free-form operation accepting independent collection, ID, schema version, commitment, emitter, or raw event fields. Its trusted Rust interface uses closed typed enums:

```rust
enum BodyInput<'a> {
    Tribute(&'a TributeBody),
    NodItem(&'a NodItemBody),
    NodBucket(&'a NodBucketBody),
}

enum EntityRef {
    Tribute(EntityId36),
    NodItem(EntityId36),
    NodBucket(EntityId36),
}

struct VerifiedBody {
    // Private proof/capability fields; no public constructor.
    collection: TypedCollection,
    entity_id: EntityId36,
    commitment: B256,
    stored_body: StoredBody,
    payload: VerifiedPayload,
}

enum VerifiedPayload {
    Tribute(TributeBody),
    NodItem(NodItemBody),
    NodBucket(NodBucketBody),
}
```

`TributeBody`, `NodItemBody`, and `NodBucketBody` are the generated semantic Protobuf payload types fixed by ADR-B-OCD-006. Domain runtime converts its semantic record into the matching typed payload before calling the module. The `BodyInput` variant determines collection namespace, active codec, commitment mapping, event signature, and owning emitter; the complete ID is read from the body rather than supplied a second time.

`VerifiedBody` is an opaque value capability constructed only by successful module reads. Domain code receives typed read-only accessors for the semantic payload, but cannot construct or mutate its private collection, identity, commitment, or canonical envelope evidence.

Capability validity is value-based, not mutation-generation-based: update/delete accept it when the overlay-aware current collection, EntityId36, and leaf still match. An intervening same-leaf update or delete→mint of the identical ID/body may produce ABA and leaves the old capability valid because the current authenticated value is equal. Strict mutation freshness would require a revision counter and is deliberately not claimed.

The narrow interface is conceptually:

```rust
mint(
    storage: StorageHandle<'_>,
    new_body: BodyInput<'_>,
) -> Result<()>;

update(
    storage: StorageHandle<'_>,
    current: VerifiedBody,
    new_body: BodyInput<'_>,
) -> Result<()>;

delete(
    storage: StorageHandle<'_>,
    current: VerifiedBody,
) -> Result<()>;

read(
    storage: StorageHandle<'_>,
    parent: &impl ParentBodySource,
    entity: EntityRef,
) -> Result<Option<VerifiedBody>>;
```

For mint, the module derives the new identity, requires overlay-aware absence, encodes the active schema, calculates the leaf, derives new query memberships, writes state/overlay/index deltas, and emits the Stored event.

For update/delete, the module first re-reads the overlay-aware current commitment and requires exact equality with the consumed `VerifiedBody` capability. Update also requires equal collection and EntityId36 between `current` and `new_body`; WWD cannot move. A capability whose current identity/leaf no longer matches, a wrong typed body, or a mismatched identity fails before mutation.

The verified current payload supplies old owner/day/global memberships without a second Mongo read. Update compares those memberships with the new typed body; delete removes them. The operation then updates the direct map, body overlay, index deltas, touched lists, and canonical event in one checkpoint.

The caller cannot provide or override schema/commitment versions, previous/new commitments, leaf, pending state, collection storage key, emitter, or event signature. There is no `delete(EntityRef)` overload because deleting correctly requires the old verified body to update query memberships. Methods return `()` because callers do not need commitment internals; tests observe the same read/state/event interface as production callers.

### Parent body source seam

The consumer-owned fallback seam is:

```rust
trait ParentBodySource {
    fn get(&self, entity: EntityRef) -> Result<Option<StoredBody>>;
}
```

ADR-B-OCD-005 `RuntimeBodyReaders` is the production adapter over the typed Mongo repositories. The test adapter uses Memory repositories. MongoDB/BSON/session types do not cross the seam.

`read` hides the complete selection and verification flow:

```text
Set                       -> strict-decode and verify overlay StoredBody
Deleted                   -> None
Untouched + mapping ZERO  -> None without parent access
Untouched + mapping leaf  -> ParentBodySource.get + strict verification
```

A returned parent body is verified against the current direct-map leaf exactly as in ADR-B-OCD-006. `None` from the parent while the leaf is non-zero is `CommittedBodyMissing`, not absence. The parent adapter is never called for Set, Deleted, or mapping-zero identities.

Finalized RPC/proof reads use a finalized-state reader rather than an in-progress execution overlay. The same canonical body/leaf verifier is reused, but RPC code does not receive a mutable execution `StorageHandle`.

### Atomicity and error taxonomy

Each public mutation method applies its own local `StorageHandle::with_checkpoint` around commitment, body/index overlay, touched-list, and canonical-event writes. The surrounding domain transaction/checkpoint remains responsible for its business writes and product events. Trusted domain callers must propagate a generic lifecycle error; catching it and committing partial domain state is forbidden.

Deterministic operation reverts include:

- invalid EntityId36 length, collection variant, or WWD-prefix/body identity;
- mint of overlay-aware present state;
- update/delete of overlay-aware absent state;
- `VerifiedBody` whose identity/current leaf no longer matches, wrong collection, or update identity mismatch;
- unsupported fork-active mutation schema or canonical payload validation failure;
- derived present leaf equal to zero;
- mutation through a static/forbidden call context;
- invalid page limit/cursor/query combination;
- insufficient mutation, cleanup-reserve, or read/merge gas.

These failures commit no generic state or canonical event. Domain-specific authorization/business failures occur before the generic mutation where possible and use their domain errors.

Local parent-body outcomes retain ADR-B-OCD-005 and ADR-B-OCD-006 classification:

- technical backend `Unavailable` aborts the local execution request and enters shared `MongoUnavailable` recovery without a negative vote;
- mapping non-zero plus parent `None` is fatal `CommittedBodyMissing`;
- non-canonical body, wrong identity/schema, commitment mismatch, malformed/dangling/out-of-order parent index, or query-predicate mismatch is deterministic local corruption and fatal;
- mapping zero is canonical absence and does not access the parent adapter.

Internal state defects are deterministic fatal block errors:

- wrong storage schema marker;
- invalid pending or index-delta word;
- missing/unexpected pending body or body identity record;
- duplicate touched entry, body/index record/hash mismatch, or unreachable cleanup entry;
- non-empty begin-block overlay;
- failed dynamic-tail cleanup, post-condition, post-block flush, or state-root notification;
- any execution read, list, or mutation attempted after compressed-entity end-block cleanup.

Fatal/local-unavailable outcomes never become a `false` consensus vote. Proposer forfeits or verifier withholds under the ADR-B-OCD-005 policy; deterministic user/domain reverts remain ordinary transaction outcomes reproduced by all nodes.

## Working result

After implementation:

- Tribute, Nod item, and Nod bucket use one trusted `mint/update/delete/read/list` lifecycle seam;
- direct commitments, full same-block bodies, and owner/day/global membership changes are deterministic across all transactions in a block;
- update/delete consume a verified value capability and cannot apply a mismatched or forged prior value; same-value ABA is intentionally valid;
- every successful mutation emits one independently replayable ADR-B-OCD-006 event while final overlay/index state is compacted per touched key;
- nested reverts, failed transactions, and out-of-gas execution remove commitment, overlay, index, touched-list, and event effects together;
- finalized-parent MongoDB is accessed only for untouched non-zero identities/queries and is never the source of same-block state;
- end-block cleanup removes every temporary body/index slot before Reth state-root finalization;
- in ADR-B-OCD-007 schema v1 the final EVM state retains only direct commitment mappings and the storage schema marker at `0xEE0D`; ADR-B-OCD-008 replaces them with one persistent root slot;
- proposer and validator produce equal receipts, gas, commitments, balances, and state roots from equivalent independent parent projections.

## Accepted limitations

- Direct per-entity EVM mappings remain the commitment authority; there is no SMT, compressed-state root, proof, sharding, or Root Catalog.
- The overlay fixes same-block changes relative to the parent projection but does not authenticate parent secondary-index completeness.
- MongoDB/body availability and recovery remain local operator responsibilities.
- `outbe-compressed-entities` has three fork-fixed collections; there is no dynamic domain registry or Gem onboarding in this ADR.
- Partition retirement is unavailable.
- Full bodies and index records are written temporarily into EVM storage and cleaned every block, which increases execution/gas work.
- Exact production capacity remains subject to the later benchmark/limits stage; ADR-B-OCD-007 pins deterministic charging but makes no production throughput claim.
- Finalized RPC/proof reads do not expose an in-progress execution overlay.

## Consequences

### Positive

- Same-block point and list semantics no longer depend on MongoDB projection timing or a temporary fence.
- Existing REVM journaling supplies rollback instead of a second manual undo system.
- One deep module concentrates existence, hashing, events, overlay, pagination merge, gas reserve, and cleanup invariants.
- Domain callers retain business ownership while losing access to commitment/event internals they could accidentally desynchronize.
- ADR-B-OCD-008 can replace the direct-map backend inside the same seam without changing callers or same-block semantics.

### Negative

- `0xEE0D` becomes a protocol-critical state address and EIP-161 marker entry.
- Full body bytes and query records cause transient EVM storage writes and mandatory cleanup work.
- List reads scan block-local touched deltas in addition to MongoDB pages.
- Domains must carry/consume opaque `VerifiedBody` capabilities through their business flows.
- Canonical event emission from domain addresses requires the central module to encode logs for several typed signatures.
- Gas accounting includes deferred-cleanup reserves and merge work beyond ordinary storage charges.

## Alternatives considered

### Process-memory overlay and manual undo journal

Rejected because it would have to mirror every REVM nested checkpoint, failed transaction, and proposer/validator execution path. Ordinary EVM storage already supplies the required journal.

### EIP-1153 transient storage

Rejected because transaction-scoped transient storage does not provide read-your-write state across separate transactions in one block.

### Separate domain-owned overlays

Rejected because it duplicates pending encoding, touched lists, query merge, cleanup, and future SMT integration across Tribute and Nod.

### Commitment-only overlay

Rejected because a later same-block operation needs the complete new body before MongoDB projects the block.

### Body overlay without index deltas

Rejected because owner/day/global list queries would remain stale and would require the temporary same-block fence ADR-B-OCD-007 is intended to avoid.

### Keep temporary same-block fencing

Rejected because it removes valid composition and duplicates lifecycle machinery immediately replaced by this permanent overlay.

### Permanently retain complete EVM bodies

Rejected because it undoes the ADR-B-OCD-005 body cutover and creates two persistent body authorities.

### Collapse multiple same-block mutations into one event

Rejected because receipts are the canonical ordered mutation/recovery stream. Only final overlay/index state is compacted; successful operation history remains observable.

### Permanent generic ID tombstones

Rejected because historical reuse policy differs by domain and would add one persistent entry per deleted ID. Generic lifecycle enforces current existence; domains enforce stricter lifetime rules.

### Update/delete by raw identity without verified prior body

Rejected because index removals require the old owner/day/global memberships and accepting caller-supplied prior fields would weaken commitment verification.

### Strict mutation-generation freshness for `VerifiedBody`

Rejected because the accepted lifecycle permits same-leaf updates and delete→mint of an identical current value. Detecting that history would require a persistent/journaled per-identity revision counter that adds no current-value integrity. `VerifiedBody` is deliberately value-based: equal current identity and leaf remain valid.

### Free cleanup with a separate mutation-count cap

Rejected in favor of first-touch cleanup prepayment. Gas-bounded work composes with the existing block resource model without a second arbitrary consensus limit.

## Verification

### Storage and key vectors

Pin exact vectors for:

- `0xEE0D`, marker bytecode, and slots 0–10;
- internal collection IDs and direct-map slots;
- body locator, reversible body identity record, and index delta-key domain-separated Keccak preimages;
- `Untouched`, canonical Set leaves around the BN254 modulus boundary, `Deleted`, and invalid pending words;
- index `NeverTouched/Added/Removed/NoChangeTouched` words and invalid values;
- every canonical index record kind/partition length;
- `StorageBytes` short/long writes, shrink, delete, tail clearing, and idempotent cleanup.

### Mutation matrix and event order

For every typed collection, cover:

- absent/present mint, update, and delete;
- mint→update, mint→delete, mint→delete→mint;
- update→update, update→delete, delete→mint;
- rejected mint→mint, delete→update, and delete→delete;
- same-leaf update as a successful ordered event;
- domain rejection before generic mutation;
- mismatched-leaf/wrong-kind/wrong-ID `VerifiedBody` rejection and accepted same-value ABA;
- previous/new event commitment continuity for every successful sequence;
- no duplicate body touched locator.

### Checkpoint and failure atomicity

Inject failure before/after each direct-map, pending word/body/identity-record, touched-list, index-delta, cleanup-reserve, and canonical-event operation. Cover nested subcall revert, parent revert after successful child mutation, failed transaction, out of gas, and receipt-visible system transaction failure. Assert no partial generic/domain state or canonical log survives.

### Point and list reads

Verify:

- Set avoids parent access and returns the pending body;
- Deleted/mapping zero avoid parent access and return absence;
- Untouched non-zero performs exactly one verified parent point read;
- `CommittedBodyMissing`, corruption, backend unavailable, and mismatched value-capability classifications;
- all four query kinds over empty/non-empty parent pages;
- add/remove/no-change deltas and repeated membership changes;
- owner change, mint/delete cancellation, delete/recreate, and Nod global changes;
- additions before/between/after parent IDs;
- removals at page boundaries and enough parent over-fetch to fill the page;
- exclusive cursor, limit one/max, exact lookahead, final page, and wrong-day cursor;
- malformed, duplicate, unordered, wrong-partition, Added-already-parent, and Removed-missing parent data;
- every merged body still satisfies its query predicate.

### Lifecycle and state-root parity

Verify dirty begin-block rejection; deterministic first-touch order; both touched-list cleanup; dynamic body/record tail removal; no mutation after cleanup; final `0xEE0D` transient slots empty; persistent commitments correct; post-block state changes delivered before state-root completion; and equal proposer/validator state roots.

Crash/restart and full replay must see only finalized persistent commitments, never a committed block overlay. ExEx projection replay remains governed by ADR-B-OCD-004 and ADR-B-OCD-006 and converges from canonical events.

### Gas

Golden tests cover schema-maximum first-touch body/index reserves, repeated-touch non-duplication, revert behavior, list scan/parent/body/hash charges, insufficient-gas rollback, cleanup with no refund, and equal proposer/validator gas accounting. Benchmarks record worst-case gas-saturated touched and list-query workloads before deployment.

### End-to-end

Run combined Tribute issue/update/burn/Lysis and Nod issue/qualification/mining/Gratis flows with multiple dependent transactions in one block. Use independent MongoDB projections for proposer and validators and assert equal outputs, events, mapping state, final empty overlays, balances, and roots.

## Reset policy

ADR-B-OCD-007 is implemented on the same branch as ADR-B-OCD-005 and ADR-B-OCD-006 and continues directly into ADR-B-OCD-008 through ADR-B-OCD-010 without testnet activation. Schema v1 direct maps are a focused implementation/reference stage only; no genesis or network is started with them.

The single first CES1 deployment occurs after ADR-B-OCD-010 and combined verification. Its final genesis allocates marker-preserved `0xEE0D` using the then-current root/overlay schema, not this temporary direct-map layout. The address is added to the protocol registry, README, EIP-161 allowlist, and relevant architecture/debt records in that combined implementation change.

There is no migration from legacy EVM bodies/U256 IDs, intermediate direct maps, or unsharded state; no overlay reconstruction, dual-write/read, or temporary fence. MongoDB starts empty at the one coordinated reset and rebuilds from final CES1 events.

## Next unlocked step

ADR-B-OCD-008 replaces the three direct maps with an authenticated unsharded CKB reference stage, then ADR-B-OCD-009 and ADR-B-OCD-010 complete sharding and Root Catalog before the first testnet activation, while preserving the trusted domain interface, canonical events, same-block body/index overlay, and rollback semantics defined here.

## Open questions and technical debt

- **Critical:** prove the overlay is installed and cleared on every execution exit,
  including validation, RPC simulation, revert, panic and nested calls.
- Replace distributed flags with typed lifecycle states and reject mutation without a
  verified prior generation.
- Demonstrate one checkpoint owns EVM writes, CE deltas, query indexes and events, with
  rollback injection at every step.
- Bound mutation count, overlay bytes, index deltas and cleanup work; flat base gas is
  not proportional metering.
- Add exhaustive same-block create/update/delete/retire transition tests through the
  production executor.
