# T07 — CompressedEntityStore: 0xEE0B overlay and generic lifecycle

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §3.1, §6.1, §8.1 (Q2, Q5, Q8, Q21, Q23)
Depends on: T02, T05, T06, T31 (provisional bounds)
Blocks: T08, T09, T10, T12, T23, T33 (same-block predicate)

## Summary

Implement the store core: fixed storage layout v1 at `0xEE0B` (slots 0–5), the journaled execution overlay
for entities and retired collections, and the generic lifecycle `mint/update/delete/retire_partition` plus
`read_commitment`.

## Context

The overlay lives in ordinary journaled EVM storage so existing journaling supplies nested-call revert,
failed-tx revert, cross-tx read-your-write, and log+mutation rollback together. Layout v1: slot 0 schema
version (=1, immutable), slot 1 `last_sealed_root` (only persistent semantic value), slot 2 pending entity
map base, slot 3 `touched_entities` StorageVec base, slot 4 pending retired-collection map base, slot 5
`touched_collections` StorageVec base. Pending word encoding: `0 = Untouched`, `1 <= w < p = Set(BE32(w))`,
`U256::MAX = Deleted`, `p <= w < MAX = invalid` (deterministic block-execution failure).

## Scope

- Module skeleton per repo module-structure standard: `schema.rs` (layout v1), `state.rs` (overlay CRUD),
  `runtime.rs` (lifecycle orchestration), `errors.rs`; generated via `#[contract]`/`#[storage_schema]`
  macros with explicit `StorageHandle`.
- Lifecycle per §6.1: mint (require absent, leaf != ZERO), update (require present, leaf != ZERO), delete
  (require present), retire_partition (policy allows, partition exists, not already retired).
- Overlay semantics per §8.1: first-touch appends composite locator `(collection_key, tree_key)` to
  `touched_entities` once; repeated mutations replace pending value only; retirement appends to
  `touched_collections`, makes the whole collection absent for subsequent same-block reads, rejects later
  entity mutation in that collection; begin-block requires both touched vectors empty.
