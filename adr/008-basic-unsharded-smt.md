# ADR-008: Replace direct commitment mappings with a vendored unsharded CKB SMT

- **Status:** Proposed
- **Date:** 2026-07-15
- **Depends on:** ADR-007

## Context

ADR-007 places Tribute, Nod item, and Nod bucket behind one compressed-entity lifecycle seam. It keeps three direct EVM commitment mappings as the current-state authority and records one reversible, journaled final body mutation set for deterministic end-block cleanup and later sealing.

Direct mappings authenticate point reads but do not produce one compressed-entity root or portable inclusion/non-inclusion evidence. ADR-008 replaces only that backend with one unsharded sparse Merkle tree while retaining ADR-006 bodies/events and ADR-007's domain interface, body/index overlay, transition semantics, rollback, and same-block reads.

ADR-008 adopts the ready CKB `sparse-merkle-tree` engine and finalized persistence mechanics selected by `compressed_entities_concept_v6_proposed_10-07-2026.md`. It does not design a new tree engine. The sequential ADRs remain authoritative where the older all-at-once concept differs: ADR-006 uses EntityId36 plus strict-canonical Protobuf; ADR-007 uses `0xEE0D`, domain-address mutation events, and body/index overlays; ADR-008 is deliberately one unsharded tree without the concept's domain registry, collection shards, Root Catalog, retirement, or header carrier. Those later capabilities remain separately staged.

ADR-008 does not add sharding, collection roots, partition retirement, a header artifact, public proof RPC, portable snapshots, or production capacity claims.

## Starting system

The starting system has:

- three logically separate direct commitment mappings at `0xEE0D`;
- zero as canonical absence and non-zero ADR-006 leaves as current values;
- a typed collection plus EntityId36 identifying each body;
- a journaled overlay with final Set/Delete state and a reversible body identity record;
- exact-parent MongoDB bodies verified against the current commitment;
- no single authenticated root or persistent tree materialization for all compressed entities.

## Added capability

One consensus-enforced unsharded CKB SMT commits every current Tribute, Nod item, and Nod bucket leaf under one EVM-authoritative root. A CE-owned MDBX retains the finalized in-place tree and atomic progress marker; immutable staged batches represent non-finalized candidates. Point reads and end-block sealing use the exact parent tree view and reproduce the same root on proposer and validator paths.

## Decision

### EVM root authority

The exact parent SMT root stored in ordinary journaled EVM state at `0xEE0D` is the sole consensus authority. Local leaves, internal nodes, paths, indexes, staged batches, and MDBX markers never become a second authority.

The local **Authenticated Tree Materialization** is bound to:

```text
(block_number, block_hash, smt_root, commitment_scheme_version)
```

A view for another height/hash/root, including an ahead view, is never accepted as fallback. Before proposing, validating, or serving proof data, the node requires the selected parent tree view to match the exact parent block and its EVM root. Evidence consumed through a local adapter is checked against that root; malformed or root-mismatched local data fails closed.

For every ADR-007 `Untouched` point or list-body identity, execution calls `read_leaf_verified(tree_key, parent_root)`: the exact-parent adapter returns the claimed leaf plus CKB membership/non-membership evidence, and `outbe-compressed-entities` verifies it before the leaf can influence domain execution. Zero is authenticated absence. A non-zero verified leaf remains the expected commitment for the exact-parent MongoDB body. ADR-007 `Set` and `Deleted` remain authoritative for same-block reads and avoid parent tree/body access.

A block-scoped cache may reuse already verified immutable parent evidence keyed by `(parent_root, tree_key)` across read-only and mutating calls. It contains no unverified result, does not survive the execution scope, and does not replace the end-block aggregate proof for the complete touched set. Thus read-only operations that affect execution are authenticated even though they never enter ADR-007's touched list.

No exact-parent view is `TreeUnavailable`: the local proposer forfeits or validator abstains under the same no-negative-vote principle as ADR-005. Malformed evidence or a root/marker mismatch is local tree corruption and triggers fail-closed recovery; it is not itself proof that the proposed block is invalid. With a correct parent view, proposer and validator deterministically recompute the same next root and normal block-validity rules apply.

### Vendored CKB engine

The private tree implementation is CKB `sparse-merkle-tree` release `v0.6.1`, vendored from immutable upstream commit `ad555350c866b2265d87d2d7fbd146fbc918bfe5` inside `outbe-compressed-entities`. The vendored directory retains its MIT license plus an `UPSTREAM.md` recording repository URL, tag, full commit, imported file set, and every local diff.

The vendored fork retains upstream key/path, update/delete, compact-zero, proof, MergeValue, and storage mechanics. Outbe does not rewrite tree traversal, topology, update, delete, or proof algorithms and does not expose a public tree-backend abstraction while only this implementation exists.

