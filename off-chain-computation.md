# Off-chain computation: current protocol contract

This document describes the OCOMP behavior implemented by the node. It is an
operator and code-review reference, not a history of earlier prototypes.

## 1. Membership source

OCOMP has no independent committee.

```text
OCOMP membership = ordered ACTIVE ValidatorSet from the pinned snapshot
N                = pinned_snapshot.member_count
quorum           = simplex_n3f1_quorum(N)
```

The caller cannot choose the voter list, `N` or quorum. A job attempt pins the
historical ValidatorSet snapshot that is current when the request is committed.
Later validator joins and exits affect only later attempts.

The wire field `result_committee_set_hash` is the hash of that pinned consensus
ValidatorSet. Its historical name does not create a second OCOMP committee.

## 2. Validator admission

A joining node starts outside the active set. While `PENDING`, the operator calls
`confirmValidatorReady(registration)`. The registration is bound to:

- chain ID and genesis hash;
- validator address plus the current 48-byte BLS MinPk public key;
- one secp256k1 OCOMP public key;
- `key_epoch = 1` and the result-signing purpose;
- a proof of possession made by the OCOMP key.

The call verifies the binding and proof, reserves the OCOMP key against reuse,
stores the registration and marks the validator ready. A validator without this
registration is excluded from the next DKG target and cannot become `ACTIVE`.
Only a certified DKG/reshare boundary installs the new active set and BLS shares.
There is no owner shortcut that directly activates a validator.

After exit or transition to `INACTIVE`, readiness is cleared. Re-entry requires a
new `confirmValidatorReady`. Exact replay of the same registration is accepted;
replacement releases the old reverse reservation atomically. Changing the BLS key
changes validator identity and clears the old OCOMP registration.

An operational EVM delegate may submit the outer vote transaction. Delegation
does not create membership and does not replace the registered OCOMP signing key.

## 3. Genesis bootstrap

Genesis seeds the ordered ACTIVE ValidatorSet in the usual ValidatorSet storage.
The OCOMP fork-install artifact carries one founder registration for every member
in that exact order. These registrations bootstrap keys only.

Founder material contains no participant indices, `N`, threshold or committee
hash. At OCOMP lifecycle activation the node validates exact coverage, ordering,
PoP, purpose, key epoch and key uniqueness, then imports all registrations into
ValidatorSet in the same atomic checkpoint as the fork install.

Registrations bind the final genesis hash. They therefore live in the chain-config
install artifact rather than genesis alloc; placing them in alloc would create a
genesis-hash fixed point. Missing, extra, reordered or duplicate founder material
is rejected.

## 4. Historical snapshot extension

The consensus `CommitteeEntry`, `CommitteeSnapshot` and
`committee_set_hash_v2` are unchanged. OCOMP data is stored in a separate
ValidatorSet extension keyed by the historical snapshot key. It contains:

- epoch and consensus set hash;
- `ocomp_binding_hash`;
- member count;
- ordered OCOMP key and key epoch for every member.

`ocomp_binding_hash` commits to the epoch, consensus hash, ordered addresses,
OCOMP keys and key epochs. The consensus snapshot and extension are written in
one boundary checkpoint. Exact replay must be byte-identical. Ring eviction and
replacement are atomic, and an evicted or mismatched snapshot is treated as
missing rather than replaced with the current set.

The previous Metadosis static-committee storage slot remains reserved and zero so
the storage layout hash does not change.

## 5. Job creation and voting

When `commit_ocomp_request` creates an attempt it stores:

- `result_validator_set_epoch`;
- `result_committee_set_hash`;
- `result_ocomp_binding_hash`;
- `N` and the quorum derived from the pinned snapshot.

All three bindings are covered by the vote signing preimage and the sign-once
journal. They distinguish an exact retry from equivocation across boundaries and
restarts.

`submitLysisResult(bytes)` is an ordinary EVM transaction. Processing is:

1. bounded prefix decode;
2. load the job and attempt;
3. compare all three vote bindings with the attempt;
4. load and validate the pinned historical snapshot;
5. resolve the member in O(1) by `u16` participant index;
6. verify the OCOMP signature and operational signer/delegate;
7. record the first vote, retry or equivocation;
8. derive quorum and apply the canonical result when reached.

Malformed, late, unknown-member and missing-snapshot votes revert as public vote
rejections. They do not produce a fatal block error. There is no fallback to the
current ValidatorSet.

## 6. Accountability

Vote state is one bounded `OcompVoteAccountabilityV1` value with a dynamic
`Vec<Option<ResultVoteSlotV1>>`. It stores the job bindings, `N`, quorum, slots,
formed quorum and closed summary. No per-member storage subsystem or private
membership cap is introduced.

Bitmaps have `ceil(N/8)` bytes. Member `i` uses byte `floor(i/8)` and bit
`i mod 8` (LSB0); unused high bits in the final byte must be zero. The summary
records matching, divergent, missing and equivocating participants and remains
bound to the historical snapshot.

## 7. Node roles

Validators execute the deterministic OCOMP program, use their registered OCOMP
key through the supervisor/enclave path and submit result votes. Node-side lookup
uses the finalized job bindings. If its pinned snapshot is unavailable, the node
abstains, emits an error log and metric, and never signs against the current set.
Startup of an OCOMP-enabled validator requires its complete signing configuration.

A FullNode does not vote and needs no OCOMP signing key or delegate. It imports and
executes the same canonical blocks as validators and must reach the same EVM/state
roots and on-chain Lysis/Nod state.

Independent local recomputation of Lysis by FullNodes, local result-chunk storage
and comparison with the voted result are a separate required follower task. This
document does not claim that follower exists.

## 8. Capacity and failure behavior

OCOMP reads the validator bound from consensus `MAX_VALIDATORS`; it has no fixed
four-member model and no OCOMP-specific maximum. The current synthetic boundary
gate runs at `N=256`, quorum `171`. The worst closed monolithic accountability
encoding is 58,760 bytes, each bitmap is 32 bytes, and the checked quorum work fits
the 64-block response window. These are synthetic bounds, not hardware evidence.

If a future consensus bound no longer fits codec, storage or response-window
limits, release is blocked. The implementation must not silently cap OCOMP
membership or change the active ValidatorSet.

Missing founder registrations fail genesis installation. Missing registration on
a certified ACTIVE boundary violates admission and fails closed. Missing historical
state rejects the affected vote; a job without quorum expires normally. Storage
checkpoints make registration replacement, boundary snapshots, vote writes and
quorum application atomic and replay-safe.

## 9. Review map

The main code-review surfaces are:

- `crates/system/validatorset`: registration, readiness and historical extension;
- `crates/system/ocomp-protocol`: canonical types, bindings, bitmaps and capacity;
- `crates/core/metadosis`: job pinning, vote processing and fork installation;
- `crates/system/zerofee` and `crates/blockchain/txpool`: shared vote prefix policy;
- `crates/blockchain/node/src/ocomp`: pinned-snapshot signing and restart recovery;
- `bin/outbe-chain`, `bin/outbe-keygen` and `xtask`: genesis and canonical artifacts;
- `testing/e2e-harness`: FullNode-to-validator join and historical-job scenarios.

The acceptance process scenario starts with four ACTIVE validators, opens job A,
joins a fifth node through FullNode sync and normal validator admission, then opens
job B. The expected values are `A: N=4/quorum=3` and `B: N=5/quorum=4`; those
numbers describe this test only, not protocol constants.
