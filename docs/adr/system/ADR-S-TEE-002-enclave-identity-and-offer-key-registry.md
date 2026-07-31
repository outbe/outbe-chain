# ADR-S-TEE-002: TeeRegistry owns on-chain enclave identity and offer-key epochs

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/teeregistry`; enclave registrations,
  attestation-policy commitment, committee group key and Tribute offer-key identity
- **Depends on:** ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003 and ADR-B-EVM-004, ADR-S-VAL-001, ADR-S-TEE-001
- **Related:** ADR-C-TRB-001 Tribute, ADR-C-TRB-002 TributeFactory
- **Supersedes:** The registry-local portions of the deleted pre-space TEE/Tribute aggregate

## Context

Clients need one consensus-authoritative public key for encrypted Tribute offers,
while nodes need authenticated per-validator enclave keys for Noise, result
attestation, DKG share delivery and key handoff. Local quote verification or local
enclave availability cannot make these facts canonical; the registry is their
on-chain owner.

## Decision

TeeRegistry is the sole owner of:

- whether TEE bootstrap has committed;
- active Tribute offer public key and its derivation/rotation epoch;
- attestation policy hash, DKG transcript and committee-snapshot binding;
- active committee DKG group verification key; and
- the current verified enclave registration bundle for each validator.

Only an authenticated bootstrap/boundary system command may establish or rotate
global/committee facts. A validator may register or rotate its own enclave identity
only with a chain-verifiable quote, active/eligible ValidatorSet identity and keys
bound by that quote. Local host claims are never sufficient.

## State model and identities

Global state contains a one-time bootstrap marker, offer public key, policy hash,
key epoch, offer-key epoch, transcript hash, committee snapshot block/hash,
registered count and chunked current group public key.

Each validator registration is one aggregate:

```text
validator
recipient_x25519
attestation_pub
noise_static_pub
mrenclave, mrsigner, isv_svn
keys_hash = keccak256(domain || every preceding field)
```

Boundary-announced recipient keys are a distinct provisional channel fact and may
not silently override a fully verified registration. Missing registration needs an
explicit typed absence; an all-zero record is not a valid registered enclave.

Required invariants:

```text
bootstrapped => nonzero valid offer key, policy/snapshot/transcript and group key
registration exists <=> keys_hash is nonzero and recomputes exactly
registered_count = number of existing registration aggregates
all public keys are nonzero, valid and unique in their identity role
registration.validator is eligible under the referenced committee/ValidatorSet
group key and registrations belong to the same activated committee epoch
```

## Registry state machines

```text
Unbootstrapped --validated block-1 bootstrap--> Bootstrapped(epoch E, offer O)

Absent validator --verified quote + eligibility--> Registered(version V)
Registered(V) --verified newer/authorized quote--> Registered(V+1)
Registered --committee removal/revocation policy--> Retired

Committee(E) --prior-group-endorsed reshare--> Committee(E+1)
OfferKey(E) --explicit rotation artifact--> OfferKey(E+1)
```

Bootstrap is terminal and duplicate bootstrap rejects. Registration rotation must
bind old/new intent and cannot be a blind overwrite. Committee reshare and offer-key
rotation are separate transitions: current code preserves the offer key across
reshare, which must remain explicit rather than inferred from omitted writes.

## Bootstrap authority and atomicity

The Phase-3b bootstrap handler validates committee membership, signatures, policy,
registrations, transcript and snapshot before calling `write_bootstrap`. Registry
defense-in-depth must repeat all local structural invariants. Global fields, group
key chunks, registration aggregates, count and `bootstrapped` commit in one system
transaction; the marker is written last so incomplete data is unreachable after
rollback.

The handler capability, not public access to the generated storage facade, conveys
authority. Corrupt or contradictory bootstrap input is fatal consensus failure.

## Registration and key delivery

Mid-chain registration must verify a quote whose report data binds recipient,
attestation and Noise keys; enforce policy measurements/SVN; bind caller to the
validator identity; and enforce the appropriate ValidatorSet status/committee
eligibility. First registration increments count exactly once. Rotation preserves
count, records a version/history or revocation boundary, and prevents stale-key
replay.

Offer-key delivery is an asynchronous encrypted artifact addressed to the verified
recipient key. Its production contract must be one of:

- deterministic bytes already included and consensus-validated in the transaction
  or boundary artifact; or
- a post-commit delivery protocol whose bytes are not in the EVM receipt/state root.

Consensus execution must never call a node-local enclave and conditionally emit a
log based on whether that node has it configured.

## Reshare activation and group key

A reshare artifact is verified using the prior stored group public key and binds the
new committee's registrations/group key. All new registration aggregates,
provisional announcements, group-key bytes, epoch and committee binding change in
the same boundary checkpoint.

Chunked group-key replacement must clear every obsolete trailing chunk when the new
encoding is shorter, validate canonical decoding before publication and write its
length/availability marker last. Readers fail closed on malformed length/chunks.

## Replay, retry and failure

Bootstrap replay rejects. Registration same-intent retry should return the original
typed registration/delivery receipt without repeating count or delivery; different
intent for the same registration version rejects. Boundary replay binds the complete
artifact identity and returns the prior result.

Invalid user quotes/eligibility are reverts. Missing or corrupt canonical policy,
committee snapshot, group key, count/index equivalence or impossible activated
artifact is fatal. An unavailable local enclave is operational failure and cannot
alter consensus-visible execution.

## Determinism, bounds and compatibility

Registration lists use canonical validator ordering and are bounded by committee
maximum. Key encodings, keys-hash domain, policy hash, group public key codec,
attestation verification, epochs and bootstrap/reshare artifact formats are
hard-fork surfaces. Count increments and encoded lengths use checked exhaustion.

Storage is append-only by declared slot order but needs an explicit schema version
and migration for new registration/version/revocation data. Measurement-policy and
offer-key rotations require activation overlap so old ciphertext and sealed state
have a defined fate.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`, macro-generated precompile,
tests, Tee bootstrap builders/consumers, EVM begin-block bootstrap/boundary handlers,
ValidatorSet readers and local enclave delivery helper. The current implementation
cannot pass architecture review or be called production-safe because the public registration trust
gate is an unconditional stub and execution contains a node-local external effect.

