# I0 checkpoint — inactive V1 manifest and deterministic context

Date: 2026-07-30
Audited tree: `f00cabbae368a3e7d9fe0015c34a81969911e525`
after rebasing onto `777624e625f7ce8ef32c1f2cc8a8c620d5b59912`
(`origin/main`).

## Result

I0 is complete. Direct harnesses can encode and hash the inactive V1
attestation values, calculate the normative resource charges, and observe
chain ID, genesis hash, block number, and block timestamp. No V1 selector,
storage layout, dispatch route, or active ChainSpec manifest was installed.

The final single-`NodeHost` decision is reflected in the protocol vocabulary:
the quote binds `node_host_authorization_hash`; there is no role-key set.
`REPORT_DATA` is fixed to the canonical intent commitment followed by the
genesis/policy/single-`NodeHost` commitment. The intent also binds the exact
operation (`register`, `renew`, measurement transition, or replacement).

The 2026-07-30 decision amendment keeps V1 scoped to x86_64 Intel SGX and
encodes two exact Platform TCB admission sets. The initial policy admits
`UpToDate` and `SWHardeningNeeded`; a later governance policy may tighten this
to `UpToDate` only. QE remains `UpToDate` only, and TCB Info schema v3 remains
mandatory.

## Outcome and includes audit

| Requirement | Evidence | Status |
|---|---|---|
| Canonical V1 codecs, domains, enums, bounds, and golden vectors | `tee_attestation_v1` defines node IDs, operation-bound registration intents, DCAP/dev evidence, exact Platform TCB status sets, strict QE status, measurement rules, policies, policy schedules, registry gas schedules, resource schedules, and their domain hashes. Tests pin canonical bytes and hashes. | Pass |
| `TeePolicyScheduleV1` | Strict version/chain/genesis identity, sorted activation heights, monotonic versions, predecessor hashes, bounded policies/rules, and height selection. | Pass |
| `TeeRegistryGasScheduleV1` | Only the normative coefficient vector encodes/decodes; all arithmetic is checked. | Pass |
| `ResourceScheduleV1` | Commits both schedule hashes and the exact `500,000,000` block-1 / `30,000,000` steady gas limits. Non-normative values do not encode. | Pass |
| Canonical execution context | `genesis_hash()` is available beside `chain_id()` on every storage provider. `OutbeEvmConfig` sources it from canonical `ChainSpec`; top-level, direct-hook, and nested precompile execution preserve it. Proposer, validator, and follower execution use that shared factory path. Explicit HashMap and read-only constructors cover direct/test and follower-view harnesses. | Pass |
| Exact byte caps | Quote `16 KiB`; evidence and every component `896 KiB`; evidence-call framing `16 KiB`; policy `32 KiB`; active rules `64`. DCAP encoding preflights complete aggregate length before allocating its output buffer. | Pass |
| Inactive feature/manifest | `tee-attestation-v1` is opt-in, default features remain empty, and `ACTIVE_TEE_ATTESTATION_V1_MANIFEST` is `None`. | Pass |

## Acceptance audit

| Criterion | Verification | Status |
|---|---|---|
| Duplicate, unknown, trailing, and noncanonical encodings reject | Exact eight ordered collateral kinds; unknown mode/operation/node/component/status versions reject; mode/intent mismatch rejects; trailing bytes reject; schedule/rule duplicates and invalid predecessor chains reject; non-normative schedules reject. Declared caps are checked before variable payload allocation. | Pass |
| Equal chain ID with different genesis rejects | `RegistrationIntentV1::validate_chain_identity` test accepts the expected genesis and rejects another genesis under the same chain ID. | Pass |
| `cap-1`, `cap`, `cap+1`, and overflow | Aggregate evidence boundary vectors cover all three sizes; quote/component/framing/rule caps and `usize`→gas overflow reject deterministically. | Pass |
| No active V1 selector or storage layout | Repository scan finds the V1 module only behind the direct-harness Cargo feature and its tests. The module declares no address, selector, state schema, or runtime registration. | Pass |
| Legacy behavior byte-identical | Default feature set remains empty. The complete default `outbe-primitives` suite and complete `outbe-evm` suite pass; the only active-path change is additive immutable context propagation. | Pass |
| Revised TCB policy is governance-tightenable | The public schedule vector starts with `UpToDateOrSWHardeningNeeded`, activates `UpToDateOnly` through a predecessor-bound policy, round-trips both values, and rejects an unknown status set. QE has only the canonical `UpToDate` value. | Pass |
| Production architecture is bounded | Decision, evidence and implementation documents consistently specify x86_64 Intel SGX only. ARM TEE, aarch64 fixtures and cross-architecture release gates are explicit V1 non-goals. | Pass |

## Fixed vectors

- Registry gas schedule hash:
  `11edf34f5614ee89ceb28c4597c309ad055cc67a752e335affa69fbc177c3da8`
