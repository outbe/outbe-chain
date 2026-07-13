# T05 — Strict DAG-CBOR canonical body codec

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §5.1
Depends on: T30 (normative body grammar)
Blocks: T07, T23

## Summary

Implement the consensus body-encoding layer: typed DAG-CBOR arrays with schema-fixed field order, a strict
validator that rejects every non-canonical form, and the golden byte grammar that is authoritative over the
pinned codec crate.

## Context

Bodies are opaque canonical bytes to the store, but domains must produce them through one shared strict
codec. §5.1: free-form maps forbidden; schema-fixed integer signedness/width; floats, indefinite-length
forms, unknown tags, duplicate map keys, and non-canonical encodings rejected; normalization happens before
consensus encoding as a domain rule.

## Scope

- Encoder/decoder facade over a pinned CBOR crate producing/validating the strict subset:
  definite-length arrays only, canonical integer widths, schema-declared field order, no floats, no
  free-form maps, byte-string/text rules with schema-defined optional-value representation.
- Schema descriptor type (field order, types, optionality) that domain schemas (T23) instantiate.
- Text-normalization canonicality (audit-final M-09): T30 fixes the normalization rule, T23 normalizes
  domain input BEFORE encoding; T05's validator REJECTS non-normalized text as non-canonical — the
  executable owner split; Unicode/length/invalid vectors committed.
- Round-trip stability: decode(encode(x)) == x and encode(decode(b)) == b for canonical b.
- Golden byte grammar: committed vectors for representative schema shapes; vectors are authoritative — a
  codec-crate upgrade that changes bytes fails the vectors.

## Out of scope

- Concrete Tribute/Nod schemas (T23; Gem deferred); hashing (`body_hash_f` lives in T02 path).

## Acceptance criteria

1. Rejection tests for each §5.1 forbidden form (floats, indefinite lengths, unknown tags, duplicate keys,
   non-canonical ints, trailing bytes).
2. Golden vectors for encode/decode round-trips (§19.2 body encoding item).
3. Fuzz decoding never panics; all failures are structured errors (§19.17).
4. Normalization vectors (audit-final M-09): normalized forms accepted; non-normalized, overlong, and
   invalid-Unicode text rejected as non-canonical.

## Invariants

- One canonical byte representation per semantic record; any accepted body re-encodes to identical bytes.

## Tests

- Unit rejection matrix, round-trip proptests, coverage-guided fuzz target for the decoder.

## Files

- `crates/core/compressed_entities/src/body_codec.rs` (+ `tests/vectors/body_*.json`)
