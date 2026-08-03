# I3 checkpoint — validator enclave registration

Date: 2026-07-31

Status: `PASS` for I3. The validator path is implemented and verified, but the
V1 public route remains inactive until I9. This checkpoint does not claim an
accepted hardware registration: fresh accepted Processor evidence in the exact
release `gramine-sgx` artifact remains a fail-not-skip I9 gate. Platform
evidence is verified when a real Platform node joins.

## Outcome

A permissionless relay can carry one validator's canonical, intent-bound DCAP
evidence and both proofs of possession into the inactive V1 TeeRegistry route.
The active policy and block timestamp come only from consensus state. Full
native QVL verification runs inside the existing Gramine enclave, while the
host receives only a signed canonical `Accepted(verdict)` or stable
`Rejected(code)` outcome. An accepted outcome creates one leased validator
binding; an exact replay is idempotent and conflicting node, enclave, binding,
evidence, profile, nonce, key, measurement or status inputs reject.

Real `outbe-chain` validator startup now loads its EVM and BLS signers, derives
the canonical validator identity, creates or reloads the owner-only persistent
`NodeHost` key below the node datadir, completes the I2 initialization protocol
and installs only the resulting `AuthorizedEnclaveClient`. Loss, malformed
state or a conflicting manifest fails closed and never regenerates or recovers
the old enclave identity.

## Focused I3a trust boundary

The full `verify_dcap_evidence` operation moved into `outbe-tee-enclave`; there
is no host QVL verdict, QvE/TVL/QPL/PCCS call, network fetch, wall-clock input,
fallback verifier or second consensus implementation. The host commits the
exact canonical evidence bytes, active policy bytes and consensus timestamp in
one request hash. An authenticated production Noise session transfers them by:

1. `BeginDcapVerificationV1`, which checks the evidence and policy caps before
   allocation;
2. sequential `DcapVerificationChunkV1` frames of at most 60 KiB with exact
   request hash and offset;
3. `FinishDcapVerificationV1`, which requires exact total length, recomputes the
   request hash, runs the pinned native QVL and returns a canonical outcome
   signed by the manifest-bound enclave attestation key.

Any upload error clears the partial session. Missing native QVL support,
unavailable or development transport, malformed outcome, request-hash drift or
invalid response signature is fatal to local consensus execution; it is never
converted into deterministic evidence rejection. The consensus binary does not
link Intel QVL/DCAP, while the native verifier enclave imports only the pinned
quote-verification entry points.

## Acceptance audit

| I3 criterion | Authoritative evidence | Result |
|---|---|---|
| Full verifier is enclave-resident and input-exact | authenticated bounded Begin/Chunk/Finish implementation; request-hash and signed-outcome tests; production-only transport gate | `PASS` |
| Real Processor evidence reaches enclave QVL | intent-bound Intel Processor fixture traverses authorized Noise and returns the stable accepted testnet verdict for `ConfigurationAndSWHardeningNeeded` with advisories preserved | `PASS`; exact-release fresh hardware flow remains I9 |
| Unattested evidence rejects at the byte boundary | canonical `GramineDirectDev` evidence traverses the same authorized Noise upload and returns `EvidenceNonCanonical`; development session cannot invoke the verifier | `PASS` |
| Validator identity and both proofs of possession are exact | actual EVM address plus BLS MinPk public key, validator-set lookup, node ECDSA PoP and enclave Ed25519 PoP over one intent hash | `PASS` |
| Wrong profile and registration conflicts reject | canonical FullNode-profile intent at the post-verifier boundary plus one-to-one node/enclave/binding, stale nonce, key, measurement and strict-status negatives | `PASS` |
| Relay has no authority | caller is absent from the state transition; only the two intent-bound PoPs authorize it | `PASS` |
| V1 policy storage is append-only | V1 hash uses appended slot 45; a pre-seeded legacy slot-2 hash remains byte-identical across install/read/idempotent replay | `PASS` |
| Proposer, validator and follower agree | three independent stores traverse canonical ABI preflight, normative precharge and the private typed post-verifier capability, then compare outcome, state, ordered events, storage operations and total gas | `PASS` |
| Exact replay is idempotent | same intent and exact evidence hash returns `Idempotent`, emits no second event and does not increment the registration count | `PASS` |
| Readiness is lease-bound | absent and expired validator bindings report not ready | `PASS`; consensus membership enforcement is owned by its separate plan |
| Validator startup uses persistent NodeHost authorization | public production transport initialization/reconnect tests plus `outbe-chain` compile with real signer/datadir wiring | `PASS` |

