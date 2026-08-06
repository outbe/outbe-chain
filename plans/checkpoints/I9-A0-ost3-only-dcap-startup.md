# I9 A0 checkpoint — OST3-only DCAP startup activation

Status: `PASS` for the A0 deterministic gate and `PENDING` for a reachable
devnet rerun after the chain-identity correction. The A0 implementation,
post-review deterministic matrix, forbidden-path audit and isolated staged
scope are complete. This checkpoint does not claim accepted Intel hardware
evidence or testnet release readiness; B1 through E1 remain fail-not-skip.

## Outcome and fixed boundary

A runnable Outbe ChainSpec has exactly one genesis-fixed V1 mode from block 1:

- testnet uses `DcapRequired`, chain ID `54322345`, explicit nonzero
  release measurements and mandatory canonical policy/resource schedules;
- devnet uses `GramineDirectDev`, chain ID `424242`, a separate
  genesis and explicit non-hardware measurements;
- both modes use the OST3 producer and consumer. OST2 is neither produced nor
  accepted;
- every node connects to the genesis-selected enclave before its protected role
  starts. There is no mode fallback, tee-less path, old registration route,
  post-genesis key handoff or lost-key recovery.

A0 activates the protocol surface. It does not close the I9 release gates for
the exact `native-dcap` bundle, accepted Processor evidence, hardware
timing or testnet E2E.

## C1 independent review and disposition

Two read-only reviews used fixed point
`9d345f461f2979e4dd8404ddee59d99ead3ff0cd` and the current working-tree diff.

| Finding | Disposition |
| --- | --- |
| A hand-authored `GramineDirectDev` manifest could use an arbitrary chain ID because only the generator enforced identity separation. | Fixed in `tee_attestation_activation.rs`: runtime validation now requires devnet chain ID `424242` for `GramineDirectDev`, testnet chain ID `54322345` for `DcapRequired`, and rejects both crossover directions plus unknown identities. |
| Operator docs still described `remote_attestation = none`, a verifier stub, optional TEE and obsolete `tee join` arguments. | Fixed in the SGX launch, role onboarding and release guides plus the TEE ADRs and CLI help. Current docs distinguish executable development behavior from still-open I9 hardware gates. |
| ADR-B-NOD-001 still presented the pre-activation optional TEE socket and in-process stub as part of the node lifecycle. | Fixed: the lifecycle now requires the genesis-selected enclave, records the distinct FullNode/Validator readiness points and states that GramineDirectDev is a separate devnet identity rather than a testnet fallback. |
| Component/offline EVM reexecution derived the immutable V1 ChainSpec authority but dropped it when `RuntimeBodyReaders` was absent. | Fixed by forwarding the same authority through `dispatch_with_tee_attestation`; the counterfactual genesis reexecution test failed before the fix and passes after it. No verifier or policy alternative was added. |
| The restart harness asserted absence of a removed key-handoff timeout log, so that check could never fail for a real product behavior. | Fixed by deleting the dead log assertion. The reachable scenario now proves each identity unseals its own permanent key and that DKG is not rerun; no peer redelivery path is implied. |
| This A0 evidence index was absent. | Fixed by this checkpoint. |
| One review treated Reth service construction before the Validator key gate as execution-before-key. | Not a product gap under the accepted role matrix. Validator equality is before threshold material, consensus actors and canonical transaction execution; a proven fresh founder is intentionally keyless until the block-1 ceremony. FullNode equality is stricter and occurs before Reth launch. Base service construction is not authority to propose, finalize or execute protected chain work. |

## Requirement-by-requirement evidence matrix

