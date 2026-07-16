# Off-chain entity storage: evolutionary ADR index

This index turns the final off-chain storage concept into a sequence of independently implementable ADRs.

The concept describes the target system. This index describes how to reach it from the current node, one working increment at a time.

## Core rule

After every ADR:

- the node starts and works;
- the newly added capability is exercised by real code, not left as a dormant abstraction;
- the result can be verified locally or on a restarted testnet;
- current security and functionality limitations are stated explicitly;
- the next ADR adds a capability or strengthens a guarantee without changing domain callers unnecessarily.

An intermediate system does not need the guarantees of the final system. For example, the first Mongo-backed runtime may have no SMT, no independently trusted body commitment, no proof service, and no snapshot recovery. That is acceptable when the limitation is deliberate, observable, and closed by a later ADR.

There is no `Production Testnet Activation Guard`. There is no production network yet. Consensus-visible steps may use a hard fork, node restart, coordinated testnet reset, or complete deletion and rebuild of testnet-derived state.

## Pre-production evolution policy

Until production/mainnet state exists:

- backward compatibility between ADR stages is not required;
- migration of old Tribute, Nod, Mongo, or SMT state is not required;
- event, body, leaf, and tree formats may change through a coordinated testnet reset;
- an internal implementation may be replaced behind a stable facade;
- useful concepts that form the next stage — canonical bodies, identity, commitments, domain repositories — should be carried forward rather than redesigned without need;
- version registries and multi-version migration paths are introduced only after a real compatibility requirement exists.

## What counts as a working result

An ADR does not have to introduce a public RPC method. A result is sufficient when the new capability is integrated into the node or an active execution/projection path and has an observable, testable output.

Examples:

- both storage adapters pass one conformance suite;
- canonical events appear in real receipts;
- ExEx fills Mongo from canonical blocks;
- body-dependent runtime code reads through the facade;
- commitments are calculated and checked on real reads;
- SMT roots match across locally running nodes;
- a proof returned by the node verifies against the committed root.

A library that compiles but is not exercised by any active path is not a completed ADR result.

## Required structure of every ADR

Every ADR produced from this index must contain these sections:

1. **Starting system** — the working behavior inherited from previous ADRs.
2. **Added capability** — the one primary functional improvement introduced here.
3. **Decision** — the architecture and interface chosen for that improvement.
4. **Working result** — what can be run or observed after implementation.
5. **Accepted limitations** — security, availability, performance, or functionality not solved yet.
6. **Verification** — local integration, multi-node test, benchmark, or testnet evidence proving the result.
7. **Reset policy** — whether node restart, Mongo rebuild, hard fork, or complete testnet reset is expected.
8. **Next unlocked step** — the capability that can now be added without guessing.

## Dependency path

```text
001 Storage facade
  -> 002 Tribute/Nod body boundary
  -> 003 Full-body receipt events
  -> 004 Reth ExEx Mongo projection
  -> 005 Mongo execution reads
  -> 006 Body commitment and verification
  -> 007 Generic lifecycle and journaled overlay
  -> 008 Basic unsharded SMT
  -> 009 SMT sharding
  -> 010 Collections and Root Catalog
  -> 011 Partition retirement
  -> 012 Header root carrier
  -> 013 Proofs and verified point reads
  -> 014 Persistent SMT storage
  -> 015 Crash and restart reconciliation
  -> 016 Snapshots and bootstrap
  -> 017 Gas, quotas, and performance closure

Future triggers:
  018 Versioned format evolution
  019 Domain registry and later domains
```

---

# Main implementation sequence

## ADR-001 — `001-offchain-storage-facade.md`

### Starting system

Tribute and Nod bodies live in EVM storage and are accessed through their existing contract facades.

### Added capability

A single off-chain storage seam with two real adapters: in-memory and MongoDB.

### Decision

Define a small read/write facade for entity bodies, bounded queries, projection batches, typed backend failures, and projection checkpoints. Mongo collection layout and BSON remain private adapter details. Both adapters implement identical semantics and run the same conformance suite.

Domain code does not use this facade yet.

### Working result

The in-memory and MongoDB implementations can store, read, list, replace, and delete representative bodies with equivalent observable behavior. The node still uses its existing EVM body path and remains fully functional.

### Accepted limitations

There are no domain events, ExEx projection, Mongo execution reads, body commitments, SMTs, or proofs.

### Verification

Run one adapter conformance suite against memory and an isolated MongoDB instance. Verify atomic batch/checkpoint behavior and typed failure cases.

### Reset policy

No chain reset is required. Test Mongo collections may be deleted freely.

### Next unlocked step

Define which Tribute and Nod data is accessed through typed repositories and which protocol state remains in EVM.

---

## ADR-002 — `002-tribute-nod-body-boundary.md`

### Starting system

The generic facade and both adapters work, while Tribute and Nod still use their existing EVM records.

### Added capability

Typed Tribute and Nod repositories and an explicit body/protocol-state boundary.

### Decision

Define complete `TributeBody` and `NodBody` domain models above the generic storage facade. Identify every Lysis, Tribute, NodFactory, Gratis, metadata, and query consumer.

Full per-entity bodies are future off-chain data. Domain aggregates and control structures that still directly drive protocol transitions remain explicit EVM state, such as Tribute day totals/sealing state and Nod bucket/bin-tree state.

This ADR introduces the typed seam but does not switch the live read path.

### Working result

All body-dependent operations can be expressed against typed repositories in tests and integration harnesses. The production node continues using its existing EVM implementation through an adapter and remains functional.

### Accepted limitations

Bodies are still stored on-chain. No receipt can rebuild the repository, and Mongo is not yet a live projection.

### Verification

Round-trip complete Tribute and Nod bodies through both storage adapters. Verify the field boundary against all current runtime readers and retained protocol aggregates.

### Reset policy

No chain reset is required because live storage has not switched.

### Next unlocked step

Publish complete bodies in receipts so the off-chain store can be rebuilt from chain data.

---

## ADR-003 — `003-full-body-receipt-events.md`

### Starting system

Typed repositories exist, but live execution still reads EVM bodies and Mongo has no canonical input stream.

### Added capability

Complete receipt-visible body events for successful Tribute and Nod mutations.

### Decision

