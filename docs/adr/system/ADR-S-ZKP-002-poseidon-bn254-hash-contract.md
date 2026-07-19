# ADR-S-ZKP-002: Poseidon-BN254 hashing accepts canonical field elements under a frozen profile

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** protocol cryptography maintainers
- **Scope:** Poseidon precompile and shared BN254 hash parameters/encoding
- **Depends on:** ADR-B-WIR-001, ADR-B-EVM-001
- **Related:** ADR-B-CLI-001, ADR-S-ZKP-001, ADR-B-OCD-006 and ADR-B-OCD-008

## Context

The stateless Poseidon precompile hashes one to twelve 32-byte values using Circom
BN254 parameters. Wallets, circuits, compressed-entity commitments and on-chain code
must agree on field encoding and parameter set. The current decoder reduces arbitrary
256-bit words modulo the field, allowing multiple byte strings to represent one
field value.

## Decision

Publish a frozen `PoseidonBn254CircomV1` profile: field modulus, width/rate/capacity,
round counts, matrices/constants, domain/arity behavior, byte order and output
encoding, all tied to immutable artifact digests.

The precompile accepts `1..=12` exact 32-byte **canonical** big-endian field elements
strictly below the BN254 scalar modulus. Non-canonical values revert rather than
reduce modulo the field. Arity is part of the hash domain/profile; applications that
hash structured values use their own explicit domain tag and canonical typed
encoding before this primitive.

Initialization/parameter construction is deterministic, bounded and cannot depend
on environment. Gas covers validation and the arity-specific permutation cost.
Cross-language golden vectors cover every supported arity and boundary field value.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Parameters and field encoding | `PoseidonBn254CircomV1` profile |
| Raw hash ABI and arity limit | Poseidon precompile |
| Structured domain separation | owning protocol format ADR |
| Address/gas/result mapping | ADR-B-WIR-001 and ADR-B-EVM-001 |

## Invariants

- One accepted byte string represents exactly one field element.
- Equal canonical inputs and arity produce exact equal 32-byte output everywhere.
- Empty, unaligned, oversized or non-field input never reaches permutation code.
- Work and memory are bounded by the twelve-element limit.
- The primitive does not invent domain separation for owning protocols.

## Atomicity, concurrency and replay

The function is pure and stateless. Parameter caches, if introduced, are immutable
after validated initialization and return identical results concurrently. Replay is
always identical; replay protection belongs to structured protocols using the hash.

## Compatibility and migration

Parameters, arity semantics, canonical input/output and gas are protocol ABI. A
different parameter set or permissive/reducing decoder requires a new profile and
address/version with cross-language vectors.

## Production-interface verification evidence

Inspected Poseidon decoding, Circom constructor, output serialization, gas helper,
EVM registration and unit vectors for arities 1, 2 and 4. Current tests compare the
same Rust dependency rather than an independent circuit/wallet artifact and accept
modular-reduced inputs. Status remains Proposed.

## Consequences

Protocols can cite a precise hash primitive while retaining ownership of their
structured encoding/domain tags. Canonical field inputs eliminate malleable aliases.

## Rejected alternatives

- **Combine with proof verification:** different algorithms, artifacts and failure
  modes.
- **Reduce every uint256 modulo the field:** multiple calldata values hash identically.
- **Let callers choose arbitrary parameters:** destroys protocol interoperability.
- **Use one Rust implementation as its own oracle:** cannot prove wallet/circuit
  compatibility.

## Open questions and technical debt

1. `Fr::from_be_bytes_mod_order` accepts values at/above the modulus and aliases them
   to smaller fields. Switch to strict canonical decoding and add malleability tests.
2. Pin the exact `outbe-poseidon` revision/parameter digest in the profile, not only a
   Cargo tag/lock entry.
3. Current “off-chain reference” tests instantiate the same Rust library as the
   implementation. Add independent Circom/Noir/wallet golden vectors.
4. Only arities 1, 2 and 4 have success tests. Add every arity 1 through 12 plus zero,
   modulus-minus-one, modulus and max-word boundaries.
5. Clarify whether `new_circom(n)` gives arity-separated parameterization/domain or
   merely different width; publish exact semantics and collision expectations.
6. Parameter setup occurs on every call and returns allocated string errors. Consider
   immutable prevalidated per-arity parameters while preserving bounded concurrency.
7. Gas pricing is a linear placeholder formula. Benchmark setup plus permutation per
   arity and account for malformed-input validation.
8. ADR-B-EVM-001 currently may price `SharedBuffer` calldata as empty before materializing
   it, undercharging this input-dependent base gas for contract-originated calls.
9. Gas arithmetic uses unchecked `base + per_input * n`; current bound makes it safe,
   but calculate after validation with checked operations and keep the proof local.
10. The raw ABI has no explicit version/domain tag. Preserve the address as exactly
    one frozen primitive and require structured callers to prefix typed domain data.
11. Audit all callers for direct concatenation, inconsistent byte order or missing
    length/domain binding; link each construction to its format ADR and vectors.
12. Verify output serialization is always canonical 32-byte big-endian field encoding
    and independently test leading-zero outputs.
13. Decide how existing data/proofs created from modular-reduced non-canonical inputs
    migrate if strict decoding activates.
