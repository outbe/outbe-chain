# ADR-009: Add provisional fixed sharding to the CKB SMT

- **Status:** Superseded; historical input only
- **Canonical mapping:** [`docs/adr/legacy-reconciliation.md`](../docs/adr/legacy-reconciliation.md)
- **Date:** 2026-07-16
- **Depends on:** ADR-008

## Context

ADR-008 implements the exact-parent authenticated CKB SMT, immutable candidate batches, CE-owned MDBX persistence, finalized-marker ordering, and the EVM-authoritative root seam. It deliberately remains an unsharded pre-activation reference stage. Its production constructors still use prebenchmark CE work/cache bounds, and its Criterion harness is a microbenchmark rather than activation evidence.

ADR-009 evaluates and introduces fixed power-of-two sharding behind the existing `AuthenticatedParentTree`/`CompressedTreeService` interface. It does not change ADR-006 body/leaf bytes, ADR-007 lifecycle or overlays, exact finalized-parent requirements, EVM root authority, candidate isolation, finalization ordering, or the rule that local tree failure never becomes a negative consensus vote.

ADR-009 is not deployed independently. ADR-010 places the same shard-set mechanism under collections and the Root Catalog using provisional pre-production `K = 16`. ADR-017 later benchmarks the complete ADR-001–016 plus co-located off-chain-computation system and selects the production shard count. Neither the unsharded reference topology nor the ADR-009 intermediate topology consumes a `commitment_scheme_version`.

## Starting system

The implemented ADR-008 system has:

- one 256-level Poseidon-BN254 CKB SMT containing all three typed entity namespaces;
- exact-parent authenticated reads and aggregate touched-key sealing;
- one `ProvisionalTreeBatch`/`StagedTreeBatch` containing ordered branch and leaf changes;
- one finalized MDBX branch/leaf namespace and atomic `last_applied` marker;
- candidate-cache and CE-work interfaces whose production numerical limits remain prebenchmark;
- an `adr008` Criterion harness covering 256-key in-memory updates/proofs and synthetic staged MDBX apply.

## Added capability

A private fixed-layout **Shard Set** groups tree mutations into independent CKB SMT shards, prepares their updates without persistent side effects, and derives one deterministic shard-set root. The existing lifecycle and domain callers continue to submit typed entity mutations and receive one sealed root plus one immutable candidate batch.

## Decisions fixed in this pass

### Provisional shard count

ADR-009 through ADR-016 use:

```text
K_PROVISIONAL = 16 shards
k_PROVISIONAL = log2(K_PROVISIONAL) = 4
```

The implementation keeps `K` explicit/parameterized, but the active value is fork-fixed rather than a CLI, environment, Mongo/MDBX-local, or operator setting. All nodes, replay/proof tools, and genesis derivation for the same chain use the same value.

`K` must be a power of two (`K = 2^k`), not merely an even number. Sixteen exercises a four-level top tree and is sufficient for correctness, testnet integration, ADR-010 collections, and later protocol work without pretending to be a production performance conclusion.

ADR-017 compares power-of-two candidates on the completed system and selects `K_PRODUCTION`. If it differs from 16 before production, a complete pre-production/testnet reset rebuilds genesis, EVM `R_sealed`, proofs, and CE MDBX under the final value. Reusing existing state in place instead would be a commitment-topology change requiring a new scheme/migration decision.

### Shard selection

For `K = 2^k`, the shard is selected from the low `k` **numeric** bits of the canonical BN254 `tree_key_f`. The implementation expresses this only through pinned CKB `H256::get_bit` semantics over the existing direct `BE32` bridge:

```text
ckb_key = CKB_H256::from(BE32(tree_key_f))

for j in 0..k:
    ckb_bit_index(j) = 8 * (31 - floor(j / 8)) + (j mod 8)
    shard_bit(j)     = ckb_key.get_bit(ckb_bit_index(j))

shard_index = sum(shard_bit(j) << j, j = 0..k-1)
```

