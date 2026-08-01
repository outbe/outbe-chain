# DCAP and remote-attestation implementation plan

Status: draft implementation backlog; no tracker issues published

Design authority:
`plans/dcap-remote-attestation-decision-map.md`

Engineering evidence:
`plans/evidence/dcap-qvl-engineering-gates.md`

The decision map overrides stale portions of
`/home/ubuntu/piolium/remediation-plan.md`, especially `OperatorRecovery`,
extra role keys, continuity/migration machinery and the former 1-MiB evidence
cap. The work below stays focused on DCAP and remote attestation.

Active-goal amendment (2026-07-30): any earlier orchestration text requiring
an aarch64 matrix or exact `dcap-qvl 0.5.2` Ring-only production verifier is
stale and is superseded by this plan and its decision map. Completion requires
the x86_64 Intel SGX Processor/Platform and pinned enclave-resident native
QVL/dependency matrix. QvE/TVL and an Intel SGX SDK migration are not V1
dependencies. All other scope, checkpoint, real-case, reporting and blocker
controls remain in force.

## Dependency order

```text
I0 manifest/context
 ├─> I1 enclave-resident native QVL
 └─> I2 enclave initialization
       ├─> I3 validator registration ─┐
       └─> I4 full-node registration ├─> I5 renewal/expiry ─> I6 sessions
I1 ──────────────────────────────────┘
I3/I4 ─> I7 governance policy
I1/I3 ─> I8 genesis bootstrap
I0..I8 ─> I9 production activation
```

Each item is a vertical, testable behavior. Horizontal refactors are included
only when that slice needs them.

## I0 — Install the inactive V1 protocol manifest and deterministic context

**Outcome:** direct V1 harnesses can encode policy/evidence/resource schedules
and see canonical chain/genesis/block-time context without changing active
production behavior.

**Includes:**

- canonical V1 codecs, domains, enums, bounds and golden vectors;
- `TeePolicyScheduleV1`, `TeeRegistryGasScheduleV1` and
  `ResourceScheduleV1`;
- `genesis_hash()` beside `chain_id()` in every proposer, validator, follower
  and test execution context;
- 896-KiB evidence/component caps, 16-KiB quote/call-framing caps and checked
  gas calculators;
- an inactive manifest/feature gate so legacy chains remain byte-identical.

**Acceptance:**

- duplicate/unknown/trailing/noncanonical encodings reject before allocation;
- two chains with equal chain ID and different genesis reject cross-chain
  intents;
- cap-1/cap/cap+1 and checked-overflow vectors pass;
- no V1 selector or storage layout is active yet.

## I1 — Verify one canonical DCAP evidence value deterministically

**Outcome:** the consensus-facing
`verify_dcap_evidence(evidence, policy, block_timestamp)` boundary returns a
stable Outbe verdict or reject code backed by pinned native Intel QVL executing
inside the Outbe Gramine enclave, with no local host inputs.

**Includes:**

- completed feasibility gate (`c679649`, hardened by `bc96db2`, `d5ab89e` and
  `fd1598c`, with QVL-scoped tracing in `8dcebae`): Intel DCAP QVL
  `1.26.100.1-noble1`, its native dependencies and the
  `x86_64-unknown-linux-gnu` target are exact-pinned, and a real Processor-CA
  quote reaches a cryptographically verified native-QVL result both natively
  and under Gramine Direct using submitted collateral and explicit consensus
  time; the public adapter preserves its authentic strict-policy rejection;
- exact-pinned native QVL and required dependency artifacts, integrity-pinned
  as Gramine trusted files;
- canonical DER/signed-JSON to native collateral adapter with no QPL/PCCS
  substitution during consensus;
- in-enclave `sgx_qv_verify_quote` invocation with
  `p_qve_report_info = NULL` and no host-verdict input;
- full outer/inner quote consumption and type-5 chain equality;
- pinned Intel root, FMSPC/PCE ID, TCB evaluation number, platform/QE status,
  advisory and measurement enforcement;
- stable reject-code ordering and exact QVL gas precharge.

