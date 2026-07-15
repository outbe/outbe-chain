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

Automatic receipt-to-Mongo projection through Reth ExEx.

### Decision

Install a Reth ExEx that processes canonical body events in block, transaction, and log order. Apply idempotent block batches through the storage facade and persist a `{height, block_hash}` checkpoint. Handle canonical replacement or replay according to the pinned Reth notification model.

ExEx materializes accepted events; it does not rerun Tribute/Nod business rules.

### Working result

Running the node and submitting Tribute/Nod transactions automatically creates, updates, and deletes Mongo bodies and indexes. Deleting Mongo and replaying retained events reproduces the same materialization. Runtime reads still use EVM.

### Accepted limitations

Mongo is not yet used by Lysis or other domain logic. Projection correctness is not cryptographically checked, and failure affects only the new materialization.

### Verification

Run real node integration tests for upsert, delete, duplicate delivery, restart from checkpoint, and canonical replacement. Compare the memory and Mongo projector results.

### Reset policy

Mongo may be dropped and rebuilt. A chain reset is unnecessary unless ADR-003 event format changed simultaneously.

### Next unlocked step

Switch body-dependent execution and query paths to the populated repository.

---

## ADR-005 — `005-mongo-execution-reads.md`

### Starting system

Mongo is continuously populated from receipts, but all consensus/domain body reads still use EVM records.

### Added capability

The first complete off-chain Tribute/Nod runtime.

### Decision

Switch Lysis, Tribute processing/burn, NodFactory mining/payment, Gratis inputs, metadata, and body/query reads to the typed facade backed by MongoDB. Remove active full per-entity EVM body storage while retaining only the protocol aggregates and control structures identified in ADR-002.

Missing, malformed, unavailable, or lagging rows fail explicitly. They are not interpreted as absence and do not fall back to a hidden EVM body source.

### Working result

A Tribute or Nod is created, published in a receipt, projected by ExEx, read from Mongo by domain logic, and consumed by the next operation. Lysis and Nod-to-Gratis flows complete through the off-chain facade.

### Accepted limitations

MongoDB is deliberately an execution dependency. A well-formed altered row cannot yet be compared with a consensus commitment. There is no SMT, proof service, authenticated list completeness, automatic recovery, or production availability guarantee.

These limitations are normal for this stage.

### Verification

Run end-to-end Tribute -> ExEx -> Mongo -> Lysis and Nod -> ExEx -> Mongo -> mining -> Gratis flows on locally running nodes. Exercise missing, malformed, unavailable, and lagging rows.

### Reset policy

Use a coordinated hard fork and complete testnet reset. Legacy per-entity EVM bodies do not require migration.

### Next unlocked step

Give every body a deterministic commitment and compare Mongo reads against it.

---

## ADR-006 — `006-body-commitment-and-verification.md`

### Starting system

All body-dependent logic reads MongoDB through the facade, but a well-formed altered body is not detectable.

### Added capability

Deterministic body commitments and verification on real Mongo reads.

### Decision

In one functional step, define:

- canonical Tribute and Nod body bytes;
- the hash suite and byte-to-field/hash rules;
- canonical entity identity;
- the leaf commitment binding identity and body.

Store the current per-entity commitment in a simple EVM mapping. Emit the same commitment with the body event. On every body-dependent read, canonicalize the returned body, derive its identity and commitment, and compare it with EVM state before use.

The commitment format is deliberately chosen so it can become the future SMT leaf.

### Working result

Lysis, Tribute, NodFactory, and Gratis continue to read Mongo, but modified, wrong-identity, or stale bodies fail commitment verification.

### Accepted limitations

Commitments still consume one EVM entry per entity. There is no global current-state root, membership proof, sharding, or scalable tree.

### Verification

Use golden body/identity/leaf vectors and end-to-end runtime tests. Mutate every body field, entity ID, and stored commitment independently and prove that reads fail.

### Reset policy

Use a hard fork and testnet reset for the new EVM layout and canonical event/body format. No old body migration is required.

### Next unlocked step

Move Tribute and Nod mutations behind one generic commitment lifecycle while preserving EVM journaling semantics.

---

## ADR-007 — `007-generic-lifecycle-and-journaled-overlay.md`

### Starting system

Per-domain code writes and verifies per-entity commitments, but generic existence, same-block, and rollback behavior is not centralized.

### Added capability

One generic `mint/update/delete` lifecycle with deterministic same-block behavior.

### Decision

Introduce a generic compressed-entity mutation facade over the current EVM commitment mapping. It owns present/absent checks, `Set/Delete` pending values, unique touched keys, read-your-write behavior, repeated mutation rules, nested-call and transaction rollback, and end-block cleanup.

Existing EVM journaling keeps body events, commitment changes, and domain writes in the same revert scope.

### Working result

