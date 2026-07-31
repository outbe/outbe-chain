# ADR-B-OCD-010: Commit independent compressed-entity collections through a Root Catalog

- **Status:** Proposed; migrated design, current implementation evidence requires reconciliation
- **Date:** 2026-07-16
- **Depends on:** ADR-B-OCD-009

## Context

ADR-B-OCD-009 splits the authenticated map into a parameterized shard set using provisional fork-fixed `K = 16` and preserves one atomic exact-parent candidate/persistence boundary. ADR-B-OCD-010 groups those shard sets into independent collection instances and commits their roots through one dynamic Root Catalog before the first CES1 testnet activation.

The current runtime already distinguishes three typed authenticated namespaces: Tribute, Nod item, and Nod bucket. The older all-at-once concept described one singleton Nod domain, but combining Nod item and Nod bucket under one collection would require a new entity-kind discriminant in key/proof/replay derivation and would weaken the direct mapping from the current typed interface to authenticated state.

ADR-B-OCD-010 uses **compressed-entity domain** to mean an authenticated body namespace, not a business runtime module. A runtime owner may own multiple compressed-entity domains.

## Starting system

The ADR-B-OCD-009 implementation provides:

- a closed typed entity interface for Tribute, Nod item, and Nod bucket;
- one canonical body/leaf per typed entity;
- deterministic `tree_key -> shard_index` selection;
- one fixed-size shard set with ordered shard-root aggregation;
- one atomic staged batch and one finalized MDBX marker;
- no collection-key namespace or Root Catalog.

## Decision

Each compressed-entity domain has one partition policy and produces one or more independent collection instances. Every collection instance owns its own ADR-B-OCD-009 shard set and collection root. A Root Catalog SMT commits the current collection roots and yields one final `R_sealed` for EVM state and later header/proof work.

## Decisions fixed in this pass

### Three authenticated domains

The first CES1 topology has three compressed-entity domains:

```text
Tribute
  runtime owner: Tribute
  partition policy: Partitioned by Worldwide Day
  collection instances: one per canonical WWD

NodItem
  runtime owner: Nod
  partition policy: Singleton
  collection instances: exactly one

NodBucket
  runtime owner: Nod
  partition policy: Singleton
  collection instances: exactly one
```

NodItem and NodBucket share the Nod runtime owner and may participate in one business operation, but they have distinct authenticated collection keys, shard sets, collection roots, catalog leaves, and proof paths. A lookup never retries the sibling Nod domain on absence.

For Tribute, the canonical partition key is the first four bytes of `EntityId36`, interpreted as the existing big-endian `WorldwideDay` value. The lifecycle derives it from the typed identity and rejects any independently supplied mismatch. NodItem and NodBucket have no partition key.

A **collection instance** is identified conceptually by:

```text
(compressed_entity_domain, canonical_partition_key_or_none)
```

A business runtime address/emitter is not a collection identifier. Multiple domains may retain the same runtime owner without sharing authenticated keys.

### Stable domain IDs and collection keys

The fork-fixed unsigned 16-bit domain IDs preserve ADR-B-OCD-008's typed collection numbering:

```text
DOMAIN_ID_RESERVED   = 0
DOMAIN_ID_TRIBUTE    = 1
DOMAIN_ID_NOD_ITEM   = 2
DOMAIN_ID_NOD_BUCKET = 3
```

ID `0` is invalid/reserved. IDs are protocol surface: Rust enum ordering, names, runtime addresses, emitters, and ABI signatures never derive or renumber them. A registry/build-time definition rejects duplicate IDs and local override.

Collection keys use the already reserved CES1 tag:

```text
TAG_COLLECTION_KEY = CES1_TAG_BASE + 13

collection_key_f = PBytes(
    TAG_COLLECTION_KEY,
    domain_id_be2
    || partition_presence_u8
    || partition_key_len_be4
    || partition_key
)

collection_key = BE32(collection_key_f)
```

Canonical inputs are:

```text
Tribute(wwd):
  0x0001 || 0x01 || 0x00000004 || WWD_BE4

NodItem:
  0x0002 || 0x00 || 0x00000000

NodBucket:
  0x0003 || 0x00 || 0x00000000
```

`partition_presence_u8` accepts only `0` or `1`. `0` requires zero length and no bytes; `1` requires the exact registered domain partition length/encoding. Tribute requires exactly four WWD bytes and equality with its EntityId36 prefix. Singleton domains reject any partition bytes. No address, emitter, event selector, schema version, body type name, or operator input enters the collection key.