- Registration intent hash:
  `7866748eebd89640c998bfaf64d5c5a44b4849a44c7b0e1ac32d118560a85e13`
- Report-policy/`NodeHost` commitment:
  `378c9bbee1671eeb2d8447ba76919a81f2175148800244ff0ca20c2e907d5216`
- Policy hash:
  `4dbc61a63c5c3107b56a75eb3a38f640e22650a34ad25cb52060726b1f7baacd`
- Policy schedule hash:
  `ff3de6da76820b4a84e27f68d0de81b28278b1244c62cf3962707e409660786c`
- Maximum DCAP verification charge:
  `9,405,024`
- Maximum registration transaction gas:
  `28,768,784`
- Maximum replacement transaction gas:
  `29,133,784`

## Verification record

All commands ran offline.

```text
cargo test -p outbe-primitives --offline
PASS: 233 unit tests plus all integration, compile-fail, and doc tests

cargo test -p outbe-primitives --features tee-attestation-v1 --offline
PASS: 233 unit tests plus all integration, compile-fail, and doc tests,
including 14/14 V1 codec/boundary/golden tests

cargo clippy -p outbe-primitives --features tee-attestation-v1 --all-targets --offline -- -D warnings
PASS

cargo check -p outbe-evm --offline
PASS

cargo test -p outbe-evm --offline
BASELINE TEST-DEFECT: 174 unit tests pass and
`zero_fee_oracle_vote_from_delegated_feeder_keeps_zero_balance` fails before
its EVM assertion. The same focused failure reproduces from a clean
`origin/main@777624e` archive; see the finding below. It is not an I0
regression and no DCAP/product code was changed to hide it.

cargo clippy -p outbe-evm --all-targets --offline -- -D warnings
PASS

cargo test -p outbe-evm --test outbe_precompile_registration --offline
PASS: 8 tests

git diff --check
PASS
```

The post-rebase audit reran the complete default and
`tee-attestation-v1`-enabled `outbe-primitives` suites, both all-target clippy
gates, and the exact active-precompile route enumeration. The V1 integration
suite remains 14/14, including the broad-to-strict Platform policy transition
and unknown-set rejection.

## Baseline regression finding

```text
ID: I0-BASELINE-ORACLE-DELEGATION-TEST
Status: test-defect
Observed: outbe-evm unit test fails while reading its seeded Oracle delegate.
Expected: the fixture creates a production-reachable active Oracle delegate.
Production reachability: the current public setDelegate selector writes the
  role-scoped ValidatorSet forward/reverse indexes; production resolution
  reads those indexes.
Reproduction command: cargo test -p outbe-evm
  executor::tests::zero_fee_oracle_vote_from_delegated_feeder_keeps_zero_balance
  --offline -- --exact --nocapture
SHA/environment: both f00cabb and clean origin/main@777624e,
  x86_64-unknown-linux-gnu, 2026-07-30T16:02:16Z.
Evidence: both trees fail with "caller is not an active ORACLE signer";
  the test writes legacy oracle.feeder_delegation directly, while
  resolve_validator_for_feeder delegates to
  ValidatorSet::resolve_validator_for_role.
Test setup validity: invalid after 777624e; it bypasses setDelegate and creates
  a state the current production route does not create.
Affected invariant/postcondition: none in I0; failure occurs before the tested
  transaction and before any V1 path.
Competing causes excluded: the clean origin/main archive fails identically;
  origin/main introduced the role resolver and left this older fixture
  unchanged; I0's diff does not change either resolver or fixture.
Root cause: stale direct-storage test fixture after the role-delegation schema
  migration in 777624e.
Counterfactual proof: not needed for product behavior; this is a test-defect
  present in the upstream baseline. No product edit is warranted in I0.
Proposed minimal fix: in the owning non-DCAP change, drive setDelegate through
  its public precompile route before executing the Oracle vote.
Regression: tracked explicitly here; it does not waive any I0-relevant test.
Verdict scope: only this one outbe-evm test setup; no claim about unrelated
  Oracle or delegation behavior.
```

## Scope and fail-open audit

- No TeeRegistry selector, storage slot, ABI, genesis field, or dispatch route
  was added.
- No QVL implementation, host collateral fetch, registry mutation, enclave
  initialization, Noise transport, governance execution, DKG, BLS, reshare,
  recovery, migration, deletion proof, light client, or consensus-readiness
  mechanism entered I0.
- The default/test convenience constructors retain zero genesis only for
  legacy callers that cannot consume V1 while the feature is inactive.
  Canonical production EVM construction cannot take that path: all
  `OutbeEvmConfig` constructors bind `ChainSpec.genesis_hash()`.
- Missing V1 activation remains fail-closed as absence: the active manifest is
  `None` and there is no callable production V1 route.

No blocker or unresolved I0 acceptance item remains. I1 may begin.
