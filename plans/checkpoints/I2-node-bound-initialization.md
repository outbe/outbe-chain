# I2 checkpoint — node-bound enclave initialization

Date: 2026-07-31

Status: `PASS` for I2. This checkpoint proves the deterministic initialization,
authorization and quote-binding state machines. It does not claim an accepted
hardware quote: fresh accepted Processor evidence and the empirical
exact-release `gramine-sgx` run remain fail-not-skip I9 gates. A real Platform
node is checked by the same path when it joins.

## Outcome

A validator or full-node enclave now creates one sealed identity and accepts one
canonical node-signed initialization manifest. The manifest binds chain,
genesis, profile, exact node identity, a one-use initialization challenge, the
persistent `NodeHost` Noise initiator and every persistent enclave public key.
Initialization is serialized and write-once: an exact replay and any conflicting
manifest reject, including after restart.

After initialization, production quote generation is reachable only through a
Noise session whose initiator static key is the manifest-bound `NodeHost` key.
The enclave checks that key immediately after Noise message 1 and before message
2, transport-mode entry, command decoding or side effects. Public production
`GetQuote` rejects; the authorized command derives SGX `REPORT_DATA` from the
exact canonical registration or renewal intent and re-parses the returned quote
to verify that binding before returning it.

## Acceptance audit

| I2 criterion | Authoritative evidence | Result |
|---|---|---|
| Replayed or conflicting initialization rejects | `initialization_is_write_once_and_restores_the_same_node_bound_identity`; exact replay plus a separately signed conflicting manifest; sealed manifest hash and serialized prepare/commit transition | `PASS` |
| Challenge, chain, profile, node and enclave-key substitution reject | canonical manifest tests plus `initialization_rejects_challenge_key_and_chain_substitution` | `PASS` |
| Unknown initiator cannot decode or cause side effects | `production_initialization_binds_noise_initiator_before_request_decode`; responder validates `get_remote_static()` immediately after message 1 | `PASS` |
| Validator and full-node initialization use their exact node proof | EVM signer recovery and exact compressed Reth P2P key tests | `PASS` |
| Authorized initial and renewal quote paths use exact intent/policy commitments | `quote_report_data_is_exact_for_initial_and_renewal_intents`; dev-mode intent negative; `generated_quote_binding_rejects_post_syscall_report_data_substitution` | `PASS` for deterministic pre/post binding; combined real syscall and accepted hardware execution remain I9 |
| Only the persistent NodeHost key can reconnect | public UDS validator/full-node lifecycle tests reload the same owner-only key and reconnect; unknown key rejects | `PASS` |
| Gramine development behavior is isolated | development manifest is explicitly test/mock-only; production rejects dev seed, dev-mode intents and legacy `GetQuote`; production and mock targets compile separately | `PASS`; exact release feature/ELF isolation remains I9 |
| Command authorization is deny-by-default | exhaustive profile/readiness matrix covers every command class/state; secret handoff commands remain unconditionally denied until I6 verifies finalized proofs | `PASS` |

## Identity and persistence boundary

One sealed random seed derives the Noise responder, recipient X25519,
attestation Ed25519 and DKG encryption/BLS identities with distinct HKDF domain
labels. BLS derivation consumes the complete 256-bit domain-separated seed via
the versioned `TEE_BLS_IDENTITY_V1` RFC 9380 scalar mapping; a known-answer test
pins the exact seed-to-public-key result so a dependency change cannot silently
rotate the persistent identity. The explicitly insecure test helper
`from_seed(u64)` is not used. Restored and freshly generated identity seeds,
decrypted sealed payloads, DKG encryption secrets and derived offer secrets use
zeroizing ownership end to end; borrowed APIs avoid plain secret copies, and
the AEAD input buffer is zeroizing on both success and authentication failure.
Production refuses to start without an SGX sealing key, rejects deterministic
development seeds, never overwrites corrupt existing identity state, and rejects
a missing identity when sealed node authorization still exists. First creation
and every authorization write use owner-only, fsynced, no-clobber files.

The host-side `NodeHost` private key is created once with mode `0600`, fsynced,
loaded with `O_NOFOLLOW` from a current-user-owned regular file of exactly 32
bytes and held in
zeroizing memory. Existing, missing, malformed or incorrectly permissioned key
state never triggers silent regeneration. There is no `OperatorRecovery` path:
loss of this key means the sealed enclave identity cannot be controlled and the
node must initialize a new enclave identity.

The host storage layer can delete or roll back its own files. I2 does not claim
remote proof of deletion or rollback-proof host storage; the security result is
that neither condition authorizes another initiator or recovers the old sealed
identity.

## Reachable verification

The checkpoint was closed with these offline commands:

```bash
cargo test --locked --offline -p outbe-primitives \
  --features tee-attestation-v1 --test tee_attestation_v1
cargo test --locked --offline -p outbe-tee --lib
cargo test --locked --offline -p outbe-tee-enclave --tests
cargo check --locked --offline -p outbe-tee-enclave \
  --bin outbe-tee-enclave --target-dir /tmp/outbe-i2-prod
cargo check --locked --offline -p outbe-tee-enclave --features mock \
  --bin outbe-tee-enclave-mock --target-dir /tmp/outbe-i2-mock
cargo check --locked --offline -p outbe-tributefactory -p outbe-gratis \
  -p outbe-promis -p outbe-rpc
cargo fmt --all -- --check
git diff --check
```

The reachable suite covers both profiles over the public UDS transport:
challenge discovery, signed initialization, authorized command execution,
process-equivalent `NodeHost` key reload and reconnect. The production path
uses the real Gramine quote syscall. Hardware-free tests independently prove the
exact binding supplied before the syscall and the parser/comparison applied to
its returned bytes, including a substituted-`REPORT_DATA` negative. They do not
claim to execute the syscall; that combined path remains I9 hardware evidence.

## Deferred, not waived

I3/I4 must replace the legacy `EnclaveClient` callers in real `outbe-chain`
startup with node signer/datadir persistence and `AuthorizedEnclaveClient`
wiring for validator/full-node registration. I8 must migrate the existing
bootstrap/handoff callers. Until I6 supplies canonical finalized proof
verification inside the enclave, `SealTributeOfferHandoff`,
`SealOfferKeyForRegistry` and `IngestTributeOfferHandoff` remain denied in the
production command matrix; `NodeHost` assertions alone cannot authorize key
material movement.

I9 must execute the same authorized initial and renewal path with fresh accepted
evidence on the exact release enclave, then close the Processor, full-block
budget and Docker/activation gates described
in the implementation plan. Synthetic cap-boundary vectors remain parser and
resource-limit tests and are never described as hardware evidence. I9 also
checks the exact production target/features/ELF so a crate-wide mock feature
cannot enter the release artifact.