Collection key zero is a valid CKB/Root Catalog key position; zero is reserved as absence only for leaf values. A local derivation mismatch is structured corruption/invalid input, never a fallback search by another key.

### Final entity and tree keys

ADR-B-OCD-010 preserves ADR-B-OCD-006 identity and body-leaf commitments byte-for-byte:

```text
id_f = PBytes(TAG_ID, EntityId36)

body_f = PBytes(TAG_BODY, canonical_payload)

leaf_f = P(
    TAG_LEAF;
    commitment_scheme_version,
    schema_version,
    id_f,
    payload_byte_len,
    body_f
)
```

Only the ADR-B-OCD-008 and ADR-B-OCD-009 intermediate tree-key namespace changes before first activation:

```text
tree_key_f = P(
    TAG_KEY;
    commitment_scheme_version,
    collection_key_f,
    id_f
)
```

The existing direct `BE32(tree_key_f) -> CKB_H256` bridge and ADR-B-OCD-009 shard-index derivation remain unchanged. Tribute validates that the WWD encoded in `collection_key_f` equals the first four EntityId36 bytes before deriving the key. NodItem/NodBucket require their exact singleton domain key. Domain/collection selection remains closed typed Rust state, never calldata/operator input.

A bare `leaf_f` is not a globally namespaced entity identifier and may be equal for equal ID/body bytes in different domains. Authenticated identity is the full chain `domain/partition -> collection_key -> tree_key -> leaf`; events and runtime capabilities retain their typed domain context. Proof/replay tooling must never accept a leaf without the expected domain, collection, raw EntityId36, and tree-key derivation.

This supersedes ADR-B-OCD-008's temporary `P(TAG_KEY; scheme, collection_id, id_f)` formula before any network activation. Body commitments, mutation event commitment fields, Mongo verification, and `VerifiedBody` equality do not change or require recommitment.

### Per-domain shard-count field

Every fork-fixed compressed-entity domain definition contains its own power-of-two `collection_shard_count = K_domain`. Every collection instance of that domain uses the same value. For the first scheme-1 activation:

```text
K_Tribute   = K_PROVISIONAL = 16
K_NodItem   = K_PROVISIONAL = 16
K_NodBucket = K_PROVISIONAL = 16
```

The pre-production/testnet topology deliberately uses the same provisional value without claiming it is performance-optimal. ADR-B-CAP-001 later benchmarks the completed system and selects `K_PRODUCTION`.

The collection-root commitment and proof derivation must bind the resolved `K_domain`; callers, RPC requests, events, MDBX-local configuration, and operators cannot select it. A later new domain may be fork-activated with another measured shard count without changing the generic interface. Changing `K_domain` for an already populated/activated domain changes commitment topology and requires an explicit new scheme/migration/rebuild decision.

This retains a per-domain model without prematurely optimizing Tribute, NodItem, and NodBucket differently. The cost is three initially redundant registry values and an obligatory domain-registry lookup in proof/replay derivation.

### Collection root and presence states

Collection roots use the reserved CES1 tag:

```text
TAG_COLLECTION_ROOT = CES1_TAG_BASE + 14

R_collection = P(
    TAG_COLLECTION_ROOT;
    commitment_scheme_version,
    collection_key_f,
    K_domain,
    shard_top_root
)
```

`K_domain` is the fork-fixed unsigned shard count embedded canonically in `Fr`; `shard_top_root` follows ADR-B-OCD-009. A Poseidon result of `ZERO` for a present collection is `TreeHashError`, because Root Catalog value zero means absence/delete.

The current topology distinguishes:

```text
NeverPopulated:
  Root Catalog leaf absent
  physical shard namespace may be absent

PresentNonEmpty:
  catalog[collection_key] = non-zero R_collection

EmptiedByDelete:
  every shard root = ZERO
  catalog[collection_key] = non-zero R_collection(all-ZERO shard top)

Retired:                         // introduced by ADR-B-OCD-011
  Root Catalog leaf absent
  permanent non-reuse enforced by domain state/event
```

The first successful mint into `NeverPopulated` starts from the canonical all-ZERO shard vector and creates the catalog leaf atomically with its entity mutation. Ordinary deletion of the last entity does not delete the collection leaf or require a persistent entity counter/full-shard scan. Later mint into `EmptiedByDelete` is allowed only when the owning domain lifecycle permits it. ADR-B-OCD-011 retirement is the only generic operation that deletes a present collection leaf and enables finalized physical namespace reclamation.

