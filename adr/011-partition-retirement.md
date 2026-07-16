# ADR-011: Retire completed Tribute partitions through one Root Catalog delete

- **Status:** Proposed
- **Date:** 2026-07-17
- **Depends on:** ADR-010

## Context

ADR-010 gives every Tribute Worldwide Day (WWD) an independent authenticated collection whose non-zero collection root is stored as one Root Catalog leaf. Ordinary entity deletion deliberately retains an empty collection leaf, while no operation can remove a finished collection or reclaim its finalized CE MDBX namespaces.

The current Lysis implementation is not a safe retirement boundary. It creates Nods only for Tributes whose computed `gratis_load` is non-zero and fits the remaining allocation, preserves skipped Tributes "for potential reprocessing", and can then let Metadosis complete the WWD. That skip is a confirmed bug: a completed Lysis has no later per-Tribute reprocessing phase. Completion must instead mean that the complete input partition was transformed atomically.

Lysis is expected to move to co-located off-chain computation later. This ADR fixes the consensus apply/retirement semantics without designing that computation architecture prematurely. The first implementation remains a testnet path and deliberately does not add production-grade same-block read-after-retirement behavior.

## Starting system

After ADR-010:

- Tribute domain `1` has one `K_PROVISIONAL = 16` collection per canonical `WWD_BE4` partition;
- every present collection owns exactly 16 shard namespaces and one non-zero Root Catalog leaf;
- `EmptiedByDelete` retains its Catalog leaf and 16 explicit ZERO shard roots;
- journaled entity `Set`/`Deleted` state supplies same-block body semantics;
- collection changes are derived from entity mutations, with no collection-level operation;
- one hierarchical candidate and one marker-last CE MDBX transaction atomically commit collection shards and Root Catalog changes;
- MongoDB is an asynchronous finalized receipt projection and never delays finalization or Marshal ACK.

## Added capability

A successful Lysis can retire its complete Tribute WWD collection with one domain-authorized operation. Retirement removes the collection leaf from the new Root Catalog, makes the collection absent from the next committed root, emits one WWD event for finalized Mongo range deletion, and atomically reclaims the finalized CE MDBX shard namespaces.

Retirement is not a per-entity delete loop. Failed Lysis leaves the complete Tribute partition present.

## Decision

### Lysis is an all-or-nothing partition transformation

For a non-empty Tribute WWD input, successful Lysis has this invariant:

```text
for every verified Tribute in WWD:
  exactly one corresponding Nod is created

number_of_created_nods == number_of_input_tributes

then:
  Tribute DayTotals.tribute_count          = 0
  Tribute DayTotals.tribute_nominal_amount = 0
  the WWD remains sealed
  Metadosis may commit COMPLETED
  the Tribute collection is retired
```

A Tribute is never skipped successfully. The current branches that continue when `gratis_load == 0` or `gratis_load > remaining` become failure paths. An existing non-empty partition with zero total interest is also not a successful empty result. Any inability to produce one required Nod fails the complete Lysis attempt.

Nod creation, contributor/economic writes, Tribute aggregate updates, Metadosis completion, the retirement request, and the canonical retirement event share one surrounding EVM checkpoint. Failure after any partial Nod work rolls all of those effects back. In particular:

```text
Lysis FAILED:
  no retirement request
  no TributePartitionRetired event
  no Root Catalog delete
  no per-Tribute deletion
  the Tribute collection remains present
```

Whether the outer Metadosis state machine records a separate failure outcome or retries is not retirement authority. ADR-011 has only one authorization fact: the successful all-or-nothing completion path.

The successful path performs one bulk Tribute accounting transition rather than calling the generic entity `delete` operation for every body. It validates the verified input count and nominal sum against `DayTotals` before zeroing them and adjusting total supply. Mismatch is deterministic failure, not partial cleanup.

### Only completed, present Tribute collections retire

The first retirement policy is closed and domain-specific:

```text
retireable domain: Tribute (domain_id = 1)
partition key:     canonical WWD_BE4
trigger:           successful all-or-nothing Lysis completion
```

`FAILED`, active, merely sealed, or otherwise unfinished WWDs cannot retire. NodItem and NodBucket are singleton domains and reject retirement.

A WWD with no historical Tribute collection already has no Catalog leaf and performs no retirement operation or event when Metadosis completes it. An `EmptiedByDelete` collection is still present and is retired normally. Therefore:

