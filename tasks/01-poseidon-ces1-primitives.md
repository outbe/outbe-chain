# T01 — CES1 Poseidon primitives and normative tag registry

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §4.2 (Q7, Q18)
Depends on: —
Blocks: T02, T03, T04

## Summary

Implement the commitment-scheme-v1 hash layer: the 14-entry CES1 tag registry, the tagged Poseidon
primitive `P(tag; …)`, and the byte-to-field primitive `PBytes(object_tag, bytes)`, with golden vectors.

## Context

Everything above this layer (keys, leaves, SMT nodes, collection tops, Root Catalog, R_sealed) is defined
as compositions of `P` and `PBytes`. The tag table in §4.2 is normative; Rust constants mirror it and never
redefine it. `outbe-poseidon` v0.11.0 already provides `Poseidon::<Fr>::with_domain_tag_circom(m, tag)`
(arity cap 12 inputs; widest CES1 formula is leaf_f with 7).

## Scope

- New crate or module (suggested: `crates/core/compressed_entities/src/hash/` or a dedicated
  `outbe-ces-hash` crate if T04's reference model needs it without the store).
- `CES1_TAG_BASE = 0x4345533100000000`; const table for tag IDs 1..=14 exactly as §4.2
  (`TAG_BYTES_INIT..TAG_SEALED_ROOT`, `TAG_COLLECTION_KEY` = 13, `TAG_COLLECTION_ROOT` = 14).
- `P(tag; x_0..x_{m-1})` wrapper over `with_domain_tag_circom(m, tag)`; reject arity 0 or > 12.
- `PBytes`: 31-byte left-to-right chunking, right-zero-padded BE `Fr` chunks, init/absorb/final chain per
  §4.2; empty-bytes case (n = 0, absorb skipped); `byte_len`/`n`/`i` as u64-range-checked canonical embeds.
- Canonical `Fr` wire form `BE32`; reject non-canonical inputs `>= p` (never reduce).
- Tag ID 0 and IDs 15..=65535 are unassigned: no constant exists for them; misuse is unrepresentable.

## Out of scope

- Key/leaf composition (T02), SMT merge codec (T03), any circuit/gadget work.

## Acceptance criteria

1. Constants byte-for-byte equal to the §4.2 table (test asserts all 14 canonical BE encodings).
2. `PBytes` golden vectors: empty bytes, 1 byte, exactly 31 bytes, 32 bytes (2 chunks), 62/63 bytes,
   large multi-chunk input — vectors committed as consensus fixtures (§19.2).
3. Non-canonical 32-byte input (`>= p`) rejected with a structured error, never reduced.
4. Reducing-mod-p path does not exist anywhere in the crate (no `from_be_bytes_mod_order` on external input).
5. Doc comment states the circuit note: non-zero domain tags ⇒ `PoseidonEx` with `initialState = tag`,
   not stock circomlib `Poseidon(nInputs)`.

## Invariants

- Every CES1 hash uses a registry tag; callers cannot pass arbitrary tags, parameters, or fields.
- All tag values < 2^64 < p; canonical embedding without reduction.

## Tests

- Unit: table equality, arity bounds, chunking boundaries, empty input, range rejection.
- Golden vector files under `tests/vectors/` (format reused by T04's reference model and future circuits).
- Property test: `PBytes` injectivity smoke — distinct (len, bytes) pairs with equal prefixes hash distinct.

## Files

- `crates/core/compressed_entities/src/hash/{mod.rs,tags.rs,pbytes.rs}` (or `crates/system/ces-hash/`)
- `crates/core/compressed_entities/tests/vectors/*.json`
