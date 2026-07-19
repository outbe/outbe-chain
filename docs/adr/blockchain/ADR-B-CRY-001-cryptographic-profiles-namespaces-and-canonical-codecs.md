# ADR-B-CRY-001: Cryptographic profiles, namespaces and canonical codecs

- **Status:** Proposed; implementation has strong local profiles but no complete registry
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, cryptography, consensus, execution and storage maintainers
- **Scope:** consensus-critical primitives, domain separation, canonical encodings and implementation pins
- **Depends on:** ADR-B-WIR-001
- **Related:** ADR-B-GEN-001, ADR-B-EVM-003, ADR-B-CLI-001,
  ADR-B-MCP-001, ADR-B-OCD-006, ADR-S-KEY-001, ADR-S-TEE-001 through
  ADR-S-TEE-002, ADR-S-ZKP-001 through ADR-S-ZKP-002

## Context

Outbe hashes and signs many different objects: Ethereum transactions/tries, Simplex
votes and certificates, threshold VRF seeds, DKG artifacts, committee commitments,
system transactions, CE bodies/keys/SMT nodes, ZK proofs, TEE attestations and sealed
messages. “BLS”, “Poseidon” or “ABI” does not uniquely identify a protocol. Curve
variant, serialization, byte order, subgroup checks, domain tag, message framing,
library behavior and dependency revision are all consensus-relevant.

Today many individual seams are deliberately versioned and tested, but their
authority is distributed across constants, Commonware namespace builders, Solidity
ABI declarations, protobuf helpers, CE persistence strings, Cargo tags and lockfile
commits. This ADR defines the registry that makes those choices reviewable and
activatable as one protocol profile.

It does not choose the business meaning of signed messages or the state machines
that consume proofs; their owning ADRs import the profiles defined here.

## Decision

### One generated cryptographic profile manifest

Each protocol version names an immutable `CryptoProfileManifestV1`. Every entry has:

- stable purpose ID and owner ADR;
- primitive and complete parameter set: curve/field, hash/permutation, signature
  variant, ciphersuite, key/signature encoding and validation rules;
- canonical preimage grammar with field order, widths, byte order, optionality,
  collection ordering and maximum lengths;
- domain/namespace derivation, including chain, genesis, fork/profile, committee,
  epoch/material/schema version binding where relevant;
- strict decoder rules and malleability rejection;
- implementation package, feature set and audited source revision;
- activation/deactivation heights and historical verification lifetime; and
- golden positive/negative vectors produced independently where possible.

The checked-in human-readable manifest and generated Rust/Solidity/test artifacts
derive from one source. Runtime code refers to typed purpose IDs instead of composing
unregistered byte strings or selecting a library default.

### Required profiles

At minimum the registry covers:

| Purpose family | Required identity |
|---|---|
| Ethereum execution | active hardfork, Keccak/RLP/trie rules, secp256k1 signature and low-s/recovery semantics |
| Consensus votes/certificates | BLS12-381 MinPk identity/vote keys, aggregation, bitmap/order, Commonware codec and namespaces |
| Threshold randomness/DKG | BLS12-381 MinSig threshold material, polynomial/share encoding, VRF message and material version |
| Committee commitment | canonical participant ordering, key encoding, length framing and `OUTBE_COMMITTEE_V1` Keccak domain |
| System transactions | chain id, unsigned envelope/signature preimage, phase/kind/version and authorized signer profile |
| CE commitments/tree | BN254 field conversion, Poseidon parameters/tags, CES1 grammar, CKB-SMT semantics and vendor revision |
| CE bodies/proofs | protobuf/canonical body schema, proof encoding and strict persistence codecs |
| ZK verification | curve/backend, circuit ID/version, VK/CRS hashes and public-input canonicalization |
| TEE transport/custody | attestation evidence, Ed25519/X25519/AES-GCM profiles, transcript/AAD/nonces and sealed format |
| ABI/storage | selector/event Keccak preimages, Solidity ABI canonical forms and storage-slot derivation |

Using the same primitive in two purposes never implies the same domain or codec.

### Domain separation