Tribute and Nod use the same lifecycle for commitment mutation. Same-block create/update/delete sequences and reverted transactions produce deterministic commitments and events while the node still uses the simple mapping backend.

### Accepted limitations

The mapping remains unscalable and has no Merkle root or proof. Partition retirement is not available.

### Verification

Run mutation-sequence, same-key, same-block, nested revert, failed transaction, and proposer/validator equivalence tests against the active runtime.

### Reset policy

A hard fork/testnet reset is allowed for the journal layout. Mongo can be rebuilt from the new events.

### Next unlocked step

Replace the simple commitment mapping with one authenticated tree without changing domain callers.

---

## ADR-008 — `008-basic-unsharded-smt.md`

### Starting system

Generic lifecycle and journaled mutations work over an EVM commitment mapping.

### Added capability

One consensus-enforced unsharded sparse Merkle tree.

### Decision

Replace the per-entity commitment mapping as the current-state authority with one unsharded SMT. The existing leaf commitment becomes the tree value. End-block sealing consumes the final journaled mutation set, updates the tree, writes the resulting root to an EVM root slot, and cleans the overlay atomically.

The initial tree may be in memory or a simple rebuildable local store. On restart, replay from genesis is acceptable.

### Working result

Locally running proposer and validator nodes execute Tribute/Nod flows, calculate the same SMT root, and reject a block whose EVM root does not match deterministic execution.

### Accepted limitations

One tree has no sharding, collection isolation, Root Catalog, header artifact, durable persistence, fast restart, or proof RPC. Restart may require full replay.

### Verification

Run reference-model and differential SMT vectors, multi-node root-equality tests, delete/non-membership tests, full replay, and invalid-root rejection.

### Reset policy

Use a coordinated hard fork and complete testnet reset. The previous commitment map is not migrated.

### Next unlocked step

Improve tree scalability without changing lifecycle, leaf, or domain repository interfaces.

---

## ADR-009 — `009-smt-sharding.md`

### Starting system

One unsharded SMT produces a consensus EVM root and works correctly under local/testnet load.

### Added capability

Fixed power-of-two SMT sharding.

### Decision

Split the tree into a fixed number of shards selected deterministically from `tree_key`. Define shard namespaces, independent shard updates, deterministic shard-root aggregation, and parallel preparation where safe. Preserve worst-case correctness when all mutations hit one shard.

### Working result

The same Tribute/Nod operations and leaf commitments produce a sharded consensus root. Multi-node execution agrees, and load tests demonstrate better working-set behavior or throughput than the unsharded stage.

### Accepted limitations

Collections and partitions are not yet independently represented. The shard count is fixed for this stage, and changing it requires a reset.

### Verification

Run equal-root tests across architectures, all-in-one-shard adversarial workloads, parallel/sequential equivalence, and comparison with the unsharded logical map.

### Reset policy

Use a hard fork and complete testnet/tree reset because tree topology and root derivation change.

### Next unlocked step

Group shards into independent domain/partition collections and commit them under one root.

---

## ADR-010 — `010-collections-and-root-catalog.md`

### Starting system

A sharded tree works, but all Tribute and Nod entities share one logical collection/root structure.

### Added capability

Independent collections combined by a Root Catalog.

### Decision

Define collection identity for Nod and Tribute partitions, calculate one root from each collection's shard roots, and store collection roots as leaves of a Root Catalog SMT. Derive one final `R_sealed` from the catalog root.

This ADR introduces collection presence and empty-collection semantics but not bulk retirement.

### Working result

Tribute WWD collections and the Nod collection update independently while every block still commits one `R_sealed`. Multi-domain mutations produce the same result on proposer and validators.

### Accepted limitations

Finished Tribute partitions cannot yet be removed in one operation. Empty collections remain present, and local tree state is still replay-based.

### Verification

Run single-collection, multi-collection, empty-collection, cross-collection ordering, catalog proof-vector, and equal-root tests.

### Reset policy

Use a hard fork and testnet/tree reset because collection keys and final root topology change.

### Next unlocked step

Add a lifecycle operation that removes an entire completed partition safely.

---

## ADR-011 — `011-partition-retirement.md`

### Starting system

Root Catalog collections work, but deleting all entities of a completed Tribute partition requires individual mutations or leaves an empty collection.

### Added capability

One-step partition retirement.

### Decision

Add `retire_partition` as a domain-authorized generic lifecycle operation. Retirement removes the collection leaf from the Root Catalog, makes every contained entity absent from the new current root, forbids partition reuse, emits one canonical retirement event, and allows physical namespace reclamation after finality.

### Working result

A completed Tribute WWD partition can be retired in one block without per-entity deletes. Nod and active Tribute collections continue to work unchanged.

### Accepted limitations