The feasibility gate does not complete I1. Canonical grammar, signed-document
time enforcement, final Outbe policy mapping and stable consensus verdict
vectors remain in this item. I1 proves the native cryptographic path with a
real Processor-CA quote. Platform-CA grammar uses Intel's v1.26 `PlatformPEM`
parser vector. Private pure Platform/QE policy tests start from the exact Intel
QVL 1.26 raw SGX result ABI; Intel's dynamically generated, self-signed
Platform-CA verification tests are topology provenance only, never public
Outbe evidence. A real Intel-rooted Platform-CA quote requires registered
multi-package SGX hardware and is a fail-not-skip I9 release gate.

**I1 checkpoint (2026-07-31):** a real Processor-CA negative corpus is now
bound to canonical `RegistrationIntentV1::report_data()`. It exercises the
public verifier and is rejected as `PlatformTcbRejected` because the rented
capture host returns `ConfigurationAndSWHardeningNeeded`; policy is not
weakened. The QVL 1.26 supplemental-data reconciliation now uses the combined
Platform/QE evaluation reference, accepts a zero unavailable QE-specific
reference, and enforces the native all-collateral time window. Exact-expiration
and malformed-reference tests cover those reachable results.

Capture remains outside the production command surface: quote generation uses
a separate required-feature enclave executable, while QPL/PCCS collateral
acquisition is host-side fixture tooling only. The immutable real corpus is
replayed by the mandatory `.github/workflows/ci.yml` `dcap-replay` x86_64 job
through the public verifier and pinned native QVL at a fixed historical
consensus timestamp, without SGX hardware, QPL/PCCS, network or live collateral
fetch. `scripts/release/test_dcap_replay_ci.sh` is the identical local entry
point. Its authentic `ConfigurationAndSWHardeningNeeded` result is preserved;
no fake verifier turns it into an accepted verdict. The complete
compiler-observed Intel QVL header closure is digest-verified and staged into
an isolated include tree; the C adapter compile-time asserts all nine SGX
result values before the independent Rust status matrix executes.
Full criterion-by-criterion evidence is recorded in
`plans/checkpoints/I1-deterministic-verifier-closure.md`.

A second real intent-bound Processor-CA capture from a host with an allowed
Platform status remains mandatory, but it is I9 release evidence rather than
an I1 implementation blocker. This relocation does not authorize a broader
status matrix or synthetic positive.

Toolchain staging is intentionally separate from container delivery:
`release/project-toolchain-v1.json` is the single exact version pin and is
bound into ELF and SGX release inputs, while its activation remains
`pending-container-delivery`. This state does not claim that the current
production ELF/SGX builders use the unified image, and it does not complete
the native-DCAP release gate. Image delivery and activation must be resolved
before the production enclave is built with `native-dcap`.

**Acceptance:**

- a real Processor-CA cryptographic corpus bound to
  `RegistrationIntentV1::report_data()` reaches its authentic strict-policy
  result through the public verifier and pinned native QVL;
- immutable real-corpus replay, tamper and time-boundary tests run offline at a
  fixed historical consensus timestamp on every supported x86_64 CI build;
- the official Intel Platform-CA parser vector covers CA classification, while
  exact Intel QVL 1.26 raw SGX status ABI vectors drive private pure
  Platform/QE status mapping and every stable rejection branch, but are never
  represented as real hardware evidence or passed to the public verifier;
- synthetic statuses are limited to private pure policy tests; no fake QVL,
  runtime verifier injection or production-selectable test feature can return
  a positive result from the public verifier;
- upstream trailing-byte cases reject;
- time boundary and strict status matrix pass;
- no environment, filesystem, network, wall clock, optional verifier or
  upstream error string reaches consensus;
- a host verdict is not part of the interface and tampered evidence rejects;
- byte-stable fixture verdicts pass across supported x86_64 validator builds;
- exact native versions and digests match the release manifest; a missing or
  mismatched QVL or dependency fails closed;
- no second pure-Rust production verifier or live collateral-fetch path enters
  the consensus dependency tree.

**Hardware-free downstream test boundary (I2–I8):**

- a private `#[cfg(test)]` capability starts strictly after
  `verify_dcap_evidence` and accepts only a typed pre-verified `DcapVerdictV1`;
  it cannot parse evidence, replace QVL or make the public verifier accept;
- registry, lease, finalized-view, Noise-session and bootstrap state-machine
  tests may use that capability to exercise accepted downstream transitions;
- the capability is absent from non-test compilation and from every production
  Cargo feature/target; its outputs are synthetic state-machine inputs, never
  real hardware evidence or a DCAP end-to-end result;