Poseidon is connected through an external `PoseidonCkbHasher` implementing CKB's existing transcript-style `Hasher` seam. It recognizes only the three upstream transcript shapes for base, normal, and merge-with-zero nodes and maps them to `TAG_SMT_BASE`, `TAG_SMT_NORMAL`, and `TAG_SMT_ZERO`; upstream `merge.rs` ordering/compact-zero mechanics remain unchanged. CE MDBX and speculative overlays implement the existing `StoreReadOps`/`StoreWriteOps` seams.

The local upstream diff is deliberately minimal: panic/unchecked failure paths reachable from runtime input or local corruption become structured errors, and provenance records every changed line. It does not create an Outbe-owned tree algorithm.

The vendored production subset is limited to upstream `h256.rs`, `merge.rs`, `tree.rs`, `merkle_proof.rs`, `traits.rs`, `error.rs`, and minimal crate glue. `default_store.rs` and upstream fixtures may be retained for conformance tests only. Blake2, C/`smtc`, `trie_tree`, WASM, CLI, benchmarks, and upstream build script are excluded.

The only allowed upstream-source edits are:

- replace the two `tree.rs` `expect`, one `unreachable`, one `debug_assert`, and one `assert_eq` with existing/new structured errors;
- replace the guarded `merkle_proof.rs` `unwrap`/`unreachable`/`debug_assert` sites with structured proof/stack errors;
- remove excluded feature/hash/build wiring from minimal crate glue.

H256 path semantics, MergeValue representation, merge/merge-with-zero, update/update_all, delete, proof opcodes, and upstream sort/dedup algorithms are not changed. `UPSTREAM.md` carries file checksums and an allowlisted diff; CI rejects drift outside that list, scans the vendored production subset for panic/unsafe constructs, and runs differential roots/proofs against pristine `v0.6.1`.

CKB's `Hasher::finish()` is infallible at the type level while Outbe's fixed-arity Poseidon API returns `Result`. `PoseidonCkbHasher` therefore uses `H256::MAX` as `HASH_ERROR`, a poison value strictly outside canonical BN254 field encodings. Unexpected transcript shape, non-canonical non-zero input, or Poseidon construction/hash failure returns poison without panic. MDBX/staging store adapters reject any branch/leaf containing poison before persistence, and the tree facade rejects a poison/non-canonical computed root as structured `TreeHashError`. No legitimate tree key, leaf, node, or root can equal the poison value.

The engine remains a 256-level H256 sparse tree. ADR-008 does not replace it with a custom 254-level implementation. The byte bridge is exact and has no byte reversal:

```text
ckb_key = CKB_H256::from(BE32(tree_key_f))
```

Because `tree_key_f` is a BN254 field element, its two highest numeric bits are zero, but all 32 bytes remain in CKB's 256-level path. CKB `get_bit(i)` reads bit `i % 8` from byte `i / 8`, `parent_path` uses the same representation, and CKB `Ord` compares bytes in its upstream reversed-byte order. Sealing, proof vectors, staged-map ordering, and future shard selection use those exact CKB semantics, not an independently assumed big-endian numeric bit order.

The ADR-006 non-zero body commitment is stored verbatim as the CKB leaf value:

```text
present -> SMT.update(tree_key, leaf_value)
delete  -> SMT.update(tree_key, ZERO)
```

There is no additional SMT leaf-wrapper hash. CKB deletion removes the leaf and newly empty branches according to its compact-zero semantics.

### Tree key

All three logical collections share this stage's one tree, so the key binds the fork-fixed typed collection outside EntityId36 and the ADR-006 leaf:

```text
TAG_KEY = CES1_TAG_BASE + 5

tree_key_f = P(
  TAG_KEY;
  commitment_scheme_version,
  collection_id,
  identity_f
)

identity_f = PBytes(TAG_ID, EntityId36)
commitment_scheme_version = 1
collection_id = 1 Tribute | 2 NodItem | 3 NodBucket
```

Collection is included because equal EntityId36/`identity_f` values in different typed collections must occupy different positions. Collection is selected by ADR-007's closed typed interface and is never caller-controlled.

`tree_key_f` is a canonical Poseidon-BN254 output, never arbitrary bytes reduced modulo the field. Key zero is a valid position; zero is reserved only as the absent leaf value. This exact key recipe is normative for the unsharded reference stage. ADR-010 may supersede fixed `collection_id` with its final collection-key derivation before first activation without consuming a version. After scheme 1 is deployed, changing its tag/input order, collection derivation, H256 encoding, or CKB path semantics is a commitment-scheme change.

ADR-006 through ADR-010 have no intermediate testnet activation. This unsharded tree is therefore a reference/benchmark milestone using the final CES1 hash/leaf primitives, not a separately deployed commitment scheme. ADR-009 and ADR-010 may change topology without consuming versions before launch. The first deployed complete collection/Root Catalog construction after ADR-010 is `commitment_scheme_version = 1`; `tree_format/vendor_revision` remains local MDBX metadata. Only a semantic change after that activation increments the network scheme version.

