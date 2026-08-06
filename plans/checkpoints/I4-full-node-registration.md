| Exact persistent Reth identity | real `NetworkArgs::secret_key` test proves explicit-path selection and exact reload; production lifecycle test persists a FullNode manifest and reconnects | `PASS` |# I4 checkpoint — full-node enclave registration

Date: 2026-07-31

Status: `PASS` for I4. The FullNode path is implemented and verified, but the
V1 public route remains inactive until I9. This checkpoint does not claim an
accepted hardware registration: fresh accepted evidence for the exact release
`gramine-sgx` artifact remains a fail-not-skip I9 gate.

## Outcome

A full node now binds the exact persistent compressed secp256k1 public key used
by its Reth P2P network identity to one initialized, attested enclave. Startup
resolves the secret through Reth's own `NetworkArgs::secret_key` API and the
same chain-specific `discovery-secret` default path used by the network
builder, including the existing `--p2p-secret-key` and
`--p2p-secret-key-hex` overrides. It does not create a second identity.

The FullNode profile uses the same owner-only persistent NodeHost state,
production initialization protocol, authenticated bounded Noise upload,
enclave-resident native QVL, policy, lease, one-to-one enclave/binding maps,
exact-evidence idempotency and signed verdict as the validator profile. The
old full-node development/mock connection branch was removed from
`outbe-chain`; a configured TEE sidecar now fails closed unless production
NodeHost authorization succeeds.

## Shared state machine

No storage slot was added. Both profiles are keyed by the existing canonical
`node_id_hash` and use the existing generic V1 maps and reverse maps. One
registration implementation performs canonical decoding, chain/policy
binding, both proofs of possession, profile-specific measurement lookup,
lease/collateral checks, replay checks, common writes and the fixed-size event.
Only the node-auth adapter varies:

- Validator verifies the EVM+BLS identity against `ValidatorSet`, writes the
  validator address index and preserves the legacy validator registration
  projection.
- FullNode verifies recoverable ECDSA against the exact compressed Reth P2P
  public key and does not touch validator membership or legacy validator state.

The FullNode read API takes fixed `uint8 prefix + bytes32 x` arguments, rebuilds
the SEC1-33 identity and derives its node hash. It introduces no dynamic key
allocation. Primitive validation now parses the complete compressed
secp256k1 point rather than checking only the prefix and non-zero bytes.

## Acceptance audit

| I4 criterion | Authoritative evidence | Result |
|---|---|---|
| Exact persistent Reth identity | startup uses Reth's own secret resolver/default path; production lifecycle test persists a FullNode manifest and reconnects | `PASS` |
| Same NodeHost and enclave QVL | validator and FullNode wrappers enter one common NodeHost implementation; startup installs only `AuthorizedEnclaveClient` | `PASS` |
| Shared registration semantics | one profile-generic apply function owns policy, measurement, lease, replay, reverse-map and event transitions | `PASS` |
| Credentials cannot cross profiles | validator signature rejects for FullNode intent; P2P signature rejects for validator intent; wrong P2P key rejects | `PASS` |
| Registration and exact duplicate replay | FullNode creates one binding; exact evidence replay is idempotent and emits no second event | `PASS` |
| Rejection vectors match validator | common code covers enclave PoP, versions/nonces, measurement, platform status, lease/collateral, one-to-one ownership and exact evidence hash | `PASS` |
| Lease-bound readiness | absent binding is not ready, active binding is ready, expiry disables readiness | `PASS` |
| No weaker/unattested path | legacy full-node mock startup branch removed; production verifier remains inaccessible to development transport and accepts DCAP evidence only | `PASS` |
| Proposer, validator and follower agree | three stores traverse canonical ABI preflight, normative precharge and private post-verifier capability, then compare outcome, state, ordered events, operations and gas | `PASS` |
| No schema or gas expansion | FullNode reuses slots 22..45 and performs 23 bounded fresh writes, within the existing inclusive register storage allowance | `PASS` |
| Hardware acceptance remains I9 | I4 accepted-path tests begin strictly after the typed verifier boundary | `PASS` scope boundary |

## Reachable verification

The checkpoint was closed with:

```bash
env CARGO_TARGET_DIR=/tmp/outbe-i4-target \
  scripts/release/test_dcap_replay_ci.sh
cargo test -p outbe-primitives --features tee-attestation-v1 \
  --test tee_attestation_v1
cargo test -p outbe-teeregistry --features tee-attestation-v1
cargo test -p outbe-tee-enclave --features native-dcap --tests
cargo test -p outbe-chain full_node_identity_uses_reth_secret_resolver_and_persists_exact_key
cargo check -p outbe-chain
cargo fmt --all -- --check
git diff --check
```

Results:

- mandatory deterministic replay and pinned native-QVL artifact gate passed;
- primitive V1 attestation tests, including real SEC1 point validation, passed;
- TeeRegistry passed all 21 tests, including FullNode lifecycle and three
  independent ABI/state/event/gas replicas;
- native verifier enclave unit and integration tests passed, including the
  production FullNode NodeHost initialization/reconnect lifecycle;
- the real Reth resolver test selected the operator path, left the default path
  unused and restored the same compressed P2P public key;
- `outbe-chain` compiled with default features and without native QVL linked
  into the consensus binary;
- format and whitespace checks passed.

The FullNode three-replica harness observes 23 fresh binding writes. The
validator harness remains at 32 because it additionally writes its address
index and legacy validator projection. Both consume the same schedule-hashed,
inclusive registration maximum and remain below its storage allowance.

## Review closure

Two independent reviews checked the complete worktree against the I4
acceptance criteria and repository standards. Any findings were corrected and
both final re-reviews reported no remaining findings.

## Deferred, not waived

I5 owns renewal, expiry transitions and bounded supersession. I6 owns remote
session admission from finalized active state. I7 owns governance policy and
rolling measurement overlap. I8 owns block-1 bootstrap. I9 must build the exact
release artifact on supported Intel SGX hardware, capture fresh accepted
Processor evidence, measure the release
budget and only then activate the V1 route. Until that checkpoint,
`ACTIVE_TEE_ATTESTATION_V1_MANIFEST` remains `None`.
