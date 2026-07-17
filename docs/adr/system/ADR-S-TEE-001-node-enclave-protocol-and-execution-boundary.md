# ADR-S-TEE-001: TEE defines the node-to-enclave protocol and secret execution boundary

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
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

The quote also binds the X25519 handoff recipient and Ed25519 result-attestation
keys. Per-offer result signatures are verified against this pinned key before the
host consumes results.

## Protocol commands and authority

`GetQuote` is the only cleartext pre-handshake command. Authenticated session
commands include initialization/public keys, stateful DKG phases A–F, offer-batch
processing, peer handoff sealing/ingestion and deterministic registry delivery.

Each command needs a typed phase guard:

```text
fresh process -> Initialize -> Ready
Ready -> DkgOpen(ceremony) -> Dealer/Player phases -> PlayerFinalized
PlayerFinalized -> offer partial -> RecoverTributeOffer -> ReadyWithOfferKey
ReadyWithOfferKey -> ProcessOffer | SealHandoff | SealForRegistry
Ready -> IngestHandoff(expected on-chain public) -> ReadyWithOfferKey
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

Recovery derives the offer key through HKDF bound to chain id and offer-key epoch,
stores it in a process-wide `OnceLock`, returns only its public key and seals the
recoverable secret for restart. A second divergent derivation must fail. SGX sealing
uses an explicit measurement/signer policy; development fixed keys are not
production custody.

Handoff servers independently verify the newcomer's quote and authorization before
sealing to its attested X25519 key. Newcomer ingestion accepts the secret only when
the derived public equals the expected on-chain TeeRegistry key.

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

Sealing is local durability, not network consensus. Failure to seal after a public
on-chain key becomes authoritative needs a recoverable handoff/bootstrap path.
Connection errors affect one request/thread and do not stop the server; node
readiness must decide whether loss of enclave capability is fatal for proposing,
validating or only Tribute operations.

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

## Production-interface and architectural evidence

Inspected evidence includes protocol/codec/client, quote verification, DKG driver,
handoff coordinator, enclave transport/run/key/seal paths, UDS/TCP Noise tests and
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

## Open questions and technical debt

- Add an explicit wire protocol version/capability handshake. Serde enum variant
  indexes and “both binaries built together” are not a safe compatibility policy.
- Version and migrate sealed root/DKG/offer-key blobs; define rollback protection,
  SVN upgrade/downgrade rules and recovery after partial/corrupt sealed writes.
- Define intent ids and query/resume semantics for every stateful DKG/recovery/
  ingestion command. A timeout after enclave commit is currently ambiguous.
- Bound the thread-per-connection server, concurrent sessions, DKG participant
  count, sealed shares, batch offers and cryptographic work; test saturation and
  fairness rather than relying only on 64 KiB frames.
- Enforce production chain-id policy that rejects `dev_accept_any_measurement`,
  unattested fallback, mock/fixed sealing keys and loopback shortcuts.
- Pin the DCAP trust-store/TCB collateral freshness and offline/revocation policy;
  document what happens when collateral cannot be refreshed.
- `tribute_offer_attestation_preimage` uses `serde_json::to_vec(...).unwrap_or_default()`;
  serialization failure must be explicit, never silently sign an empty result body.
- Replace JSON in the result-attestation preimage with a versioned canonical binary
  encoding and independent test vectors.
- Validate request/response variant correspondence and ceremony phase through typed
  session methods rather than one universal enum request interface.
- Define node readiness/fatality when enclave connection, attestation, sealing or
  resident offer key is unavailable for proposer versus validating/follower roles.
- Prove deterministic registry sealing across enclave implementations and restarts;
  randomized P2P handoff blobs must never enter consensus state.
- Define offer-key rotation and overlap: epoch activation, old ciphertext handling,
  rollback, simultaneous keys and deletion/retention of retired secrets.
- Handoff currently allows a configurable `min_confirmations` with a floor of one.
  Bind the threshold and responder authorization to on-chain committee policy.
- The host supplies Oracle price and public offer fields; bind all of them to the
  executing block/state and reject stale/different state rather than relying only
  on eventual state-root mismatch.
- Add deterministic fault simulation for dropped/duplicated/truncated responses,
  timeout after commit, reconnect, concurrent ceremonies, crash during seal, stale
  quote, revoked TCB and handoff with Byzantine responders; retain seeds/schedules.
- Add production-binary mixed-version and sealed-state upgrade tests plus measured
  throughput/cap tests at cap-1/cap/cap+1.