A proof against the current root distinguishes `EmptiedByDelete` from catalog absence, but catalog absence alone does not distinguish `NeverPopulated` from `Retired`; domain state and canonical retirement evidence own that historical/lifecycle distinction.

This avoids last-entity accounting and keeps retirement explicit. The cost is retained empty catalog leaves and shard namespaces until retirement, plus a longer non-membership proof for an entity in an emptied collection.

### Root Catalog and sealed root

The Root Catalog is one unsharded instance of the same pinned CKB/Poseidon SMT used by ADR-B-OCD-008 and ADR-B-OCD-009. It uses the same `TAG_SMT_BASE`, `TAG_SMT_NORMAL`, `TAG_SMT_ZERO`, compact-zero semantics, proof codec, canonical field checks, and direct BE32-to-CKB bridge:

```text
catalog_key   = CKB_H256::from(BE32(collection_key_f))
catalog_value = R_collection

create/update collection:
  RootCatalogSMT.update(catalog_key, R_collection)

ADR-B-OCD-011 retirement:
  RootCatalogSMT.update(catalog_key, ZERO)

catalog_root = RootCatalogSMT.root()
```

`R_collection` is stored verbatim as the non-zero catalog leaf value; there is no second leaf wrapper. Root Catalog key zero is valid. A non-empty catalog root that hashes to `ZERO` is `TreeHashError`; the structurally empty catalog root is canonical `ZERO`.

The final scheme-1 authority is domain-separated and scheme-bound:

```text
TAG_SEALED_ROOT = CES1_TAG_BASE + 12

R_sealed = P(
    TAG_SEALED_ROOT;
    commitment_scheme_version,
    catalog_root
)
```

A computed `ZERO` `R_sealed` is invalid. Consequently the empty system has a deterministic non-zero `P(TAG_SEALED_ROOT; 1, ZERO)` root. EVM `0xEE0D.slot1` stores `R_sealed`, retaining the root-backed schema-v2 slot while changing its pre-activation meaning from ADR-B-OCD-009's intermediate shard top to the final sealed root.

The local exact-parent materialization stores `catalog_root` and recomputes the wrapper before requiring equality with the EVM-authoritative `R_sealed`. A shard/collection proof is not trusted until its Root Catalog proof recomputes that stored catalog root and the wrapper matches the selected parent authority. ADR-B-OCD-012 may later copy the same `R_sealed` into a header artifact without changing commitment semantics.

The Root Catalog remains unsharded and updates after changed collection roots are known. This adds one CKB proof/update layer and one final Poseidon hash, but supports a dynamic number of Tribute WWD collections and compact membership/non-membership evidence without a custom fold/tree algorithm.

### Atomic hierarchical candidate and MDBX namespaces

ADR-B-OCD-010 retains one immutable candidate and one finalization transaction. It extracts ADR-B-OCD-009's already validated/canonically encoded shard-set payload rather than duplicating its vector/map codec:

```text
ProvisionalShardSetBatch {
    shard_count,
    parent_shard_top_root,
    new_shard_top_root,
    parent_shard_roots,
    new_shard_roots,
    changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    encoded_size,
}

CollectionBatch {
    domain_id,
    parent_collection_root: Option<R_collection>,
    new_collection_root: R_collection,
    shard_set: ProvisionalShardSetBatch,
}

ProvisionalTreeBatch {
    block_number,
    parent_block_hash,
    parent_r_sealed,
    new_r_sealed,
    parent_catalog_root,
    new_catalog_root,
    changed_collections: BTreeMap<CollectionKey, CollectionBatch>,
    catalog_batch: ProvisionalCatalogBatch,
    encoded_size,
}
```

The current ADR-B-OCD-009 `ProvisionalShardBatch`, complete vectors, envelope validation, CKB ordering, Set/Delete discriminants, and canonical size visitor remain the single implementation. ADR-B-OCD-010 extends that visitor hierarchically for collection keys/metadata and the Catalog batch; it does not create a second encoded-size or checksum representation. Freeze adds the block hash once at the outer candidate and preserves every nested byte/ordering decision.

`CollectionKey` ordering follows the same pinned CKB key order used to build/prove the Root Catalog, never completion/touch order. `parent_collection_root = None` is valid only for a proven absent catalog leaf and first successful mint; its logical parent shard vector is all ZERO. The same exact-parent snapshot must also show no root/node/leaf records under any would-be collection shard namespace; orphan records behind Catalog non-membership are local corruption requiring rebuild, not an implicit parent collection. ADR-B-OCD-010 has no collection-delete/retirement batch. Every changed collection produces a non-zero new root and one catalog set/update. The catalog batch is prepared only after all collection roots are known and contains one aggregate CKB proof/update over the unique changed collection keys.