### Outbe CKB MergeValue codec

Outbe supplies the CKB engine with a typed Poseidon-BN254 MergeValue codec using the immutable CES1 tags:

```text
TAG_SMT_BASE   = CES1_TAG_BASE + 8
TAG_SMT_NORMAL = CES1_TAG_BASE + 9
TAG_SMT_ZERO   = CES1_TAG_BASE + 10
```

The normative forms are:

```text
merge(ZERO, ZERO) = ZERO

base_node = P(
  TAG_SMT_BASE;
  base_height,
  base_key_f,
  base_value_f
)

normal_node = P(
  TAG_SMT_NORMAL;
  height,
  node_key_f,
  left_hash_f,
  right_hash_f
)

merge_with_zero = P(
  TAG_SMT_ZERO;
  base_node_f,
  zero_bits_f,
  zero_count
)
```

The field names and values are those produced by the pinned CKB MergeValue algorithm; adapters do not reinterpret or omit them. Every non-zero H256 consumed or produced by the codec must be a canonical BN254 field encoding. Tree keys, their parent paths, and compact `zero_bits` therefore remain canonical field values.

`zero_count` preserves upstream wire behavior exactly: it is `u8`, and increments wrap modulo 256. A complete 256-level compact-zero path therefore encodes `zero_count = 0`, not 256.

Structural emptiness is CKB `ZERO`: the empty unsharded tree root is zero. If any Poseidon result computed for non-empty content evaluates to zero, sealing fails deterministically. Body leaf zero is already rejected by ADR-006.

The vendored implementation is differentially tested against an independent, simple reference model and fixed vectors for every CKB merge form, including the `zero_count` 255→0 boundary.

### `0xEE0D` storage schema v2

ADR-008 replaces temporary schema v1 on the development branch with the root-backed layout below, but does not activate it on testnet before ADR-009/010:

```text
slot 0   storage_schema_version = 2
slot 1   last_smt_root
slot 2   reserved  // former Nod direct-map base
slot 3   reserved  // former NodBucket direct-map base
slot 4   pending_word
slot 5   pending_body
slot 6   touched
slot 7   index_delta_word
slot 8   index_delta_record
slot 9   touched_index_deltas
slot 10  body_identity_record
```

The former Tribute direct-map base at slot 1 becomes the root slot. Former mapping-base slots 2–3 remain empty/reserved in v2 rather than shifting the permanent ADR-007 overlay or prematurely assigning future topology state. No direct-map entry is migrated, read, or retained through fallback; the complete reset guarantees their derived storage keys do not exist in the new chain.

During execution of block `B`, slot 1 remains `R(B-1)` until compressed-entity end-block sealing. Same-block current values come from ADR-007's overlay over the exact parent SMT. Successful seal writes `R(B)` in the same buffered post-block change set that clears overlay storage.

The empty unsharded CKB reference root is ZERO. ADR-008 harness/replay initialization uses a matching height-0 marker, but this state is not a network genesis. The first deployed empty root is ADR-010's derived scheme-1 `R_sealed` over the final shard/collection/catalog topology and may be non-zero; final slot 1 and CE marker must match that derivation exactly. The root is never an independent operator value.

### Journaled execution and staged sealing

The persistent CKB tree is not mutated transaction by transaction and is not part of REVM checkpoint/rollback. ADR-007's journaled EVM overlay remains the only block mutation accumulator. Ordered operation history remains in canonical receipt events; sealing consumes one final value per touched key.

ADR-008 makes the lifecycle output type explicit across the workspace:

```rust
pub trait BlockLifecycle {
    type EndBlockResult;

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()>;
    fn end_block(ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult>;
}
```

Every ordinary lifecycle implementation uses `EndBlockResult = ()`. `CompressedEntitiesLifecycle` uses `EndBlockResult = SealOutput`. The executor calls the concrete trait implementation and retains that typed value directly; no global registry, type erasure, implicit context, or mutable result slot is introduced. This trait/rule change is implemented atomically across existing lifecycle implementations, README, `.ruler`, and generated agent rules.

`CompressedEntitiesLifecycle::end_block`:

1. reads and validates every unique ADR-007 body identity record and final pending word;
2. maps Set to its non-zero leaf and Deleted to ZERO;
3. derives every `tree_key`; two distinct identity records producing the same key are fatal `TreeKeyCollision`, never CKB last-write-wins;
4. if the touched set is empty, verifies the exact parent view/root, skips CKB `merkle_proof` and `update_all`, sets `R_next = R_parent`, and proceeds to empty-overlay post-conditions/identity batch;
5. otherwise sorts unique keys by the pinned CKB `H256::Ord` and generates/verifies one parent multi-proof for all corresponding parent leaves against the exact parent root;
6. removes final entries whose `parent_leaf == final_leaf` and passes only effective changes to CKB `update_all`;
7. prepares one staged batch against the exact parent view without persistent MDBX side effects;
8. computes `R_next` and requires a canonical result under the MergeValue codec;
9. writes the root through the scoped `StorageHandle`;
10. clears body/index overlay records and touched lists regardless of effective-change count;
11. flushes the complete root/cleanup change set before notifying Reth's state-root task;
12. returns a typed local result:

```text
SealOutput {
  parent_root,
  new_root,
  staged_tree_batch
}
```

Any tree, parent-proof, root, cleanup, state-flush, or executor-finish error aborts construction/validation and drops the staged batch. A parent multi-proof mismatch is local materialization corruption, not a negative vote. A partially sealed block is never produced or accepted.

Net no-op examples include parent `A -> B -> A`, delete→mint of identical `A`, absent mint→delete, and same-leaf update. Their successful canonical events remain ordered receipts; only the redundant final tree write is removed. There is no EVM gas refund or CE-unit release because body hashing/events, authenticated parent evidence, overlay work, and cleanup were still performed/reserved.

A block with no effective tree changes still produces an immutable identity batch carrying its block/parent hashes and equal parent/new roots with empty branch/leaf changes. Finalized persistence advances `last_applied` for that block atomically even though no tree node changes; exact block-parent chaining and Marshal ACK therefore never skip zero-change blocks.

After successful executor finish and block sealing, the executor assigns the block hash and publishes an immutable staged batch keyed by that hash. Multiple competing candidates may coexist while awaiting finality; losing candidates are discarded without touching persistent MDBX.

### Exact finalized-parent view and speculative candidate batches

ADR-005's testnet execution profile permits block execution only over an exact finalized parent whose Mongo projection is ready. ADR-008 adds the matching tree prerequisite: that same parent must already be committed in CE MDBX with equal height/hash/root. A non-finalized staged batch is never an execution parent in this stage.

Every successfully executed but non-finalized candidate is represented by:

```text
StagedTreeBatch {
  block_number,
  block_hash,
  parent_block_hash,
  parent_root,
  new_root,
  branch_changes: BTreeMap<BranchKey, Set(BranchNode) | Delete>,
  leaf_changes:   BTreeMap<TreeKey, Set(LeafValue) | Delete>,
  encoded_size,
}
```

A block-scoped `AuthenticatedTreeView` owns one consistent CE MDBX read snapshot for the exact finalized parent. Opening it verifies marker identity, commitment scheme, exact parent number/hash, equality with the parent EVM slot, and the CKB root. Another candidate, height, hash, or root is never fallback.

Sealing wraps that snapshot in a local `StagingCkbStore` implementing `StoreReadOps + StoreWriteOps`. Reads resolve its own candidate writes before the immutable finalized base; CKB changes accumulate only in deterministic ordered branch/leaf maps. It never mutates another candidate or opens an MDBX write transaction, and a complete tree is never copied.

`CompressedEntitiesLifecycle::end_block` returns provisional `SealOutput`. Only after successful executor finish and block sealing supplies the block hash may the executor freeze/publish the candidate batch. Failed execution/sealing drops it. Re-publication for the same block hash is idempotent only when typed metadata and both ordered maps are structurally equal; a conflicting batch is local corruption. No consensus meaning depends on incidental memory/MDBX serialization.

Process restart discards every non-finalized candidate batch and retains only the root-verified finalized MDBX marker. A candidate returns only through verified Reth/Marshal redelivery and deterministic reexecution against that finalized parent. Until then this node forfeits/abstains; it never reconstructs from unspecified local candidate retention.

After the winning finalized batch commits atomically to CE MDBX, its cache entry and all losing competitors are removed. Already-open candidate views retain their original MDBX snapshot and immutable local maps until dropped.

Speculative cache limits cover candidate count and encoded branch/leaf bytes. Numerical values come from benchmark evidence. A candidate required for pending finalized commit is never silently LRU/wall-clock evicted; capacity pressure produces local `TreeCacheCapacity` and forfeiture/abstention or verified reexecution, never a different root.

`AuthenticatedTreeView`/`StagingCkbStore` are private implementation types, not a public generic backend seam, and are injected through execution wiring without process globals or implicit persistent-state context. Executing descendants of a certified/non-finalized candidate would also require candidate full-body/index materialization and is explicitly deferred to a separate future ADR.

### CE-owned MDBX

ADR-008 includes durable finalized SMT storage from its first implementation. It does not defer baseline persistence to ADR-014.

The CKB store adapter owns a separate MDBX environment:

```text
<datadir>/compressed_entities/smt/
```

It never shares Reth's primary MDBX environment. One node owns one CE environment and one active writer. The environment owns its schema, map-size/capacity checks, reader snapshots, writer lifecycle, and identity metadata binding at least:

```text
local_storage_schema_version = 1
chain_id
genesis_hash
commitment_scheme_version
tree_format/vendor_revision
```

