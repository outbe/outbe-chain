# T15 — CE-owned MDBX environment and atomic marker commit

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §9.1, §13.2 (Q16, Q23)
Depends on: T03
Blocks: T12 (read-view API), T14, T16, T18, T21, T22

## Summary

Create the separate CE-owned MDBX environment at `<datadir>/compressed_entities/smt/`: namespaced collection
shard tables, Root Catalog tables, identity metadata, the atomic nodes+roots+`last_applied` commit, and read
snapshots for proof serving.

## Context

The environment does not extend or share Reth's primary database. Nodes are namespaced by
`{collection_key, shard_index}`; the Root Catalog SMT has its own namespace. One transaction per finalized
block writes all changed tree nodes, all current collection shard roots / collection-top metadata / Root
Catalog metadata, and the complete `last_applied` marker `{scheme_version, height, block_hash,
parent_block_hash, parent_root, new_root}`. The transaction requires contiguous height and matching
parent block/root/scheme; same-marker re-apply is an idempotent no-op; a conflicting marker is corruption.
Identity metadata binds `{chain_id, genesis_hash, commitment_scheme_version}`; mismatch fails startup.

## Scope

- Environment lifecycle: open/create at the fixed path, own schema/table set, map-size and capacity checks,
  single-writer discipline, growth policy.
- Table design: node tables namespaced by `{collection_key, shard_index}`, catalog tables, roots/metadata,
  marker table; T03 store-trait binding.
- Atomic commit API: `apply_finalized(batch, marker) -> Ok | IdempotentNoop | Corruption | GapOrMismatch`
  with the §13.2 precondition checks.
- Identity metadata write at init; verified every startup.
- Read snapshots: one read transaction spanning marker + all nodes/metadata for a proof package (10.4
  single-snapshot discipline) — API consumed by T18.
- NAMED design outputs (audit §7/T15, dependency change 7): the tree-backend **read-view API** (parent
  tree + staged-ancestor composition consumed by T12's `ParentTreeView` production impl), the
  staged-batch API and its table encoding, and the environment growth policy — written down as reviewable
  design artifacts of this task, not discovered during T12. The staged-batch table encoding IMPLEMENTS the
  normative grammar owned by the T30 wire spec (postfix PF-M07) — the canonical serialized footprint in
  which `max_staged_tree_bytes` is measured (audit-final H-12); golden vectors pin the implementation to
  the grammar.
- Typed local storage failures (audit-final H-13): disk-full, MDBX map-full, growth failure, and I/O
  errors surface as typed local outcomes (never panics, never silent stalls); a failed commit leaves
  marker/nodes untouched (single-tx atomicity) and is safely retryable; T16 owns the ACK/lease posture
  for these failures.
- Namespace reclamation hook for retired collections — background-safe deletion by namespace prefix,
  permitted only after the CE `last_applied` marker has COMMITTED the retirement block (audit-final M-02:
  marker commit implies finality via the T16 barrier; consensus finality alone is NOT sufficient — the
  tree may still lag); read snapshots opened before a reclamation remain consistent (MDBX snapshot
  isolation), tested.
- Explicit note per spec: copying only this directory is not a consistent full-node backup.

## Out of scope

- The barrier/ordering coordinator (T16); snapshot network format (T22).

## Acceptance criteria

1. Atomicity: crash injection between nodes and marker is unrepresentable (single tx); partial states never
   observed on reopen.
2. Precondition matrix: gap height, wrong parent hash/root, wrong scheme → typed failures; same complete
   marker → idempotent no-op.
3. Identity mismatch (chain_id/genesis/scheme) fails startup.
4. Concurrent proof-read snapshot during a commit sees a consistent {marker, nodes} pair (torn-read test,
   §19.14 concurrent proof-serving/tree-commit).
5. Retired-namespace reclamation removes bytes without touching live namespaces (§19.14 reclamation item);
   reclamation before the marker covers the retirement height is unrepresentable; a read snapshot opened
   pre-reclamation still serves consistently (audit-final M-02).
6. Capacity fault tests (audit-final H-13): induced disk-full/map-full/growth/I/O failures produce the
   typed outcomes; retry after space is freed succeeds; no partial commit is ever observed.

## Invariants

- Single writer; readers use MDBX snapshot isolation; marker and nodes always move together.

## Tests

- Crash/reopen tests, concurrency tests, MDBX growth smoke (pairs with T24 benchmarks).

## Files

- `crates/core/compressed_entities/src/persist/{env.rs,tables.rs,commit.rs,snapshot.rs}`
