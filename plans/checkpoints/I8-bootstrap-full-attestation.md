# I8 checkpoint - full-attestation bootstrap

Date: 2026-08-02

Status: `PASS` for the staged I8 scope. The evidence-carrying OST3 path is
canonical, bounded, gas-accounted and wired through validator execution to the
enclave-resident DCAP verifier. Production activation, real SGX execution and
the startup producer switch remain I9 gates and are not claimed here.

## Outcome

`TeeBootstrapV2` carries the exact active V1 policy, existing DKG/offer-key
authority, a complete validator participant set, per-validator quotes and
proofs, a canonical deduplicated collateral pool and one committee signature
per participant. The codec rejects oversized or non-canonical input before
state access or cryptographic work and reconstructs a complete logical
`AttestationEvidenceV1` for every validator.

The block-1 DCAP handler binds the payload to:

- the authoritative active policy;
- the complete active consensus committee, with no subset admission;
- the exact epoch-0 committee snapshot;
- valid recoverable committee signatures over the complete unsigned body;
- public `register_enclave_v1` verification for every reconstructed evidence
  item before the existing bootstrap state is committed.

Any bootstrap revert is block-fatal. Block 1 requires `CycleTick`,
`BoundaryOutcome`, `TeeBootstrap`, `OracleSlashWindow` and `HookEvents`, permits
no user transaction and rejects bootstrap replay at every other height.

## Wire and activation boundary

I8 adds OST3 as a separate selector. It does not reinterpret or remove legacy
OST2. The existing startup/DKG producer and bridge remain typed as OST2 before
A0, and their proposer/validator parity remains covered.

I9/A0 must atomically switch the producer and ChainSpec to OST3 and reject OST2
under the activated production manifest. This staging avoids mixed wire
interpretation and does not claim that pre-A0 production already emits DCAP
bootstrap evidence.

## Canonical gas and capacity

`SystemGasScheduleV1` canonically commits:

```text
OST3 =
    300,000
  + 1 * full_calldata_len
  + 100,000 * participant_count
  + sum(QVL_DCAP(logical_evidence_len_i, active_rule_count))
  + 15,000 * logical_collateral_component_ref_count
  + 10,000 * committee_signature_count
```

Its golden hash is
`8879dd524fc4c5ccfc1c353b1f6840502f6e4f1eebc9825b27a8039bedf029a9`.
`ResourceScheduleV1` binds that hash and the normative TeeRegistry schedule;
its updated golden hash is
`83015a5531a87b9c5d782a06bdc270c67c73364a4e5c644f728ae23fc0a50e0b`.

The complete OST3 calldata cap is 1,310,720 bytes, including selector and
version. Deduplicated collateral reduces encoded bytes only: every participant
still pays eight logical component references and one full QVL charge. Bounded
bootstrap storage work is included in the precharge and is not charged again.

At 32 participants, the 896-KiB logical evidence cap and 64 active rules:

- each QVL charge is `9,405,024`;
- OST3 precharge is `309,931,488`;
- maximum intrinsic gas is `20,992,520`;
- combined OST3 gas is `330,924,008`;
- `169,075,992` remains in the 500-million bootstrap block.

The proposer selects 500 million gas only at height 1 and 30 million otherwise;
the consensus validator rejects any different header value. Protocol precharge
is both reserved before the CycleTick remainder and published in receipt and
block `gas_used`; it is not exposed as compressed-entity gas.

## Existing DKG integration seam

`TeeBootstrapV2::assemble_unsigned` accepts the existing policy/snapshot/DKG
authority and complete per-validator evidence submissions. It sorts
participants, deduplicates collateral, derives canonical references and emits
the exact committee-signature slots whose bytes are excluded from the signing
hash. It introduces no DKG, BLS, reshare, VRF, recovery or continuity design.

The assembler validates the canonical calldata cap. The same visible gas plan
used by proposer and validator rejects a payload that cannot fit the selected
block gas limit. Before committee signing, `preflight` computes the exact
canonical length with checked arithmetic without allocating the aggregate
body; assembly consumes and moves submitted collateral bytes through canonical
deduplication instead of cloning an aggregate pool. It then rejects worst-case
intrinsic gas plus normative OST3 precharge above 500 million. Genesis
`key_epoch`, `tribute_offer_epoch` and transcript hash may
remain zero, matching the existing DKG producer and registry semantics. I9
wires this seam to real quote/collateral collection and the startup producer.

## Acceptance audit

