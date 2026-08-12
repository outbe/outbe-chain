# ADR-S-TEE-002: TeeRegistry owns on-chain enclave identity and offer-key epochs

- **Status:** Accepted for the V1 DCAP activation boundary; I9 hardware evidence remains gated
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/teeregistry`; enclave registrations,
  attestation-policy commitment, committee group key and Tribute offer-key identity
- **Depends on:** ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003 and ADR-B-EVM-004, ADR-S-VAL-001, ADR-S-TEE-001
- **Related:** ADR-C-TRB-001 Tribute, ADR-C-TRB-002 TributeFactory
- **Supersedes:** The registry-local portions of the deleted pre-space TEE/Tribute aggregate

## Context

Clients need one consensus-authoritative public key for encrypted Tribute offers,
while every node needs one durable enclave identity for Noise, result attestation,
DKG share delivery and one-time registry onboarding. That NodeHost identity must
survive changes in the node's ValidatorSet role. Local quote verification or local
enclave availability cannot make these facts canonical; the registry is their
on-chain owner.

## Decision

TeeRegistry is the sole owner of:

- whether TEE bootstrap has committed;
- permanent genesis Tribute offer public key at fixed epoch zero;
- attestation policy hash, DKG transcript and committee-snapshot binding;
- active committee DKG group verification key; and
- the current verified enclave registration bundle for each role-neutral
  NodeHost;
- the authenticated, one-way EVM-address-to-NodeHost association used when the
  same node later enters the ValidatorSet lifecycle.

Only an authenticated bootstrap/boundary system command may establish global and
committee facts. Initial `registerEnclave` atomically registers a NodeHost and an
EVM-address-to-NodeHost association authorized by both keys. It does not consult
ValidatorSet and grants no validator role. Local host claims are never sufficient.

## State model and identities

Global state contains a one-time bootstrap marker, offer public key, policy hash,
key epoch, offer-key epoch, transcript hash, committee snapshot block/hash,
registered count and chunked current group public key.

`NodeIdV1` contains exactly one persistent identity field:

```text
reth_p2p_public (compressed secp256k1)
```

Each NodeHost registration is one aggregate keyed by `node_id_hash`:

```text
node_id_hash
recipient_x25519
attestation_pub
noise_static_pub
mrenclave, mrsigner, isv_svn
keys_hash = keccak256(domain || every preceding field)
```

Initial registration also carries `ValidatorNodeBindingV1`:

```text
chain_id, genesis_hash, evm_address, node_id_hash
EVM signature + NodeHost P2P signature
```

The separately provisioned EVM and consensus-BLS keys do not participate in
`NodeIdV1`, the NodeHost manifest, enclave identity or sealed state. Merely
possessing them creates no role. ValidatorSet remains the sole owner of
registration status, stake, BLS proof of possession, DKG eligibility and ACTIVE
membership.

Boundary-announced recipient keys are a distinct provisional channel fact and may
not silently override a fully verified registration. Missing registration needs an
explicit typed absence; an all-zero record is not a valid registered enclave.

Required invariants:

```text
bootstrapped => nonzero valid offer key, policy/snapshot/transcript and group key
registration exists <=> keys_hash is nonzero and recomputes exactly
registered_count = number of existing registration aggregates
all public keys are nonzero, valid and unique in their identity role
initial association node_id_hash = the NodeHost registered by the same operation
one EVM address has at most one immutable NodeHost association
NodeHost identity is independent of FullNode/Validator process mode and ValidatorSet status
```

## Registry state machines

```text
Unbootstrapped --validated block-1 bootstrap--> Bootstrapped(epoch E, offer O)

Absent NodeHost + absent association --verified evidence + dual-signed binding-->
  Registered NodeHost(version V) + Associated EVM address
Registered(V) --verified newer/authorized quote--> Registered(V+1)

EVM/BLS keys --registerValidator/stake/readiness--> REGISTERED/PENDING
PENDING --certified DKG boundary--> ACTIVE
ACTIVE --jail/exit/re-entry--> ValidatorSet status changes only