| Requirement | Network | Source and symbol | Positive evidence | Fail-closed evidence | Status and remaining gate |
| --- | --- | --- | --- | --- | --- |
| Mandatory V1 manifest from block 1 | Testnet and devnet | `bin/outbe-chain/src/main.rs::OutbeChainSpecParser::parse`; `tee_attestation_activation.rs::TeeAttestationChainSpecStateV1`; activation height constant `1` | Testnet and devnet generator/parser round trips | Missing, malformed, wrong hash, wrong resource schedule and wrong activation height reject | `PASS` deterministic; signed release identity remains B1 |
| Testnet and devnet identities cannot overlap | Both | `tee_genesis_v1.rs::initial_tee_policy_v1`; `tee_attestation_activation.rs::validate_activation` | Explicit construction for both network identities | Runtime and generator reject both mode/chain-ID crossover directions | `PASS` after C1 fix |
| Authorized SGX/quote boundary | Testnet | `main.rs` DcapRequired NodeHost initialization; `initialization.rs::InitializationState::production`; `tee_bootstrap.rs::build_local_tee_bootstrap_submission_v2` | Deterministic NodeHost, quote-binding and offline verifier tests from I2-I8 | Missing socket, sealing key, manifest, quote/collateral or rejected policy stops startup/registration | `PASS` for code boundary; fresh accepted SGX is H1/E1 |
| Separate runnable development network | Devnet | `GRAMINE_DIRECT_DEV_CHAIN_ID`; `bootstrap-testnet.sh`; `run-testnet.sh`; E2E localnet builder | The retained four-validator run used the superseded shared identity `54322345`; a run on `424242` is still required | DCAP measurement arguments, non-devnet identity, missing enclave and bare-host mode reject | `PENDING` reachable devnet rerun on `424242` |
| Complete OST3 producer and consumer at block 1 | Both | `stack.rs` founding coordinator; `tee_bootstrap.rs`; `system_tx.rs::TEE_BOOTSTRAP_SELECTOR`; EVM config/executor block-1 checks | Canonical assembly, committee signatures, proposal injection and block-1 execution tests | Missing/incomplete/mismatched/oversized OST3 and OST3 outside block 1 reject | `PASS` deterministic; same-server QVL/full-block timing is P1 and reachable Validator/FullNode paths are E1 |
| Total OST2 rejection | Both | `system_tx.rs` defines only selector `OST3`; canonical routing and system-set validation | Exact selector/collision and block-1 membership tests | `OST2` has no decoder or producer; source occurrence is one negative explanatory comment | `PASS` |
| V1-only TeeRegistry admission | Both | `precompile_routes.rs`; `teeregistry::v1_precompile::dispatch` | Exact V1 view route and full Validator/FullNode registration suites | Exact old six-argument selector rejects; malformed evidence/signatures/caps reject before mutation | `PASS` |
| Permissionless relay with node and enclave authority | Both | `teeregistry::v1`; NodeHost manifest and signature validation | Validator and FullNode tests use canonical evidence and profile-specific authority | Relay caller is never admission authority; wrong node/enclave proof rejects | `PASS` deterministic; real accepted evidence is E1 |
| One-time permanent offer-key delivery | Both | `OfferKeySealedForRegistryV1`; `V1RegistrationOutcome`; enclave registry artifact ingestion | Created emits and ingests exactly one bounded deterministic artifact | Idempotent, renewal and replacement do not reseal or redeliver; missing/malformed enclave output is fatal | `PASS` |
| Validator readiness before consensus | Validator | `stack.rs::validate_offer_key_before_threshold_work` and its call before `obtain_threshold_material` | Proven fresh founder, exact existing key and ready verifier-join cases | Existing missing, zero, canonical-zero-with-state and mismatch reject before threshold work/consensus actors | `PASS` |
| FullNode readiness before execution | FullNode | `main.rs` upstream read and resident comparison before `.launch()`; `follow_transport.rs` defence in depth | Exact 32-byte upstream word and equal resident key | ABI lengths 0/31/33, zero, unavailable and mismatch reject before Reth launch | `PASS` |
| Permanent-key loss has no recovery | Both | terminal sealed-state load; public `DkgFinalizeTributeOffer` capability matrix; removed handoff and force-DKG paths | Exact sealed-key restart retains canonical public key | Corrupt/wrong-chain/unsealable state stops; no OperatorRecovery, governance replacement, peer handoff or public lost-key DKG | `PASS` |
| Evidence labels are honest | Both | this checkpoint and operator guides | Dev E2E, private post-verifier tests and synthetic caps are separately labelled | None is accepted as SGX/DCAP hardware proof | `PASS`; hardware rows remain fail-not-skip I9 gates |

## Startup order evidence

### FullNode

1. The product parser validates `teeAttestationV1`.
2. `main.rs` establishes the profile-bound NodeHost enclave.
3. The selected certified upstream returns exactly one 32-byte canonical offer key.
4. `resident_offer_public_key_v1` requires a ready nonzero permanent key and
   exact equality.
5. Only then does the Reth builder launch networking, RPC, sync and execution.
6. The follower stack repeats equality before constructing its executor.

### Validator

1. The product parser validates `teeAttestationV1` and the entrypoint establishes
   the Validator NodeHost enclave.
2. After reading canonical startup state,
   `validate_offer_key_before_threshold_work` distinguishes only:
   - a proven fresh block-1 founder, which may be keyless while creating OST3;
   - a ready empty-DB verifier join, which repeats equality after certified sync;
   - an existing identity, which must match the nonzero canonical key exactly.
3. The gate precedes DKG-channel use, threshold material, live join, consensus
   actors, proposal/finalization and protected transaction execution.

