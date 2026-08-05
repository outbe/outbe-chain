# OCOMP verification guide

This document explains how to verify the current OCOMP contract. It does not
preserve the removed static-committee design.

## What must be true

1. Voting membership exactly equals the ordered ACTIVE ValidatorSet pinned by the
   job attempt.
2. `N` and quorum are derived by consensus code; a caller cannot supply them.
3. Every ACTIVE validator has one admitted OCOMP registration.
4. Existing jobs retain their historical membership while later jobs use the new
   active set.
5. Votes bind epoch, consensus set hash and OCOMP key binding hash.
6. FullNodes replay the same canonical state transitions but do not vote.

## Focused verification

Run the protocol and state tests before process E2E:

```bash
cargo test -p outbe-ocomp-protocol
cargo test -p outbe-validatorset
cargo test -p outbe-metadosis
cargo test -p outbe-node --lib
cargo test -p outbe-ocomp --lib
```

The required focused behaviors include:

- registration PoP, chain/genesis/BLS identity and unique-key checks;
- readiness exclusion from DKG before `confirmValidatorReady`;
- certified activation as the only production path to `ACTIVE`;
- exact registration replay and atomic replacement/re-entry cleanup;
- boundary creation of consensus snapshot plus OCOMP extension;
- two live historical snapshots across a membership change;
- dynamic `N`, quorum, slots and LSB0 bitmaps;
- old-member authorization for an old job after current membership changes;
- ordinary vote rejection for a missing/evicted snapshot;
- identical ZeroFee classification in pool and execution;
- sign-once exact retry across restart and boundary;
- validator startup failure without OCOMP config and FullNode startup success.

## Genesis and artifact checks

Genesis tooling must consume the ordered validator manifest and one registration
per entry. It must reject missing, extra, reordered and duplicate-key material.
The generated fork install may contain founder registrations but must contain no
result committee, participant indices, member count or threshold.

```bash
cargo test -p outbe-chain ocomp_genesis::tests --bin outbe-chain
cargo xtask ocomp registry --check
cargo xtask ocomp shape --check
cargo xtask ocomp final-artifacts \
  --capacity testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/generated-capacity-v1.json \
  --base-genesis testing/e2e-harness/fixtures/ocomp-final-v1/base/genesis.json \
  --validators testing/e2e-harness/fixtures/ocomp-final-v1/base/validators.json \
  --output-dir testing/e2e-harness/fixtures/ocomp-final-v1/artifacts \
  --check
```

The checked artifact set must not contain `result-committee-v1.ocb1` or
`result-committee-public-v1.json`.

## Storage and capacity checks

The removed committee field remains one zero reserved slot. The Metadosis layout
hash and all later base slots must remain unchanged.

```bash
cargo test -p outbe-metadosis \
  fork_install_is_exactly_profile_plus_bundle_and_keeps_reserved_slot_zero --lib
cargo test -p outbe-metadosis test_storage_dsl_layout_slots --lib
cargo test -p outbe-ocomp-protocol \
  dynamic_membership_capacity_fits_the_consensus_validator_bound \
  --test generated_capacity -- --nocapture
```

At the current consensus bound the synthetic gate expects `N=256`, quorum `171`,
a 58,760-byte worst closed accountability value and 32-byte bitmaps. The gate also
checks canonical decode and response-window work. It is not hardware benchmark
evidence.

## Reachable process E2E

The final harness scenario uses real node roles and normal admission:

1. start four ACTIVE validators;
2. open job A and observe `N=4`, quorum `3`;
3. start node 5 as a FullNode and synchronize canonical state;
4. restart node 5 in validator mode;
5. register/stake it, configure delegation, call `confirmValidatorReady`, complete
   DKG/reshare and wait for the certified boundary;
6. assert node 5 is ACTIVE;
7. open job B and observe `N=5`, quorum `4`;
8. prove node 5 votes in B but cannot vote in A;
9. prove A still completes against its original snapshot;
10. restart participants and prove both live-job and sign-once state recover;
11. compare finalized height, block hash and state root on validators and FullNode.

The values in this scenario are examples used to expose the membership transition.
They are not OCOMP constants.

## What this does not prove

- It does not prove independent FullNode Lysis recomputation; that follower is a
  separate task.
- It does not provide DCAP or production hardware evidence.
- It does not add OCOMP key rotation, expiry or recovery; `key_epoch` remains 1.
- It does not preserve or migrate a legacy static committee; networks start from a
  fresh genesis.

## Release gates

After the focused and process tests pass, run formatting, Clippy, workspace tests,
`cargo machete`, registry/shape checks and exact fixture reproducibility. Any
capacity failure, layout-hash change, fallback to current membership, or independent
OCOMP committee is a release blocker rather than a condition to waive.