Every successful create or update emits one event containing the complete resulting entity body. Every successful delete or burn emits one identity-only delete event. Event emission shares the mutation's journaled execution scope, so reverted execution leaves no canonical body event.

The first event representation needs only deterministic decoding and complete reconstruction. Cryptographic body canonicalization is deferred to ADR-006.

### Working result

Real Tribute and Nod transactions produce receipts from which the complete current body mutation can be reconstructed. Existing EVM reads continue to work unchanged.

### Accepted limitations

No projector consumes the events. Event bodies are not yet commitments, and Mongo remains empty unless populated manually.

### Verification

Execute create/update/delete and revert scenarios. Decode receipts and assert complete body reconstruction, exact operation ordering, and absence of events after revert.

### Reset policy

A hard fork or testnet restart may be used if event changes affect deterministic execution. Existing testnet history does not need migration.

### Next unlocked step

Consume the events through Reth ExEx and build a real Mongo materialization.

---

## ADR-004 — `004-reth-exex-mongo-projection.md`

### Starting system

Receipts contain complete Tribute and Nod body changes, while the node still reads EVM bodies.

### Added capability

Automatic finalized receipt-to-Mongo projection through Reth ExEx.

### Decision

Install a mandatory ExEx on validator and full-node modes; missing projector/MongoDB configuration stops node startup. Use `provider.finalized_block_stream()` as the sole finalized target source, replay exact intermediate blocks from a durable number/hash checkpoint, and never advance Reth `FinishedHeight` above that checkpoint. During ADR-004 the mandatory ExEx remains asynchronous and execution does not wait for each checkpoint.

Validate and simulate the complete block before writes. Apply all body/index mutations from one successful EVM receipt through one backend-neutral atomic storage batch. MongoDB must provide transaction capabilities through a replica set or sharded cluster; topology remains an operator choice. Persist local network/schema identity and exact receipt provenance, while leaving snapshot transport to MongoDB tooling.

ExEx materializes accepted events; it does not rerun Tribute/Nod business rules.

### Working result

Running the node and submitting Tribute/Nod transactions automatically creates, updates, and deletes Mongo bodies and indexes. Duplicate delivery, partial-block crash, and restart converge from the checkpoint. Runtime reads still use EVM.

### Accepted limitations

Mongo is not yet used by Lysis or other domain logic. Projection failures stall only the new materialization and do not stop the EVM-backed node. Different receipts in a block may become visible progressively. Projection correctness is not cryptographically checked, and there is no built-in snapshot transport or automatic recovery.

### Verification

Run storage transaction conformance and real node integration tests for finalized-only filtering, full-block preflight, receipt atomicity, upsert, delete, duplicate delivery, crash replay, restored-checkpoint validation, missing history, and checkpoint/`FinishedHeight` ordering. Compare Memory and Mongo projector results.

### Reset policy

Mongo may be dropped and rebuilt from retained compatible receipts or restored through operator tooling. A chain reset is unnecessary unless ADR-003 event format changed simultaneously.

### Next unlocked step

Switch body-dependent execution and query paths to the populated repository and promote projector health into a node readiness requirement.

---

## ADR-005 — `005-mongo-execution-reads.md`

### Starting system

Mongo is continuously populated from finalized receipts, but all consensus/domain body reads still use EVM records.

### Added capability

The first complete Mongo-backed Tribute/Nod runtime.

### Decision

Switch Lysis, Tribute processing/burn, NodFactory mining/payment, Gratis inputs, metadata, and body/query reads to the typed facade backed by MongoDB. Remove active full per-entity EVM body storage while retaining only the protocol aggregates and control structures identified in ADR-002.

Require state and Mongo projection to catch up together before business readiness or validator participation. ExEx remains the sole asynchronous writer, and Marshal acknowledgment/finality does not wait for projection. Before local proposal, verification, or full-node execution consumes Mongo-dependent successor state, a local readiness handle checks the required finalized-parent checkpoint. Proposal/verification waits only within the existing remaining view budget; expiry causes local abstention rather than a `false` vote, while the rest of the network continues by quorum. Because Mongo is unversioned, only an exact checkpoint/required-parent match is executable; checkpoint ahead returns local `ProjectionAhead` and never reads future state.

A synchronous execution read is bounded by `min(remaining view budget, 1 second)`; read-side `Unavailable` aborts only the local request and enters shared recovery without a `false` vote. Execution/projector Mongo access requires primary read preference and majority read/write concerns. A technical MongoDB outage receives an immediate recovery attempt followed by non-overlapping one-second retries within one total eight-second deadline. The long-lived ExEx runner recreates only its Mongo session, keeps draining notifications, coalesces pending finality to the latest exact target, and replays every intermediate block from the durable checkpoint after recovery. A projection supervisor triggers graceful whole-node shutdown on deadline expiry; deterministic `Fatal`, unexpected ExEx exit, or readiness-channel closure shuts down immediately without using the Mongo retry window. ADR-005 approves a narrow testnet-only exception to the ExEx observability-only rule; implementation records it through the normal README/debt/ruler workflow with a hard production disable.

ADR-005 startup also requires `Mongo checkpoint <= local Reth finalized/executed checkpoint`; Mongo-ahead startup is rejected, and first-start snapshots restore a matching Reth/Mongo finalized height/hash pair. A genuine missing row produces the normal domain `NotFound`/revert result. Backend, corruption, and lag errors remain explicit and never fall back to a hidden EVM body source. Until authenticated absence exists, a local omission may make that validator disagree with the correctly materialized quorum.

Implement ADR-005 through ADR-010 consecutively on one branch without an intermediate runtime gate or deployment. ADR-005 introduces no temporary fence; ADR-006/007 add canonical commitments and permanent overlay, ADR-008 is the unsharded reference/benchmark, ADR-009 shards it, and ADR-010 completes the first deployed Root Catalog topology before one testnet reset.

### Working result

The branch implements Mongo read cutover, commitments, overlay, CKB tree, sharding, and Root Catalog sequentially before combined end-to-end validation.

### Accepted limitations

Before ADR-006 lands on the branch, MongoDB is unauthenticated only in focused implementation tests; no intermediate binary is deployed. Projection readiness affects only the local node's ability to participate or execute; it is not consensus protocol data. The first testnet deployment after ADR-010 assumes a protocol quorum has correct independent projections. A locally omitted or altered row fails the CES1 commitment/tree check. Local Mongo failure may remove a validator from voting. There is still no public proof service, authenticated list completeness, automatic snapshot recovery, or production availability guarantee.

