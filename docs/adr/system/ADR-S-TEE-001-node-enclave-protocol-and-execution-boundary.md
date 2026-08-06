# ADR-S-TEE-001: TEE defines the node-to-enclave protocol and secret execution boundary

- **Status:** Accepted for the V1 DCAP activation boundary; I9 hardware evidence remains gated
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/tee`, `bin/outbe-tee-enclave`; wire protocol,
  attested channel, enclave command execution, DKG secret handling and sealed restart
- **Depends on:** ADR-B-GEN-001, ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CNS-003 and ADR-B-EVM-004, ADR-B-CRY-001
- **Related:** ADR-B-CNS-002 DKG, ADR-S-TEE-002 TeeRegistry, ADR-C-TRB-001 Tribute, ADR-C-TRB-002
  TributeFactory
- **Supersedes:** The transport/enclave-local portions of the deleted pre-space TEE/Tribute aggregate

## Context

The host node must orchestrate DKG and Tribute execution without learning threshold
shares, the recovered group signature, the derived offer private key or decrypted
offer payload. The enclave is an asynchronous external process with a different
failure and persistence domain; an in-process Rust call cannot describe its
atomicity, retry or compatibility contract.

## Decision

The TEE module owns a versioned request/response protocol carried over a mutually
bound attested Noise channel. The untrusted host may route public envelopes and
opaque ciphertexts, but secrets are created, opened, recovered, used and sealed
inside `outbe-tee-enclave`. On-chain state never treats a local transport response
as committed authority without the validation and consensus steps owned by the
calling module.

UDS and TCP are interchangeable carriers only. Every post-quote byte is protected
by Noise IK with the enclave static key bound into SGX `REPORT_DATA`. Carrier choice
must not weaken peer identity, confidentiality or message limits.

## Attestation and channel establishment

The client sends a fresh quote nonce before Noise, verifies that
`keccak256(noise_static || recipient_x25519 || attestation_pub)` equals report data,
and for a non-empty hardware quote verifies quoted measurements, report-data
binding, DCAP signature/TCB, measurement allowlists and minimum SVN. It then pins
the attested Noise static key and uses an ephemeral host static key for that session.

An empty quote or arbitrary measurement is accepted only under explicit dev policy.
`dev_fallback_if_unattested` applies only to empty quotes;
`dev_accept_any_measurement` also relaxes a real quote and is forbidden in
production. The attestation label is diagnostic; trust derives from verification,
not the enclave's self-reported string.

The quote also binds the one-time X25519 registry-onboarding recipient and
Ed25519 result-attestation keys. Per-offer result signatures are verified against
this pinned key before the host consumes results.

## Protocol commands and authority

The production preamble is restricted to initialization discovery, one-time
initialization and authorized reconnect. Authenticated owner sessions expose
public keys, stateful founding DKG phases A–F, offer processing, DCAP quote/QVL
operations and deterministic one-time registry delivery. There is no peer
key-delivery or lost-key recovery command.

Each command needs a typed phase guard:

```text
fresh process -> Initialize -> Ready
Ready -> DkgOpen(ceremony) -> Dealer/Player phases -> PlayerFinalized
PlayerFinalized -> offer partial -> FinalizeTributeOffer -> ReadyWithOfferKey
ReadyWithOfferKey -> ProcessOffer | SealForRegistry
Ready -> IngestFinalizedRegistryArtifact(expected on-chain public) -> ReadyWithOfferKey
```

All unlisted phase/ceremony combinations reject. Ceremony ids, round, participant
identities and canonical intent bind every stateful request. DKG session state is
currently per connection, so one ceremony's ordered phase sequence must remain on
the same authenticated session unless explicit resumable state is added.

## Secret custody and persistence

The enclave owns root/identity keys, DKG share-encryption secret, threshold share,
recovered group signature and derived Tribute-offer private key. Dealers return only
public commitments and recipient-sealed shares. Offer partials are sealed separately
to each participant; the host cannot combine plaintext partials.

Founding Seam F finalizes the offer key through HKDF bound to chain id and the
fixed genesis offer-key epoch, stores it in a process-wide `OnceLock`, returns only
its public key and seals the exact secret for restart. A second divergent
finalization fails. SGX sealing uses an explicit signer policy; development fixed
keys are not production custody.

For a newly admitted identity, every consensus node deterministically constructs
the same one-time registry artifact for the attested recipient. Ingestion accepts
it only when its derived public equals the permanent on-chain TeeRegistry key.
Idempotent registration, renewal and replacement do not redeliver it.

## Offer execution contract

The host supplies public owner/flags, ciphertext envelope and a price read from
committed Oracle state. The enclave decrypts the private payload, computes economics
and Poseidon token id, and returns only the public `TributeOfferResult`, replay
markers and reward-routing addresses. It never returns plaintext input or private
key material.

The shared canonical-input hash length-prefixes every offer field. The enclave signs
an attestation preimage binding that input hash to the ordered results; the host
recomputes and verifies both. The result is still only a proposed deterministic
execution outcome: TributeFactory must validate shape, input/result correspondence,
status and cross-module invariants before journaled on-chain mutation.

## Framing, bounds and concurrency

Frames are four-byte big-endian length plus at most 64 KiB of postcard bytes. Noise
messages, handshake messages and cleartext quote responses use the same cap. Every
client socket read/write has a 30-second timeout.

The enclave server currently spawns one OS thread per accepted UDS/TCP connection.
Keys and recovered offer key are shared; DKG session stores are connection-local.
Production must bound connections, threads, memory, cryptographic work and request
size/participant counts independently of the frame cap.

## Atomicity, replay and failure domains

Noise request/response changes transport counters and may mutate enclave-resident or
sealed state, but cannot join the EVM journal. A timeout or disconnect is ambiguous:
the enclave may have completed while the host did not receive the response. Each
stateful command therefore needs intent-bound idempotency or an inspect/resume query.
Blind retry on a new session is unsafe unless the command contract says otherwise.

On-chain callers must follow a prepare/validate/commit model:

```text
enclave external operation -> authenticated typed receipt
host validates receipt against canonical request/on-chain state
EVM transaction commits public outcome or commits nothing
```

Sealing is local durability, not network consensus. Once the public key is
authoritative, missing, corrupt, wrong-chain or unsealable permanent state is
terminal for that identity. Startup does not invoke DKG, reshare, peer delivery,
governance replacement or development fallback. Node readiness blocks consensus
and transaction execution without the exact resident key.

## Determinism and consensus

Postcard/serde enum representation, DKG codecs, canonical hashes, crypto domains,
HKDF context and result ordering must be byte-identical across binaries. Host inputs
come from committed chain state. All validators must either obtain the same public
result or reject the block; attestation proves origin, not semantic correctness or
cross-enclave determinism by itself.

No wall clock, process scheduling, random sealing nonce or connection order may
enter consensus-visible results. Operations deliberately requiring randomness must
return a public commitment whose acceptance rules tolerate byte variance; the
registry-delivery sealing command is explicitly deterministic because its bytes are
committed on-chain.

## Compatibility and activation

The current protocol uses serde enum variant ordering and comments assume host and
enclave are built together. That is insufficient for rolling upgrades, mixed node
binaries, sealed state across restarts and on-chain evidence longevity. The wire,
sealed blob and every cryptographic domain need explicit version negotiation and an
activation matrix.

Measurement allowlists/minimum SVN are policy state and must coordinate with binary
rollout so a node never accepts an obsolete vulnerable enclave or rejects the only
activation-compatible one. Dev attestation flags must fail startup on production
chain ids.

Testnet release identity has two independently verified layers. Operators/CI verify the
Cosign signature on the exact OCI digest before launch; Gramine verifies the embedded
SIGSTRUCT at enclave load. The ReleaseManifest binds that OCI digest to MRENCLAVE,
MRSIGNER, ISVPRODID, ISVSVN and the signed Gramine archive. Runtime containers receive no
private signing key and do not render or sign their manifest.

A normal enclave upgrade preserves MRSIGNER, never decreases ISVSVN and pre-authorizes an
overlap of old/new MRENCLAVE before rolling nodes. MRSIGNER sealing therefore survives the
code measurement change. A different MRSIGNER cannot inherit the identity or key and must
onboard as a new identity; there is no governed key handoff/rebootstrap. Exact-release SGX
and accepted DCAP evidence remain I9 release gates.

## Production-interface and architectural evidence

Inspected evidence includes protocol/codec/client, quote verification, DKG driver,
registry onboarding, enclave transport/run/key/seal paths, UDS/TCP Noise tests and
real ceremony/transport integration tests. Evidence covers channel binding and core
happy/failure paths but not every ambiguous disconnect, concurrent connection,
sealed-state migration or mixed-version history.

This boundary has not passed architecture review. Closure requires explicit protocol versions,
typed command receipts and phase transitions, durable intent/idempotency state for
ambiguous operations, bounded server scheduling, production-chain policy gates and
fault simulation through the real transport/enclave binary.

## Consequences and rejected alternatives

Keeping secrets resident in the enclave prevents the host from impersonating DKG
participants or decrypting Tribute offers. Plain UDS/TCP was rejected because local
host/root and network carriers are not trusted. Returning plaintext shares/partials
was rejected. Treating attestation as proof of deterministic business correctness
was rejected: consensus and caller validation remain necessary.

## Remaining technical debt and release evidence

- Add an explicit wire protocol version/capability handshake. Serde enum variant
  indexes and “both binaries built together” are not a safe compatibility policy.
- Version sealed root/DKG/offer-key blobs and define rollback plus SVN
  upgrade/downgrade rules. Partial or corrupt permanent-key writes remain terminal.
- Define intent ids and query/resume semantics for every stateful DKG/
  ingestion command. A timeout after enclave commit is currently ambiguous.
- Bound the thread-per-connection server, concurrent sessions, DKG participant
  count, sealed shares, batch offers and cryptographic work; test saturation and
  fairness rather than relying only on 64 KiB frames.
- Freeze the exact `native-dcap` release feature graph and trusted native-artifact
  digests. Runtime ChainSpec parsing already confines `GramineDirectDev` to its
  reserved development identity and rejects production-to-development fallback.
- Retain fresh Processor and root collateral provenance for the exact release.
  Retain Platform collateral with the real Platform node's admission when one
  joins. Missing or stale collateral is rejection, never a live consensus fetch.
- `tribute_offer_attestation_preimage` uses `serde_json::to_vec(...).unwrap_or_default()`;
  serialization failure must be explicit, never silently sign an empty result body.
- Replace JSON in the result-attestation preimage with a versioned canonical binary
  encoding and independent test vectors.
- Validate request/response variant correspondence and ceremony phase through typed
  session methods rather than one universal enum request interface.
- Complete exact-release SGX proof for deterministic registry sealing and restart;
  hardware-free tests and the isolated development E2E already cover byte equality,
  no redelivery and fail-closed permanent-key loss. Randomized peer delivery remains
  unreachable.
- Keep the genesis offer key permanent. A future rotation design is out of scope
  and cannot be inferred from dormant epoch fields.
- The host supplies Oracle price and public offer fields; bind all of them to the
  executing block/state and reject stale/different state rather than relying only
  on eventual state-root mismatch.
- Add deterministic fault simulation for dropped/duplicated/truncated responses,
  timeout after commit, reconnect, concurrent ceremonies, crash during seal, stale
  quote, revoked TCB and malformed registry artifacts; retain seeds/schedules.
- Add production-binary mixed-version and sealed-state upgrade tests plus measured
  throughput/cap tests at cap-1/cap/cap+1.
