# I5 checkpoint — renewal, expiry and enclave supersession

Date: 2026-08-01

Status: `PASS` for I5. Validator and FullNode renewal/replacement are
implemented and verified, but the V1 public route remains inactive until I9.
This checkpoint does not claim accepted SGX hardware evidence. I9 still owns
the fail-not-skip exact-release Processor acceptance gate; Platform evidence is
verified when a real Platform node is admitted.

## Outcome

An active attested binding is now a renewable lease. Renewal opens only in the
final third, requires the exact next registration version and renewal nonce,
and may recover an expired current binding. Exact evidence replay is
idempotent. A stale, conflicting or superseded identity cannot renew.

A validator or full node may stage a fresh enclave B without changing normal
startup from committed enclave A. B receives fresh enclave identity keys and
one exact replacement intent while preserving the node's persistent NodeHost
authority. Registry replacement permanently consumes the former enclave and
binding IDs. Local NodeHost promotion remains impossible until an opaque
authorization binds the exact finalized replacement intent and candidate
manifest.

## Exact and stable commitments

`EnclaveInitializationManifestV1::authorization_hash()` remains the exact
node-signed initialization and sealing commitment. It contains chain/genesis,
profile, node identity, one-use initialization challenge, persistent NodeHost
public key and all three enclave public keys.

`node_host_authorization_hash()` is a separate domain-separated commitment over
the protocol version, chain, genesis, profile, canonical node identity and
persistent NodeHost Noise public key. It excludes the challenge and enclave
keys. Changing the node, profile, chain or NodeHost key changes the commitment;
initializing a fresh enclave for that same node does not. The frozen known-answer
vector is
`b13eb9d78b874da96d5a8262fcb2ff1237d2be7ee82c127d3138fb47b653e838`.

The Registry stores this stable commitment at append-only slot 47. Slot 46
stores `lease_started_at`, which makes the final-third decision independent of
the requested duration of the next lease.

## Registry lifecycle

One shared, profile-generic state machine owns register, renew and replace.
Every accepted mutation verifies the expected operation, node and enclave
proofs of possession, active policy and measurement, exact counters, lease
bounds, collateral margin, reverse ownership and exact evidence replay.

Renewal preserves enclave ID, binding ID, all quote-bound public keys,
measurement and NodeHost authority. Replacement preserves node/profile and
NodeHost authority but requires fresh enclave and binding IDs and the exact next
binding/registration versions. Reverse ownership entries are append-only, so a
superseded identity is never reusable. Governance policy changes do not
retroactively shorten an existing lease, and a verifier/PCS outage cannot
extend one.

At the normative 896-KiB evidence cap, renewal costs at most `28,668,784` gas
and replacement at most `29,133,784`, both below the 30-million steady block.
Preflight caps and every gas calculation use checked arithmetic.

## Candidate A-to-B lifecycle

Candidate state is bounded, canonical and owner-only under the existing
NodeHost data directory:

- `replacement-candidate.v1` binds B's full manifest to A's exact predecessor
  manifest hash;
- `replacement-submission.v1` durably stores exact canonical evidence plus node
  and enclave signatures;
- `replacement-promotion.v1` is the completed-promotion receipt and binds the
  exact pair `(intent_hash, candidate_manifest_hash)`;
- `state.lock` is an owner-only interprocess lock held across every public
  NodeHost state transition, so competing first writers cannot both observe an
  absent candidate or submission and silently replace one another;
- `.next` journal files make candidate refresh and active-manifest promotion
  crash-recoverable with file and parent-directory `fsync` ordering. Initial
  candidate staging and submission persistence use the same atomic journal
  protocol rather than exposing a partially written final file. Bytes first
  reach an uncommitted `replacement-write.tmp`; only a complete, fsynced file is
  renamed to `.next`, and restart discards a torn scratch file.

All owned state reads are descriptor-based, reject symlinks/foreign ownership
or incorrect modes, and read through `take(maximum + 1)` before enforcing the
cap. This keeps a concurrently growing file bounded after the metadata check.
The public submission reload path reconciles interrupted journal writes, then
revalidates exact manifest binding and the real enclave proof of possession
before returning transaction bytes.

Staging first persists B, then initializes it over the existing authenticated
Noise protocol. Restart reconnects the exact initialized B. If the process died
after staging but before initialization, it may accept a refreshed one-use
challenge only when all three enclave identity keys are unchanged; identity
substitution rejects. Once a submission exists, loss of B fails closed instead
of rewriting transaction material.