The root is committed through EVM state only. There is still no direct header carrier, proof RPC, durable tree storage, or snapshot recovery.

### Verification

Run active/empty/retired distinction tests, repeated/unauthorized retirement, post-retirement access, non-reuse, Mongo range deletion, and root-equality tests.

### Reset policy

A hard fork is expected. A full reset is allowed if the retirement encoding or catalog layout changes.

### Next unlocked step

Expose the now-stable final root directly in the block header artifacts.

---

## ADR-012 — `012-header-root-carrier.md`

### Starting system

`R_sealed` is consensus-enforced through EVM state, but an external verifier needs an EVM state proof to obtain it from a finalized block.

### Added capability

A direct finalized-header trust anchor for compressed-entity state.

### Decision

Add `R_sealed` and its current commitment-scheme identifier to `OutbeBlockArtifacts`. The proposer publishes the root computed by execution; validators recompute it and require exact equality with both the header artifact and EVM root slot.

### Working result

Every accepted block carries a directly extractable compressed-entity root. A mismatched proposer root is rejected, and external tools can select a finalized root without reading MongoDB.

### Accepted limitations

The node does not yet expose entity proofs. Tree state is still replayable rather than durably checkpointed.

### Verification

Run proposer/validator parity, missing/wrong artifact, EVM/header mismatch, block hash, and finalized-root extraction tests.

### Reset policy

Use a hard fork and testnet reset for the artifact-envelope change.

### Next unlocked step

Serve inclusion/non-inclusion proofs and bind Mongo bodies to the finalized header root.

---

## ADR-013 — `013-proofs-and-verified-point-reads.md`

### Starting system

Finalized headers contain `R_sealed`, and the node can reconstruct the current tree through replay.

### Added capability

Independently verifiable entity reads.

### Decision

Define inclusion and non-inclusion proofs from entity identity through shard, collection root, Root Catalog, and finalized `R_sealed`. Add a point-read RPC that returns the body, proof, selected block identity, and required commitment metadata.

Distinguish `present`, `absent`, `unavailable`, and `unsupported`. An unverified secondary-index list does not claim completeness.

### Working result

A client requests a Tribute or Nod body from the node and verifies it independently against the selected finalized block header. Tampered bodies, identities, paths, roots, or block bindings fail verification.

### Accepted limitations

Proof generation depends on the current in-memory/replayed tree. Restart may require full replay, and historical proof generation is not guaranteed.

### Verification

Run valid inclusion/non-inclusion, tampered field/path/root, stale block, wrong identity, unavailable body, and multi-node proof-equivalence tests.

### Reset policy

A hard fork may be unnecessary if only RPC/proof transport is added. Testnet reset remains allowed if proof work exposes a tree-format defect.

### Next unlocked step

Persist the working proof-capable tree so normal restart no longer requires genesis replay.

---

## ADR-014 — `014-persistent-smt-storage.md`

### Starting system

The authenticated tree and proofs work, but local tree state must be replayed after restart.

### Added capability

Durable finalized SMT checkpoints.

### Decision

Add a CE-owned MDBX environment. Persist changed tree nodes, shard/collection/catalog metadata, and one complete `last_applied` marker atomically after the corresponding finalized Reth state is durable. Bind the environment to chain and genesis identity.

This ADR establishes the normal finalized commit path. Exhaustive crash reconciliation is ADR-015.

### Working result

After a clean restart, the node opens the persisted tree at its finalized marker, verifies the stored root against chain state, resumes execution, and immediately serves current proofs without replay from genesis.

### Accepted limitations

Interrupted commits and cursor disagreement have only basic fail-closed handling. Automatic recovery of every crash window is not yet implemented. Portable snapshots do not exist.

### Verification

Run clean shutdown/restart, atomic transaction, same-marker idempotency, wrong chain/genesis, root mismatch, and proof-after-restart tests.

### Reset policy

Local MDBX may be deleted and rebuilt from chain replay. A chain reset is not required unless persistence reveals a consensus-format defect.

### Next unlocked step

Define deterministic recovery for every relationship between Reth, SMT, Mongo, and finality progress.

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

Benchmark the real worst-case path on minimum supported validator hardware. Fix deterministic body-byte, mutation-attempt, unique-key, per-transaction, per-block, staged-memory, and deferred-seal gas limits from measured results. Include worst-case single-shard concentration and concurrent persistence/proof work.

### Working result

A saturated block remains within the accepted execution/certification budget, proposer and validator enforce identical limits, and oversized work fails or defers deterministically.

### Accepted limitations

The limits describe the tested hardware floor and current implementation. Changing tree/hash/persistence paths requires new measurements.

### Verification

Produce a reproducible benchmark report with hardware profile, commands, datasets, raw results, worst-case workloads, and safety margin. Run a sustained multi-node testnet workload at the accepted limits.

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