- real corpus replay continues to exercise the public verifier's cryptographic
  and negative-policy path in ordinary CI;
- every I3/I4/I8 acceptance below that requires an accepted public-verifier
  result is finally closed by I9's real `DcapRequired` hardware job, not by the
  private capability.

## I2 — Initialize an enclave once and restrict quote generation to NodeHost

**Outcome:** a validator or full-node enclave seals one node-bound identity and
only its persistent `NodeHost` Noise initiator can request later quotes.

**Includes:**

- initialization challenge and node-signed manifest bound to chain/genesis,
  node/profile and persistent enclave keys;
- one canonical `NodeHost` key, no `OperatorRecovery` or extra role keys;
- deny-by-default profile/state command matrix;
- initiator static-key rejection after Noise message 1 and before request
  decode;
- removal of the public production `GetQuote` behavior after initialization.

The exact full-manifest `authorization_hash()` remains the node-signed,
write-once initialization/sealing commitment and includes the initialization
challenge plus all three enclave public keys. The separate domain-separated
`node_host_authorization_hash()` commits only to protocol version, chain,
genesis, profile, canonical node identity and the persistent `NodeHost` Noise
public key. That stable authority may therefore be preserved across a fresh
enclave initialization without weakening the exact manifest signature.

**Acceptance:**

- replayed/conflicting initialization rejects;
- unknown initiator cannot decode or cause command side effects;
- authorized initial/renewal quote embeds the exact intent and policy
  commitments;
- Gramine development behavior exists only under the separate dev manifest.

**I2 checkpoint (2026-07-31):** canonical node-signed write-once
initialization, sealed identity restore, persistent owner-only `NodeHost`
credentials, initiator rejection before request decode/side effects, the
deny-by-default command matrix and separate production/mock builds are closed.
Validator and full-node UDS lifecycle tests cover initialization, key reload and
authorized reconnect. Exact initial/renewal `REPORT_DATA` binding is closed
deterministically. Secret-bearing handoff commands remain fail-closed until I6
adds in-enclave finalized-proof verification. Real `outbe-chain` validator and
full-node signer/datadir wiring is deliberately closed with their registration
slices in I3/I4; bootstrap caller migration remains I8.
Fresh accepted execution of the real Gramine quote path remains a fail-not-skip
I9 release gate. See
`plans/checkpoints/I2-node-bound-initialization.md`.

## I3 — Register a validator enclave end to end

**Outcome:** a permissionless relay can submit a validator's fresh,
intent-bound evidence and create exactly one active leased binding.

**Includes:**

- validator node ID: EVM address plus consensus BLS public key;
- node and enclave proof-of-possession signatures;
- consensus-facing enclave-resident native QVL call, platform admission and
  exact measurement rule;
- append-only TeeRegistry V1 schema, views and bounded events;
- exact `registerEnclave` gas and idempotent-current replay;
- deterministic active-binding/readiness view for downstream consensus users.

**Acceptance:**

- proposer, validator and follower reach identical state/verdict/gas;
- relay identity has no authority;
- second node binding, second active enclave, stale nonce, wrong profile,
  unattested evidence and strict-status negatives reject;
- the view reports a validator without an active binding as not ready;
  consensus/execution enforcement consumes this view in its separate plan.
- hardware-free I3 tests drive the post-verifier registry boundary with the
  private typed capability and separately replay real policy-negative evidence;
- an accepted real public-verifier-to-active-binding validator flow is an I9
  release E2E gate and is not claimed by the synthetic downstream test.

I3 is closed for the inactive validator route, including the focused
enclave-resident bounded Noise verifier and real validator NodeHost startup.
See `plans/checkpoints/I3-validator-registration.md`.

## I4 — Register a full-node enclave through the same policy

**Outcome:** a full node binds its persistent Reth P2P key to one attested
enclave using the same enclave-resident native QVL, lease and registry
semantics as a validator.

**Includes:**

- canonical compressed secp256k1 P2P node identity and signature;
- full-node enclave profile and exact measurement rule;
- shared registration implementation with only the node-auth adapter varying.

**Acceptance:**

- validator credentials cannot authorize a full-node identity and vice versa;
- full-node registration, duplicate replay and rejection vectors match
  validator semantics;
- there is no weaker full-node/unattested path.
- an accepted real full-node `DcapRequired` registration is closed by I9; I4's
  hardware-free accepted-path test begins only after the verifier boundary.