| I8 criterion | Authoritative evidence | Result |
|---|---|---|
| Canonical evidence payload | OST3 round-trip, signing-hash, pool order/uniqueness, exact authority and cap-plus-one tests | `PASS` |
| Complete logical verification | every participant reconstructs eight typed components and is passed to public `register_enclave_v1` | `PASS` |
| Exact committee authority | handler requires participant set equality, one ordered signature per participant and exact epoch-0 snapshot | `PASS` |
| Fatal missing/invalid bootstrap | block-1 membership/layout checks, OST3 dispatch of canonical invalid evidence to the public verifier, and `TeeBootstrap::revert_fails_block()` | `PASS` |
| No user work in block 1 | canonical layout rejects every non-empty user zone | `PASS` |
| Deduplicated bytes, non-deduplicated work | two-participant shared collateral test retains two QVL charges and 16 logical refs | `PASS` |
| Dense gas closure | exact `309,931,488` precharge vector and checked arithmetic/cap tests | `PASS` |
| 32-validator capacity | 64-rule, near-896-KiB shared-collateral payload and the exact five transaction encodings fit gas/P2P caps; private post-verifier state fixture creates exactly 32 ready bindings | `PASS` |
| Visible gas accounting | gas plan reserves precharge; success/failure receipt harness publishes intrinsic + precharge + explicit CE gas | `PASS` |
| Proposer/validator block schedule | payload builder selects 500M/30M; consensus rejects 500M after height 1 | `PASS` |
| Pre-A0 compatibility | default OST2 codec, engine bridge, legacy proposer injection and full builder re-execution remain green | `PASS` |
| Producer rejects block-1 users | production payload-builder harness carries a valid OST2 bootstrap and a non-empty pool; output contains exactly five system transactions and no user zone | `PASS` |
| Hardware evidence boundary | private post-verifier fixture is not called DCAP E2E; real accepted OST3 block and timing remain fail-not-skip I9 evidence | `PASS` scope boundary |

## Reachable verification

The checkpoint was closed with:

```bash
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test \
  -p outbe-primitives --features tee-attestation-v1 \
  --test tee_bootstrap_v2 --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test \
  -p outbe-primitives --features tee-attestation-v1 \
  --test tee_attestation_v1 --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test \
  -p outbe-primitives --features tee-attestation-v1 --lib system_tx --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test \
  -p outbe-primitives --lib system_tx --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test \
  -p outbe-teeregistry --features tee-attestation-v1 --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test -p outbe-evm --lib --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test -p outbe-consensus --lib --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo test -p outbe-node --lib --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo check -p outbe-node --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo check -p outbe-engine --offline
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo clippy \
  -p outbe-evm --all-targets --no-deps --offline -- -D warnings
CARGO_TARGET_DIR=/tmp/outbe-i8-gas-target cargo clippy \
  -p outbe-node --all-targets --no-deps --offline -- \
  -D warnings -A clippy::too_many_arguments
cargo fmt --all -- --check
git diff --check
```

Observed results:

- OST3 codec/gas/cap/assembly: 12 passed, including aggregate-cap and >500M
  pre-signing rejection;
- V1 policy and schedule vectors: 25 passed;
- default and feature-enabled system transaction suites: 31 passed each;
- TeeRegistry state machine, including the 32-validator private
  post-verifier fixture: 36 passed;
- EVM proposer/verifier, fatality and receipt accounting: 177 passed;
- consensus validation and existing DKG behavior: 345 passed;
- node production payload-builder block-1 pool suppression: covered in the
  complete 79-test node suite;
- node and engine compiled with the legacy pre-A0 producer bridge intact;
- all EVM targets and all node targets passed strict `clippy`; the node run
  carries one narrow command-line allowance for `too_many_arguments` because
  the pre-existing `tee_remote_session.rs::seed_binding` helper (introduced by
  commit `df5fb6d7` and untouched by I8) has nine parameters. The initial
  unqualified `-D warnings` run failed only at that baseline helper; no lint
  allowance was added to source and no unrelated cleanup was pulled into I8.

## Evidence-based fixture triage

The first full runs exposed test setup drift, not product defects. Block-1 EVM
fixtures still created a 30-million EVM environment while their transaction
builder used the new 500-million schedule; consensus fixtures wrote the
envelope subtotal into the header instead of the protocol gas limit. Focused
RED reproductions failed at signature/header checks, and the minimal fixture
changes made those same tests GREEN without changing validation order.

One independent Oracle test directly wrote a retired Oracle-local delegation
slot while production resolution uses `ValidatorSet` role delegation. Replacing
the direct storage mutation with the public delegation path made the focused
test and the complete EVM suite GREEN. Verdict scope: these were test-defects;
no I8 product behavior was changed to satisfy them.

The independent closeout reviews then found three product defects before the
commit: the assembler could return an aggregate-over-cap payload, genesis-zero
DKG epochs were rejected by OST3, and the production block-1 builder still
iterated the user txpool. Focused regressions reproduced each issue. Checked
length/gas preflight, genesis authority compatibility and unconditional
block-1 pool suppression made those tests GREEN. The reviews also found that
the original 32-validator RLP fixture used tiny evidence and only one system
transaction; it was replaced by a 64-rule near-cap logical-evidence fixture
that encodes all five mandatory block-1 transactions. Hardware provenance is
still explicitly not claimed.
The allocation re-review additionally required participant-count rejection
before assembly and move-based collateral deduplication; the final re-review of
the resulting temporary-to-canonical index remap and early cap ordering was
`PASS` with no remaining actionable findings.

## Deferred, not waived

I9 owns real `gramine-sgx` execution, fresh accepted Processor evidence,
empirical exact-release full-block timing,
real quote/collateral producer wiring, fail-not-skip release CI, production
feature allowlisting and the atomic OST2-to-OST3 activation cutoff.

I8 adds no ARM/aarch64 path, DKG/BLS redesign, offer-key recovery, proof of key
deletion, PPID/controller allowlist, continuity state machine or additional
consensus admission mechanism.
