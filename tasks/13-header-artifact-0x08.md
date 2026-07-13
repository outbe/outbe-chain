# T13 — Header artifact tag 0x08 and validator root equality

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §9.3 (Q19)
Depends on: T12, T30 (exact artifact envelope version)
Blocks: T14 (block-1 checks), T18 (root extraction)

## Summary

Add the compressed-entity record to `OutbeBlockArtifacts` under tag `0x08` with the required envelope
version bump, and enforce the validator triple-equality (EVM slot ≡ artifact ≡ recomputed root) for B ≥ 1.

## Context

`0x07` is committee pre-announcement (`reshare_artifact.rs:52`); `0x04` is retired; `0x08` is the next free
tag. Payload: `{ commitment_scheme_version, R_sealed(B) }`. Encoded artifacts must stay within
`OUTBE_MAX_EXTRA_DATA_SIZE = 64 KiB`. Genesis (B = 0) is the sole carrier exception: empty `extra_data`,
seeded EVM slot only; verifiers of height 0 use the trusted chainspec derivation.

## Scope

- Codec: tag `0x08` record encode/decode, envelope format-version bump, byte-for-byte deterministic encoding;
  legacy-version decoding rules preserved per existing artifact-codec conventions.
- Proposer path: header-artifact builder consumes the exported root from T12 step 5.
- Validator path: decode artifact, require exact equality with locally recomputed root and post-seal EVM
  slot 1; mismatch rejects the block.
- Block 0 rule: no 0x08 record; block 1 must carry the first 0x08 artifact (validation of both directions).

## Out of scope

- Light-client verifier tooling; seal computation (T12); genesis derivation (T14).

## Acceptance criteria

1. Codec round-trip + conformance vectors (committed as consensus fixtures — the §19.2 artifact-encoding
   vector item); unknown-tag/short-payload/duplicate-tag rejection consistent with existing envelope rules;
   envelope version bump covered by version-boundary tests.
2. Validator rejects on: artifact ≠ recomputed, slot ≠ recomputed, missing artifact at B ≥ 1, present
   artifact at B = 0.
3. Extra-data size guard: record fits alongside existing worst-case artifacts under 64 KiB (static assert/test).
4. Proposer/validator round-trip on a localnet-style two-node executor test.

## Invariants

- The tag is a wire identifier only — not part of the Poseidon commitment scheme.
- Artifact encoding is byte-for-byte deterministic across validators.

## Tests

- Codec unit + fuzz (§19.17 codec target), executor integration for equality enforcement.

## Files

- `crates/blockchain/primitives/src/reshare_artifact.rs` (or the artifacts codec module)
- `crates/blockchain/evm/src/{builder.rs,config.rs}` (artifact build/verify seams)