The final local MDBX materialization uses one typed namespace codec:

```text
TreeNamespace::Catalog
  = 0x00

TreeNamespace::CollectionShard(collection_key, shard_index)
  = 0x01 || collection_key_BE32 || shard_index_BE4

branch_record_key = tree_namespace || BranchKey_33
leaf_record_key   = tree_namespace || TreeKey_32
```

Unknown kind bytes, wrong lengths, non-canonical collection keys, shard indices outside the fork-fixed domain `K`, and namespace/key derivation mismatch are corruption. Catalog and collection shards share the same branch/leaf value codecs but cannot address each other's records.

One typed root table stores exactly one root per materialized namespace:

```text
CeTreeRootsV3:
  TreeNamespace::Catalog -> catalog_root
  TreeNamespace::CollectionShard(...) -> shard_root
```

ADR-B-OCD-010 advances `CE_MDBX_LOCAL_SCHEMA_VERSION` to 3; EVM `0xEE0D.storage_schema_version` remains 2. Genesis explicitly stores `Catalog -> ZERO`. A never-populated collection has no shard namespace records. First mint atomically creates exactly `K_domain` root entries, including explicit ZERO roots; after creation, missing or extra shard roots are corruption. Emptied-by-delete collections retain exactly `K_domain` explicit ZERO root entries.

`R_collection` is not duplicated in a separate metadata/root table: the Root Catalog leaf is its persisted representation, while recomputation from the exact shard-root vector authenticates it.

ADR-B-OCD-009's single `EnvironmentIdentity.shard_count` is replaced in V3 by the full canonical topology descriptor accepted for ADR-B-OCD-010:

```text
CeTopologyV1 {
    topology_version          = 1_u32,
    collection_key_scheme     = 1_u32,
    catalog_scheme            = 1_u32,
    domains ordered by domain_id: [
        { domain_id = 1_u16, policy = WwdBe4(1_u8),
          partition_len = 4_u32, shard_count = 16_u32 },
        { domain_id = 2_u16, policy = Singleton(0_u8),
          partition_len = 0_u32, shard_count = 16_u32 },
        { domain_id = 3_u16, policy = Singleton(0_u8),
          partition_len = 0_u32, shard_count = 16_u32 },
    ]
}
```

The environment identity stores this descriptor's canonical bytes directly and compares them byte-for-byte; it does not store only an opaque fingerprint. Counts/integers are big-endian, domains are strictly ascending/unique, policies accept only the registered discriminants and lengths, and trailing/unknown fields reject. `commitment_scheme_version`, `tree_format`, and vendor revision remain separate existing identity fields.

This is required because the empty Catalog/R_sealed root does not itself bind domain IDs, partition policies, or K. A topology mismatch requires rebuild/migration and can never open the existing environment through fallback. The cost is a variable-length local identity codec and explicit version evolution.

Finalized apply performs one MDBX write transaction:

1. verify marker, parent `R_sealed`, stored parent catalog root, and wrapper;
2. verify every changed collection's parent catalog membership/non-membership and exact shard-root vector;
3. apply changed collection shard records/root entries in ordered collection/shard order;
4. recompute every new `R_collection` and require the candidate values;
5. apply the aggregate Root Catalog branch/leaf changes;
6. write `TreeNamespace::Catalog -> new_catalog_root`;
7. require its wrapper equals candidate/EVM `new_r_sealed`;
8. write `last_applied` last and commit once.

No collection/shard/catalog progress becomes visible through this module independently. Candidate freeze/publication/idempotency, uncertain-commit recovery, retention advancement, and Marshal ACK retain ADR-B-OCD-008 and ADR-B-OCD-009's single-batch semantics.

A block with no effective collection changes still publishes the ordinary identity candidate:

```text
parent_catalog_root == new_catalog_root
parent_r_sealed     == new_r_sealed
changed_collections is empty
catalog_batch is empty
last_applied still advances to the finalized block
```

It performs no tree-record/root rewrite beyond the atomic marker progression.

This design makes multi-domain and first-collection mutations atomic and avoids a duplicate collection-root table. Its costs are longer variable-length MDBX keys, full vectors for each changed collection, `K_domain` root writes on first mint, sequential catalog preparation after collection results, and one final pre-activation local schema rebuild.