These limitations are normal for this stage.

### Verification

After ADR-010, run the combined Tribute -> ExEx -> Mongo -> Lysis and Nod -> ExEx -> Mongo -> mining -> Gratis plus CKB/shard/catalog suite. Exercise startup gating, finalized-parent readiness, recovery before and failure at the eight-second deadline, missing/malformed/unavailable rows, graceful shutdown, same-block overlay behavior, and proposer/validator parity across independent databases.

### Reset policy

Do not deploy any intermediate ADR-005–009 binary. After ADR-010 and combined verification, use one complete coordinated testnet reset to deploy ADR-003 through ADR-010 with an empty Tribute/Nod per-entity genesis body set. Projection starts at the first executable block; no legacy body migration, dual-read, dual-write, fallback, or temporary fence is introduced.

### Next unlocked step

Implement ADR-006 through ADR-010 on the same branch, then run combined verification and deploy only the completed CES1 path.

---

## ADR-006 — `006-body-commitment-and-verification.md`

### Starting system

All body-dependent logic reads MongoDB through the facade, but a well-formed altered body is not detectable.

### Added capability

Deterministic body commitments and verification on real Mongo reads.

### Decision

In one functional step, define:

- append-only strict-canonical Protobuf bodies with an authenticated per-body `schema_version`;
- one fork-global CES1-tagged Poseidon-BN254 commitment scheme, with `PBytes` and leaf rules and no simultaneous scheme coexistence;
- exact 36-byte identities: Tribute/Nod use `WWD_BE4 || full Poseidon_BE32`, while Nod bucket uses `WWD_BE4 || bucket_key_BE32`; the typed collection provides the namespace;
- the leaf commitment binding that identity and body.

Tribute/Nod switch from `uint256` IDs to custom ABI `bytes` validated as exactly 36 bytes; no digest truncation or surrogate ID remains. Store current commitments in three direct typed EVM mappings keyed by `identity_f = PBytes(TAG_ID, EntityId36)`; zero is canonical absence. Emit public verifiable transitions containing the raw ID, commitment/body schema versions, previous and new commitments, and the exact canonical Protobuf payload. On every body-dependent read, canonicalize the returned body, derive its identity and commitment, and compare it with EVM state before use.

The commitment format is deliberately chosen so it can become the future SMT leaf.

### Working result

Lysis, Tribute, NodFactory, and Gratis continue to read Mongo, but modified, missing-while-committed, wrong-identity, wrong-version, non-canonical, or stale bodies fail before domain use. Finalized events provide complete independently replayable commitment transitions.

### Accepted limitations

Commitments still consume one EVM entry per entity. There is no global current-state root, membership proof, sharding, or scalable tree. Secondary-index list completeness remains unauthenticated even though every returned member is verified.

### Verification

Use golden Protobuf envelope/payload, 36-byte identity, pinned `outbe-poseidon` v0.11.0/CES1 `PBytes`, and leaf vectors plus event-replay and end-to-end runtime tests. Mutate every body field, WWD, entity ID, schema version, canonical payload, event commitment, and stored EVM commitment independently and prove that reads or projection fail explicitly.

### Reset policy

Implement ADR-006 on the ADR-005–010 branch; its direct maps are a focused test backend. Use the single reset only after ADR-010 for the final EVM/tree layout and canonical event/body format. No old U256 ID, Postcard body, event, or commitment migration is required.

### Next unlocked step

Move the three typed mappings behind one generic commitment lifecycle and add the permanent journaled body overlay in ADR-007.

---

## ADR-007 — `007-generic-lifecycle-and-journaled-overlay.md`

### Starting system

Per-domain code writes and verifies per-entity commitments, but generic existence, same-block, and rollback behavior is not centralized.

### Added capability

One generic `mint/update/delete` lifecycle with deterministic same-block behavior.

### Decision

Add the internal `outbe-compressed-entities` module at system state address `0xEE0D` with no public mutating precompile. It physically owns the three logically distinct direct commitment namespaces, shared body/index first-touch lists, lifecycle cleanup, and a block-scoped full-body EVM overlay: pending `Set` stores the non-zero leaf plus exact canonical `StoredBody`, pending `Deleted` is same-block absence, and `Untouched` falls through to finalized-parent MongoDB. Unique touched identities drive mandatory cleanup before state-root calculation. A second journaled delta overlay covers `TributeByOwner`, `TributeByDay`, `NodByOwner`, and `NodAll`: list reads deterministically merge finalized-parent Mongo IDs with same-block Added/Removed memberships before resolving bodies overlay-first. The fixed `0xEE0D` layout uses slots 0–3 for schema/direct maps, slots 4–6 for body pending word/bytes/touches, slots 7–9 for index delta word/record/touches, and slot 10 for the reversible body identity record required by cleanup and ADR-008; pending encodes `0 = Untouched`, canonical non-zero BN254 leaf = Set, and `U256::MAX = Deleted`.

A closed typed-enum Rust interface exposes only `mint`, `update`, `delete`, and verified `read`; the module derives collection, ID, active schema, leaf, mapping key, and canonical event itself. `read` is the only constructor of an opaque value-based `VerifiedBody`; update/delete consume that capability, accept same-value ABA when identity/leaf still match, reject a mismatched current value, and use its old typed body to derive index removals without a second Mongo read. Finalized-parent Mongo fallback is hidden behind a consumer-owned `ParentBodySource` implemented by ADR-005 `RuntimeBodyReaders`. Existing EVM journaling keeps overlay bytes, touched keys, body events, commitment changes, and domain writes in the same revert scope. No process-memory undo journal is introduced. Generic transitions use overlay-aware current existence: mint requires absent, update/delete require present, delete→mint and same-leaf update are allowed, and every successful operation remains a separate ordered event. First body touches prepay the active schema's maximum cleanup footprint and first index touches prepay their fixed deferred cleanup, and list reads charge per scanned delta/parent ID/verified body, so no separate mutation-count cap is introduced. `CompressedEntitiesLifecycle::begin_block` requires an empty overlay; `end_block` runs after the final receipt-visible body mutation and before buffered post-block changes notify Reth's state-root task, after which further execution reads, lists, or mutations of compressed bodies are forbidden. Tribute/Nod retain business-state and stricter lifetime-reuse ownership; only trusted typed Rust paths select a collection, and canonical events remain emitted at their domain addresses.