For `K_PROVISIONAL = 16`, `k = 4` and selection reads CKB bit indices `248..251`. `K = 1` has `k = 0` and always selects shard `0`.

There is no byte reversal, `% K`, extra Poseidon/hash step, caller-supplied shard, or use of CKB bits `0..k-1`. Bits `0..k-1` live in the first byte of the BE field representation and inherit the BN254 modulus's high-byte distribution; using them would bias candidate shard counts, especially at larger `K`. The selected numeric low bits avoid that high-byte bias.

The complete original 256-bit `tree_key` remains the CKB key inside the selected shard; selected bits are not stripped or rewritten. Every independently derived locator must satisfy `shard_index < K`. A mismatch between derived shard and persisted/staged namespace is corruption, never a fallback search across shards.

Golden vectors include at least the following canonical synthetic tree keys:

| `tree_key_f` | `K` | `shard_index` |
|---|---:|---:|
| `0x00..00` | 1 | 0 |
| `0x00..00` | 16 | 0 |
| `0x00..01` | 16 | 1 |
| `0x00..0f` | 16 | 15 |
| `0x00..10` | 16 | 0 |
| `0x00..ff` | 16 | 15 |
| `0x0f00..00` | 16 | 0 |

Production vectors also derive real Tribute, NodItem, and NodBucket `tree_key` values through ADR-008 and pin their shard indices. Cross-architecture tests must reproduce the same indices byte-for-byte.

### Shard-root aggregation

ADR-009 aggregates the fixed ordered shard-root vector with the concept's reserved CES1 tag:

```text
TAG_TOP_NODE = CES1_TAG_BASE + 11

top[0][i] = shard_root[i]                         for 0 <= i < K

top[level + 1][j] = P(
    TAG_TOP_NODE;
    level,
    top[level][2*j],
    top[level][2*j + 1]
)                                                  for 0 <= level < k

shard_top_root = top[k][0]
```

Shard roots occupy leaves in ascending `shard_index`; implementations may not reorder them by touched order, root value, completion order, or MDBX key order. `level` is the zero-based input level: hashes directly over shard roots use `level = 0`.

A structurally empty CKB shard has root `ZERO`. For `K > 1`, a top node is present even when both children are `ZERO`; its Poseidon output must be non-zero. Any Poseidon output of `ZERO` for a present aggregation node fails sealing with `TreeHashError`. For `K = 1`, no top-node hash exists and `shard_top_root = shard_root[0]`, including `ZERO` for the empty state.

On exact-parent open, the local materialization supplies exactly `K` shard roots. The CE module recomputes the complete top in ascending-index order and requires `shard_top_root == authoritative_parent_root` before any shard leaf/proof may affect execution. A missing, extra, malformed, reordered, or mismatching shard root is local corruption. The module never searches another shard or accepts an individually valid shard proof before this parent binding succeeds.

Sealing starts from that verified parent vector, replaces only roots produced by touched shards, and recomputes the complete fixed top. With candidate `K <= 32`, the normative simple path performs exactly `K - 1` top-node hashes for a changed block rather than introducing a persistent top-node cache. A zero-touch block retains the already verified parent root and emits the ordinary ADR-008 identity candidate.

The ADR-009 intermediate EVM root is `shard_top_root`, not `R_sealed`. No temporary wrapper or temporary tag is introduced. ADR-010 reuses the same `TAG_TOP_NODE` construction and binds `commitment_scheme_version`, `collection_key`, `K`, and `shard_top_root` under `TAG_COLLECTION_ROOT` before committing collection roots through the Root Catalog.

#### Consequences and trade-offs

Benefits:

- the formula is identical to the shard-top portion of ADR-010 rather than a throwaway topology;
- changed shards can prepare independently while aggregation remains small and deterministic;
- a shard path adds exactly `log2(K)` ordered siblings;
- exact-parent recomputation detects missing, swapped, stale, or corrupt local shard roots;
- no second CKB tree, linear fold, runtime plugin, or public sharding interface is added.

