# ADR-C-TRB-002: TributeFactory owns encrypted-offer admission and issuance orchestration

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/tributefactory`, its public ABI and synchronous
  enclave-offer client
- **Depends on:** ADR-C-MET-001, ADR-C-AGR-001, ADR-S-TEE-001, ADR-C-TRB-001, ADR-S-ORC-001
- **Supersedes:** The encrypted-offer admission portions of the deleted pre-space TEE/Tribute aggregate

## Context

TributeFactory converts one opaque client offer into public consensus effects. It
does not own the offer secret, attestation policy, Oracle, WorldwideDay, Tribute or
AgentReward state. Its architectural role is to bind those authorities into one
validated, atomic command. PFS-001 specifies the larger transaction-to-finalized-
projection saga.

## Decision

`ITributeFactory.offerTribute` is the sole user mutation interface. It accepts no
value and binds the transaction sender to the requested owner. The factory reads a
canonical price for an offering day, constructs the exact enclave request, verifies
the response against the session-pinned attestation key, independently validates
public identity/day/replay constraints, then atomically consumes SU hashes, issues
one Tribute and records optional WAA/SRA AgentReward contributions.

The enclave is the authority for decrypted payload validity and private economic
calculation. The host remains responsible for every check it can derive from public
chain state. Attestation is evidence of which code produced a result; it is not a
substitute for binding returned public fields to the exact request.

## Authoritative interface and command

The ABI supplies ciphertext, nonce, ephemeral X25519 public key, reference currency,
Intex-exclusion flag and four reserved ZK byte fields. The dispatcher currently
ignores all ZK fields. It builds `OfferTributeInput` with the actual caller and
invokes crate-private `offer_tribute` under the EVM transaction journal.

The command sequence is:

1. accept only reference currency 840;
2. select an active `OFFERING` day and resolve the maximum of that day's Oracle VWAP
   and active S-curve value for the registered settlement pair;
3. synchronously send sender, encrypted envelope, public flags and price to the
   configured attested enclave;
4. verify the enclave's canonical-input hash and Ed25519 attestation signature;
5. require a created result, an offering returned day, and canonical
   `Poseidon(sender, day)` owner/token identity;
6. require the Tribute identity absent, parse and consume all returned SU hashes,
   and validate paired WAA/SRA address lists;
7. issue the immutable Tribute from the returned economics and increment the
   AgentReward counters.

## State and invariants

TributeFactory owns only `used_su_hashes: B256 -> bool`. A successful command must
satisfy:

- every returned SU hash is exactly 32 decoded bytes and transitions globally
  from unused to used exactly once;
- the Tribute id equals the public canonical derivation and was absent before the
  command;
- returned day is offering and the Tribute partition accepts issuance;
- Tribute body fields are the accepted enclave result for the exact signed request;
- WAA and SRA lists are either both empty or both nonempty and every entry parses
  as an address;
- SU markers, Tribute issuance, day/supply aggregates, events and all reward
  increments commit or roll back together.

`used_su_hashes` is a permanent replay set. It has no delete, expiry or reuse
transition. Tribute duplicate identity is an independent replay guard and must not
substitute for SU uniqueness.

## Trust, determinism and availability

The enclave owns ciphertext decryption, hidden-payload parsing, amount
normalization and private economics. ADR-S-TEE-001 owns transport, attestation and sealed
secret rules; ADR-S-TEE-002 owns committee offer-key identity. TributeFactory trusts
accepted enclave code for hidden computation but independently binds public sender,
day and identity. The response signature proves which locally attested enclave
produced it; consensus agreement still requires every executor to obtain the same
result.

The synchronous enclave call is on the consensus execution path. Missing client,
timeout, transport error or invalid response reverts the offer today. Node startup
only connects when a socket argument is present, while the execution adapter has no
in-process fallback; TEE-required readiness must therefore be a uniform chain rule,
not validator-local configuration.

## Atomicity, replay and failure classification

Normal validation failures and enclave rejection revert the user transaction.
Storage corruption and canonical-id derivation failure retain fatal classification.
Although `offer_tribute` has no private checkpoint, the EVM transaction journal is
the authoritative atomic boundary and must include the compressed-entity overlay
and emitted projection events.

A repeated SU hash or existing owner/day identity reverts without partial effects.
An execution retry may repeat the enclave call before state commit; it must return
byte-equivalent public results. A timeout is ambiguous with respect to enclave-local
work but safe only because offer processing has no durable enclave-side mutation.

## Bounds and compatibility

Ciphertext, nonce, result batch, SU list, address lists and strings cross allocation
and iteration boundaries. Explicit protocol caps, ABI preflight and gas accounting
must agree with enclave frame limits. The current single-offer host sends a batch of
one and must require exactly one result.

ABI selectors, the wire protocol shared by client, host and enclave, the
attestation preimage, AEAD envelope, price rule, currency semantics and SU marker
format are compatibility surfaces. They require versioned activation plus explicit
old/new compatibility rules. An enclave measurement change alone does not safely
migrate ciphertext or consensus rules.

## module audit profile and production evidence

The intended module is one typed `AdmitEncryptedOffer` command returning a receipt
that records the accepted public result and all downstream effect receipts. It must
have a closed enclave verifier, deterministic canonical-state inputs, bounded
outputs and one commit boundary.

Evidence inspected includes TributeFactory schema/state/runtime/precompile/tests,
`ITributeFactory.sol`, the enclave offer adapter, TEE protocol/result/signature
codec, node startup wiring, and Tribute/AgentReward/Metadosis/Oracle callees.
Closure evidence still requires a real production-interface happy path plus
rollback, replay, malformed response, multi-validator sidecar failure and resource
limit tests.

## Consequences and rejected alternatives

The host does not learn offer secrets and does not recompute private economics.
Public checks limit the enclave's authority to fields the host cannot independently
derive. Keeping only SU replay state makes this an orchestrator, not another ledger,
and allows Tribute and AgentReward invariants to remain with their modules.
Host-side recomputation of private economics was rejected
because it breaks confidentiality. Blind trust in an attested response was rejected
because public request bindings are independently checkable. Folding this flow into
a broad TEE ADR was rejected because it obscures the transaction authority and its
atomic side effects.

## Open questions and technical debt

- Bind all returned public echoes to the request. The host currently checks only
  owner/token identity and offering status; it does not compare returned reference
  currency, exclusion flag or Tribute price with the values sent to the enclave.
- Require exactly one result for the one-offer request. The current code takes the
  first result and silently ignores any additional signed results.
- Bind the priced WorldwideDay to the returned WorldwideDay. Price is resolved from
  the first active `OFFERING` day, but a result for a different simultaneously
  offering day is accepted with that price.
- Remove the stale node-startup claim that an unset socket uses an in-process stub,
  or implement and explicitly restrict such a fallback to a dev-only chain policy;
  the current execution adapter returns “client not configured”.
- Make enclave readiness a chain-wide block-execution prerequisite. A local
  sidecar outage/configuration difference can otherwise make honest validators
  disagree on whether an offer-bearing block executes.
- Reject or verify the four reserved ZK ABI fields. Silently ignoring proof-shaped
  inputs creates a false verification claim and consumes an unversioned surface.
- Define and enforce nonce length, ephemeral X25519 encoding, ciphertext/plaintext,
  batch, SU-marker and reward-address cardinality limits before allocation and
  iteration; current ABI/runtime paths expose no explicit caps.
- Require a nonempty SU set if that is the economic anti-replay identity, and
  define whether duplicate markers inside one enclave result are corruption or a
  normal revert.
- Replace string-encoded SU hashes and reward addresses in the enclave result with
  fixed typed wire values and a versioned codec.
- Persist or otherwise make auditable the enclave measurement/key epoch that
  authorized a result. The per-offer signature is discarded after local checking.
- Replace `serde_json::to_vec(...).unwrap_or_default()` in the shared attestation
  preimage with an infallible canonical codec or propagate serialization failure;
  an empty fallback could sign/verify an unintended representation.
- Add full ABI tests for success, duplicate identity/SU, malformed and duplicate
  hashes, response count mismatch, echo/day mismatch, reserved ZK fields, every
  downstream failure, rollback, replay and multi-validator receipt/state equality.
- Version and bind the encrypted envelope to chain id, contract, caller, offer-key
  epoch and public flags through AEAD associated data to prevent cross-domain replay.
