# T08 — Canonical mutation events (WriteV1 / DeleteV1 / PartitionRetiredV1)

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §7 (Q4, Q23)
Depends on: T07, T30 (event ABI)
Blocks: T09 (canonical events for the system-mutation AC), T10 (`encoded_len` metering input), T17 (rebuild decoder), T20, T23

## Summary

Emit exactly one canonical receipt-visible event per successful core operation from `0xEE0B`, in the same
journaled scope as the mutation, in the three discriminated forms of §7.2.

## Context

Canonical events are the only generic input for ExEx/MongoDB rebuild and event-based recovery. For
mint/update the event `body` is byte-identical to the canonical bytes hashed into `leaf_value`. Delete
carries `{domain_id, partition_key_or_none, id_bytes}` (amended shape — the spec's original
`{domain_id, id_bytes}` form is insufficient for partitioned domains, see the amendment block below).
Partition retirement carries canonical `{domain_id, partition_key}` only. Event format is versioned by
signature/topic; historical decoders remain available after upgrades.

## Scope

- Event definitions in the module's canonical interface (`contracts/precompiles/src/ICompressedEntities.sol`
  per repo events convention, imported via `sol!` in `precompile.rs`):
  `CompressedEntityWriteV1(domain_id, partition_key_or_none, id_bytes, operation, schema_version, hash_version, leaf_value, body)`,
  `CompressedEntityDeleteV1(domain_id, partition_key_or_none, id_bytes)`,
  `CompressedEntityPartitionRetiredV1(domain_id, partition_key)`.
- **Spec amendment #1 (§7.2) — APPLIED to the concept**: `partition_key_or_none` (empty for Singleton) is
  carried in both forms because `partition_key` is not derivable from the hash `id_bytes` and `DeleteV1`
  has no body to consult. Consumer verification contract (per amended §7.2): validate the field's canonical
  SHAPE against the fork-active partition policy (presence, length, encoding) and derive
  `collection_key`/`tree_key` from the event alone. The `raw_id → partition_key` binding is guaranteed by
  the consensus-validated core emitter and is NOT independently re-checkable from the event (no `raw_id`
  in the event; adding it was considered and rejected as unnecessary).
- Emission wired inside T07 lifecycle success paths — one event per successful op, same journaled scope
  (revert/OOG/failed nested call leaves neither mutation nor event).
- Decoder library for projection/recovery consumers (T20, T22, T17 rebuild path) with fail-closed handling
  of unknown versions and malformed payloads.
- Canonical `encoded_len` helper (audit-final B-01): the canonical event encoder exposes the exact encoded
  byte length consumed by T10's `CeResourceUsageDelta` metering — single owner; T10 never recomputes event
  sizes independently.
- Emitter-address discipline documented for consumers: only `0xEE0B`-emitted events are canonical; same
  signature from another address is ignored (enforced in T20's projector filter).
- `PartitionRetiredV1` exists only for CORE retirement of a present collection (postfix PF-H09): a
  never-populated active partition retires domain-state-only and emits NO canonical event — projections
  and coverage views learn it from domain state (`ActiveTributePartitionsView`), not from the event
  stream.

## Out of scope

- Projector logic (T20); raw-hook rejection tests (T09 owns the entrypoint surface, §7's raw-hook rule).

## Acceptance criteria

1. Byte-identity test: event `body` equals the exact hashed canonical bytes (§19.6 coherence).
2. One-event-per-op: multi-mutation tx yields ordered logs per op; reverted subcall emits nothing (§19.5).
3. Delete/retirement events carry no body/leaf/version surplus fields (shape tests); `partition_key_or_none`
   is empty for Singleton and shape-valid per the fork-active partition policy for Partitioned (consumer
   replay test reconstructs `collection_key`/`tree_key` from the event alone for both domain kinds — no
   raw_id-derivation check, which is emitter-guaranteed).
4. Decoder fail-closed tests: unknown version topic, truncated payload, wrong-emitter filtering (§19.10 inputs).
5. Golden vectors: committed consensus fixtures for the full encodings of all three event forms
   (`WriteV1`/`DeleteV1`/`PartitionRetiredV1`) under `tests/vectors/` — the §19.2 "event" gate.
6. `encoded_len` equals the actual emitted encoding length for all three forms at boundary sizes
   (metering input for T10 — audit-final B-01).

## Invariants

- Mutation and event are atomic under the journal; no mutation without event, no event without mutation.
- Event order is `block_number → transaction_index → log_index_in_receipt`.

## Tests

- Execution-level tests through the EVM journal (not just unit emit calls), incl. OOG mid-tx.

## Files

- `contracts/precompiles/src/ICompressedEntities.sol`
- `crates/core/compressed_entities/src/{precompile.rs (sol! import), events_decode.rs}`