```text
COMPLETED + Catalog leaf present:
  retire and emit TributePartitionRetired

COMPLETED + Catalog leaf absent because never populated:
  no retirement request, no event, no root change
```

The generic module does not create an empty collection merely to delete it, and it does not emit a lifecycle-only event without an authenticated Catalog transition.

### Closed trusted retirement interface

The generic compressed-entity module adds a narrow internal operation conceptually equivalent to:

```rust
retire_partition(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    partition: PartitionRef::TributeWwd(WorldwideDay),
) -> Result<RetirementOutcome>;
```

`PartitionRef` is a closed typed Rust value. The module derives domain ID, canonical WWD bytes, `collection_key_f`, and `collection_key`; the caller cannot supply any of those independently. It verifies the fork-active domain policy and exact-parent Catalog membership through the same immutable `AuthenticatedCatalogView` used by ADR-010.

There is no public mutating ABI at `0xEE0D`, no operator/CLI retirement command, and no arbitrary domain or partition bytes from user calldata. The only first-version caller is the trusted successful Lysis/Metadosis path.

An exact-parent absent collection returns `NotPresent` and records nothing. A duplicate request, a request for a singleton/unknown domain, a non-canonical WWD, or a request outside the active execution phase fails deterministically.

### Minimal journaled retirement request

Retirement cannot update `R_sealed` during Lysis because all collection changes in the block must still be combined once in `end_block`. The operation therefore records a minimal EVM-journaled command:

```text
pending_retirement[collection_key] = Requested
retirement_touched = unique first-touch collection identities
```

This state exists only to:

- roll back with the surrounding Lysis checkpoint;
- deduplicate and authorize the end-block Catalog delete;
- enforce mutation exclusivity;
- drive deterministic cleanup;
- bind the canonical retirement event to the successful request.

It is not a full read-aware collection overlay. Point and list reads do not consult it in the first testnet implementation.

At begin block, both retirement structures must be empty. `end_block` consumes them before clearing every dynamic record and list entry. A dirty begin state, duplicate/unreachable touched record, malformed marker, cleanup failure, or non-empty post-condition is fatal block execution, matching ADR-007 overlay rules.

Adding this journaled structure advances the `0xEE0D` EVM storage schema at the ADR-011 activation fork. The additive migration requires the ADR-010 entity overlay and new retirement state to be empty at the boundary, initializes the new fields to empty, and updates slot 0 through the normal hard-fork migration path. It does not add dual reads, dual writes, or a compatibility mode.

### Strict same-block mutation exclusivity

A collection cannot contain both entity mutations and retirement in one block:

```text
entity Set/Delete in collection C + retire C = invalid block execution
retire C + later entity mint/update/delete in C = deterministic rejection
```

The production ordering remains explicit: Lysis executes in the system begin-zone for an already sealed WWD before user transactions and before other compressed-entity mutation work on that partition. The pending request lets later generic mutations reject immediately. End-block independently rejects any earlier touched-entity final state grouped under the same collection key, so an executor-ordering bug cannot be normalized as "retirement wins".

NodItem and NodBucket mutations created by Lysis are unaffected because they belong to different collection keys. A block may atomically retire one Tribute WWD while creating/updating both Nod collections.

This rule avoids ambiguous event replay, gas refunding, and body mutations that Mongo would immediately erase. It also avoids adding a second general collection-mutation overlay merely to support combinations the domain lifecycle forbids.

### Deliberately stale same-block reads on testnet

Because bulk retirement writes no per-entity `Deleted` entries and reads ignore the retirement request, a later read in the retirement block may still resolve an untouched Tribute through the exact finalized parent:

```text
Lysis completed and retirement requested
R_sealed still represents the parent until end_block
same-block point/list read may return the parent Tribute body
```

This is an explicit testnet limitation, not authenticated current-state semantics. The result is deterministic across proposer and validators and cannot reintroduce a valid mutation because the WWD is sealed and generic mutation paths reject the pending retirement. After end-block sealing, the new root omits the collection; the next block and finalized readers observe absence.

ADR-011 does not add WWD filtering to point, owner, day, or pagination paths. Production read semantics are reconsidered with the later off-chain-computation apply boundary. If that boundary guarantees retirement after the last compressed-body read, no read-aware overlay is needed; otherwise a later ADR may strengthen it with measured implementation evidence.