## Executed evidence before the final C1 rerun

The following results were recorded by the closed G1, V1 and R1 Beads:

- `cargo test -p outbe-primitives --lib`: 226 passed.
- `cargo test -p outbe-chain --bin outbe-chain`: 30 passed, including seven
  `tee genesis` tests.
- `cargo test -p outbe-teeregistry --all-features`: 35 passed plus doc tests.
- `cargo test -p outbe-evm tee_route_accepts_v1_and_rejects_the_legacy_registration_selector --lib`: 1 passed.
- `cargo test -p outbe-tee --lib`: 31 passed.
- `cargo test -p outbe-tee-enclave`: 113 unit tests plus DKG, Noise and transport
  integrations passed; one throughput benchmark is deliberately I9/P1.
- `cargo test -p outbe-engine --lib`: 160 passed.
- `cargo test -p outbe-cli --bin outbe-cli`: 223 passed.
- `cargo test -p outbe-e2e-harness --lib`: 82 passed; one unrelated FullProof
  generator is ignored.
- `cargo fmt --all -- --check`, `git diff --check`, Python compilation and shell
  syntax checks passed.

C1 added and passed:

```text
cargo test -p outbe-evm tee_attestation_activation::tests --lib
4 passed; 0 failed; 0 ignored; 164 filtered out
```

The final post-review validation reaffirmed the affected matrix:

| Command | Final result |
| --- | --- |
| `cargo test -p outbe-primitives --lib --quiet` | 226 passed; 0 failed; 0 ignored |
| `cargo test -p outbe-tee --lib --quiet` | 31 passed; 0 failed; 0 ignored |
| `cargo test -p outbe-teeregistry --all-features --quiet` | 35 passed; 0 failed; 0 ignored; rerun outside the restricted sandbox because the in-sandbox run passed 34 tests and received `EPERM` only at `UnixListener::bind` |
| `cargo test -p outbe-evm --lib` | 168 passed; 0 failed; 0 ignored |
| `cargo test -p outbe-engine --lib --quiet` | 160 passed; 0 failed; 0 ignored |
| `cargo test -p outbe-chain --bin outbe-chain --quiet` | 30 passed; 0 failed; 0 ignored |
| `cargo test -p outbe-cli --bin outbe-cli --quiet` | 223 passed; 0 failed; 0 ignored; rerun outside the restricted sandbox because all 19 in-sandbox failures were `EPERM` from local mock-RPC listener binds while the other 204 passed |
| `cargo test -p outbe-tee-enclave --quiet` | 113 unit tests, one DKG integration, one Noise integration and three transport tests passed; one throughput benchmark remains deliberately ignored for P1 |
| `cargo test -p outbe-e2e-harness --lib --quiet` | 82 passed; 0 failed; one unrelated FullProof generator ignored |

The EVM counterfactual
`genesis_block_with_header_artifact_reexecutes_deterministically` was red with
`TEE attestation ChainSpec authority is not bound` before the no-reader dispatch
fix and green after it. Targeted terminal-ordering, noncritical OOG,
signature-hash mismatch and proposer-signer mismatch tests each passed 1/1. The
last two retain the fail-closed negative behavior.

Post-edit static gates passed:

```text
cargo fmt --all -- --check
git diff --check
bash -n scripts/bootstrap-testnet.sh scripts/localnet-stack.sh scripts/run-testnet.sh
python3 compile() over scripts/prepare_network.py without writing bytecode
```

The forbidden-path scan found no active `OperatorRecovery`, `DkgCoordinator`,
`DeliveryRelay`, force-DKG command, OST2 decoder/producer, TEE key-handoff
client/server, governance offer-key replacement or old six-argument
registration route. Remaining `OST2`, `OperatorRecovery` and `force-dkg` text is
negative documentation or an exact rejection test. Cryptographic uses of the
word recovery refer to reconstructing signatures/shares, not restoring a lost
permanent identity key.

## Historical reachable-development evidence

The retained run below predates the identity correction and used chain ID
`54322345`. It remains useful as historical behavior evidence, but it does not
prove reachability of the current devnet on `424242` and no longer satisfies
the reachable-devnet row above.

Historical retained run:

- command lane: four validators, `--tee mock`, isolated `GramineDirectDev`;
- feature: `Active validator restarts without a new DKG ceremony`;
- scenario: `Registration survives a joining node and enclave restart`;
- result: one scenario, 3/3 steps passed in `308877 ms`;
- run directory:
  `/tmp/outbe-dcap-a0-r1-redacted-20260802/run-1785686702-3078308`;
- evidence:
  `/tmp/outbe-dcap-a0-r1-redacted-20260802/evidence/run-1785686702-3078308/scenario-001.json`;