Costs and limitations:

- the sharded root intentionally differs from ADR-008's unsharded root for the same logical leaves;
- `K > 1` makes the all-empty top non-zero and requires `K - 1` Poseidon hashes when recomputed;
- proofs gain `log2(K)` top siblings and verifiers must know the fork-fixed `K`;
- exact-parent open must obtain all `K` shard roots even when execution touches one shard;
- all-keys-in-one-shard workloads gain no preparation parallelism and still pay aggregation overhead;
- the pre-activation ADR-009 `shard_top_root` does not independently encode `K`; the configured layout and local environment identity bind this test stage, while ADR-010 adds the explicit consensus hash binding before first deployment;
- `K = 1` is a deliberate special case with no `TAG_TOP_NODE` hash.

Rejected alternatives are a second CKB SMT over shard indices (unnecessary proof/update/storage machinery), a linear Poseidon fold (O(K) path proofs and weaker parallel structure), and a temporary `TAG_SHARD_SET_ROOT` wrapper that ADR-010 would immediately remove.

### Atomic multi-shard candidate batch

ADR-009 retains one immutable block candidate and one finalized commit boundary. The existing batch role becomes a typed shard-set envelope:

```text
ShardIndex = u32                         // canonical codec is BE4

ProvisionalTreeBatch {
    block_number,
    parent_block_hash,
    parent_root,
    new_root,
    shard_count: K,
    parent_shard_roots: [B256; K],       // ascending shard index
    new_shard_roots: [B256; K],          // ascending shard index
    changed_shards: BTreeMap<ShardIndex, ProvisionalShardBatch>,
    encoded_size,
}

ProvisionalShardBatch {
    parent_shard_root,
    new_shard_root,
    branch_changes: BTreeMap<BranchKey, TreeChange<BranchNode>>,
    leaf_changes: BTreeMap<TreeKey, TreeChange<LeafValue>>,
}
```

`freeze(block_hash)` produces the immutable `StagedTreeBatch` without changing any root, map, order, or size. Candidate publication remains hash-addressed and idempotent only when the complete typed envelope is structurally equal. Cross-crate consumers receive identity/root/size accessors and pass the opaque immutable batch back to the tree module; shard maps are not a new caller-managed interface.

Both root vectors have exactly `K` canonical field elements. Their array position is the shard index; no redundant index travels inside a vector entry. `changed_shards` is strictly ordered by numeric `ShardIndex`, contains only indices `< K`, and contains an entry exactly when effective CKB changes make `parent_shard_root != new_shard_root`. Parent-equal/net-no-op shards are absent. Each entry's roots must equal the corresponding vector positions. A changed-root entry with no effective branch/leaf change, an unchanged-root entry, duplicate/misderived shard, or a change whose full `tree_key` derives to another shard is invalid.

A zero-touch or all-net-no-op block carries equal parent/new vectors and an empty `changed_shards` map while retaining the ADR-008 identity candidate and marker progression. Complete vectors cost `2 * K * 32` bytes (2 KiB at `K = 32`) before small fixed framing, and that cost is included in `encoded_size` and candidate-cache accounting.

### MDBX sharded local codec

All shards remain in one CE-owned MDBX environment. ADR-009 advances `CE_MDBX_LOCAL_SCHEMA_VERSION` from 1 to 2 and uses new V2 tables; this is distinct from the unchanged EVM `0xEE0D.storage_schema_version = 2`. It never interprets ADR-008 unsharded keys as shard `0`:

```text
CeShardRootsV2:
    key   = shard_index_BE4                         // 4 bytes
    value = shard_root_BE32                         // 32 bytes

CeBranchesV2:
    key   = shard_index_BE4 || BranchKey            // 4 + 33 bytes
    value = existing canonical BranchNode codec

CeLeavesV2:
    key   = shard_index_BE4 || TreeKey               // 4 + 32 bytes
    value = existing non-zero LeafValue codec

CeMetadataV2:
    environment_identity
    last_applied
```