### One canonical WWD retirement event

A successful present-collection request emits exactly:

```solidity
event TributePartitionRetired(uint32 indexed worldwideDay);
```

The emitter is `TRIBUTE_ADDRESS`, following the existing typed canonical Tribute event surface. No domain ID, collection key, commitment version, previous collection root, entity count, or body list is duplicated in the event. Fork-active rules at the event's block derive domain `1`, the WWD partition bytes, collection key, and commitment topology.

The event shares the Lysis/retirement journal: a failed or reverted attempt leaves no log. It replaces all per-entity `TributeBodyDeleted` events for this bulk transition. Product-level Metadosis/Lysis events may coexist but are not projection or tree-replay authority.

Projection accepts only the exact `TRIBUTE_ADDRESS`/event-signature pair, decodes the canonical `uint32` WWD, and applies one idempotent Mongo range delete for that WWD in receipt order. The range delete and the projector's durable block checkpoint retain ADR-004's block-apply atomicity. Mongo remains the sole responsibility of ExEx; projection lag or failure never delays CE MDBX finalization, Marshal ACK, or becomes a negative consensus vote.

### Catalog-only authenticated transition

At end block, retirement is a Root Catalog mutation rather than a shard mutation:

```text
catalog_key = CKB_H256::from(BE32(collection_key_f))
RootCatalogSMT.update(catalog_key, ZERO)
```

The exact-parent snapshot must prove a non-zero Catalog leaf and the complete `K_domain = 16` shard-root vector whose recomputed `R_collection` equals that leaf. Retirement does not enumerate collection leaves, prepare per-shard SMT deletes, or calculate a replacement empty `R_collection`.

ADR-010's collection change envelope becomes an explicit canonical variant, conceptually:

```text
CollectionOperation =
    Mutate(CollectionBatch)
  | Retire(RetirementBatch)

RetirementBatch {
    domain_id,
    collection_key,
    parent_collection_root,
    parent_shard_roots: [B256; K_domain],
}
```

The canonical batch visitor owns the operation discriminant, ordering, encoded size, and checksum; there is no retirement-specific parallel codec. The exact bytes are pinned with ADR-011 golden vectors after the mandatory seam review against the implemented ADR-010 envelope.

Changed collection operations remain ordered by the pinned CKB Catalog key order. The aggregate Catalog batch contains `Delete` for retired keys and `Set` for mutated collection roots. Mixed Tribute retirement and Nod mutations produce one candidate, one `new_catalog_root`, and one `new_r_sealed`. A failure in any collection or Catalog preparation discards the complete candidate and block result.

A block with an absent never-populated WWD has no retirement operation. A block containing only an effective retirement still advances the ordinary outer candidate identity and changes `R_sealed`; a block with no effective body or retirement changes advances only the finalized identity marker as in ADR-010.

### Atomic finalized namespace reclamation

A finalized retirement physically removes its collection shards in the same CE MDBX write transaction that commits the Catalog delete. The existing local schema and namespace codec remain V3; no tombstone namespace or garbage-generation table is added.

Finalized apply performs:

1. verify the current marker, exact parent block/hash/root, stored parent Catalog root, and `R_sealed` wrapper;
2. verify the retirement's parent Catalog membership, non-zero `R_collection`, exact domain `K`, and complete shard-root vector;
3. delete every branch and leaf record under each of the 16 typed `CollectionShard(collection_key, shard_index)` prefixes;
4. delete all 16 corresponding `CeTreeRootsV3` entries;
5. apply the aggregate Root Catalog batch, including the collection leaf delete;
6. write `TreeNamespace::Catalog -> new_catalog_root`;
7. recompute and require the candidate/EVM `new_r_sealed`;
8. write `last_applied` last and commit once;
9. permit Marshal ACK only after the successful CE MDBX commit.

MDBX readers observe either the complete old materialization or the complete retired materialization. A committed Catalog non-membership can never coexist with orphan shard records/root entries, preserving ADR-010's corruption invariant. Transaction abort, process crash, uncertain commit, and redelivery follow ADR-008–010 marker/idempotency recovery; no partially reclaimed namespace is accepted.

Synchronous prefix reclamation can increase finalized-apply latency for a large WWD. The testnet accepts that cost. ADR-017 measures the completed path and owns any later bounded garbage-collection/generation design; ADR-011 does not add an asynchronous local cleanup protocol speculatively.