Local codec v1 is deterministic and fail-closed, although it is rebuildable local format rather than a consensus wire format:

```text
branch key   = height_u8 || ckb_node_key_32
branch value = merge_value(left) || merge_value(right)

merge_value(Value) = 0x00 || value_32
merge_value(MergeWithZero) =
  0x01 || base_node_32 || zero_bits_32 || zero_count_u8

leaf key     = ckb_tree_key_32
leaf value   = leaf_value_32

last_applied =
  commitment_scheme_version_u32_be
  || height_u64_be
  || block_hash_32
  || parent_block_hash_32
  || parent_root_32
  || new_root_32
```

Unknown tags/lengths, trie-only variants, non-canonical non-zero fields, poison, and trailing bytes are corruption. MDBX deletes remove records; Delete tombstones exist only in staged change maps. A local codec/schema change requires rebuild or explicit local migration but does not change network roots. Staged `encoded_size` uses this same canonical record sizing, while publication idempotency remains typed structural equality.

For finalized block `B`, one atomic CE MDBX transaction writes all changed CKB nodes plus:

```text
last_applied = {
  commitment_scheme_version,
  height: B,
  block_hash,
  parent_block_hash,
  parent_root,
  new_root
}
```

The transaction requires contiguous height and exact parent hash/root/scheme. Reapplying the same complete marker is an idempotent no-op; a conflicting marker is corruption.

### Finalized commit ordering and ACK

The persistent in-place tree advances only after all of the following hold for block `B`:

1. Commonware Marshal durably synced the block and finalization certificate;
2. Reth accepted execution and finalized forkchoice;
3. Reth durably persisted the canonical block, receipts, and EVM state through `B`;
4. a DB-only provider verifies exact height/hash and the EVM SMT root equals the staged batch metadata;
5. the CE MDBX atomically commits changed nodes and `last_applied`;
6. only then is `B` acknowledged to Marshal.

Required invariant:

```text
persistent_tree_height <= min(durable_evm_height, consensus_finalized_height)
```

Pinned Reth v2.2.0 commit `88505c7fcbfdebfd3b56d88c86b62e950043c6c4` exposes `PersistedBlockSubscriptions` after the provider persistence commit. Compressed-storage startup therefore requires the settings that force a real per-block persistence barrier (`persistence_threshold = 0`, `memory_block_buffer_target = 0`) and `MAX_PENDING_ACKS = 1` for finalized delivery. Incompatible configuration fails startup. MongoDB/ExEx projection remains asynchronous and outside this ACK-critical path.

### CE recovery-retention cursor

CE behind-recovery requires the contiguous canonical receipts/events and historical EVM state needed to replay mutations and verify each committed `0xEE0D` root. Mongo ExEx `FinishedHeight` may advance independently, so it cannot be the sole Reth pruning fence.

The CE coordinator publishes:

```text
CeRetentionHeight = CE_MDBX.last_applied.height
```

Startup seeds it only from the root-verified marker. It advances only after the atomic CE nodes+marker commit has a known successful outcome. A commit error/unknown outcome withholds ACK and cursor advancement until restart reopens MDBX and classifies the actual marker.

Reth receipt/event and required historical-state pruning horizon is bounded by the minimum of CE retention, Mongo ExEx, and every other retention client. CE retention is not an ExEx tree writer, does not make Mongo synchronous, and does not alter finality; it only preserves local canonical recovery inputs while CE is behind. If pinned Reth integration cannot register a dynamic CE retention client, compressed-storage startup must disable the relevant receipt/state pruning entirely as the safe fallback.

### Baseline restart contract

At startup the node compares the CE marker with durable finalized Reth state and consensus finality:

```text
CE marker == durable finalized EVM checkpoint
  -> verify height/hash/root/scheme and resume;
     equal redelivery after MDBX commit but before ACK is idempotent

CE marker behind durable finalized EVM checkpoint
  -> replay every missing finalized block contiguously from durable canonical
     receipts/events, recompute each expected root, commit, then resume

CE marker ahead of durable EVM state or consensus finality
  -> invariant violation; fail closed and rebuild/resync

same height with different hash/root/scheme
  -> corruption; fail closed and rebuild/resync

next marker with wrong parent hash/root or a gap
  -> stop incremental apply and recover from a verified checkpoint
```

MDBX may be deleted and rebuilt from canonical history while the necessary history exists. It is an authenticated acceleration/persistence structure; only the EVM root and finalized chain select the accepted state.

### EVM gas and CE work-unit budget

ADR-008 preserves ADR-007's decision not to add a separate mutation-count cap and separates two accounting dimensions.

Ordinary EVM transaction gas covers synchronous call work: body hashing/encoding, event bytes, overlay/index storage, point/list reads, and ADR-007's first-touch cleanup/seal surcharge. It is charged by the transaction, appears in receipts/cumulative header `gas_used`, follows normal revert/OOG rules, and is pinned to the active EVM schedule.