### Working result

Tribute and Nod use the same lifecycle for commitment mutation and verified point/list reads. Same-block create/update/delete/index sequences, mismatched-capability rejection and accepted same-value ABA, dependent transactions, and reverted execution produce deterministic commitments, bodies, lists, gas, and events; end-block removes every temporary slot while direct mappings persist.

### Accepted limitations

The mappings remain unscalable and have no Merkle root or proof. Parent secondary-index completeness remains unauthenticated, partition retirement is unavailable, finalized RPC does not expose an in-progress overlay, and exact production capacity remains subject to later benchmarks.

### Verification

Run complete mutation matrices, body/index key vectors, parent-page merge boundaries, opaque-capability misuse, nested revert/failure injection, cleanup/state-root notification, golden gas accounting, ExEx replay, and multi-node proposer/validator equivalence tests.

### Reset policy

Do not deploy schema-v1 direct maps. Continue through ADR-008–010 and use one combined ADR-003–010 reset. Final genesis allocates marker-preserved `0xEE0D` with the Root Catalog root schema and empty overlays; Mongo rebuilds from final CES1 events. No migration, intermediate domain-owned maps, dual path, or fence is introduced.

### Next unlocked step

Replace the three direct mappings with one authenticated unsharded SMT inside the same module without changing domain callers or the body/index overlay.

---

## ADR-008 — `008-basic-unsharded-smt.md`

### Starting system

Generic lifecycle and journaled mutations work over an EVM commitment mapping.

### Added capability

One consensus-enforced unsharded sparse Merkle tree.

### Decision

Vendor and minimally panic-sanitize CKB `sparse-merkle-tree` `v0.6.1` at immutable commit `ad555350c866b2265d87d2d7fbd146fbc918bfe5` inside `outbe-compressed-entities`, retaining MIT license and exact provenance; no custom tree engine or public backend abstraction is introduced. Outbe leaves traversal/topology/update/delete/proof algorithms intact: `PoseidonCkbHasher` maps CKB's existing Hasher transcripts to the three CES1 SMT tags, using non-canonical `H256::MAX` only as an internally rejected hash-error poison, while finalized MDBX and speculative stores implement CKB's existing read/write seams. The vendored production subset excludes Blake2/C/trie/WASM and permits only allowlisted panic-to-structured-error edits in tree/proof paths, enforced by checksums, source diff, panic/unsafe scan, and pristine-upstream differential tests. The tree retains CKB's 256-level H256 key/path, update/delete, proof, and compact-zero MergeValue semantics. ADR-006 leaf commitments are stored verbatim and ZERO deletes them. Outbe supplies the exact CES1 Poseidon codec using `TAG_SMT_BASE/NORMAL/ZERO`, including upstream `u8` wrapping `zero_count` semantics.

Storage schema v2 keeps ADR-007 overlay slots 4–10 stable: slot 0 is schema version 2, slot 1 is `last_smt_root`, and former direct-map bases 2–3 remain reserved/empty after the complete reset. The exact-parent root in `0xEE0D` is the sole consensus authority. `tree_key_f = P(TAG_KEY; commitment_scheme_version, collection_id, identity_f)` converts directly as `CKB_H256::from(BE32(tree_key_f))` with no byte reversal; its two high numeric bits are zero but all bytes follow pinned CKB `get_bit`/parent-path/`Ord` semantics. The unsharded tree is a non-deployed reference/benchmark milestone using CES1 primitives; local vendor/codec metadata is not a network version. Because ADR-006–010 have no intermediate activation, the first deployed scheme 1 is defined only by ADR-010's final sharded collection/Root Catalog topology. The CE-owned MDBX at `<datadir>/compressed_entities/smt/` stores the finalized in-place tree and atomic complete `last_applied` marker. ADR-005/008 execution uses only one exact finalized parent whose Mongo projection and CE MDBX marker both match height/hash/root; a non-finalized candidate is never a descendant execution parent in this stage. Each candidate uses one block-scoped finalized `AuthenticatedTreeView`, while `StagingCkbStore` captures ordered branch/leaf changes without copying or mutating MDBX. Batches publish only after executor finish/sealing, re-publication is idempotent only when typed metadata/ordered maps are structurally equal, and losing candidates never touch MDBX. Restart discards them; candidates return only through verified redelivery/reexecution.

Every untouched point/list body uses block-cached `read_leaf_verified` evidence against the exact parent root. End-block has an explicit zero-touch proof/update bypass, otherwise derives strictly unique CKB-ordered keys, rejects cross-identity `tree_key` collision, verifies one parent multi-proof for all touched leaves, drops `parent_leaf == final_leaf` net no-ops, and runs `update_all` only for effective changes. Canonical operation events, EVM gas, and reserved CE work units remain even for net no-op. A zero-change block still publishes/commits an empty identity batch so `last_applied` and exact block-parent chaining advance without gaps. `BlockLifecycle` has associated `EndBlockResult`; ordinary modules use `()`, while compressed entities returns typed `SealOutput { root, staged_tree_batch }`, writes the EVM root, and cleans the overlay before Reth state-root notification. ADR-007 transaction gas remains receipt/header EVM gas. Separately, one executor-local CE work-unit meter spans user/system lanes: every block reserves base seal/marker units and every first unique key reserves worst-case proof/update/staging/MDBX/cleanup units. Repeated touches and net no-ops receive no duplicate reserve/release. No mutation-count cap or provisional numbers are activated; reproducible worst-case benchmarks fix coefficients, total budget, and local cache limits before deployment. After Marshal finality and durable Reth block/receipt/EVM persistence, one CE MDBX transaction commits nodes plus marker; only then is the finalized block ACKed. `CeRetentionHeight = last_applied.height` independently fences Reth receipt/event/historical-root pruning for behind recovery; if pinned Reth cannot register that dynamic retention client, startup disables the relevant pruning. Missing exact-parent state causes local forfeiture/abstention, and corrupt/mismatched local state fails closed/rebuilds rather than becoming a negative vote.

### Working result

