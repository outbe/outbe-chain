# ADR-C-POW-001: Entity mining proof-of-work binds a versioned domain-separated challenge

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** entity-factory and protocol-security maintainers
- **Scope:** `outbe-common::pow`, GemFactory and NodFactory admission
- **Depends on:** ADR-B-WIR-001
- **Related:** ADR-C-GEM-002, ADR-C-NOD-002, ADR-C-LBM-001

## Context

GemFactory and NodFactory share a SHA-256 proof-of-work gate. Today it hashes
lowercase ASCII hex of an identity followed by an eight-byte nonce and accepts one
leading zero byte. Encoding, difficulty and replay domain are a protocol-security
decision, not miscellaneous math.

## Decision

Define `MiningChallengeV1`, a canonical length-delimited binary encoding binding
version, chain id, factory/domain, fixed-width entity identity, immutable challenge
anchor, beneficiary where required, difficulty profile and nonce. Hash algorithm and
full 256-bit target comparison are fixed.

The shared library only encodes, hashes and verifies with typed errors. Each factory
owns challenge issuance/consumption and atomically maps verification into its state
machine. Difficulty changes are protocol-scheduled and never depend on local time or
load. Cross-chain, cross-factory, cross-identity and consumed-challenge replay fails.
Verification is constant-work and covered by EVM gas.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Challenge bytes/hash/target | versioned PoW library |
| Difficulty activation | protocol schedule |
| Issuance, uniqueness and consumption | factory owner ADR |
| Gas/EVM outcome | ADR-B-EVM-001 and factory ABI |

## Invariants

- One challenge has one canonical encoding and digest.
- Work cannot cross chain, factory or identity domains.
- Nonce width and target comparison are explicit.
- Verification is deterministic and bounded.
- Consumption and entity creation commit together or neither does.
- Factories map verifier errors exhaustively.

## Atomicity, replay and failure

Verification runs before mutation inside the factory checkpoint. Challenge
consumption and mint commit together. Retry of consumed work cannot mint twice and
returns the documented owner-specific replay result.

## Compatibility and migration

Encoding fields, hash, target and nonce width are protocol ABI. New semantics use a
new version and activation specifies treatment of outstanding prior challenges.

## Production-interface verification evidence

Inspected `compute_pow_hash[_bytes]`, `validate_pow[_bytes]` and callers in GemFactory
and NodFactory. Tests cover the local SHA-256 construction and nonce bounds, but not
domain separation, replay ownership or cross-language vectors. Status is Proposed.

## Consequences

Mining admission becomes an auditable boundary while factories reuse one verifier
without sharing valid work across unrelated domains.

## Rejected alternatives

- **Classify PoW as generic math:** hides replay/security ownership.
- **Keep unversioned ASCII concatenation:** prevents safe evolution.
- **Copy verifier per factory:** encoding/error drift.
- **Use local adaptive difficulty:** validators can disagree.

## Open questions and technical debt

1. Current bytes omit version, lengths, chain, factory, beneficiary and state anchor.
   Design and activate a domain-separated challenge.
2. `POW_DIFFICULTY = 1` is about eight bits of work. Establish the actual anti-spam
   objective and measure whether this is meaningful.
3. One compile-time difficulty serves all factories. Define domain profiles and
   deterministic activation.
4. Leading-zero-byte comparison allows only 8-bit steps; use a canonical full target.
5. The bytes API accepts arbitrary identity widths. Require typed fixed-width domain
   identities and vectors for 32-byte versus 36-byte ids.
6. Accepting a `U256` nonce only to reject above `u64::MAX` weakens the boundary;
   accept `u64` internally and define ABI conversion separately.
7. Audit factory replay, front-running and beneficiary substitution; record exact
   consumption rules in ADR-C-GEM-002 and ADR-C-NOD-002.
8. Current lack of chain/domain binding permits work reuse for equal ids elsewhere;
   treat this as a protocol security gap.
9. Replace brute-force-only tests with fixed Rust/CLI/miner valid, invalid and
   boundary golden vectors.
10. Pin `ring` behavior/features and verify with independent standard vectors.
11. Make factory mappings exhaustive when `PowError` evolves; invalid work is a
    domain revert, not fatal block error.
12. Bind hashing CPU to ADR-B-EVM-001 gas and test batch/spam limits.
13. Decide whether PoW remains useful given fee/eligibility controls; remove it by
    activation if it has no defensible objective.