Committee(E) --prior-group-endorsed reshare--> Committee(E+1)
```

Bootstrap is terminal and duplicate bootstrap rejects. Exact initial replay is
idempotent; a conflicting address or NodeHost association rejects atomically.
Registration rotation binds old/new intent and cannot be a blind overwrite.
Committee reshare preserves the permanent offer key. ValidatorSet transitions,
including BLS replacement, never recreate or rebind NodeHost identity.

## Bootstrap authority and atomicity

The Phase-3b bootstrap handler validates committee membership, signatures, policy,
NodeHost registrations, their dual-signed EVM associations, transcript and snapshot
before calling `write_bootstrap`. Registry defense-in-depth repeats all local
structural invariants. Global fields, group-key chunks, NodeHost registrations,
associations, count and `bootstrapped` commit in one system transaction; the marker
is written last so incomplete data is unreachable after rollback.

The handler capability, not public access to the generated storage facade, conveys
authority. Corrupt or contradictory bootstrap input is fatal consensus failure.

## Registration and key delivery

V1 registration verifies canonical self-contained evidence whose intent binds the
recipient, attestation and Noise keys, active policy and the persistent compressed
Reth P2P key. The same call verifies `ValidatorNodeBindingV1` with the separately
provisioned EVM key and NodeHost P2P key, requires it to reference that exact
NodeHost, and writes the existing `validator_v1_node_hash` association in the same
checkpoint. The transaction sender is only a permissionless relay. There is no
public `bindValidatorNodeHost`, reverse map or second TEE join. Created,
idempotent, renewal, replacement and measurement transition are explicit versioned
outcomes and prevent stale-intent replay.

A FullNode may store EVM/BLS key material, but its runtime does not load it and the
association confers no consensus or OCOMP authority. Promotion stops the FullNode
process and starts validator mode on the same datadir. The ordinary
`registerValidator`/stake/readiness/DKG lifecycle alone grants the role; the Reth
P2P key, manifest, enclave identity, Noise identity, sealed state and resident
offer key remain byte-identical.

Offer-key delivery is an asynchronous encrypted artifact addressed to the verified
recipient key. Its production contract must be one of:

- deterministic bytes already included and consensus-validated in the transaction
  or boundary artifact; or
- a post-commit delivery protocol whose bytes are not in the EVM receipt/state root.

The selected V1 contract is the first option. Every consensus node is required to
run an authorized enclave before execution. For a Created binding, that enclave
uses static-static X25519 and derived nonce with no RNG to return byte-identical
bounded bytes; missing enclave, zero resident commitment, malformed bounds or
prefix mismatch is fatal. Idempotent registration, renewal and replacement never
redeliver. A node lacking the exact resident key does not enter consensus or
transaction execution.

## Reshare activation and group key

A reshare artifact is verified using the prior stored group public key and binds the
new committee's registrations/group key. All new registration aggregates,
provisional announcements, group-key bytes, epoch and committee binding change in
the same boundary checkpoint.

Chunked group-key replacement must clear every obsolete trailing chunk when the new
encoding is shorter, validate canonical decoding before publication and write its
length/availability marker last. Readers fail closed on malformed length/chunks.

## Replay, retry and failure

Bootstrap replay rejects. Initial registration replay succeeds only when both the
NodeHost record and EVM association are exact idempotent replays; partial or
conflicting states reject without writes. Registration same-intent retry returns
the original typed registration/delivery receipt without repeating count or
delivery; different intent for the same registration version rejects. Boundary
replay binds the complete artifact identity and returns the prior result.

Invalid user quotes, signatures and bindings are reverts. Missing or corrupt canonical policy,
committee snapshot, group key, count/index equivalence or impossible activated
artifact is fatal. An unavailable local enclave is operational failure and cannot
alter consensus-visible execution.

## Determinism, bounds and compatibility

Registration lists use canonical validator ordering and are bounded by committee
maximum. Key encodings, keys-hash domain, policy hash, group public key codec,
attestation verification, epochs and bootstrap/reshare artifact formats are
hard-fork surfaces. Count increments and encoded lengths use checked exhaustion.

This decision lands with a network wipe. There is one canonical wire and storage
model: no migration, compatibility decoder, dual read/write, activation gate,
reserved legacy layout or fallback. Measurement-policy updates use rolling
measurement overlap; the permanent offer key does not rotate.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`, the exclusive V1 precompile,
tests, OST3 builders/consumers, EVM block-1 handlers, ValidatorSet readers and the
authorized enclave delivery helper. There is no caller-authorized verification stub
or alternate public registration dispatcher. Canonical evidence and signatures are
validated before mutation; deterministic onboarding bytes share one implementation
for production and private tests. Exact-release SGX execution and accepted
Processor evidence remain fail-not-skip I9 release gates; a real Platform node
is checked fail-closed by the same verifier at admission.

## Consequences and rejected alternatives

An on-chain registry lets every node and client use the same authenticated enclave
identity and offer key without conflating machine identity with ValidatorSet role.
Trusting a host-provided measurement, deriving NodeHost from role/EVM/BLS/OCOMP,
standalone rebind, and treating boundary announcements as attestation were
rejected. Randomized or optional local output was also rejected. The accepted
enclave-resident path is mandatory, fail-stop and byte-deterministic across nodes
holding the same permanent OST3 key.

## Remaining release evidence

- Freeze the exact Intel QVL and Gramine release graph and testnet feature set.
- Capture fresh accepted Processor release evidence; retain a real Platform
  node's accepted evidence when it joins rather than fabricating a release row.
- Prove exact-release QVL and maximum reachable full-block timing on the same
  SGX server.
- Complete reachable real Validator and FullNode `DcapRequired` paths; no
  32-validator network is required.
- Close the final requirement, forbidden-path and signed-artifact audit without
  treating development or synthetic vectors as hardware evidence.