Proposer and validator execute Tribute/Nod flows through the same vendored tree semantics, calculate the same EVM root, retain speculative branches safely, atomically persist finalized tree progress, and restart from a root-verified marker.

### Accepted limitations

One tree has no sharding, collection isolation, Root Catalog, header artifact, public proof RPC, portable snapshot, or production capacity evidence. MDBX may still require canonical replay/rebuild after corruption.

### Verification

Run independent-reference and differential CKB vectors for every MergeValue form and the `zero_count` 255→0 boundary; multi-node root equality; delete/non-membership; speculative branch/drop; atomic marker/idempotent ACK; behind/ahead/conflicting restart rows; invalid-root rejection; and full replay.

### Reset policy

No separate reset or deployment. ADR-008's unsharded state is discarded after benchmark; the single ADR-003–010 reset initializes only the final scheme-1 shard/collection/catalog root and CE MDBX marker.

### Next unlocked step

Measure and add fixed sharding over the same vendored engine without changing lifecycle, leaf, or domain repository interfaces.

---

## ADR-009 — `009-smt-sharding.md`

### Starting system

The implemented unsharded SMT produces the reference consensus root and persistent exact-parent materialization, but no sharded topology has been selected or activated.

### Added capability

Fixed power-of-two SMT sharding.

### Decision

Without deployment or version bump, split the reference tree into `K_PROVISIONAL = 16` full CKB SMT shards; keep K parameterized and restricted to powers of two, but fork-fixed for any running chain rather than operator-configurable. For `K = 2^k`, derive `shard_index` from the low `k` numeric bits of `tree_key_f` through the exact CKB `H256::get_bit`/BE32 mapping; keep the full key inside its shard and never search fallback shards. Aggregate roots in ascending shard-index order with the fixed-depth `TAG_TOP_NODE` Poseidon tree. Exact-parent open recomputes that top from exactly `K` local roots before trusting shard evidence.

Use one atomic multi-shard batch with complete ordered parent/new root vectors and only effective changed-shard maps. Persist all namespaced shards in one CE MDBX environment and one transaction whose single marker is written last. Sequential shard preparation is the correctness reference; any parallel optimization must be byte-identical and remain internal.

Use 16 through ADR-016 and testnet integration without claiming production optimality. ADR-017 later compares power-of-two candidates on the completed system plus co-located off-chain-computation contention and selects `K_PRODUCTION`. Production worker/concurrency/reader/cache bounds, final CE coefficients, strict activation gates, and full artifact machinery are likewise ADR-017 scope. `K` is fork-fixed and never an operator/CLI setting.

### Working result

The same Tribute/Nod lifecycle and byte-identical scheme-1 leaf values produce the sharded candidate root used by ADR-010. Multi-node execution agrees, and load tests demonstrate better working-set behavior or throughput than the unsharded stage.

### Accepted limitations

Collections and partitions are not yet independently represented. `K_PROVISIONAL = 16` may be suboptimal and production selection is intentionally deferred. Changing K while preserving a running chain's state requires an explicit new scheme/migration; a complete pre-production reset may rebuild a new chain directly under its final scheme-1 topology.

### Verification

Run cross-architecture root vectors, all-in-one-shard workloads, sequential correctness, optional parallel/sequential byte equivalence, atomic MDBX/restart tests, and comparison with the unsharded logical map using active K=16. ADR-017 owns the later candidate matrix and full production activation/performance report.

### Reset policy

No reset or testnet activation. The unsharded reference state is disposable; continue directly to ADR-010 on the same branch.

### Next unlocked step

Group shards into independent domain/partition collections and commit them under one root.

---

## ADR-010 — `010-collections-and-root-catalog.md`

### Starting system

A sharded tree works, but all Tribute and Nod entities share one logical collection/root structure.

### Added capability

Independent collections combined by a Root Catalog.

### Decision

Complete the first activated `commitment_scheme_version = 1` without recommitting unchanged body leaves. Define three compressed-entity domains with stable `u16` IDs: `1 = Tribute`, `2 = NodItem`, `3 = NodBucket`, with `0` reserved. Tribute is WWD-partitioned; NodItem/NodBucket are singleton and share the Nod runtime owner but retain independent authenticated collection keys, shard sets, roots, catalog leaves, and proof paths. Derive `collection_key_f = PBytes(TAG_COLLECTION_KEY; domain_id_BE2 || partition_presence || partition_len_BE4 || partition_bytes)` with exact WWD/singleton encodings; runtime address/emitter/schema and operator input never select it. Preserve `id_f = PBytes(TAG_ID, EntityId36)` and ADR-006 leaf commitments byte-for-byte; the final `tree_key_f = P(TAG_KEY; commitment_scheme_version, collection_key_f, id_f)` replaces only ADR-008/009's temporary numeric collection input. Each domain definition carries fork-fixed `K_domain`; through ADR-016 and pre-production/testnet all three equal `K_PROVISIONAL = 16`, while ADR-017 later selects production K on the completed system and future new domains may use another measured value. No caller/operator selects `K`, and changing it for an active populated domain requires a new scheme/migration decision. No fallback or new entity-kind discriminant is introduced. Calculate `R_collection = P(TAG_COLLECTION_ROOT; scheme, collection_key_f, K_domain, shard_top_root)` and store non-zero collection roots as Root Catalog leaves. Never-populated collections have no leaf; ordinary deletion of the last entity retains a non-zero collection leaf over an all-ZERO shard vector; only later ADR-011 retirement removes it. Catalog absence therefore means never-populated-or-retired, with non-reuse/history owned by domain state/events. The Root Catalog is one unsharded instance of the pinned CKB/Poseidon engine keyed by `collection_key` and storing non-zero `R_collection` verbatim. Derive `R_sealed = P(TAG_SEALED_ROOT; commitment_scheme_version, catalog_root)`; the empty catalog root is ZERO but the sealed empty-system root must be non-zero. EVM `0xEE0D.slot1` is the sole final authority, while local MDBX stores/recomputes `catalog_root` and wrapper. One hierarchical candidate extracts/reuses ADR-009's validated canonical `ProvisionalShardSetBatch` inside each ordered changed collection, plus one aggregate Catalog batch; one canonical visitor owns bytes/size/checksum. `CE_MDBX_LOCAL_SCHEMA_VERSION = 3` uses typed namespaces `0x00` Catalog and `0x01 || collection_key || shard_BE4` collection shard with one root per namespace. Environment identity replaces one global shard count with direct canonical `CeTopologyV1` bytes covering domain IDs, partition policies, and K=16; the empty root cannot hide topology drift. First mint creates all K shard-root entries; finalization applies collection shards, Catalog, catalog root, and one last marker in a single transaction. No separate collection-root table or partial progress exists. Same-block collection creation/update is derived from ADR-007's journaled touched-entity final states; there is no second collection overlay. One immutable `AuthenticatedCatalogView` snapshot supplies all Catalog/collection/shard evidence for an execution scope. It opens only Catalog eagerly and lazily constructs/caches shard sessions for accessed present collections; untouched collections allocate nothing. A block-local cache retains only facts verified against parent `R_sealed`. Reads remain entity-overlay first, while end-block independently verifies aggregate touched-key and Catalog proofs; an absent parent collection plus all-final-ZERO new entities produces no catalog leaf.