I4 is closed for the inactive FullNode route. It reuses the I3 bounded
enclave-resident verifier, common registration state machine and persistent
NodeHost lifecycle. See `plans/checkpoints/I4-full-node-registration.md`.

## I5 — Renew, expire and supersede an attested binding

**Outcome:** an active binding renews with a fresh quote in its final third and
stops authorizing new work at expiry.

**Includes:**

- one-hour minimum, seven-day maximum, one-hour collateral margin;
- exact next renewal nonce/version and fresh intent-bound quote;
- idempotent exact replay, expired-current renewal and superseded rejection;
- bounded replacement of a binding without any claim that an already delivered
  offer key was erased or revoked.
- a durable candidate-B lifecycle for both profiles: active manifest A remains
  the normal startup target while B is initialized with fresh enclave keys and
  the same stable `NodeHost` authority;
- canonical write-once replacement evidence plus node/enclave proofs of
  possession, with exact replay idempotency and conflicting replay rejection;
- atomic A-to-B promotion only after an opaque capability binds the exact
  finalized replacement intent and candidate manifest. I5 consumes that
  capability; I6 is the first production issuer.

I5 does not hot-swap a live client, deliver or migrate the permanent offer key,
claim that candidate B is consensus-ready, or add recovery/governance paths.
Before finalized promotion, restart and ordinary NodeHost startup continue to
select A. B remains only a keyless attestation candidate.

**Acceptance:**

- early renewal, stale/conflicting replay and collateral-margin underflow
  reject;
- PCS/PCCS outage never extends a lease or enables fail-open renewal;
- governance changes do not retroactively shorten an existing lease;
- all evidence-bearing mutators fit the 30-million steady block at their
  normative caps.
- A and B have different full manifest/enclave identities but one exact stable
  `NodeHost` authority, and the intent quoted by B reaches Registry replacement
  byte-for-byte unchanged;
- crash/restart before candidate initialization, after durable submission,
  immediately before manifest rename and immediately after rename is
  deterministic, fail-closed and never auto-promotes A to B;
- promotion rejects any finalized authorization for another intent or
  candidate, while exact completed promotion is idempotent.

## I6 — Admit remote Noise sessions only from finalized active state

**Outcome:** peers establish a live session only with the quote-bound Noise
responder key of an active, unexpired registration.

**Includes:**

- node-local verifier reading its own consensus-finalized state;
- external verifier interface requiring a trusted genesis/checkpoint and
  finalized storage proof;
- fresh Noise proof of possession and session deadline capped by lease expiry;
- explicit RPC-trust mode when no finalized anchor is supplied.
- production construction of the opaque finalized replacement authorization
  consumed by I5, binding the exact active Registry replacement intent and
  candidate manifest hash.

**Acceptance:**

- unfinalized/stale/wrong-profile/wrong-key proofs reject;
- sessions close at lease expiry and no fresh DCAP quote is required per
  connection;
- no continuous light client is added inside the enclave for session admission;
- secret-bearing commands keep their separate on-demand finalized proof checks.

## I7 — Activate measurement and platform policy through governance

**Outcome:** anyone can propose, and any relay can execute, a complete
governance-approved policy activation with rolling measurement overlap.

**Includes:**

- canonical proposal body, predecessor hash, activation height and full rules;
- exact `MRENCLAVE` plus signer/product/min-SVN matching;
- chain-scoped platform commitment derived from authenticated PPID and the
  agreed controller;
- old/new rolling overlap and finite old-code cutoff.

**Acceptance:**

- bare measurement, local override and signer-only admission are impossible;
- ambiguous/multiple rule matches reject;
- old leases remain valid until expiry but cannot renew after cutoff;
- raw PPID is absent from separate calldata/state/event/API fields and
  linkability limitations are documented.

## I8 — Bootstrap 32 validator enclaves with full block-1 attestation

**Outcome:** block 1 verifies every initial validator's complete DCAP evidence
before committing bootstrap state.

**Includes:**

- V1 `TeeBootstrap` integration with the existing committee/DKG result, without
  redesigning DKG/BLS/reshare;
- encoded collateral deduplication with per-participant logical QVL charging;
- 1,310,720-byte bootstrap cap, five exact system transactions, no user tx;
- height-selected 500-million/30-million gas limits and full block-size checks.

