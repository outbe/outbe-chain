# I0 checkpoint — inactive V1 manifest and deterministic context

Date: 2026-07-30
Base: `d44bf85609e1cb5da2526795874d3d80c4e03f3d` (`main`)

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

## Outcome and includes audit

| Requirement | Evidence | Status |
|---|---|---|
| Canonical V1 codecs, domains, enums, bounds, and golden vectors | `tee_attestation_v1` defines node IDs, operation-bound registration intents, DCAP/dev evidence, strict TCB status, measurement rules, policies, policy schedules, registry gas schedules, resource schedules, and their domain hashes. Tests pin canonical bytes and hashes. | Pass |
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

## Fixed vectors

- Registry gas schedule hash:
  `11edf34f5614ee89ceb28c4597c309ad055cc67a752e335affa69fbc177c3da8`
- Registration intent hash:
  `7866748eebd89640c998bfaf64d5c5a44b4849a44c7b0e1ac32d118560a85e13`
- Report-policy/`NodeHost` commitment:
  `378c9bbee1671eeb2d8447ba76919a81f2175148800244ff0ca20c2e907d5216`
- Policy hash:
  `2fd9eba57340225c00e45f8c4eb5fa3463f0a708918e5301351e587f65996e47`
- Policy schedule hash:
  `1bf8cdff9e59ec9739d655d846bdfc72b330e78ff5254c5932ad8fbb066bf9cd`
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
PASS: 231 unit tests plus all integration, compile-fail, and doc tests

cargo test -p outbe-primitives --features tee-attestation-v1 --offline
PASS: default suite plus 14 V1 codec/boundary/golden tests

cargo clippy -p outbe-primitives --features tee-attestation-v1 --all-targets --offline -- -D warnings
PASS

cargo check -p outbe-evm --offline
PASS

cargo test -p outbe-evm --offline
PASS: 175 unit tests and all integration/doc tests
NOTE: one pre-existing `call_trampoline_full_block_diff` test remains explicitly ignored by its owning code; it is unrelated to I0.

cargo clippy -p outbe-evm --all-targets --offline -- -D warnings
PASS

cargo test -p outbe-evm --test outbe_precompile_registration --offline
PASS: 8 tests

git diff --check
PASS
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