Big-endian `u32` makes lexical namespace order equal numeric shard order and avoids narrowing casts. The environment identity binds `CE_MDBX_LOCAL_SCHEMA_VERSION`, chain/genesis, commitment scheme, pinned vendor revision, sharded tree-format identifier, and exact `K`. A directory created for another `K`, unsharded format, or schema is rejected/rebuilt; it is never opened through compatibility fallback.

ADR-009 height-0 initialization writes exactly `K` explicit `shard_index -> ZERO` records and the marker in one MDBX transaction. The marker's `new_root` is the deterministic aggregate of that complete ZERO vector (`ZERO` only for `K = 1`, normally non-zero for `K > 1`). Initialization/rebuild also requires the EVM-authoritative test-stage root to equal that aggregate. Missing ZERO records are not synthesized on read; after initialization, fewer or more than `K` root records is corruption.

One immutable exact-parent read snapshot supplies the marker and exactly `K` roots. Their aggregate must equal the EVM-authoritative parent root before shard records are used. Shard branch/leaf reads always include the derived BE4 prefix and cannot scan another prefix on miss.

Finalized apply uses one MDBX write transaction:

1. verify candidate identity, `K`, encoded size, complete root vectors, changed-shard derivations, and aggregate parent/new roots;
2. require `last_applied` to be the exact candidate parent;
3. read exactly `K` persisted roots and require equality with `parent_shard_roots` plus the authoritative parent aggregate;
4. apply each changed shard in ascending index order, using prefixed branch/leaf keys;
5. write only changed entries in `CeShardRootsV2` and require the resulting complete vector to equal `new_shard_roots`;
6. require its aggregate to equal candidate/EVM `new_root`;
7. write the complete `last_applied` marker last;
8. commit once.

No shard root, node, or marker becomes visible independently through this module. The existing uncertain-commit recovery remains unchanged: withhold ACK, reopen the environment, and classify the single marker as equal, behind, or conflicting. Marshal ACK and `CeRetentionHeight` advance only after this one transaction has a known successful/equal outcome.

#### Consequences and trade-offs

Benefits:

- preserves ADR-008's one candidate, one atomic commit, one marker, and one ACK boundary;
- complete parent/new vectors make the envelope self-checking and bind every changed shard to both aggregate roots;
- parallel preparation does not leak partial shard batches into persistence;
- one snapshot and namespaced tables preserve exact-parent consistency;
- BE4 prefixes compose naturally with ADR-010's future `collection_key || shard_index` namespace;
- the fixed vector overhead is small for the benchmark candidate range.

Costs and limitations:

- final MDBX persistence remains serialized by one write transaction even if preparation is parallel;
- every candidate retains two complete root vectors and finalization validates all `K` roots when one shard changed;
- branch/leaf keys grow by four bytes and cache/storage accounting must include the prefix;
- V2 tables and environment identity make ADR-008 MDBX disposable rather than reusable;
- ADR-010 will still extend the local namespace with collection identity before first deployment;
- full structural validation adds CPU work, though it is bounded by `K <= 32` for this benchmark matrix.

Rejected alternatives are separate shard batches with independently visible progress (they still need a common atomic envelope) and separate MDBX environments per shard (no atomic multi-shard commit, substantially larger crash/ACK state machine).

### Deterministic shard preparation

ADR-009 requires a sequential reference implementation. Mutations are grouped by `ShardIndex`, sorted by pinned CKB order, prepared against authenticated parent shard roots, and collected in ascending shard order before the coordinator constructs the one atomic batch.

A bounded parallel implementation is permitted only as an internal optimization. It must produce byte-identical shard roots, branch/leaf maps, root vectors, encoded size, batch, CE work usage, and error classification relative to the sequential path. `StorageHandle`, EVM journal/lifecycle state, receipts, and mutable shared shard state never cross a worker seam; no background task may outlive `end_block`.