- `read_commitment(domain_id, raw_id) -> leaf_value | absent`: overlay read (pending Set → value, pending
  Deleted / retired collection → absent, Untouched → fall through to a `ParentTreeView` trait DEFINED HERE
  and stubbed in tests; T12 supplies the production implementation over persisted tree + staged ancestors
  (breaks the T07→T12 reference cycle).
- `ParentTreeView` also exposes collection presence (`collection_has_leaf(collection_key)`) — needed by
  `retire_partition`'s "partition exists" precondition (audit P1-1). The missing-vs-retired distinction is
  DOMAIN-owned (core stores no tombstone; Root Catalog absence cannot distinguish never-populated from
  retired): the domain runtime gates retirement on its own lifecycle state, core validates only current
  catalog presence.
- Same-block retirement matrix (normative behavior, tested): mint→retire (entities minted this block in
  the collection become absent; retirement wins), mutate→retire (same), retire→mutate (rejected — §8.1
  "rejects later entity mutation in that collection"), retire→retire (rejected, already retired in
  overlay).
- Current collection existence algorithm (audit v2 P1-5; matrix pinned per audit v3 P1-12): a collection
  currently exists iff (parent catalog leaf present) OR (a pending `Set` for the collection exists NOW in
  the overlay — derivable from journaled touched/pending state without unbounded scans). Normative
  matrix, each row tested:
  parent absent + mint pending Set        → retire ALLOWED;
  parent absent + mint→delete (Deleted)   → retire REJECTED (never materialized);
  parent present + delete-all this block  → retire ALLOWED (leaf present until seal);
  parent absent + mint REVERTED           → retire REJECTED (no pending entry).
  Never-populated ACTIVE partition (postfix PF-H09): a domain-opened partition with no catalog presence
  cannot be retired THROUGH THE CORE (the matrix's REJECTED rows — there is no leaf to remove); its
  retirement is DOMAIN-STATE-ONLY: no core call, no `PartitionRetiredV1` event; the domain's
  `ActiveTributePartitionsView` excludes it at the retiring checkpoint and T33's coverage comparison
  treats it as retired.
- Per-operation body-size limit enforcement point (audit v5 P0-4 ownership): the store rejects a body
  exceeding the registered per-domain limit (value from T31/T30 registry) at `mint/update` entry — this
  is T07/T23's slice of the D2 bounds table.
- Partition-list protection is NOT a T07 predicate in Stage 1 (audit v5 P0-3 decided ordering invariant,
  owned by T09/T33); the journaled `collection_mutated_this_block` dirty predicate is the documented
  fallback and would land here if the invariant fails in implementation.
- Same-block body-dependence guard (owner decision, T29/T33): expose a cheap predicate
  `entity_mutated_this_block(collection_key, tree_key)` over the journaled pending/touched state so the
  T33 adapters can raise `SameBlockBodyUnavailable` BEFORE any Mongo read; a reverted mutation
  leaves no pending entry and therefore no ban.
- Same rationale at collection level (audit-final H-09): expose `collection_retired_this_block(collection_key)`
  over the journaled retired-collection overlay so T33's UNIFIED body-read guard covers partition
  retirement, not only entity-level mutation; a reverted retirement leaves no entry and no ban.
- Same-leaf update remains a successful ordered operation.
- Derivations via T02/T06 (registry-resolved); caller cannot supply any derived locator or version.

## Out of scope

- Event emission (T08), dispatch guard (T09), attempt/gas reserve (T10), sealing (T12), SMT persistence (T15).

## Acceptance criteria

1. Lifecycle matrix tests: `mint→update→delete` finishes absent; `delete→update`, repeated delete, mint-on-
   present, update-on-absent all reject; retire on missing/already-retired partition rejects (§19.6).
2. Pending-word codec tests: all four ranges incl. invalid-range deterministic failure and `U256::MAX`
   non-collision with leaves, COMMITTED as consensus golden-vector fixtures under `tests/vectors/`, not
   only unit assertions (§19.2 pending-slot item).
3. Revert tests: nested-call revert, failed tx, OOG leave no pending entries, no touched growth (§19.5).
4. Same-block read-your-write across transactions; retired collection absent for later same-block reads.
5. Post-seal cleanliness contract (consumed by T12): helper asserting slots 2–5 structurally empty.
6. Collection-existence matrix (audit v4 P1-6): all four normative rows tested (pending-Set/allowed,
   mint→delete/rejected, delete-all/allowed, reverted-mint/rejected).
6b. Per-operation body-size bound (audit v6 P1-6): exactly-at-limit accepted; limit+1 rejected BEFORE
   hashing/journal/event work with no attempt/event/overlay mutation; proposer/validator parity. The store
   invokes the NEUTRAL resource seam at two fixed points — pre-hash (Stage A) and post-derivation/
   pre-journal (Stage B) — tested here against a STUB guard only (invocation points + rollback callback);
   the real Stage A/B ordering, reservation, and rollback semantics are T10 AC6's (audit-final B-01: T07
   acceptance must not depend on downstream T10). A body-size rejection consumes NO attempt slot and NO
   charge — the attempt did not start (postfix PF-H03, amended §15.1).
7. `entity_mutated_this_block` lifecycle: TRUE after a successful mutation; FALSE after a reverted one;
   remains TRUE through a same-block Set→Deleted sequence; RESETS after the seal cleanup so the next
   block starts unbanned (critical for the T33 same-block ban).
7b. `collection_retired_this_block` lifecycle (audit-final H-09): TRUE after a same-block retirement,
   FALSE after a reverted one, resets after seal — consumed by T33's unified same-block guard.

## Invariants

- Slot 1 is the only persistent semantic value changed by ordinary sealing.
- `Deleted` exists only in the overlay; never persisted to SMT or finalized EVM state.
- Determinism: identical op sequences produce identical overlay state and touched orders.

## Tests

- Unit + state tests per matrix above; proptest interleavings of mutations/reverts vs a model map.

## Files

- `crates/core/compressed_entities/src/{schema.rs,state.rs,runtime.rs,errors.rs,lib.rs}`
- `crates/core/compressed_entities/tests/vectors/pending_word_*.json`