Every signed/hashed transcript begins with an unambiguous registered domain and
version. Variable-length components are length-prefixed or fixed-width; concatenation
cannot admit two parses. The registry states whether the following are bound:

- chain id and genesis/chain-manifest identity;
- protocol/fork and crypto-profile version;
- message purpose and schema version;
- committee commitment and epoch/material version;
- block height/hash/round/view;
- sender/recipient/role and TEE measurement/policy; and
- circuit/VK/CRS identity.

Omission is an explicit security decision with a collision/replay argument. A domain
tag is ASCII only by convention; its actual length-prefixed bytes are normative.
No production signing or verification path may use an unset/default chain identity.

The current Simplex family remains based on `b"outbe" || chain_id_be`, with
registered subdomains for notarize, finalize, nullify, seed and seed attestation.
Individual vote namespaces remain committee-bound; threshold seed signatures remain
bound by their group key/material. These are profile entries, not ad-hoc exceptions.

### Canonical keys, signatures and verification

Decoders require exact length, consume all bytes, reject unknown tags/versions,
non-canonical integers/field elements, identity/infinity points where forbidden,
invalid subgroup points, duplicate/out-of-order participants and trailing bytes.
Verification selects the exact key role and ciphersuite; MinPk and MinSig encodings
are never inferred from byte length.

Committee ordering is the canonical Commonware `Set` order over canonical MinPk key
bytes. Bitmap width and unused-bit rules are explicit. Aggregation rejects empty or
duplicate signer sets and proves proof-of-possession/rogue-key assumptions at the
key-registration boundary.

Secret scalars, shares, nonces and plaintext keys use zeroizing containers, are not
logged/serialized through generic debug formats, and never enter the public manifest.
Randomness sources and deterministic derivations are separately named and tested.

### Canonical encoding contract

Wire, persistence, ABI and signing encodings are distinct profiles even when they
carry the same logical object. Each codec specifies:

- tag/version and exact field order;
- fixed/variable widths and endian convention;
- maximum collection/string/byte sizes before allocation;
- canonical map/set ordering and duplicate policy;
- optional-value discriminants;
- trailing/unknown field policy; and
- whether encoded bytes themselves or a digest are signed/committed.

Decode followed by encode yields the identical bytes for every accepted value.
Unknown consensus/storage versions fail closed. Forward-compatible API JSON may
ignore fields only when those fields are outside signed, hashed or state-transition
meaning.

### Dependency and implementation pinning

Consensus-compatible release artifacts pin exact resolved commits/checksums and
feature sets for Reth/revm/Alloy, Commonware, CKB-SMT, Poseidon/circuit libraries and
all crypto backends. A Cargo tag is review metadata; `Cargo.lock` resolution and the
release SBOM establish the actual source. Vendor forks record upstream base, patch
digest and conformance vectors.

Upgrade review compares semantic behavior, not only public APIs. A dependency update
that can alter accepted bytes, hash/signature/proof results, subgroup validation,
SMT root, gas or execution behavior is a protocol change activated through
ADR-S-GOV-003. Pure implementation changes require differential proof against all golden
and historical vectors before rollout.

### Startup and activation

ADR-B-OCD-006 chain identity commits the active manifest digest and permitted scheduled
successors. Startup verifies the compiled manifest, resolved dependency fingerprint,
genesis profile and every persisted store identity before any signing, consensus or
authoritative read.

Profile activation is height/epoch deterministic and dual-read only where explicitly
specified. Historical block/proof verification selects the profile active at the
object's height/version; the process-wide “current” default is not used. Old signing
keys/profiles stop producing new messages at activation but remain available for the
declared verification window.

### Evidence

CI generates vectors for every registry entry and checks them across production
signer/verifier, proposer/validator/replay, Rust/Solidity/TypeScript/Python tools and
at least one independent implementation for high-risk primitives. Negative vectors
cover every non-canonical form, wrong domain/binding/version/key role and boundary
length. Dependency updates run historical block, certificate, CE root/proof, ABI and
TEE transcript replay before merge.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Purpose/primitive/domain/codec registry | `CryptoProfileManifest` source and digest |
| Network activation | ADR-B-OCD-006 chain manifest plus ADR-S-GOV-003 schedule |
| Consensus namespaces | generated typed namespace constructors |
| Wire/persistence canonical bytes | versioned production codec for the purpose ID |
| Dependency source identity | locked release dependency graph and SBOM |
| Compatibility evidence | checked-in cross-implementation golden/negative vectors |

