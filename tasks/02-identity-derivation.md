# T02 — Canonical identity: id_f, collection_key, tree_key, leaf_f

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §4.1, §4.3, §5.2 (Q6, Q20, Q23)
Depends on: T01, T30 (concrete encoding kinds, provisional K vectors)
Blocks: T04, T06, T07

## Summary

Implement the canonical identity and leaf derivation chain for singleton and partitioned domains:
`id_f`, `partition_key` validation, `collection_key_f`, `tree_key_f`, shard index, and `leaf_f`.

## Context

Q23 introduced collections: `tree_key_f = P(TAG_KEY; scheme_version, collection_key_f, id_f)` where
`collection_key_f = PBytes(TAG_COLLECTION_KEY, domain_id_be2 || partition_presence_u8 || partition_key_len_be4 || partition_key)`.
Singleton domains have `partition_presence = 0`, empty key. Shard index is `low_k_bits(tree_key_f)` with
per-domain `K_domain = 2^k`. `leaf_f` binds scheme/domain/schema/hash versions, `id_f`, `body_len`, and
`body_hash_f = PBytes(TAG_BODY, body)`.

## Scope

- `id_f = PBytes(TAG_ID, domain_id_be2 || id_encoding_kind_u8 || raw_id_len_be4 || raw_id)`.
- `collection_key_f` per §4.3 with `partition_presence_u8 ∈ {0,1}`, `partition_key_len_be4 = 0` for Singleton;
  non-empty canonical `partition_key` required for Partitioned.
- `tree_key_f`, `id_bytes/collection_key/tree_key = BE32(·)`, `shard_index = low_k_bits(tree_key_f)`.
- `leaf_f` per §5.2 (`body_len` exact canonical byte count as u64); reject a derived leaf equal to `ZERO`
  fail-closed. Encoding note (§4.1/§5.2): `leaf_f`'s `domain_id`, `schema_version`, `hash_version`, and
  `body_len` are bare-integer `Fr` embeds (the same u16 value for `domain_id`) — distinct from the
  `domain_id_be2` BYTE STRING inside the `PBytes` preimages of `id_f`/`collection_key_f`. Golden vectors
  must pin both encodings.
- T02 OWNS the registry-entry descriptor types the derivation consumes (encoding kind, partition policy,
  `K_domain`, versions) — a pure data shape with no lookup logic; T06 later implements the height-resolved
  registry AGAINST these types (breaks the T02↔T06 cycle). Callers never pass encoding kinds, partition
  presence, or versions directly — always a resolved descriptor.
- Tribute v1 partition rule as a pluggable domain derivation: `partition_key = raw_id[0..4]` (BE `wwd_id: u32`),
  mismatch rejected; Nod Singleton (wired fully in T23; Gem deferred from Stage 1).

## Out of scope

- Registry storage/governance itself (T06); body encoding rules (T05); SMT insertion (T03/T07).

## Acceptance criteria

1. Golden vectors for: singleton key, partitioned key, empty vs non-empty partition boundary, raw-32-byte-ID
   vs variable-length-ID disambiguation, per-domain shard counts (§19.2 items: singleton and partitioned
   collection keys, per-domain shard counts).
2. Cross-domain and cross-partition domain separation demonstrated by vector inequality tests.
3. `leaf(body) == ZERO` rejected fail-closed with a structured error.
4. Shard index covers all `2^k` values for a sampled key population (reachability sanity test).

## Invariants

- One encoding/generation/partition mode active per domain version; unknown/inactive/mismatched kinds fail closed.
- Caller can never supply `id_bytes`, `collection_key`, `tree_key`, shard, or versions.

## Tests

- Unit + golden vectors as above; property test: `tree_key` uniqueness across (domain, partition, raw_id) triples.

## Files

- `crates/core/compressed_entities/src/identity.rs`
- `crates/core/compressed_entities/tests/vectors/identity_*.json`