**Acceptance:**

- a 32-validator production-shaped downstream fixture fits gas, RLP and
  wall-time budgets through the private post-verifier capability;
- the same accepted block-1 flow through real public DCAP verification is an I9
  release E2E gate and is not claimed by the production-shaped fixture;
- missing/invalid evidence or committee mismatch makes block 1 invalid;
- dense vector charges `309,931,488` OST3 precharge at the 896-KiB logical
  evidence cap and leaves the documented bootstrap headroom;
- genesis/startup refuses a committee that its actual bytes/counts cannot fit.

## I9 — Activate production DCAP and make the release gate fail closed

**Outcome:** the production chain activates the V1 manifest only after all
determinism, capacity and x86_64 SGX real-hardware evidence passes.

**Includes:**

- separate `DcapRequired` and `GramineDirectDev` chain specs/genesis;
- A0 activation of V1 selectors, schema, gas and policy schedule;
- real SGX quote generation, canonical collateral packaging, registration,
  renewal, candidate replacement, finalized lookup and Noise handshake in the
  release hardware job;
- a fresh accepted Processor-CA capture for the exact release enclave and
  policy, plus accepted registered multi-package Platform-CA evidence;
- fresh actual Processor, Platform and root CRL provenance and cap checks;
- a published minimum supported x86_64 validator CPU/core/memory/EPC profile
  and exact-release `gramine-sgx` QVL/full-block benchmark matrix on that
  profile;
- an exact production binary/feature allowlist that excludes test tooling;
- release dependency/root checks and forbidden fail-open symbol scans.

For I9, `fresh` means that the release job first freezes the candidate enclave
ELF/SGX manifest and measurement, chain/genesis and exact active policy bytes;
then chooses a new one-use non-zero `binding_id` for the canonical registration
intent, captures a quote whose `REPORT_DATA` commits that intent, acquires
collateral after the release run begins, and verifies it at the release-gate
consensus timestamp while every collateral component is current. Saved
historical replay cannot satisfy this definition.

**Acceptance:**

- real SGX/DCAP job is fail-not-skip;
- real Processor and real registered multi-package Platform x86_64
  verdict/benchmark matrix passes for the exact release artifacts and active
  policy; absence of either accepted result blocks production activation;
- fresh actual Processor, Platform and root CRLs record issuer/type, validity
  dates, byte size and SHA-256 and fit the protocol caps; the release benchmark
  uses the largest actual matching collateral bundle available from PCS;
- no undefined "large real CRL" is fabricated or required: synthetic
  cap-minus-one/cap/cap-plus-one vectors remain deterministic DoS/capacity
  evidence only and never count as Intel hardware evidence;
- a missing hardware runner fails the release gate rather than skipping it,
  while ordinary I1-I8 CI remains hardware-free through immutable replay;
- the production bundle contains only allowlisted targets and the exact
  production feature set: enclave package `outbe-tee-enclave`, binary
  `outbe-tee-enclave`, and application feature `native-dcap`; capture, mock,
  trace and any future fake-verifier surface is absent, and neither
  `--all-features` nor `--all-targets` is used;
- the I9 activation change updates
  `release/reproducible-elf-build-v1.json` from its staged empty feature list to
  exactly `["native-dcap"]`; before that activation the empty list remains an
  explicit pending state, not a production DCAP claim;
- exact-release `gramine-sgx` valid, invalid-early, invalid-late and dense
  32-validator benchmarks run on the published minimum supported x86_64
  validator profile, and its full-block execution remains inside the consensus
  timing budget;
- missing or mismatched QVL/native dependencies, collateral or SGX
  support, including an unsupported architecture, is deterministic rejection;
- pre-A0/legacy chain behavior remains covered and intentional.

## Explicit non-goals

- offer-key DKG, BLS threshold design, reshare and VRF shares;
- lost identity or offer-key recovery;
- software migration bundles and continuity state machines;
- proof that a previously delivered permanent key was deleted;
- a continuous light client inside the enclave;
- privacy-preserving/unlinkable hardware admission;
- consensus membership/readiness and protected-transaction execution gating;
- ARM TEE, aarch64 and multi-TEE portability in V1;
- a second pure-Rust production DCAP verifier or dual-verifier consensus.

Those topics require separate plans and cannot expand these issues implicitly.