- evidence SHA-256:
  `699c8a1956ca528d12c850b432fa8ed24ea7f52c7677a9189c550e6d38ca8fed`;
- environment records four validators, `tee=mock` and Gramine image digest
  `sha256:dd44c54fa0f4546a03fbe43a81f74256a7a38e543ad937602a7b3ba2e0c1ac1f`;
- log audit inspected 15 runtime logs and found zero fatal, panic, DKG-share
  reveal, VRF, SGX-resource or projection findings;
- 24 generated secret files were compared to retained logs/evidence without
  printing their values; zero plaintext matches were found.

This run observed V1 onboarding, founding readiness transition and exact
resident-key restart behavior under the superseded shared identity. It proves
neither current devnet reachability nor any SGX/DCAP-positive claim.

## ChainSpec construction artifacts

Retained ephemeral construction evidence under
`/tmp/outbe-a0-g1-evidence-20260802`:

| Artifact | Bytes | SHA-256 | Classification |
| --- | ---: | --- | --- |
| `gramine-direct-dev.json` | 459024 | `4a6a1e95ac8797db4b808cf58c48bca06d84512bbc7c0f05a85a8c327f59b59f` | development construction |
| `dcap-required.json` | 37689 | `d276f39a23d265621421f118089edf361082180ace037ca821dd61ac97aa4427` | representative deterministic construction, not signed release evidence |

Both artifacts round-tripped through the product parser and preserved their
deliberate genesis header identity. `/tmp` paths are ephemeral and the commands
and tests, not path persistence, are the durable proof.

The checked-in `testing/e2e-harness/fixtures/ocomp-final-v1` fixture also
predates the network-identity correction and remains bound to `54322345`.
It is not current devnet evidence and the strict `GramineDirectDev` generator
rejects it fail-closed. Regenerating its chain-bound OCOMP install, committee
registrations and signatures is a separate OCOMP fixture migration, not part
of this DCAP identity slice; the OCOMP final-fixture lane stays deferred until
that migration is performed.

## Operator documentation

- `docs/launching-with-sgx.md` now explains the two non-overlapping networks,
  the executable GramineDirectDev quickstart, testnet ChainSpec construction,
  key-readiness checks and the still-open I9 release gates.
- `docs/becoming-a-validator.md` now gives role-complete V1 `tee join` arguments,
  the mandatory FullNode upstream/key gate and permanent-loss behavior.
- `docs/testnet-sgx-release.md` marks the current pre-activation bundle as invalid
  for `DcapRequired` and identifies B1/H1/P1/E1 as fail-not-skip prerequisites.
- ADR-S-TEE-001 and ADR-S-TEE-002 describe one-time registry onboarding, terminal
  key loss and the remaining hardware evidence without legacy handoff/recovery.

## Remaining I9 gates after A0

The first remaining DCAP release gate is B1: freeze one reproducible x86_64
testnet bundle
with exactly the `native-dcap` application feature, Intel QVL
`1.26.100.1-noble1`, Gramine `1.9`, trusted native-artifact digests and no
capture/mock/trace/fake-verifier surface.

Separately, the reachable devnet rerun on `424242` and the OCOMP final-fixture
migration described above remain pending and are not release evidence.

Then H1 must retain fresh accepted Processor evidence; a real Platform node is
checked fail-closed when it joins. P1 must prove exact-release QVL and maximum
reachable full-block timing on the same SGX server. E1 must pass the reachable
Validator and FullNode `DcapRequired` paths; no 32-validator network is
required. Runner or evidence absence is a release failure, not a skip.

## Final C1 close record

- Final post-review commands and exact counts are recorded above; every affected
  suite is green. Local-listener `EPERM` results were rerun outside the restricted
  sandbox and were not hidden with skips.
- The forbidden-route and scope-drift scan is green. Two retained textual matches
  are negative guarantees: no `OperatorRecovery` and no valid `OST2` selector.
- The staged-path audit contains 98 accepted A0 code, test, documentation, script,
  lockfile and checkpoint paths. `git diff --cached --check` passes and the staged
  secret-pattern scan has zero matches.
- `.beads/interactions.jsonl` and untracked `.beads/issues.jsonl` are deliberately
  excluded from the Git commit. No unrelated user path was staged or discarded.
- This checkpoint is carried by one SSH-signed A0 commit. Its exact commit ID and
  local `git verify-commit` result are recorded in Beads `outbe-chain-qlm.4`,
  avoiding an impossible self-referential commit hash here.

Git push: `false`. PR: `false`. Governance action: `false`.
`bd dolt push`: `false`.
