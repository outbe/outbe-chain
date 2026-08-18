# ADR-S-VAL-001: ValidatorSet owns validator identity, lifecycle and committee eligibility

- **Status:** Accepted; OCOMP admission and certified activation implemented
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/validatorset`; validator identity/status,
  committee eligibility, epoch counters, P2P identity and historical snapshots
- **Depends on:** ADR-B-GEN-001, ADR-B-CNS-001, ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-S-STK-001 Staking, ADR-S-RWD-001 Rewards, ADR-S-SLS-001 SlashIndicator
- **Supersedes:** The ValidatorSet-local portions of the deleted pre-space validator aggregate

## Context

Consensus, staking, voting, rewards and slashing all ask whether an address is a
validator, but only one module may own that fact and its lifecycle. The answer is
not equivalent to `status == ACTIVE`: current threshold-share membership, next
reshare eligibility, non-voting P2P admission and historical certificate
verification are separate derived views.

## Decision

ValidatorSet is the sole owner of validator address/BLS identity, lifecycle status,
registry indexes, the validator's canonical OCOMP registration, current-share
membership, pending-set-change signal, epoch participation counters, versioned P2P
addresses and bounded historical committee snapshots. Staking owns economic stake
and mirrors only the compatibility fields needed here. Consensus owns DKG artifacts
but changes membership only through the atomic boundary-activation command.

Consumers must use named queries for the exact authority they need:

- `Active` validators are the present governance/reward population;
- current consensus participants are `Active`, `Exiting` and `JailRetained`;
- the next reshare target is `Active` plus `Joining` validators whose canonical
  OCOMP registration was accepted by `confirmValidatorReady(registration)`;
- non-voting secondary admission is `REGISTERED | PENDING | JAILED`; and
- historical finalized-parent verification uses the committee snapshot keyed by
  canonical epoch and committee hash, not the current registry view. Historical
  OCOMP verification uses the separate extension stored under that same snapshot
  key and additionally requires its exact OCOMP binding hash.

## Authoritative mutation interface

User-facing ABI commands are registration with BLS proof of possession,
optional owner/self P2P-address update, owner/self voluntary deactivation and self
readiness confirmation with a canonical OCOMP registration. No ordinary
owner/manual call is an independent committee-activation authority. Owner
registration may omit on-chain BLS proof of possession for bootstrap and therefore
carries an explicit out-of-band BLS-key-possession trust assumption; it does not
waive the OCOMP proof required before a joining validator enters a DKG target.

A proof-free registration constructor is permitted only for internally assembled
genesis/bootstrap state. It is not exposed by the general runtime API and becomes
unavailable after bootstrap.

System commands invoked from Staking, SlashIndicator and consensus include typed
stake-state transitions, unjail, jail/force-exit, participation accounting, epoch
transition, cooldown-gated inactive cleanup and atomic reshare boundary activation. These
commands require a closed internal capability/seam; public construction of the raw
generated `ValidatorSet` facade is not itself authority.

## Persistent state and single-source invariants

The registry is a dense one-based address array with reverse address index and a
monotonic count. For every registered validator:

```text
address_to_index[a] = i > 0 <=> index_to_address[i] = a
hash(consensus_pubkey[a]) -> a
hash(ocomp_public_key[a]) -> a
status in the closed ValidatorStatus set
status == ACTIVE => has_bls_share
status == EXITING => has_bls_share
has_bls_share => status in {ACTIVE, EXITING, JAILED}
join_confirmed => status == PENDING and canonical_ocomp_registration[a] exists
JAILED => jailed_at_height is defined
status in {REGISTERED, PENDING, UNBONDING, INACTIVE} => has_bls_share == false
```

Cleanup swap-removes only cooldown-expired `INACTIVE` records with no bonded stake
or live claim, and must atomically repair both dense indexes, BLS reverse ownership
and every removable per-validator field. The immutable OCOMP registration and key
reservation remain pinned to the validator address across cleanup. Direct
re-registration is allowed only after the same cooldown, reuses the old dense
index, replaces BLS pubkey ownership, clears lifecycle readiness/P2P/counters and
does not invent stake. Cleanup cannot erase a record early to bypass cooldown.

`active_consensus_set_hash` must commit to exactly the live post-activation set.
Every `Active` validator must be in that set; a boundary that omits one rejects as a
whole rather than persisting `ACTIVE` without a share. `pending_set_change` is false
only when the committed target and live set agree. Historical snapshot `exists` is
written last and gates all reads;
snapshot pruning and finalized-participation replay guards use bounded rings whose
retention exceeds the accepted late-finalization horizon.

The consensus `CommitteeEntry`, `CommitteeSnapshot` and
`committee_set_hash_v2` formats contain no OCOMP material. A separate append-only
snapshot extension stores the snapshot epoch, consensus hash, OCOMP binding hash,
member count and each ordered member's compressed OCOMP key and key epoch. The
OCOMP binding commits to the epoch, consensus hash, ordered validator addresses,
keys and key epochs. Strict historical reads require the epoch and both hashes;
a ring collision or any mismatch is reported as missing rather than substituted
with the current snapshot.

## Lifecycle state machine

The effective Rust lifecycle is richer than the persisted ABI status. Its mapping
is closed and explicit:

| Effective state | Persisted ABI tag | Canonical meaning |
|---|---|---|
| `Absent` | no persisted tag | no dense registry entry and no validator-owned residue |
| `WaitingForStake` | `REGISTERED = 0` | registered identity, below minimum stake, no share |
| `WaitingForReadiness` | `PENDING = 1` | minimum stake, not yet confirmed, no share |
| `Joining` | `PENDING = 1` | confirmed and eligible for the next target, no share |
| `Active` | `ACTIVE = 2` | current committee member with a live share |
| `Exiting` | `EXITING = 3` | current member retaining its share until exclusion |
| `Unbonding` | `UNBONDING = 4` | outside consensus while stake/claims settle |
| `Inactive` | `INACTIVE = 5` | no bonded stake, live claim or share; retained through cooldown |
| `JailRetained` | `JAILED = 6` | jailed current member retaining its share |
| `Jail` | `JAILED = 6` | jailed, excluded and without a share |

```text
no record --register + PoP--> WaitingForStake
WaitingForStake --minimum stake--> WaitingForReadiness --confirm--> Joining
Joining --successful boundary inclusion--> Active
WaitingForReadiness/Joining --stake below minimum--> WaitingForStake