This ADR introduces collection presence and empty-collection semantics but not bulk retirement.

### Working result

Tribute WWD, singleton NodItem, and singleton NodBucket collections update independently while every block still commits one `R_sealed`. Multi-domain mutations, including one Nod operation touching both singleton domains, produce the same result on proposer and validators.

### Accepted limitations

Finished Tribute partitions cannot yet be removed in one operation. Empty collections remain present; CE MDBX persists their current shard/catalog materialization. Nod uses two catalog leaves rather than one combined runtime-owned leaf.

### Verification

Run single-collection, multi-collection, empty-collection, cross-collection ordering, catalog proof-vector, and equal-root tests.

### Reset policy

After the combined ADR-003–010 suite, perform the one first CES1 testnet reset. With no collection leaves, genesis derives `catalog_root(0) = ZERO` and non-zero `R_sealed(0) = P(TAG_SEALED_ROOT; 1, ZERO)`. EVM `0xEE0D.slot1`, CE MDBX V3 `Catalog -> ZERO`, and the height-0 marker must match that derivation; no collections, synthetic body events, direct-map, unsharded, or pre-catalog state are migrated. The ADR-010 testnet genesis derives its topology identity with fork-fixed K=16. If ADR-017 later selects another production value, a complete pre-production reset/rebuild derives a new genesis and materializations; in-place preservation would require a new scheme/migration.

### Next unlocked step

Add a lifecycle operation that removes an entire completed partition safely.

---

## ADR-011 — `011-partition-retirement.md`

### Starting system

Root Catalog collections work, but there is no collection-level delete or finalized namespace reclamation. The current Lysis can skip Tributes that do not fit its allocation and still complete the WWD, even though no later reprocessing phase exists.

### Added capability

All-or-nothing Lysis completion followed by one-step Tribute WWD retirement.

### Decision

Treat the current successful `skip` as a bug. For every non-empty WWD, Lysis either creates exactly one Nod for every verified Tribute or fails under one checkpoint and preserves the complete source partition. Only the successful completion path may invoke the closed typed `retire_partition(TributeWwd(wwd))` operation; `FAILED`, active, singleton Nod, and absent never-populated collections do not retire. Successful bulk retirement zeros the retained sealed `DayTotals`, writes one minimal journaled end-block command, emits only `TributePartitionRetired(uint32 indexed worldwideDay)` at `TRIBUTE_ADDRESS`, and performs no per-entity body deletes/events. A present `EmptiedByDelete` collection retires normally, while a never-populated completed WWD produces no command, event, or root change. Retirement and entity mutations of the same collection are strictly mutually exclusive within a block. The testnet command is not read-aware: same-block point/list reads may still see exact-parent bodies until sealing, while mutations are rejected; production semantics are revisited with off-chain computation. End-block verifies exact-parent Catalog membership and the complete K=16 root vector, encodes an ordered `Retire` collection-operation variant, and applies one Catalog delete alongside any Nod collection updates. Finalization deletes all 16 CE MDBX shard prefixes/root entries, the Catalog leaf/root change, and the marker in one transaction before Marshal ACK. Mongo ExEx independently applies an idempotent finalized WWD range delete and never delays ACK. Generic state stores no permanent tombstone; the sealed zero `DayTotals`, monotonic WWD lifecycle, and canonical event own non-reuse/history.

### Working result

A successful Tribute WWD Lysis transforms its complete input to Nods and removes the present authenticated partition with one Catalog transition. Failed Lysis retains every Tribute and all local materialization; never-populated WWDs remain harmlessly absent. Nod and other Tribute collections continue atomically in the same block.

### Accepted limitations

Same-block reads after the testnet retirement request may return parent bodies. Synchronous finalized MDBX prefix deletion and the current verified-body Lysis path have no production resource closure; off-chain computation and ADR-017 revisit them. The root remains EVM-state-only until ADR-012, with proofs and portable recovery deferred.

### Verification

Run all-or-nothing Lysis regression tests for the current zero/over-allocation skips, failure injection after partial Nod work, present/empty/never-populated retirement, strict mutation exclusion, journal cleanup, one-event Mongo range deletion, mixed Tribute-retire/Nod-update root equality, cross-architecture vectors, and crash-safe atomic MDBX prefix reclamation before ACK.

### Reset policy

Activate through an additive testnet hard fork that advances the EVM overlay schema but preserves CES1 commitments and CE MDBX V3 layout. No dual path or body/tree migration is required; a full pre-production reset remains allowed. Review concrete bytes and field placement against the implemented ADR-010 seams before implementation.

### Next unlocked step

Expose the now-stable final root directly in the block header artifacts.

---

## ADR-012 — `012-header-root-carrier.md`

### Starting system

`R_sealed` is consensus-enforced through post-state EVM `0xEE0D.slot1` and finalized CE MDBX, but an external verifier needs an EVM storage proof merely to discover the root for a finalized block. The current proposer also encodes final `extra_data` before CE `end_block` returns its root.

### Added capability

A mandatory same-height finalized-header trust anchor for compressed-entity state.

### Decision