## Inclusive gas bound

The normative maximum remains `28,768,784` gas at the evidence/framing/rule
caps. The `600,000` fixed registration term includes a `300,000` storage-gas
allowance, derived as exactly one half of the canonical, schedule-hashed
`register_fixed` field rather than an independent consensus constant.
Dispatcher precharge excludes that allowance and the production warm-SLOAD/
SSTORE-reset meter consumes the actual bounded storage work, so the published
maximum is inclusive rather than a protocol-only subtotal.

The full three-replica harness pins a fresh registration to 32 writes, proves
the observed reads plus writes remain inside the allowance, checks the exact
formula `maximum - allowance + actual_storage_gas`, and verifies the complete
transaction charge does not exceed the normative maximum. Cap-plus-one rejects
before policy read, allocation or protocol charge.

## Reachable verification

The checkpoint was closed with these commands:

```bash
env CARGO_TARGET_DIR=/tmp/outbe-i3-target \
  scripts/release/test_dcap_replay_ci.sh
cargo test -p outbe-teeregistry --features tee-attestation-v1
cargo test -p outbe-tee-enclave --features native-dcap --tests
cargo check -p outbe-chain
cargo fmt --all -- --check
git diff --check
```

Results:

- mandatory replay gate: 10 release checks, 18 protocol vectors, 45 TEE unit
  tests, 25 public DCAP tests, 5 native-QVL tests, 2 fixture-tool tests and all
  pinned fixture hashes passed;
- TeeRegistry: 20/20 passed;
- native verifier enclave: 104/104 unit tests, both DKG integrations and 3/3
  non-benchmark transport tests passed; the throughput benchmark remains
  intentionally ignored because it is an I9 empirical release gate;
- `outbe-chain` compiled without enabling `outbe-tee/native-dcap`;
- format and whitespace checks passed.

The ELF/feature audit additionally confirmed that the built consensus binary
has no SGX/DCAP/QVL dependency or symbol. The native enclave test artifact links
`libsgx_dcap_quoteverify.so.1` and imports the pinned
`sgx_qv_get_quote_supplemental_data_size` and `sgx_qv_verify_quote` boundary.
No hardware acceptance or performance claim is derived from synthetic cap
tests.

## Review closure

Two independent worktree reviews used the I2 commit as fixed point and the
decision map, engineering gates and implementation plan as specifications.
The first pass found no standards defects and three I3 acceptance gaps: storage
gas sat above the normative maximum, replica parity bypassed full precharge,
and wrong-profile/unattested negatives were incomplete. All three were fixed;
the spec re-review reported no remaining findings. The standards re-check then
found two consensus issues: V1 policy authority reused legacy slot 2 and the
storage allowance was independent of the hashed gas schedule. V1 policy now
uses appended slot 45 while a regression test preserves a pre-existing legacy
hash; the allowance is derived only from hash-committed `register_fixed`.
Final Standards and Spec re-reviews both reported no remaining findings.

## Deferred, not waived

I4 must wire the FullNode profile through the same production NodeHost,
enclave-resident verifier, lease and registry semantics. Existing development
full-node transport cannot invoke the consensus verifier and is not evidence of
I4 completion.

I5-I8 retain their plan-owned renewal, rolling overlap, governance policy,
handoff and bootstrap transitions. I9 must build and run the exact pinned
release artifact on supported Intel SGX hardware, obtain a fresh accepted
Processor flow, measure the full-block budget with current Processor CRLs,
repeat the ELF/feature
audit on the release binaries and only then activate the V1 route. Until that
checkpoint, `ACTIVE_TEE_ATTESTATION_V1_MANIFEST` remains `None`.
