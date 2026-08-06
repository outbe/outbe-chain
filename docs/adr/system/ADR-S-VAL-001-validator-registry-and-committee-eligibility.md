# ADR-S-VAL-001: ValidatorSet owns validator identity, lifecycle and committee eligibility

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
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
registry indexes, current-share membership, pending-set-change signal, epoch
participation counters, versioned P2P addresses and bounded historical committee
snapshots. Staking owns economic stake and mirrors only the compatibility fields
needed here. Consensus owns DKG artifacts but changes membership only through the
atomic boundary-activation command.

Consumers must use named queries for the exact authority they need:

- `Active` validators are the present governance/reward population;
- current consensus participants are `Active`, `Exiting` and `JailRetained`;
- the next reshare target is `Active` plus `Joining` validators;
- non-voting secondary admission is `REGISTERED | PENDING | JAILED`; and
- historical finalized-parent verification uses the committee snapshot keyed by
  canonical epoch and committee hash, not the current registry view.

## Authoritative mutation interface

User-facing ABI commands are registration with BLS proof of possession, optional
owner/self P2P-address update, owner/self voluntary deactivation and self readiness
confirmation. No ordinary owner/manual call is an independent committee-activation
authority. If the historical `activateResharedSet` selector is retained for ABI
compatibility, it must route through the same validated consensus-boundary
capability and cannot independently mutate membership.

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
status in the closed ValidatorStatus set
status == ACTIVE => has_bls_share
status == EXITING => has_bls_share
has_bls_share => status in {ACTIVE, EXITING, JAILED}
join_confirmed => status == PENDING
JAILED => jailed_at_height is defined
status in {REGISTERED, PENDING, UNBONDING, INACTIVE} => has_bls_share == false
```

Cleanup swap-removes only cooldown-expired `INACTIVE` records with no bonded stake
or live claim, and must atomically repair both dense indexes, reverse pubkey
ownership and every per-validator field. Direct re-registration is allowed only
after the same cooldown, reuses the old dense index, replaces pubkey ownership,
clears lifecycle/readiness/P2P/counter state, and does not invent stake. Cleanup
cannot erase a record early to bypass re-registration cooldown.

`active_consensus_set_hash` must commit to exactly the live post-activation set.
Every `Active` validator must be in that set; a boundary that omits one rejects as a
whole rather than persisting `ACTIVE` without a share. `pending_set_change` is false
only when the committed target and live set agree. Historical snapshot `exists` is
written last and gates all reads;
snapshot pruning and finalized-participation replay guards use bounded rings whose
retention exceeds the accepted late-finalization horizon.

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

Readiness is reset on entry to `WaitingForReadiness` and after activation, so a
stale joiner cannot enter a DKG target before catching up. Only a finalized,
validated consensus boundary creates `Active` together with its share. There is no
normal manual `REGISTERED/PENDING -> ACTIVE` transition.

Unknown persisted status bytes are corruption and must fail closed. All unlisted
state/event combinations reject without writes; terminal replay semantics must be
defined per command rather than implemented as incidental no-op branches.

## Atomicity domains and side effects

Ordinary precompile writes and EVM events are transactionally journaled. V2 boundary
activation opens its own checkpoint and atomically writes the outgoing snapshot,
changes membership/share flags and active-set hash, then writes the incoming
snapshot. The same commit moves every excluded `Exiting -> Unbonding` and
`JailRetained -> Jail`. A failed step rolls all state changes back.

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

Counters and timestamps must use checked arithmetic or a named exhaustion policy.
Saturating cooldown/deadline arithmetic must not silently convert corrupt/future
heights into permanent states.

## Replay, retry and failure classification

Duplicate registration rejects except deliberate cooldown-complete `INACTIVE`
re-registration.
Readiness confirmation is currently effect-idempotent but re-signals the change.
Boundary activation is retry-safe only after rollback; committed replay needs the
same canonical input/result contract rather than relying on current state rejection.

Validation and user errors are reverts. Corrupt indexes/status/snapshots, impossible
committee artifacts and unsupported identity formats must be fatal invariant errors
when encountered in consensus execution. Diagnostics must never replace a typed
committed receipt.

## Security and compatibility

BLS key uniqueness and registration proof of possession defend aggregate signature
identity. Proof-free construction is an internal genesis/bootstrap operation only;
the general runtime ABI has no owner proof bypass. P2P bytes use an Outbe-owned
versioned envelope, not Commonware's unstable codec.

Storage slots, status tags, committee-hash domains, snapshot encoding, retention,
P2P version and BLS registration DST are consensus formats. Changes require an
Update migration/activation and mixed-binary compatibility evidence.

## Production-interface and architectural evidence

Inspected evidence includes `schema.rs`, `runtime.rs`, `state.rs`, `hooks.rs`,
`precompile.rs`, direct consensus/Staking/SlashIndicator callers, snapshot store
tests and lifecycle tests. The current module has not passed architecture review: its effective
mutation interface includes public raw-facade methods, and existing tests do not yet
prove every FSM/index/rollback/replay gate through the production interface.

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
- Prove that proof-free genesis construction is unreachable through the general ABI
  and ensure production builds never enable the test-only bootstrap feature.
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