Active --voluntary/forced exit--> Exiting --boundary exclusion--> Unbonding
Active --felony--> JailRetained --boundary exclusion--> Jail
Jail --self unjail + stake + cooldown--> WaitingForReadiness
Jail --full unstake--> Unbonding

Unbonding --no bonded stake/live claim--> Inactive
Inactive --cooldown + re-register--> WaitingForStake
Inactive --cooldown + cleanup--> no record
```

`Exiting` has one canonical representation and always retains its threshold share
until a boundary atomically moves it to `Unbonding`. A felony first produces
`JailRetained`; only boundary exclusion produces `Jail`. Once excluded, a fully
unstaked `Jail` moves directly to `Unbonding` rather than passing through a
shareless `Exiting`; a partial unstake leaves it jailed.

`Jail` deliberately retains the full persisted history shape rather than a reduced
history type. Finalized-parent accounting is snapshot-based and may still add a
historical missed vote after committee exclusion. `JailRetained -> Jail` clears the
old live-committee miss slate, and `Jail -> WaitingForReadiness` clears missed blocks
and votes again before rejoin; joined/deactivated heights, slash count and proposal
history survive both transitions.

Readiness is reset on entry to `WaitingForReadiness` and after activation. The
validator must submit a chain-, genesis- and validator-identity-bound OCOMP
registration with a valid proof of possession before becoming `Joining`. This
prevents a keyless or stale joiner from entering a DKG target. Only a finalized,
validated consensus boundary creates `Active` together with its share; there is no
normal manual `REGISTERED/PENDING -> ACTIVE` transition.

Unknown persisted status bytes are corruption and must fail closed. All unlisted
state/event combinations reject without writes; terminal replay semantics must be
defined per command rather than implemented as incidental no-op branches.

## Atomicity domains and side effects

Ordinary precompile writes and EVM events are transactionally journaled. V2 boundary
activation opens its own checkpoint and atomically writes the outgoing consensus
snapshot and OCOMP extension, changes membership/share flags and active-set hash,
then writes the incoming snapshot and extension. The same commit moves every
excluded `Exiting -> Unbonding` and `JailRetained -> Jail`. Snapshot replacement,
including ring eviction, is part of that checkpoint. The `exists` marker is written
last; clear reads all loop bounds before erasing fields. A failed step rolls all
state changes back.

Metrics are diagnostic and may survive rollback. The current slashing journal and
structured logs include process wall-clock time and are outside EVM state; they are
not authoritative receipts and can describe an attempted transition later reverted.
No caller may infer protocol completion from them.

Finalized participation binds replay protection to finalized block hash, updates
miss counters, then records and rings the guard in the same journaled transaction.
Once a guard is pruned, replay safety relies on the normative late-finalization
horizon being strictly shorter than retention.

## Determinism, ordering and bounds

Registry enumeration follows current dense-index order; cleanup swap-remove changes
that order. Any consensus hash or selection must therefore impose its own canonical
ordering, as committee snapshots do. Registration is capped by configured maximum
and permissionless unstaked self-registration is separately capped at 32.

Epoch reset, reshare activation and several queries scan every registered validator.
Cooldown-gated inactive cleanup is optionally bounded, but `max_removals == 0` is
unbounded. Snapshot
retention is eight epochs and participation replay retention is 64 finalized blocks;
changing either alters state roots and is a hard-fork decision.

`config_max_validators` is written only through the ValidatorSet setter and cannot
exceed the exported consensus `MAX_VALIDATORS`. OCOMP adds no independent member
limit. OCOMP-enabled startup also proves that the configured snapshot-retention
horizon strictly exceeds the maximum supported OCOMP job lifetime.

Counters and timestamps must use checked arithmetic or a named exhaustion policy.
Saturating cooldown/deadline arithmetic must not silently convert corrupt/future
heights into permanent states.

## Replay, retry and failure classification

Duplicate registration rejects except deliberate cooldown-complete `INACTIVE`
re-registration.
Byte-identical OCOMP registration replay is accepted and re-signals the pending
change. The OCOMP public key is immutable for key epoch 1; changing the BLS identity
requires a refreshed proof bound to that identity but the same pinned OCOMP key.
Re-entry resets readiness, while cleanup preserves the address-owned OCOMP key pin.
Boundary activation is retry-safe only after rollback; committed replay needs the
same canonical input/result contract rather than relying on current state rejection.

Validation and user errors are reverts. Corrupt indexes/status/snapshots, impossible
committee artifacts and unsupported identity formats must be fatal invariant errors
when encountered in consensus execution. Diagnostics must never replace a typed
committed receipt.

## Security and compatibility

BLS key uniqueness and self-registration proof of possession defend aggregate
signature identity. OCOMP admission independently verifies a chain-, genesis- and
validator-identity-bound secp256k1 proof of possession and reserves the key to one
validator. Owner registration without BLS proof is trusted bootstrap authority and
must be removed or operationally constrained before that role becomes unsafe.
P2P bytes use an Outbe-owned versioned envelope, not Commonware's unstable codec.

Storage slots, status tags, committee-hash domains, snapshot encoding, retention,
P2P version and BLS registration DST are consensus formats. Changes require an
Update migration/activation and mixed-binary compatibility evidence.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`, `state.rs`, `hooks.rs`,
`precompile.rs`, direct consensus/Staking/SlashIndicator callers, snapshot store
tests and lifecycle tests. OCOMP readiness and activation are exercised through the
production admission and certified-boundary seams; the owner/manual activation ABI
and the direct single-validator runtime activation path do not exist.