Deferred/local liveness is bounded independently by one deterministic executor-local meter spanning user and receipt-visible system lanes:

```text
CE_WORK_USED <= CE_WORK_LIMIT
```

CE work units are non-persistent, absent from receipts/header gas, and form a second payload-admission/validity dimension. They never reduce or masquerade as the EVM block gas limit. Proposer and validator recompute them over the same path, so a high internal system-transaction gas lane cannot bypass the CE bound.

Before transactions every block reserves `CE_SEAL_BASE_UNITS` for exact-parent view checks, lifecycle/root notification, empty identity batch, and finalized marker work. First touch of each unique body key reserves tree-shape-independent `CE_UNIQUE_KEY_UNITS`, covering worst-case parent multi-proof, CKB update_all, staged branch/leaf bytes, finalized MDBX work, and cleanup. Repeated mutations of that key do not duplicate this reservation. Net no-op receives neither EVM gas refund nor CE-unit release.

CE units are reserved before first-touch writes/event. Once work from an included transaction starts, its units remain consumed even if that transaction later reverts; the meter's seen-key reservation likewise prevents a later included attempt from under-reserving the same block key. If the payload builder excludes the whole speculative transaction, it restores the CE meter/seen-key checkpoint together with execution state.

A user transaction that fits an empty payload's CE budget but exceeds the remaining CE units is rolled back by the proposer and deferred without pool eviction; a validator encountering the overflow in a proposed payload rejects the block. A transaction that cannot fit the full CE work limit fails `TransactionCeWorkLimitExceeded`. Receipt-visible system bulk work uses a deterministic domain progress cursor and continues in a later block.

No provisional `50_000` charge or `600` attempt cap is activated. Before deployment, a reproducible benchmark on minimum validator hardware separately fixes EVM gas coefficients and `CE_SEAL_BASE_UNITS`, `CE_UNIQUE_KEY_UNITS`, `CE_WORK_LIMIT`, plus local speculative-cache count/branch/byte limits. The saturated workload uses cold MDBX, maximum bodies, new minimally-overlapping keys, maximum branch writes/deletes, multi-proof generation, state-root notification, finalized MDBX commit, concurrent proof reads, and worst-case future single-shard concentration. The default acceptance target is a gas/work-unit-saturated full block under two seconds with documented safety margin. Golden tests pin transaction gas, work units, payload-builder checkpoints, and proposer/validator equality.

## Alternatives considered

### Fuel Merkle without a fork

Rejected because its sparse tree hard-codes SHA-256 key hashing, value hashing, leaf wrapping, and internal-node hashing. Using it unchanged would replace the selected Poseidon-BN254 commitment/root/proof format; adapting it to Poseidon would require a larger fork than CKB's existing custom Hasher/Store seams.

### New Outbe SMT implementation

Rejected because CKB already supplies the required sparse traversal, compact-zero, batch update/delete, proof, and storage mechanics. Reimplementing them would expand the consensus-critical audit surface without adding protocol capability.

## Error and failure semantics

### Deterministic operation outcomes

ADR-007 lifecycle/business reverts remain ordinary receipt outcomes and leave no generic mutation/event state. ADR-008 adds:

- `TransactionCeWorkLimitExceeded` when one transaction cannot fit the full CE work-unit budget;
- proposer-local `BlockCeWorkCapacityExhausted` when an otherwise admissible transaction does not fit the remaining work units and must be deferred;
- block invalidity when a proposed payload exceeds the deterministic CE work-unit budget.

These outcomes are not tree corruption and do not alter EVM receipt gas semantics. Included reverted work remains counted under the accepted CE meter rules.

### Local readiness

The following prevent only this node from executing the exact parent:

- `TreeUnavailable`: the required exact finalized MDBX snapshot is missing or an MDBX read is technically unavailable;
- `TreeCacheCapacity`: a required live view cannot fit within benchmark-fixed local cache bounds;
- `TreeRebuildRequired`: local materialization is behind and the needed chain must be reconstructed.

The node forfeits proposal or validator abstains; it never turns local readiness into a negative vote. A full node/canonical importer instead pauses import, obtains/reexecutes the exact parent, and gracefully shuts down if recovery is unrecoverable; it never labels the block invalid because of local tree state. Recovery must be explicit rebuild/redelivery/replay or shutdown, never fallback to direct maps, another height, or an unrelated branch.

Mongo projection readiness and tree readiness are evaluated as one execution prerequisite against the same required parent and remaining request/view budget, preferably concurrently. They do not consume two sequential timeout windows. Mongo retains ADR-005's supervisor/recovery deadline; a longer tree rebuild leaves the node non-participating and cannot accidentally inherit or extend that Mongo timeout.

### Local corruption/protocol-fatal defects

The following fail closed, discard provisional output, and require rebuild/resync or operator investigation:

- EVM root, CE marker, finalized-parent/candidate block hash, parent root, or scheme mismatch;
- missing/malformed branch or leaf, invalid CKB primitive/MergeValue/proof program, or parent multi-proof mismatch;
- non-canonical BN254 non-zero value or `HASH_ERROR` poison at any store/root seam;
- conflicting staged batch for one block hash or conflicting idempotent MDBX marker;
- duplicate/non-unique touched identity/key input or cross-identity `TreeKeyCollision`;
- dirty schema/overlay state, failed overlay post-condition, or mutation/read after seal;
- CE tree ahead of durable EVM/finality, same-height hash/root conflict, or replay gap;
- vendored panic-safety/provenance check failure.

`TreeKeyCollision` is treated as a commitment-scheme failure rather than last-write-wins or a user-selected alias. Rebuild cannot repair a real cryptographic collision.

A CKB/Poseidon/root error on the proposer aborts payload construction. With a correct exact-parent view, validator recomputation that differs from the proposed EVM/state root is normal deterministic invalid-block handling. If the local parent evidence itself cannot be authenticated, the validator abstains instead of declaring the block false.

### Finalized persistence failure

A CE MDBX commit/fsync error has an uncertain durable outcome: the node does not assume `last_applied` stayed behind or advanced. It withholds Marshal ACK and fails fast through graceful whole-node shutdown rather than silently stalling or continuing validation. Restart reopens MDBX and classifies the actual marker as equal, behind, or conflicting before any ACK/participation. An ahead/conflicting marker never receives in-place repair.

Errors and logs include block number/hash, parent/new root, marker height/hash, module and error class where available, but no body bytes or secret material.

## Working result

After implementation:

- Tribute, Nod item, and Nod bucket share one unsharded Poseidon-BN254 CKB SMT;
- three direct commitment mappings no longer exist or participate in reads;
- same-block body/index behavior remains ADR-007 overlay-first;
- proposer and validator authenticate the exact parent tree, compact final key mutations, and compute equal roots, EVM gas, CE work units, and state roots;
- slot 1 at `0xEE0D` is the sole consensus root authority;
- non-finalized competing candidates use immutable, discardable batches without becoming execution parents or writing MDBX;
- finalized CKB nodes and complete marker commit atomically only behind durable Reth/finality and before Marshal ACK;
- clean restart resumes at an exact root-verified marker, while bounded behind state replays canonical mutations;
- CKB proofs exist internally for parent authentication and future RPC work without exposing a public proof contract yet.

## Accepted limitations

- One unsharded tree is the worst-case working-set/throughput stage; no shard/collection isolation or Root Catalog exists.
- Tribute WWD partitions cannot be retired in one tree operation.
- The root is committed only through EVM state; there is no header artifact or external proof/body RPC yet.
- Only latest available finalized tree materialization is retained; historical proof generation is not promised.
- CE MDBX can be rebuilt only while canonical replay inputs or a later snapshot remain available.
- Portable snapshots, peer recovery, authenticated secondary-index completeness, and production availability guarantees remain outside ADR-008.
- The coordinated reset has no compressed genesis entities. A future non-empty CE genesis requires its own exact import codec/vectors rather than an unspecified path in this ADR.
- Numerical gas/work-unit/cache capacity remains a mandatory benchmark closure and is not guessed by the architecture document.

## Consequences

### Positive

- One EVM word authenticates current membership/absence for all three fixed collections.
- The selected, established CKB mechanics avoid an Outbe-owned SMT algorithm while retaining Poseidon commitments.
- Existing CKB storage/proof seams support MDBX, candidate staging, differential tests, and later sharding.
- Immutable candidate batches isolate forks and ordinary EVM journaling continues to own transaction rollback.
- Atomic marker ordering makes persistent tree lag recoverable and forbids tree-ahead-of-chain state.
- The ADR-007 lifecycle/domain interface remains unchanged when the backend moves from direct maps to SMT.

### Negative

- Exact finalized CE MDBX and Mongo readiness become local proposal/validation prerequisites.
- Finalized tree persistence enters the Marshal ACK critical path and requires strict Reth durability settings.
- The node now operates and reconciles Reth, CE MDBX, candidate cache, CE pruning retention, and asynchronous MongoDB cursors.
- Vendored cryptographic tree code and its allowlisted safety diff require permanent provenance/differential maintenance.
- Parent multi-proof and end-block batch sealing add consensus execution latency and two-dimensional accounting complexity.
- Every tree-format/hash/vendor-semantic change requires explicit fork/reset or later migration design.

## Additional alternatives considered

### Store all SMT nodes in EVM state

Rejected because each unique mutation would perform a large number of EVM node reads/writes, defeating the compressed-state objective. The EVM root is sufficient authority; local nodes are authenticated materialization.

### Persist speculative branches in CE MDBX