### Same-block collection state and exact-parent reads

ADR-B-OCD-010 adds no journaled `pending_collection` mapping or collection touched vector. Collection changes are derived deterministically at end block by grouping ADR-B-OCD-007's existing unique touched-entity final states by canonical `CollectionKey` in CKB order.

One `AuthenticatedCatalogView` owns one immutable MDBX read snapshot for the complete execution scope. It opens only the Root Catalog at begin-block; it never enumerates or constructs every Tribute/Nod collection. Catalog root/proofs, catalog leaves, collection shard-root vectors, and shard proofs consumed by that scope all come from that snapshot; evidence from separately advancing snapshots cannot be mixed.

On first access/touch of a collection, the view verifies its Catalog leaf. Only a present collection lazily loads exactly its domain's K root records and constructs/caches that collection's shard-set session over the shared snapshot. An absent collection opens no shard trees; first-mint sealing uses the logical all-ZERO vector after the accepted orphan-prefix check. Untouched collections consume no per-block tree/session allocation.

The execution scope may cache only proof-verified immutable parent evidence:

```text
verified_parent_collections:
  collection_key ->
      Absent
    | Present {
          R_collection,
          shard_roots: [B256; K_domain],
      }

verified_parent_leaves:
  (collection_key, tree_key) -> leaf | absent
```

The cache is scoped to one exact parent and never survives block execution. It is not rollback state: successful entries are immutable facts verified against parent `R_sealed`; no same-block mutation is written into it. ADR-B-OCD-007's EVM-journaled body/index overlay remains the sole same-block current-value source.

Point reads execute:

1. derive/validate domain, collection key, tree key, and shard from the requested typed EntityId36;
2. resolve ADR-B-OCD-007 overlay first (`Set` returns same-block body, `Deleted` returns absence);
3. for `Untouched`, verify collection membership/non-membership through the parent Root Catalog and wrapper;
4. absent parent collection returns entity absence without opening shard namespaces;
5. present collection requires exactly `K_domain` roots, recomputed `R_collection`, and a verified selected-shard leaf proof;
6. zero leaf is authenticated absence; non-zero leaf is the expected ADR-B-OCD-006 commitment for the exact-parent Mongo body.

List reads retain ADR-B-OCD-007's deterministic parent-page/index-delta merge and resolve each returned body through the same overlay-first point path. If the parent catalog proves a collection absent, same-block list results can contain only overlay-created members; contradictory parent IDs/body rows are projection corruption/unavailability, never authenticated presence.

For a parent-absent collection, multiple same-block mints group into one first-creation batch starting from the all-ZERO shard vector. Reads of those IDs see `Set`; another untouched ID is absent without parent shard access. If all new entities end as ZERO (for example mint then delete), no collection/catalog batch is produced, although ordered events, gas, and CE work retain their existing no-refund semantics.

For a parent-present collection, only effective final leaf changes produce a collection batch. Parent-equal final state produces none. Deleting its last leaf yields the explicit all-ZERO shard vector and retained `EmptiedByDelete` catalog leaf.

End-block independently verifies the complete touched-key parent proofs and aggregate changed-collection Root Catalog proof even when point evidence was cached. It never treats a read cache as the sealing proof or authority.

This avoids duplicated journaled collection state and reuses ADR-B-OCD-007 rollback/cleanup. The cost is collection grouping at seal, a new verified parent cache shape, and deferring explicit collection operations such as retirement to ADR-B-OCD-011.

### Empty genesis and coordinated reset

The final empty scheme-1 genesis has no compressed-entity collection leaves:

```text
catalog_root(0) = ZERO
R_sealed(0)     = P(TAG_SEALED_ROOT; 1, ZERO) != ZERO
```

EVM genesis allocates the marker-preserved final system account:

```text
0xEE0D:
  code   = 0xef
  slot 0 = storage_schema_version = 2
  slot 1 = R_sealed(0)
  slots 2..3 = reserved/zero
  slots 4..10 = empty ADR-B-OCD-007 overlay state
```

The matching CE MDBX V3 initialization transaction writes:

```text
CeTreeRootsV3:
  TreeNamespace::Catalog -> ZERO

collection shard roots:
  none

last_applied:
  commitment_scheme_version = 1
  height            = 0
  block_hash        = genesis_hash
  parent_block_hash = ZERO
  parent_root       = ZERO
  new_root          = R_sealed(0)
```