Add `OutbeBlockArtifacts` tag `0x08` with fixed payload `commitment_scheme_version_BE4 || R_sealed_BE32` (36-byte payload, 39 bytes with framing). Every reset-chain block `B >= 1` carries the post-state root produced by `SealOutput(B)` even when unchanged; genesis alone keeps empty `extra_data` and anchors `R_sealed(0)` through genesis EVM/CE state. The scheme field asserts the fork schedule rather than negotiating it: the activation block of a future scheme already labels its new-scheme post-state. Execution remains authority. The CE module exposes only `SealOutput.new_root`; the block builder strips any input/payload-attribute root, finalizes CE exactly once, inserts the local root with execution summary/timestamp, then assembles/hashes the standard Ethereum header and publishes the staged candidate under that hash. The structural OART codec owns framing while execution owns semantic validation: existing-block verification requires presence and the active scheme before execution, then requires header root = local SealOutput = post-state slot 1. Finalized persistence rechecks canonical header = historical EVM = candidate/replay root before writing the unchanged marker and ACKing; no duplicate header fields are added to CE MDBX. Reserve 39 bytes permanently inside the existing 64 KiB `extra_data` cap. Add no root-only RPC: standard block `extraData` plus the shared typed decoder is the carrier interface, and ADR-013 later binds proofs to the selected finalized header. Because all old testnet history is discarded, retain pre-production OART version `0x0A`; old binaries fail closed on unknown tag `0x08`, and no `0x0B`/dual decoder is consumed.

### Working result

Every accepted post-genesis block hash commits `{scheme(B), R_sealed(B)}` for that block's post-state. Proposer and validators reproduce one root, finalized CE persistence binds it to the same canonical header/EVM state, and external tools extract it directly from an ordinary finalized block without MongoDB or a dedicated RPC.

### Accepted limitations

The carrier alone does not prove finality or serve entity proofs/snapshots. Genesis remains a chain-spec/EVM special case, only scheme 1 is active, and the fixed record consumes 39 bytes of artifact budget. Cross-scheme migration is ADR-018 work.

### Verification

Pin codec/root/block-1 vectors; test missing/duplicate/wrong scheme/length/root, zero-change blocks, stale payload-root stripping, sync/parallel trie ordering, candidate hash/discard binding, proposer/validator state-root parity, exact 64 KiB-minus-39 limits, finalized header/EVM/candidate mismatch rejection before ACK, and extraction from standard block `extraData` without a new RPC.

### Reset policy

Include ADR-012 in a complete testnet reset. Keep OART `0x0A`, genesis without tag `0x08`, and require the tag from block 1. Discard old headers/materializations instead of adding an activation-height or dual-version compatibility path. Review concrete codec order and builder/finalizer seams against the implemented ADR-010/011 code before implementation.

### Next unlocked step

Serve inclusion/non-inclusion proofs and bind Mongo bodies to the finalized header root.

---

## ADR-013 — `013-proofs-and-verified-point-reads.md`

### Starting system

Finalized headers contain `R_sealed`, and the node retains the matching latest finalized tree in CE MDBX.

### Added capability

Independently verifiable entity reads.

### Decision

Define inclusion and non-inclusion proofs from entity identity through shard, collection root, Root Catalog, and finalized `R_sealed`. Add a point-read RPC that returns the body, proof, and selected block identity; scheme/root are derived only from the independently finalized header.

Distinguish `present`, `absent`, and `unavailable`. V1 has no caller-selected block/version API; unknown proof encodings are verifier errors rather than RPC result variants. An unverified secondary-index list does not claim completeness.

### Working result

A client requests a Tribute or Nod body from the node and verifies it independently against the selected finalized block header. Tampered bodies, identities, paths, roots, or block bindings fail verification.

### Accepted limitations

Proof generation serves the latest root-verified finalized CE MDBX snapshot. Historical proof generation is not guaranteed.

### Verification

Run valid inclusion/non-inclusion, tampered field/path/root, stale block, wrong identity, unavailable body, and multi-node proof-equivalence tests.

### Reset policy

A hard fork may be unnecessary if only RPC/proof transport is added. Testnet reset remains allowed if proof work exposes a tree-format defect.

### Next unlocked step

Complete exhaustive cross-store crash-window reconciliation and recovery evidence.

---

## ADR-014 — removed; folded into ADR-008

The former `014-persistent-smt-storage.md` roadmap stage is removed. CE-owned MDBX, atomic finalized node/marker commit, Reth durability ordering, and baseline restart behavior are required by the selected CKB engine from ADR-008 onward; deferring them would create an unusable intermediate authority model.

Number 014 remains reserved so later ADR references do not silently change. Exhaustive cross-store fault reconciliation remains ADR-015.

---

## ADR-015 — `015-crash-and-restart-reconciliation.md`

### Starting system

Clean restart uses persistent SMT state, but partial progress across Reth, SMT, Mongo, and finality is not fully recoverable.

### Added capability

Deterministic crash-window reconciliation.

### Decision

Define and implement the complete restart matrix:

- equal markers resume idempotently;
- SMT behind durable finalized Reth state replays missing canonical mutations;
- SMT ahead of durable chain state fails closed and rebuilds;
- same-height conflicting hash/root is corruption;
- Mongo and SMT cursor skew selects the body version matching proof height;
- gaps and parent mismatches trigger bounded replay or full resync.

### Working result

Fault injection at each durable boundary leads either to automatic bounded recovery or an explicit rebuild path. The node never serves a proof from an unverified checkpoint.

### Accepted limitations

A new node may still require replay from genesis. Recovery speed and body completeness are not yet solved by portable snapshots.

### Verification

Inject crashes before and after Reth persistence, SMT transaction, Mongo batch/checkpoint, and final acknowledgment. Verify all restart-matrix rows and proof readiness.

### Reset policy

No chain reset should be required. Derived Mongo/MDBX state may be deleted and rebuilt when corruption is detected.

### Next unlocked step

Bootstrap or repair a node from a verified checkpoint instead of replaying complete history.

---

## ADR-016 — `016-snapshots-and-bootstrap.md`

### Starting system

Existing nodes recover deterministically, but a new or fully rebuilt node may need full historical replay.

### Added capability

Portable verified tree/body snapshots and checkpoint bootstrap.

### Decision

Define semantic snapshot records for leaves and bodies, checkpoint identity, tree and body-coverage profiles, manifests, bounded chunks, resumable logical ranges, staged import, root verification, activation, and replay from snapshot height to head.

A snapshot source is not trusted. Imported state becomes active only after reconstructing the root committed by the independently selected finalized header.