## Invariants

- Every consensus hash/signature/proof/codec use maps to one registered purpose ID.
- No production cryptographic operation runs with a default/unset chain identity.
- Accepted encoded input has exactly one logical interpretation and canonical output.
- Signer and verifier derive the same domain from the same typed inputs.
- Key role, curve variant, subgroup and proof-of-possession policy are explicit.
- Dependency/source changes cannot alter protocol outputs without activation.
- Persisted state and snapshots bind the profile needed to interpret their bytes.
- Historical verification never uses the current profile by accident.
- Secret material is excluded from manifests, logs, generic serialization and public
  snapshots.

## Atomicity, replay and failure

Profile selection happens before decoding or verification and is immutable for that
operation. Unknown, inactive or contradictory profiles fail before state mutation.
All signature/proof checks complete before the owning transition publishes effects;
failure rolls back through that module's atomicity contract.

Replay uses the profile selected by canonical height/object version. Missing
historical code/vectors is a node compatibility failure, not an excuse to reinterpret
bytes. Startup manifest mismatch is fatal. Verification failure is deterministic;
local accelerators/HSM/enclave outages are availability failures and cannot change
the cryptographic verdict.

## Compatibility and migration

New profiles receive new immutable IDs. Domain or codec changes never reuse an old
ID. Migration defines old/new production and verification windows, state/schema
conversion, key rotation and rollback limits. Cross-version objects carry an
unambiguous version or are selected by an authenticated activation height. Silent
autodetection and “try both verifiers” are forbidden unless the exact ordered rule is
itself part of a temporary activated profile.

## Production-interface verification evidence

Inspected consensus namespace construction, committee commitments, hybrid BLS
MinPk/MinSig certificate and VRF wire paths, Commonware codecs/archives, system/EVM
encoding seams, CE CES1/Poseidon/CKB-SMT commitment and persistence identity, ZK/TEE
dependencies and workspace lock resolution. Strong local evidence includes
chain-bound Simplex namespaces, strict hybrid presence tags, exact CE marker/codecs,
Poseidon vectors and pinned git revisions. No inspected artifact enumerates and
digests the complete cross-project profile set or proves all consumers are generated
from it. Status remains Proposed.

## Consequences

Cryptographic compatibility becomes an explicit protocol surface instead of a set of
library-shaped assumptions. Auditors can trace any digest/signature/root to exact
bytes and implementation, upgrades become reviewable, and module ADRs can import a
stable purpose rather than restating primitives inconsistently.

## Rejected alternatives

- **Name only the algorithm:** “BLS” and “Poseidon” omit variants, parameters,
  codecs, domains and validation behavior.
- **Treat `Cargo.lock` as the protocol specification:** it identifies code but not
  intended transcripts, canonical bytes or activation semantics.
- **Let each module invent domain tags:** collision/replay review and cross-language
  conformance become incomplete.
- **Accept multiple encodings for convenience:** malleability leaks into hashes,
  signatures, indexes and replay.
- **Upgrade crypto dependencies as ordinary maintenance:** behavioral changes can
  fork consensus or invalidate persisted roots.

## Open questions and technical debt

1. **Critical:** `consensus_chain_id()` falls back to `0` until initialization, and
   the namespace singleton caches its first value. Remove the production fallback
   and make signing/verifying impossible before explicit chain identity installation.
2. **Critical:** `init_consensus_chain_id` is process-wide first-writer-wins. A second
   differing chain/profile must fail explicitly; tests/tools must not reuse a stale
   namespace singleton across networks.
3. **Critical:** no complete `CryptoProfileManifest` exists. Enumerate every direct
   and transitive consensus cryptographic/codec use and generate typed consumers.