Environment identity binds chain ID, genesis hash, scheme 1, pinned CKB vendor revision, final tree/namespace codec, CES1 tags, the three domain IDs/partition policies, and active `K_PROVISIONAL = 16`. Startup independently derives the empty catalog/sealed roots and requires equality among the derivation, EVM slot 1, catalog root record, and marker before any execution/participation.

MongoDB starts empty and projection begins at the first executable block. Genesis contains no synthetic compressed-body events or pre-created Nod/Tribute collections. Block 1 opens the exact parent `(genesis_hash, R_sealed(0))`; first successful mint creates its collection atomically under the ordinary path.

After combined ADR-B-OCD-003 through ADR-B-OCD-010 verification, one coordinated reset discards all legacy body layouts, Mongo projections, direct maps, unsharded/intermediate sharded MDBX, candidates, and checkpoints. There is no migration, dual-read/write, lazy block-1 initialization, or compatibility fallback. Genesis tooling derives and embeds the testnet root using fork-fixed `K_PROVISIONAL = 16`.

This yields one authority from genesis and no bootstrap exception. The cost is making final topology/root derivation mandatory genesis tooling and intentionally invalidating every earlier local CE materialization.

### Consequences and trade-offs

Benefits:

- preserves ADR-B-OCD-006 through ADR-B-OCD-009's three typed namespaces and closed `EntityRef` model;
- equal EntityId36 values in NodItem and NodBucket cannot collide or prove each other;
- collection/proof/replay derivation needs no new entity-kind discriminant;
- Tribute WWD collections become independently addressable for later retirement;
- one Nod runtime can still update both singleton domains atomically in one block candidate.

Costs and limitations:

- Nod occupies two Root Catalog leaves and may update two collection roots in one business flow;
- `domain` now means authenticated body namespace rather than runtime/business module;
- there are more shard roots/catalog updates than a combined Nod collection;
- domain-to-runtime ownership must be explicit in documentation and typed wiring.

A single Nod collection is rejected because it requires adding and authenticating a new NodItem/NodBucket discriminant throughout tree-key, event, replay, proof, and snapshot derivation. Treating item/bucket as retireable partitions is rejected because their distinction is permanent type identity, not lifecycle partitioning.

## Remaining closure

No protocol-architecture choice remains open inside ADR-B-OCD-010. The completed ADR-B-OCD-009 seam review confirms reusable `ProvisionalShardBatch`, full root vectors, one canonical batch visitor, one immutable snapshot, sequential seal, BE4 namespaces, V2 atomic apply, and identity batches. ADR-B-OCD-010 therefore requires the hierarchical extraction/lazy collection composition above rather than a parallel implementation.

Before marking ADR-B-OCD-010 accepted, golden vectors and combined reset/genesis tests must confirm the formulas byte-for-byte with active `K_PROVISIONAL = 16`. Before ADR-B-OCD-010 integration, the ADR-B-OCD-009 active runtime constant should be named `K_PROVISIONAL` (or equivalently `ACTIVE_SHARD_COUNT`) rather than `K_TEST`; the comparison matrix remains benchmark-only.

ADR-B-CAP-001 later selects `K_PRODUCTION` on the complete system. If it differs, the pre-production/testnet chain and CE/Mongo materializations are reset/rebuilt; preserving state in place would require a new commitment scheme/migration.

Performance worker/cache/reader closure remains ADR-B-CAP-001 scope.

## Reset and version policy

ADR-B-OCD-010 completes the first deployed `commitment_scheme_version = 1` topology. ADR-B-OCD-008 and ADR-B-OCD-009 direct/unsharded/intermediate sharded materializations are not migrated. After combined verification, one coordinated reset initializes only the final empty collection/Root Catalog state and matching EVM/CE MDBX root.

## Next step

Prepare ADR-B-OCD-010 implementation against the reviewed ADR-B-OCD-009 seams, pin the `K_PROVISIONAL = 16` hierarchical/genesis vectors, and retain production K selection for ADR-B-CAP-001.

## Open questions and technical debt

- Reconcile the proposed three domains with the current active topology and reject
  unknown or duplicate domain/collection identifiers at startup.
- Prove catalog, collection, presence and per-shard roots with independent vectors.
- Define atomic same-block collection creation, entity mutation and retirement.
- Bind topology/version into genesis and headers so nodes cannot silently execute
  different catalogs.
- Add migration/reset and corruption tests for orphan namespaces, missing roots and
  partially persisted hierarchical batches.