Rejected because losing forks would require database rollback/version retention and could make local persistent state appear ahead of finality. Immutable staged batches keep speculative state discardable.

### Keep direct mappings as fallback/cross-check

Rejected because it preserves two authorities, doubles writes, and weakens the exact cutover. Combined reset starts only schema v2.

### Defer MDBX persistence to a later ADR

Rejected because the selected CKB store and finalized proof/readiness path need one atomic node/marker owner from the first SMT stage. The former ADR-014 persistence stage is folded into ADR-008.

## Verification

### Vendor and codec conformance

- Verify upstream tag/commit, file checksums, MIT attribution, and allowlisted local diff.
- Scan the imported production subset for panic, unsafe, excluded feature, and unreviewed source drift.
- Run pristine-vs-vendored differential update/delete/proof tests under an identical reference hasher.
- Pin transcript classification and Poseidon results for base/normal/merge-with-zero forms, canonical-field rejection, poison propagation, and `zero_count` 255→0 wrapping.
- Pin CKB H256 order/path, parent paths, proof opcodes, empty root, singleton, delete-to-empty, and BE32 keys whose high bits are zero.

### Tree/reference behavior

- Compare ordered map reference state with CKB roots for empty, mint, update, same-leaf, delete, reinsert, random sequences, and multiple-key `update_all`.
- Cover key-order independence, strict unique-input enforcement, cross-identity collision failure, parent-leaf multi-proof, tampered proof, and every net no-op sequence.
- Differentially verify membership/non-membership and corrupted store/proof programs.

### Journal/lifecycle integration

- Cover nested revert, failed transaction, out of gas, static/foreign-context rejection, first-touch rollback, repeated same-key operations, domain rejection, overlay cleanup, and no operation after seal.
- Assert slot 1 remains parent root during transactions, end-block writes exactly the next root, slots 2–3 stay empty, and slots 4–10 finish empty.
- Inject failure before/after parent proof, CKB staging write, EVM root write, cleanup, flush, state-root notification, executor finish, and batch publication; no partial root/batch survives.

### Speculative/finality behavior

- Execute competing candidates over one exact finalized parent; cover winning commit, losing-candidate drop, restart discard/redelivery-reexecution, capacity pressure, structurally identical publication, and conflicting publication.
- Assert one MDBX snapshot per opened candidate view, no candidate is used as a descendant parent, and no speculative write reaches MDBX.
- Inject crashes/failures before/after durable Reth barrier, CE transaction/uncertain commit, marker, retention-cursor advance, cache removal, and Marshal ACK; cover equal, behind, ahead, conflicting, and gap restart rows.
- Prove Reth cannot prune required receipts/events/historical root state above `CeRetentionHeight`; test dynamic-client mode and mandatory no-pruning fallback.
- Verify no-change blocks advance identity batch/marker/ACK without node writes.

### Determinism and performance

- Run proposer/validator and cross-architecture equal-root/equal-EVM-gas/equal-CE-work-unit/equal-state-root tests from independent CE MDBX materializations.
- Run full canonical replay and compare every intermediate EVM root with the CE marker.
- Fuzz mutation batches, CKB codecs/proofs, candidate-store lookup, marker decoding, and malformed/corrupt MDBX records.
- Produce the required cold/warm reproducible benchmark and golden EVM-gas/CE-work/cache limits before activation; sustain gas/work-unit-saturated multi-node testnet load under the accepted timing bound.

## Reset policy

ADR-008 is not separately deployed and performs no testnet reset. It is the unsharded reference/benchmark implementation on the ADR-005–010 branch; ADR-009/010 replace its root topology before the first CES1 network starts. There is no live migration, dual-write/read, direct-map fallback, or intermediate binary deployment.

After ADR-010 and the combined suite, one coordinated reset allocates final marker-preserved `0xEE0D`. Unless ADR-009/010 require an explicitly documented EVM layout change, root-backed schema version 2 remains: slot 1 carries the final derived empty `R_sealed`, slots 2–3 are reserved, and slots 4–10 are empty overlay storage. CE MDBX initializes the final scheme-1 sharded collection/Root Catalog marker at height 0; no unsharded MDBX state is imported. MongoDB begins empty under the same reset.

Final startup validates CE environment chain/genesis/vendor/scheme identity, exact height-0 marker/root, required Reth durability configuration, CE retention/no-pruning mode, and finalized-delivery ACK bound before consensus participation. Temporary direct-map/unsharded development state and any previous local CE MDBX directory are discarded rather than interpreted.

## Next unlocked step

ADR-009 benchmarks and introduces fixed power-of-two sharding over the same pinned CKB/Poseidon/store interfaces without an intermediate deployment or version bump. It preserves ADR-006 leaf bytes/formula, ADR-007 lifecycle/overlay semantics, exact finalized persistence ordering, and correct worst-case behavior when all mutations concentrate in one shard; ADR-010 then completes the first activated scheme-1 topology.
