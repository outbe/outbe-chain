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
stable Outbe verdict backed by pinned native Intel QVL executing inside the
Outbe Gramine enclave, with no local host inputs.

**Includes:**

- completed feasibility gate (`c679649`, hardened by `bc96db2`, `d5ab89e` and
  `fd1598c`, with QVL-scoped tracing in `8dcebae`): Intel DCAP QVL
  `1.26.100.1-noble1`, its native dependencies and the
  `x86_64-unknown-linux-gnu` target are exact-pinned, and a real Processor-CA
  quote verifies through the public adapter both natively and under Gramine
  Direct using submitted collateral and explicit consensus time;
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
real Processor-CA quote. Platform-CA grammar and policy behavior use the
official Intel parser/test vectors; a real Intel-rooted Platform-CA quote
requires registered multi-package SGX hardware and is a fail-not-skip I9
release gate.

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
acquisition is host-side fixture tooling only. I1 remains open until a second
real intent-bound Processor-CA capture from a host with an allowed Platform
status supplies the accepted positive vector. This external hardware
requirement does not authorize a broader status matrix or synthetic positive.

Toolchain staging is intentionally separate from container delivery:
`release/project-toolchain-v1.json` is the single exact version pin and is
bound into ELF and SGX release inputs, while its activation remains
`pending-container-delivery`. This state does not claim that the current
production ELF/SGX builders use the unified image, and it does not complete
the native-DCAP release gate. Image delivery and activation must be resolved
before the production enclave is built with `native-dcap`.

**Acceptance:**

- a real Processor-CA positive/negative cryptographic corpus bound to
  `RegistrationIntentV1::report_data()`;
- synthetic Intel Platform-CA parser/policy vectors cover CA classification,
  Platform/QE status mapping and every stable rejection branch, but are never
  represented as real hardware evidence;
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

**Acceptance:**

- replayed/conflicting initialization rejects;
- unknown initiator cannot decode or cause command side effects;
- authorized initial/renewal quote embeds the exact intent and policy
  commitments;
- Gramine development behavior exists only under the separate dev manifest.

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

## I5 — Renew, expire and supersede an attested binding

**Outcome:** an active binding renews with a fresh quote in its final third and
stops authorizing new work at expiry.

**Includes:**

- one-hour minimum, seven-day maximum, one-hour collateral margin;
- exact next renewal nonce/version and fresh intent-bound quote;
- idempotent exact replay, expired-current renewal and superseded rejection;
- bounded replacement of a binding without any claim that an already delivered
  offer key was erased or revoked.

**Acceptance:**

- early renewal, stale/conflicting replay and collateral-margin underflow
  reject;
- PCS/PCCS outage never extends a lease or enables fail-open renewal;
- governance changes do not retroactively shorten an existing lease;
- all evidence-bearing mutators fit the 30-million steady block at their
  normative caps.

## I6 — Admit remote Noise sessions only from finalized active state

**Outcome:** peers establish a live session only with the quote-bound Noise
responder key of an active, unexpired registration.

**Includes:**

- node-local verifier reading its own consensus-finalized state;
- external verifier interface requiring a trusted genesis/checkpoint and
  finalized storage proof;
- fresh Noise proof of possession and session deadline capped by lease expiry;
- explicit RPC-trust mode when no finalized anchor is supplied.

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

- 32-validator production fixture fits gas, RLP and wall-time budgets;
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
  finalized lookup and Noise handshake in CI;
- release dependency/root checks and forbidden fail-open symbol scans.

**Acceptance:**

- real SGX/DCAP job is fail-not-skip;
- real Processor and real registered multi-package Platform x86_64
  verdict/benchmark matrix passes;
- the slowest supported validator keeps full-block execution inside the
  consensus timing budget;
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