Closure requires a closed command/query interface, on-chain-verifiable registration
proof or consensus-validated artifact, typed absence/version/receipt, module-owned
checkpoints, no local I/O in deterministic EVM execution and invariant/property
tests through the actual dispatch/system-handler seams.

## Consequences and rejected alternatives

An on-chain registry lets every node and client use the same authenticated enclave
identity and offer key. Trusting a host-provided measurement was rejected. Treating
boundary-announced recipient keys as full attestation was rejected. Calling a local
enclave during consensus execution was rejected because validator-local availability
and bytes cannot choose receipts/state roots.

## Open questions and technical debt

- **Critical:** `verify_enclave_registration` unconditionally returns `true` and the
  ABI carries no quote. Any EOA can register arbitrary keys/measurements and increase
  `registered_count`. Disable this mutation until real quote + ValidatorSet + policy
  verification is implemented through the production ABI.
- **Critical:** `register_enclave` calls the node-local enclave while executing an
  EVM transaction and conditionally emits `OfferKeySealed`; `Ok(None)` silently emits
  nothing. Receipt/state determinism therefore depends on local enclave configuration.
  Move delivery into a consensus artifact or post-commit non-consensus protocol.
- Close raw facade/system methods so callers cannot write bootstrap, group key,
  boundary announcements or reshare registrations without validated capabilities.
- `write_bootstrap` only checks the boolean marker and list length. Revalidate
  nonzero fields, unique validators/keys, keys hashes, count, committee/policy and
  canonical group key inside the owner before any write.
- `registration()` returns an all-zero aggregate for absence. Add an existence flag
  or typed `Option` and reject partially populated records/corrupt hashes.
- `registered_count` uses `saturating_add`, allowing a successful registration
  without an accurate count at `u32::MAX`. Use checked exhaustion and a bounded set.
- Enforce uniqueness of recipient, attestation and Noise keys across validators;
  blind key reuse enables identity collapse/misdelivery.
- Add registration version, quote identity/hash, activation/revocation height and
  explicit retire/rotation policy. Current re-registration is an unaudited overwrite.
- `record_reshare_registrations` updates only three keys, leaving measurements,
  `keys_hash`, count and removed-validator records stale. Define and atomically
  replace the complete committee registration set.
- `set_group_public_key` writes length before chunks and does not clear trailing old
  chunks when replacing with a shorter key. Validate first, clear old suffix and
  publish availability/length last inside a checkpoint.
- Define ownership and update rules for `key_epoch`, `tribute_offer_epoch`, transcript
  and committee snapshot after bootstrap; current runtime exposes no complete
  rotation transition.
- Decide whether zero `policy_hash` is ever legitimate. “Skip measurement
  enforcement for backward compatibility” is unsafe for a TEE-required chain.
- Bind provisional `announced_recipient_x25519` to a signed artifact, expiry and
  reconciliation with verified registrations; “latest wins” permits silent drift.
- Add failure injection between every bootstrap/reshare/registration write and event,
  with complete aggregate/index pre-state comparison.
- Add production-interface tests for arbitrary EOA, invalid/expired/revoked quote,
  wrong validator status, duplicate keys, same/different-intent retry, count overflow,
  shorter group key, removed committee member and nodes with/without local enclave.
- Add an independent stateful reference model for bootstrap, registration rotation,
  reshare, offer-key rotation, revocation and replay, including corrupt storage and
  mixed-version migration histories.