4. CE `tree_format` and `vendor_revision` appear as repeated string literals in
   production/test construction. Generate one exact dependency/profile identity and
   bind it to genesis, MDBX, proofs and snapshots.
5. The CE vendor revision string names `ad5553...`, while the workspace dependency
   declaration does not visibly expose a `ckb-smt` pin in the root manifest output
   inspected here. Document where the fork is sourced, patched and reproducibly
   verified against that revision.
6. Cargo resolves multiple `ark-*`, `k256` and `sha2` generations transitively.
   Identify which instance implements each protocol purpose and prevent accidental
   cross-version key/field/serialization mixing.
7. Commonware is declared by tag `v2026.5.0` and resolves commit `b8b0a8d...`; bind
   the resolved commit/features into release/profile evidence and review tag movement
   or lockfile refresh as a compatibility event.
8. Reth v2.2.0/revm 38 and the patched Alloy EVM fork affect consensus execution.
   Publish exact resolved commits, patch provenance and historical differential
   vectors rather than relying on version comments.
9. Inventory every namespace suffix and prove purpose uniqueness. Current central
   Simplex/seed constructors are good evidence, but DKG, TEE, system transactions,
   PoW, ZK and module-specific hashes need the same registry.
10. Decide whether consensus namespaces must bind genesis/chain-manifest and crypto
    profile in addition to chain id. Chain IDs can be reused across distinct genesis
    states, making chain-id-only replay separation weaker than full network identity.
11. Committee commitment uses Keccak over Commonware `Set` order and MinPk bytes.
    Publish exact key codec/order vectors and prove every committee producer uses the
    same canonicalization before bitmap/signature verification.
12. Prove proof-of-possession/rogue-key protection for every validator BLS identity
    admission, imported genesis key and rotation path; aggregation safety cannot rely
    only on unique addresses.
13. Pin empty signer, identity point, infinity, subgroup, duplicate signer, unused
    bitmap bit and non-canonical compressed-point rejection across all BLS paths.
14. Hybrid certificate/VRF codecs have local strict checks, but add fixed byte vectors
    shared with archive/RPC/follower/SlashIndicator and independent decoders.
15. Define DKG share/polynomial/output codec versions and context binding. Secret
    `export-share/import-share` must reject material from another chain, committee,
    epoch, threshold or crypto profile.
16. CE CES1 constants and Poseidon vectors are well tested locally; publish the full
    field-byte conversion, rejection, CKB merge-with-zero and catalog transcript as
    profile artifacts consumable outside Rust.
17. Pin `outbe-poseidon` and circuit repositories by resolved commit in the release
    manifest, not only `v0.11.0` tags, and prove VK/CRS/circuit/public-input profiles
    cannot drift independently.
18. Audit protobuf body encoding for canonicality. Protobuf generally permits field
    ordering/unknown-field variations; committed body bytes need one canonical
    producer and strict accepted-form policy.
19. Audit every `serde`/JSON/bincode-like use in signed or persisted structures.
    Convenience serialization is not automatically stable or canonical across
    versions/languages.
20. Define TEE Ed25519/X25519/AES-GCM transcript domains, nonce uniqueness, AAD,
    measurement/policy binding, sealed-format version and downgrade rejection with
    host/enclave cross-vectors.
21. Define system-transaction signature/preimage purposes separately for every phase
    and witness kind; chain id alone must not allow cross-kind replay.
22. Generate ABI selectors/events and storage-slot vectors from one interface/layout
    manifest and compare with Solidity tooling. Handwritten string signatures can
    silently diverge while still hashing successfully.
23. Add a strict startup report showing active/historical profile IDs, manifest
    digest, resolved crypto dependency commits and store identities without exposing
    secrets.
24. Add CI that detects new hash/sign/verify/encode/decode dependencies and call sites
    without a registered purpose, vectors, owner ADR and activation policy.
25. Establish cryptographic agility policy: review/activation lead time, dual-verify
    bounds, key rotation, historical retention, emergency disable and rollback
    behavior for a broken primitive or dependency.
26. Commission independent review and cross-implementation vectors for consensus BLS/
    VRF/DKG, CES1/SMT, ZK verifier and TEE custody profiles before production status.