Required structural closure is a small command/query interface with internal
capabilities for staking, slashing and consensus; typed status decoding; a pure
transition plan; module-owned checkpoint for multi-write commands; typed receipts;
and stateful reference-model tests through the real dispatch/system-command seams.

## Consequences and rejected alternatives

One registry prevents each consumer from inventing its own validator definition,
while named eligibility views preserve the distinct clocks of voting, resharing and
historical verification. Treating the raw `ACTIVE` tag as the only consensus
predicate was rejected because `Exiting` and `JailRetained` members retain shares
until boundary activation.
Immediate activation on stake was rejected because a joining node must sync and
participate in DKG first. Deleting inactive entries immediately, or before cooldown,
was rejected because unbonding and cooldown semantics require an explicit terminal
transition and re-registration must not bypass the cooldown by cleanup.

## Open questions and technical debt

- Close the effective mutation interface: public raw facade methods currently let
  in-process callers invoke lifecycle transitions, jail, counters or cleanup
  without the ABI/system authority checks and orchestration that give them meaning.
- Owner bootstrap can register a BLS key without proof of possession. Define the
  sunset/rotation policy and production evidence for the out-of-band trust step.
- Diagnostic slashing-journal writes occur before EVM commit, use wall-clock time,
  and cannot roll back. Move committed receipts on-chain or explicitly label and
  reconcile attempted versus committed records.
- Extend atomic-boundary fault injection beyond semantic-plan rejection to every
  individual snapshot/share/hash write and committed replay point.
- Keep canonical `new_active_set` ordering validation in the executor synchronized
  with the runtime's duplicate/membership checks and the incoming snapshot builder.
- Formalize the maximum late-finalization/replay horizon that justifies pruning
  participation guards at 64 and committee snapshots at eight epochs.
- Bound or cursor the O(n) epoch/reshare/query scans and the unlimited inactive
  cleanup path; test cap-1/cap/cap+1 and starvation under swap-remove.
- Add production-interface tests for every legal/illegal transition, backward and
  boundary heights, corrupt storage, rollback after each multi-write point, duplicate
  intent, terminal replay and same-key/different-artifact activation.
- Add an independent stateful reference model covering join, ready, reshare, exit,
  jail, unjail, re-registration, cleanup and membership changes, with generator
  distribution and retained seeds.
- Define whether ValidatorSet's mirrored `val_stake` is compatibility-only and add
  an invariant check against Staking so two sources cannot silently diverge.
