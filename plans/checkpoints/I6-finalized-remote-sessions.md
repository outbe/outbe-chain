# I6 checkpoint — finalized remote Noise sessions

Date: 2026-08-01

Status: `PASS` for I6. Validator and FullNode remote admission and finalized
replacement authorization are implemented and verified. The V1 public route
remains inactive until I9. This checkpoint does not claim accepted SGX hardware
evidence; I9 still owns the fail-not-skip exact-release Processor and real
`gramine-sgx` gates. Platform evidence is node-specific at admission.

## Outcome

A node now admits a remote Noise session only from the current
consensus-finalized Registry state and only for the exact source persistent
`NodeHost` static and target quote-bound responder key. The target enclave
consumes a bounded one-use ticket before Noise IK, proves live possession of the
registered responder key and exposes only `GetPublicKeys` to the resulting
remote context.

The same node-owned finalized-state authority is the first production issuer of
I5's opaque replacement-promotion capability. It binds the exact active
Registry replacement intent to the durable candidate manifest before atomic
promotion.

## Current-finalized local authority

The production facade
`authorize_local_finalized_remote_session_v1` owns the complete local path:

1. read the provider's current finalized marker;
2. require the matching canonical hash, number and sealed header;
3. open historical state for that exact hash from the same provider;
4. verify source and target bindings, profiles, stable NodeHost commitment,
   quote-bound responder key and unexpired leases;
5. install one one-use ticket through the authenticated local enclave client.

No caller-supplied finalized token exists in this facade. The H-to-H+1
regression advances the same provider's marker and proves that the old H state
can no longer authorize either a session or replacement promotion, even though
H remains a valid historical block.

The session deadline is the earlier of source and target `valid_until`.
Admission pins chain, genesis, both canonical node identity hashes, both
profiles, the exact source `NodeHost` static and the target responder static.

## External and RPC trust modes

An external verifier accepts Registry state only through a real Ethereum MPT
account/storage proof rooted in a state root already authenticated by its
trusted genesis/checkpoint and light-client view. Tampering with a nonzero
Registry word rejects.

The separate RPC helper remains explicitly typed as RPC-trusted. It does not
claim finalized proof authority. No continuous light client was added inside
the enclave for session admission.

## Enclave capability boundary

Pending remote sessions are capped at 64. Tickets are unpredictable, one-use,
consumed before Noise IK and rejected on replay. The generic owner request API
rejects raw `AuthorizeRemoteSessionV1`; the typed path is the normal
composition seam.

This host-side API restriction is defense in depth, not the TEE boundary. A
hostile process may speak the enclave wire protocol directly. The enclave
therefore enforces the security effect itself: a remote session can invoke only
`GetPublicKeys`; every owner, DKG, attestation, DCAP-verification and
secret-bearing command remains denied. Secret commands retain their separate
finalized-proof checks.

Transport classifies EOF/reset/broken-pipe as normal closure but propagates
malformed, oversized and non-expiry timeout failures. The runtime clock closes
an honest session at its exclusive deadline. Clock enforcement is availability
hygiene, not consensus or TEE authority; a hostile host can distort its own
clock but cannot expand the public-only remote command matrix.

Peer discovery and ticket delivery are transport-neutral consumer wiring. I6
adds no P2P/RPC carrier, no authority-bearing discovery protocol and no
per-session DCAP quote.

## Finalized replacement authority

`construct_local_finalized_replacement_authorization_v1` reads the same
current finalized marker/header/historical Registry path. It requires the exact
active node/profile binding and matches all durable candidate/submission fields:
enclave and binding IDs, intent hash, versions, lease, recipient, attestation
and responder keys, and stable NodeHost authorization commitment.

A reachable integration test constructs a canonical durable replacement
candidate, real node and enclave proofs of possession, and exact transaction
bytes; it then obtains the production finalized capability and consumes it
through the real promotion path. Its quote and collateral are deliberately
synthetic transaction fixtures. The test starts after DCAP verification and is
not accepted hardware evidence.