### Domain-owned permanent non-reuse

The generic compressed-entity module stores no permanent retirement tombstone. Successful retirement leaves the compact Tribute `DayTotals` record in EVM state:

```text
initialized                    = true
is_sealed                      = true
tribute_count                  = 0
tribute_nominal_amount         = 0
```

The record is not part of the compressed body collection and is not reclaimed by ADR-011. Together with monotonic block time, date-derived WWD creation, the trusted Metadosis transition graph, and the absence of a public unseal/mutation entrypoint, it prevents the retired WWD from being populated again. The canonical `TributePartitionRetired(wwd)` log supplies historical evidence after bounded Metadosis terminal records are pruned.

Catalog non-membership alone continues to mean `NeverPopulated | Retired`. Later proof and snapshot formats must not claim to distinguish those histories without domain state or canonical chain evidence.

### Future off-chain computation boundary

Moving Lysis computation off chain does not change the retirement transition fixed here. A future computation component may calculate the complete Nod output set, but consensus execution must still validate/apply one all-or-nothing result and invoke the same typed retirement operation only after complete success.

Worker topology, computation proofs/attestation, concurrency, caches, resource sharing, and production timing are outside ADR-011. They require their own seam review and measured closure; retirement must not embed assumptions about a provisional worker implementation.

## Working result

After implementation:

- every non-empty successful Tribute Lysis creates one Nod per input Tribute or fails without partial effects;
- `COMPLETED` may retire one present WWD through one Catalog delete and one event, without per-entity SMT deletes/events;
- `FAILED` Lysis leaves all Tribute bodies, the Catalog leaf, Mongo rows, and CE MDBX namespaces present;
- a never-populated completed WWD performs no fake retirement;
- a present empty WWD can be retired normally;
- same-collection mutations and retirement cannot coexist in one block;
- finalized CE MDBX reclaims all retired shard namespaces atomically before ACK;
- finalized Mongo projection removes the WWD range asynchronously;
- Nod singleton collections and other Tribute WWDs continue unchanged.

## Accepted limitations

- Same-block point/list reads after retirement request may return exact-parent Tribute bodies until end-block sealing. This is intentional for the first testnet.
- Lysis still depends on the current verified parent-body path until the separately designed off-chain computation boundary exists.
- Synchronous finalized MDBX prefix deletion has no production latency bound yet.
- Only the Tribute WWD domain supports retirement. Singleton Nod domains reject it.
- `R_sealed` remains EVM-state-only until ADR-012; proof RPC, recovery audit, and portable snapshots remain ADR-013/015/016 work.
- Catalog absence does not by itself distinguish never-populated from retired.

## Consequences and trade-offs

Benefits:

- completion has one clear business meaning: the whole Tribute partition was transformed;
- one Catalog delete replaces per-body tree deletes and logs;
- failed computation preserves the complete source partition for diagnosis or a later policy decision;
- EVM checkpointing, one hierarchical candidate, and one marker-last MDBX transaction preserve atomicity;
- domain state owns lifecycle/non-reuse while the generic module owns authenticated collection removal;
- the design remains compatible with future off-chain Lysis computation without specifying it early.

Costs:

- bulk success cannot preserve an individually skipped Tribute;
- the first testnet temporarily exposes stale same-block reads;
- one additional journaled retirement mapping/list and EVM schema activation are required;
- finalized apply performs synchronous work proportional to the retired namespace size;
- per-entity delete history is replaced by one WWD event.

Rejected alternatives:

- retiring `FAILED` WWDs, because failure must preserve the source partition;
- treating skipped Tributes as successful leftovers, because no post-completion reprocessing phase exists;
- emitting one `TributeBodyDeleted` per entity, because that defeats collection retirement;
- adding a read-aware retirement overlay now, because its pagination/read complexity is unnecessary for the testnet and may be superseded by the off-chain apply ordering;
- allowing retirement to absorb same-block entity mutations, because it makes event replay and gas semantics ambiguous;
- emitting retirement for an absent never-populated collection, because the event must correspond to a real Catalog transition;
- emitting a generic multi-field `0xEE0D` event, because the typed Tribute emitter and WWD determine every other field through fork rules;
- deleting the Catalog leaf before asynchronous MDBX garbage collection, because it violates the no-orphan exact materialization invariant;
- adding a permanent generic retirement tombstone, because Tribute's sealed monotonic WWD lifecycle already owns non-reuse.