### Working result

A clean node imports a snapshot at finalized height `H`, verifies it, replays `H+1..head`, resumes execution, and serves verified bodies/proofs without genesis replay.

### Accepted limitations

Historical body retention and arbitrary query completeness remain outside the guarantee. Body availability still requires at least one source possessing the requested bytes.

### Verification

Run clean bootstrap, interrupted resume, corrupt/missing/duplicate/out-of-order records, wrong checkpoint/root, partial body coverage, multi-source ranges, and replay-to-head tests.

### Reset policy

Snapshot format may change through testnet reset until production compatibility is required. Failed imports discard staging state.

### Next unlocked step

Measure the complete real storage path and replace conservative bounds with evidence-based limits.

---

## ADR-017 — `017-gas-quotas-and-performance-closure.md`

### Starting system

The complete Tribute/Nod path works from mutation through Mongo, SMT, header root, proofs, persistence, recovery, and bootstrap under conservative limits.

### Added capability

Measured liveness and capacity bounds.

### Decision

Benchmark the completed ADR-001–016 plus co-located off-chain-computation path on the minimum profile (Xeon E-2388G 8c/16t, 64 GiB, local NVMe, SGX support), select power-of-two `K_PRODUCTION` from provisional baseline 16, and fix deterministic body, CE work, transaction/block, staged-memory, worker/reader, candidate-cache, and persistence limits from evidence. Preserve the conservative target discovered during ADR-009 design (`>= 100` successful samples, `p99 <= 1.0s`, `max <= 1.5s`) but first settle which real lifecycle interval(s) it governs.

Use Criterion kernels plus a deterministic Rust full-path runner with versioned manifest/hardware/raw JSONL/generated reports. Revalidate the preferred `base + first-touched-shard + unique-key` CE work model, the candidate explicitly owned bounded worker/concurrent-seal direction, and the per-candidate/total byte-and-record cache envelope against actual pinned Reth concurrency. These are retained design directions, not retroactive ADR-009 implementation requirements.

### Working result

A saturated block remains within the accepted execution/certification budget, proposer and validator enforce identical limits, and oversized work fails or defers deterministically.

### Accepted limitations

The limits describe the tested hardware floor and current implementation. The strict gate may lower usable CE capacity, controlled cold runs may be expensive/disruptive, and explicit worker/cache envelopes increase local configuration complexity. Changing hardware floor, tree/hash/persistence/execution paths, allocator-sensitive cache structures, or concurrency wiring requires new measurements.

### Verification

Produce the versioned two-layer benchmark artifacts with hardware profile, commands, deterministic datasets/checksums, raw samples, generated summaries, root/batch equality, worst-case concentrated and uniform workloads, warm/cold modes, phase timings, RSS/I/O/staged-record growth, proof/finalization contention, and safety margin. Verify proposer/validator CE accounting, defer/revert boundaries, any retained sequential/parallel path, cache envelopes without implicit eviction, and a sustained multi-node testnet workload at the accepted limits.

### Reset policy

Limit changes may use a hard fork and testnet restart. They are not a separate production activation gate.

### Next unlocked step

The first Tribute/Nod storage system is complete. Further ADRs are triggered by real evolution requirements rather than hypothetical future flexibility.

---

# Future-triggered evolution

ADRs 018 and 019 are listed so the future seam is known. They are not prerequisites for ADR-001–017 and should not be designed in detail until their trigger exists.

## ADR-018 — `018-versioned-format-evolution.md`

### Trigger

The first requirement to preserve existing production state while changing body schema, hash rules, proof encoding, or commitment/tree semantics.

### Added capability

Independent compatibility and migration rules for real format evolution.

### Decision direction

Separate `schema_version`, `hash_version`, `proof_encoding_version`, and `commitment_scheme_version`. Specify migration only for the concrete change being introduced; do not build a generic migration framework in advance.

### Working result

Old and new production data remain readable/verifiable according to the chosen transition without resetting the production chain.

### Accepted limitations

Before this trigger, testnet formats may continue to change through reset and only the current format must be supported.

### Verification

Use cross-version fixtures, migration/replay tests, and deterministic fork-boundary execution for the actual version change.

### Reset policy

This ADR exists specifically because production state can no longer be discarded. Its migration policy is defined by the triggering change.

### Next unlocked step

Preserve real historical formats while adding new schemas or commitment behavior.

---

## ADR-019 — `019-domain-registry-and-later-domains.md`

### Trigger

A real additional domain or multiple simultaneously supported runtime/domain versions, for example Gem onboarding after Tribute/Nod.

### Added capability

Fork-governed domain registration and version resolution.

### Decision direction

Register concrete domain identity, runtime entrypoints, ID encoding, partition policy, shard count, active body/hash versions, lifecycle extensions, gas profile, and activation height. Callers cannot freely choose inactive or obsolete definitions.

### Working result

The new domain uses the proven storage facade, event, commitment, SMT, proof, persistence, and recovery path without changing Tribute/Nod callers.

### Accepted limitations

No registry is introduced merely to represent two fixed genesis domains. The registry solves only real multi-domain/version variability.

### Verification

Run old/new-domain isolation, wrong-domain identity, activation-boundary, root equality, projection, proof, restart, and snapshot tests.

### Reset policy

Before production the new domain may use a testnet reset. After production, ADR-018 governs compatibility when existing state must survive.

### Next unlocked step

Onboard additional domains and runtime versions through one explicit fork-governed mechanism.

---

## Explicit non-goals of the index

- Requiring the security guarantees of ADR-013 while implementing ADR-005.
- Designing production migration before production data exists.
- Introducing a domain registry before a real third domain/version requires it.
- Treating every hash formula or storage field as a separate ADR.
- Combining Mongo projection, SMT, sharding, persistence, proofs, and recovery into one implementation step.
- Keeping a permanent legacy EVM body path after the Mongo execution cutover.
- Pretending that Mongo is independently trustworthy before commitments and finalized proofs exist.
- Requiring a long-lived shadow architecture before testing a step locally or on a resettable testnet.

## Historical input

[The proposed compressed-entities v6.1 concept](../compressed_entities_concept_v6_proposed_10-07-2026.md) remains the end-state design input. It describes the target system, not the required implementation order. This index is the normative decomposition for processing and implementing one ADR at a time.