## Acceptance audit

| I6 criterion | Authoritative evidence | Result |
|---|---|---|
| Current node-owned finality only | local facade reads marker, canonical header and historical state from one provider; no caller token | `PASS` |
| Stale/unfinalized state rejects | H admits; after marker advances H+1, retained H cannot admit or authorize replacement; missing finalized header rejects | `PASS` |
| Validator and FullNode parity | the same finalized admission path admits both profiles and rejects mixed/wrong profile, chain, genesis and key substitutions | `PASS` |
| Exact remote identity | canonical witness must reproduce the source Registry commitment; Noise IK pins source static and target quote-bound responder static | `PASS` |
| Live production facade | current finalized Registry installs a ticket in a production-mode enclave and a real UDS Noise IK client reads public keys | `PASS` |
| One-use bounded admission | 64-ticket cap, expiry pruning, consume-before-handshake and replay rejection are tested | `PASS` |
| Remote is public-only | deny-by-default matrix admits only `GetPublicKeys`; generic owner API cannot invoke the reserved authorization command | `PASS` |
| Lease expiry | deadline is the earlier lease; honest runtime timeout closes at the exclusive deadline and propagates non-expiry frame faults | `PASS` |
| External finalized proof | real MPT account/storage proof verifies against an anchored state root; tampered storage rejects | `PASS` |
| RPC downgrade is explicit | the unanchored result type states RPC trust and never claims finality | `PASS` |
| Finalized replacement promotion | exact current Registry binding produces I5's opaque capability and actual promotion succeeds; stale or mismatched bindings reject | `PASS` |
| Scope and hardware boundary | no P2P carrier, enclave light client, per-session quote, new continuity/recovery or accepted SGX claim | `PASS` |

## Reachable verification

The checkpoint was closed with:

```bash
env CARGO_TARGET_DIR=/tmp/outbe-i6-replay-target-v2 \
  scripts/release/test_dcap_replay_ci.sh
cargo test -p outbe-node --test tee_remote_session
cargo test -p outbe-tee
cargo test -p outbe-tee-enclave --features native-dcap
cargo check -p outbe-chain
cargo fmt --all -- --check
git diff --check
```

Results:

- the mandatory replay gate passed ten artifact-contract tests, 20 primitive
  attestation tests, 54 native host tests, 25 public DCAP tests, five native-QVL
  tests, three remote-session tests, two fixture-tool tests and all pinned
  corpus hashes;
- the node integration suite passed four tests: H-to-H+1/current-finalized
  admission for both profiles, the full production facade over real UDS/Noise,
  successful finalized replacement construction/promotion, and an anchored real
  MPT proof with tamper rejection;
- the default `outbe-tee` suite passed 42 unit tests and three remote-session
  integration tests;
- the native enclave package passed 110 unit tests, both DKG integration tests
  and three non-benchmark transport tests; the throughput benchmark remains
  intentionally ignored and is unrelated to I6 acceptance;
- `outbe-chain` production compilation, formatting and diff checks passed;
- final independent standards review found no hard blockers, and final spec
  review found no code/spec blocker, behavioral gap or scope creep.

## Deferred, not waived

I7 owns governance activation and rolling measurement overlap. I8 integrates
the existing bootstrap/DKG result and keyless-before-readiness rule. I9 must
prove real accepted Processor evidence and exact-release
`gramine-sgx`, fresh one-use bindings, validator/full-node lifecycle, full
block budget and 32-validator block-1 activation.

Ticket carrier/discovery can be wired by a later consumer without becoming
authority. A hostile endpoint can burn a delivered one-use ticket, which is an
availability property of that future carrier, not a reason to add a new
authority protocol to I6. Offer-key recovery, proof of deletion, governance
replacement, a new DKG/BLS/reshare system and enclave consensus latestness
remain explicit non-goals.