ADR-009 does not prescribe a worker pool, concurrent-seal session model, fairness algorithm, reader formula, or production worker count before the sharded prototype and pinned Reth execution concurrency are measured. The candidate explicit-pool design, its benefits/costs, and its open concurrency questions are retained in ADR-017.

### CE work accounting boundary

ADR-009 preserves ADR-008's deterministic pre-write CE work admission and adds shard identity to the measurements. The preferred future decomposition is `base(K) + first-touched-shard + unique-key`; worker scheduling never changes consensus units, and no post-seal branch count may retroactively invalidate receipt-visible execution.

ADR-009 does not fix new coefficients, shard meter structures, or production limits. The full model, transaction/checkpoint semantics, alternatives, benefits, and costs are retained for measurement and closure in ADR-017.

### Candidate-cache boundary

ADR-009 batches report canonical encoded bytes and changed shard/branch/leaf counts, include full root vectors/prefixes, and retain explicit no-implicit-eviction/no-disk-spill semantics. These measurements are sufficient for the sharding comparison.

ADR-009 does not replace current cache configuration with a five-dimensional production envelope. The proposed per-candidate/total byte-and-record model, its startup relationship to CE capacity/candidate concurrency, and its benefits/costs move to ADR-017.

## Developer baseline: not activation evidence

The existing ADR-008 harness was run on 2026-07-16 with:

```text
Apple M4 Max, 14 cores, 36 GiB RAM
macOS 26.6 (25G5065a)
rustc/cargo 1.96.0
```

Observed Criterion estimates:

| Benchmark | Shape | Estimate |
|---|---:|---:|
| cold `update_all` | 256 inserts | 54.12 ms |
| warm `update_all` | 256 updates | 51.49 ms |
| build tree + exact-parent proof | 256 leaves / 32 proof keys | 55.68 ms |
| warm exact-parent proof verify | 32 keys | 3.26 ms |
| warm MDBX staged apply | 64 synthetic records | 175.63 us |
| warm no-change MDBX apply | marker only | 67.74 us |

The `cold_open_and_staged_apply` Criterion case is currently invalid: repeated temporary MDBX environment creation terminates with `Cannot allocate memory (12)`. This is a benchmark-harness defect and yields no cold-open measurement. It must be corrected without weakening production MDBX settings before the result is used.

These numbers do not close ADR-009. The harness uses only 256 leaves, synthetic MDBX changes, and no complete execution/seal/finalization path.

## Verification benchmark

ADR-009 benchmarks `K_PROVISIONAL = 16` only enough to validate correctness and expose obvious regressions: deterministic insert/update/delete/mixed and all-in-one-shard cases, exact-parent proof/seal, aggregation, candidate bytes/records, finalized apply, restart, and optional sequential/parallel byte equality. Commands, fixture checksums, roots, and raw timings are retained, but they are not production capacity evidence.

ADR-017 owns the power-of-two candidate matrix, final hardware/full-path/off-chain-contention benchmark, `K_PRODUCTION`, CE/gas/resource limits, strict latency gate, and activation artifacts.

## Remaining closure

No numerical shard-count choice remains open for ADR-009 implementation: use 16. ADR-009 remains proposed until the `K = 16` sharded correctness/persistence path and tests are complete.

## Reset and version policy

ADR-009 performs no independent network activation or reset. Temporary unsharded materialization is disposable. ADR-010 completes the pre-production/testnet scheme-1 topology with `K_PROVISIONAL = 16`; ADR-017 later selects production `K` and requires a complete reset/rebuild if it changes before production. An in-place state-preserving K change is not allowed under the same scheme.

## Next step

Implement and verify the parameterized sharded path with active `K_PROVISIONAL = 16`, then continue with ADR-010 without waiting for production performance closure.