Ordinary startup and reconciliation never auto-promote. A durable `.next`
manifest with active A remains pending. Promotion atomically renames the exact B
manifest only after `FinalizedReplacementAuthorizationV1` matches both
`intent_hash` and `candidate_manifest_hash`; completed exact promotion is
idempotent only when the supplied pair matches the durable promotion receipt.
A same-manifest retry carrying another intent rejects. Restart cleans only
already-committed journal residue and retains the exact receipt. I5 exposes no
production constructor for that authority. I6 must construct it after finalized
Registry verification.

I5 deliberately does not hot-swap a live client, deliver/migrate the permanent
offer key, add recovery or governance replacement, or claim candidate B is
consensus-ready. B remains keyless until later finalized key-delivery/readiness
logic authorizes it.

## Acceptance audit

| I5 criterion | Authoritative evidence | Result |
|---|---|---|
| Final-third renewal and expired-current recovery | exact `lease_started_at` interval tests cover boundary-before, boundary-at and expired current | `PASS` |
| Exact replay and counters | exact evidence is idempotent; stale/conflicting versions, nonces and evidence reject | `PASS` |
| Supersession | old identity cannot renew and consumed enclave/binding IDs cannot be reused | `PASS` |
| Validator and FullNode parity | both profiles traverse the shared mutation state machine; three replicas compare state, events, operations and gas | `PASS` |
| Stable NodeHost continuity | two fresh production enclave initializations have distinct full hashes/keys and the frozen stable hash | `PASS` |
| Active A unaffected by candidate B | real UDS lifecycle keeps A as committed startup target while B initializes; normal startup still reconnects A | `PASS` |
| Reachable crash resume | a real candidate process restarts with sealed identity keys and a new challenge, then resumes exact B | `PASS` |
| Durable submission | the state lock serializes competing writers; candidate/evidence journals recover atomically; public reload revalidates exact binding/PoP; exact replay succeeds and conflict rejects | `PASS` |
| Finalized-only promotion | wrong intent/candidate authorization rejects without mutation; exact pair is durably receipted and alone is idempotent | `PASS` |
| Crash reconciliation | torn scratch, candidate-only and submission-only journal crashes recover exactly; post-promotion submission-only residue is receipt-checked and cleaned; pre-rename promotion never auto-promotes | `PASS` |
| Unchanged intent seam | real UDS/Noise candidate B generates and signs the quote-bound intent persisted and passed into Registry byte-for-byte; the structurally bound quote generator and accepted Registry verdict are explicitly test-only | `PASS` |
| Steady-block capacity | renewal `28,668,784`; replacement `29,133,784` at normative caps | `PASS` |
| Hardware acceptance remains I9 | accepted Registry tests start after the private typed verifier boundary | `PASS` scope boundary |

## Reachable verification

The checkpoint was closed with:

```bash
env CARGO_TARGET_DIR=/tmp/outbe-i5-racefix-replay-target \
  scripts/release/test_dcap_replay_ci.sh
cargo test -p outbe-primitives --features tee-attestation-v1 \
  --test tee_attestation_v1
cargo test -p outbe-teeregistry --features tee-attestation-v1
cargo test -p outbe-tee
cargo test -p outbe-tee-enclave --features native-dcap --tests
cargo check -p outbe-chain
cargo fmt --all -- --check
git diff --check
```

Results:

- the clean-target mandatory replay passed 19 primitive tests, 53 native host
  tests, 25 public DCAP tests, five native-QVL tests and two fixture-tool tests;
- the focused primitive, Registry and default NodeHost suites passed 19, 29 and
  41 tests respectively; Registry includes the exact candidate-generated intent
  seam;
- the native enclave package passed all 108 unit tests, both DKG integration
  tests and three non-benchmark transport integration tests;
- the broad transport suite initially exposed a harness that waited for a third
  UDS connection although its reachable scenario creates exactly discovery and
  initialize/Noise connections. The bound was corrected to two and both the
  focused native-QVL case and complete suite passed;
- default `outbe-chain` compilation remains free of native-QVL feature leakage;
- format and whitespace checks passed.

## Deferred, not waived

I6 owns finalized-state verification, production construction of the exact
promotion authorization and remote session admission. I7 owns rolling
measurement policy. I8 owns block-1 integration with the existing bootstrap/DKG
result. A later separately authorized path may use only the existing
key-delivery/readiness machinery; I9 must prove that real exact-release SGX
renewal/replacement, finalized promotion/lookup and Noise flow before
activation. Offer-key recovery, proof of deletion and software-migration
continuity remain explicit non-goals.