## Verification

### Lysis completion and rollback

Cover non-empty WWDs where:

- every Tribute produces exactly one Nod and the partition retires;
- `gratis_load == 0` for any Tribute fails the whole Lysis;
- `gratis_load > remaining` for any Tribute fails the whole Lysis;
- a later Nod creation fails after earlier Nods were staged;
- contributor/economic, Tribute aggregate, Metadosis completion, event, and retirement writes fail at every checkpoint boundary;
- verified list count or nominal sum differs from `DayTotals`.

Every failure must preserve all input Tributes, previous Nod state, Catalog/root state, retirement journal, logs, and candidate publication state. These tests close the current successful-skip bug rather than documenting it as supported behavior.

### Retirement state matrix

Verify:

- active/sealed-but-incomplete/failed WWD rejection;
- present non-empty completed collection retirement;
- present `EmptiedByDelete` completed collection retirement;
- never-populated completed WWD no-op without event;
- duplicate retirement rejection;
- NodItem/NodBucket/unknown domain rejection;
- canonical WWD and collection-key derivation;
- retained sealed zero `DayTotals` and rejected later population.

### Journal and ordering

Inject failure before and after every retirement marker, touched-list, event, cleanup, and schema-migration boundary. Verify nested/system-transaction rollback, dirty-begin rejection, unique first-touch behavior, complete cleanup, and empty post-condition.

Cover mutation-before-retirement, retirement-before-mutation, and mixed same-key end-block input as deterministic failures. Cover retirement of one Tribute WWD alongside another WWD mutation and NodItem/NodBucket mutations as a valid atomic batch.

Confirm the documented testnet limitation explicitly: a same-block read may see the parent body, while a mutation is rejected and the next-block read sees collection absence.

### Event and Mongo projection

Pin the exact signature:

```text
TributePartitionRetired(uint32)
```

Verify exact emitter/signature filtering, WWD decoding, absence after revert, one event per effective retirement, no per-body delete events, receipt-order application, idempotent WWD range deletion, crash/replay convergence, and checkpoint-last block atomicity. Mongo failure must not delay CE MDBX commit or Marshal ACK.

### Root and canonical batch vectors

Add golden vectors for:

- retirement request identity and cleanup encoding;
- present non-empty and all-ZERO collection retirement;
- Catalog delete to non-empty and structurally empty Catalog roots;
- mixed retirement plus Nod collection sets in CKB key order;
- canonical collection-operation discriminants, parent root vectors, encoded size, and checksum;
- resulting `catalog_root` and `R_sealed` under scheme 1 and K=16.

Run independent reference-model, proposer/validator, and cross-architecture equality tests. Reject absent/wrong parent leaves, incomplete/extra shard roots, conflicting mutations, malformed operations, and candidate/EVM root mismatch.

### Finalized MDBX reclamation

For every injected step and crash boundary, verify all-or-nothing visibility of:

- 16 branch/leaf namespace prefixes;
- 16 root-table entries;
- Root Catalog leaf/nodes/root;
- `last_applied` marker;
- Marshal ACK.

Exercise commit-before-ACK redelivery, restart after uncertain commit, multiple retired WWDs over time, and absence of orphan namespace records. Benchmark namespace deletion separately and in the full finalized path for later ADR-017 closure.

## Reset and activation policy

ADR-011 is an additive testnet hard fork after the ADR-010 CES1 reset. It changes no body commitment, EntityId36, collection key, tree key, shard count, collection root, Catalog key, or `R_sealed` formula. Existing CE MDBX V3 data remains structurally compatible.

The activation advances the EVM storage schema for the new empty retirement journal and activates the new canonical event and collection-operation variant at one fork height. There is no dual behavior, fallback, operator flag, or migration of body/tree data. A complete pre-production reset remains permitted if implementation sequencing makes it cheaper, but is not required by the architecture.

Before implementation, ADR-011 must be reviewed against the actual ADR-010 candidate envelope, canonical visitor, storage layout, `AuthenticatedCatalogView`, and finalized apply code. Concrete field placement and operation bytes are pinned only through that seam review and golden vectors, without changing the decisions above.

## Next unlocked step

ADR-012 carries the stable execution-computed `R_sealed` directly in block header artifacts while retaining EVM `0xEE0D.slot1` as execution authority.
