# T03 — Vendored panic-sanitized CKB SMT with typed Poseidon merge codec

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §9.1 (Q7, Q23)
Depends on: T01
Blocks: T04 (differential gate only), T12, T15
Parallel with: T04 (the differential suite lives in T04 and depends on BOTH; neither blocks the other's development)

## Summary

Vendor the CKB `sparse-merkle-tree` engine into the store module, sanitize panics, and implement the
Outbe-owned typed Poseidon merge codec (`TAG_SMT_BASE/NORMAL/ZERO`) that is part of commitment scheme v1.

## Context

Each collection shard is an independent in-place Poseidon-BN254 SMT. The engine keeps CKB key/path,
update/delete, compact-zero, proof, and storage mechanics; only the merge/hash layer is Outbe-typed.
`zero_count` preserves upstream wire semantics exactly: `u8` with wrapping addition (full 256-level path
encodes 0, not 256).

## Scope

- Vendor `nervosnetwork/sparse-merkle-tree` under the store module (private; no public tree-backend trait).
- Panic sanitation: no `unwrap/expect/panic` reachable from runtime paths; fallible APIs return structured errors.
- Merge codec per §9.1: `merge(ZERO,ZERO)=ZERO`; `base_node = P(TAG_SMT_BASE; height, key_f, value_f)`;
  `normal_node = P(TAG_SMT_NORMAL; height, node_key_f, left_f, right_f)`;
  `merge_with_zero = P(TAG_SMT_ZERO; base_node_f, zero_bits_f, zero_count)` with `zero_count: u8` wrapping.
- Every non-zero H256 consumed by the codec validated as canonical BN254 field encoding.
- ZERO semantics: empty subtree/shard root are legal ZERO values; any Poseidon output over non-empty content
  evaluating to ZERO fails sealing deterministically (fail-closed error surfaced to seal layer).
- Store trait plumbing generic over the key namespace so T15 can namespace nodes by `{collection_key, shard_index}`.
- Inclusion and non-membership proof generation/verification against a shard root.

## Out of scope

- MDBX backing tables (T15); collection top / Root Catalog composition (T04/T12).

## Acceptance criteria

1. Golden vectors for every merge form, including `zero_count` 255→0 wrapping at depth 256, delete sentinel,
   single-leaf shard, empty shard (§19.2).
2. Engine passes the T04-owned differential suite over randomized mutation sequences (§19.3) — gate closes
   when both tasks land; not a sequencing dependency.
3. Differential delete tests: branch cleanup, non-membership after delete, same-key sequences (§19.16).
4. No-reachable-panic evidence (audit §10.4 — `cargo geiger` measures unsafe, not panics, and is NOT the
   tool here): static scan for `unwrap/expect/panic/assert/unreachable/todo/unimplemented` and indexing/
   arithmetic panics over the vendored runtime paths, plus reviewed call-path tests and fuzzing of the
   decode/merge/proof entry points.

## Invariants

- Merge codec is byte-for-byte deterministic; height/path/side/zero info never omitted or reordered.
- ZERO never represents present content.

## Tests

- Unit + golden vectors; proptest mutation sequences vs reference model; fuzz malformed proof decoding (§19.17).

## Files

- `crates/core/compressed_entities/src/smt/` (vendored engine + `merge.rs` codec)
- `crates/core/compressed_entities/tests/vectors/smt_*.json`

## Notes

- Vendoring is a supply-chain event: record in `supply-chain/` (cargo-vet) and pin the upstream commit in
  the vendored README with the sanitation diff summary.
