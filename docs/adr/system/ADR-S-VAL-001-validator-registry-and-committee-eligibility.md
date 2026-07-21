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

- `ACTIVE` validators are the present governance/reward population;
- current consensus participants are `ACTIVE | EXITING | JAILED` with a live share;
- the next reshare target is `ACTIVE` plus confirmed `PENDING` joiners;
- non-voting secondary admission is `REGISTERED | PENDING | JAILED`; and
- historical finalized-parent verification uses the committee snapshot keyed by
  canonical epoch and committee hash, not the current registry view.

## Authoritative mutation interface

User-facing ABI commands are registration with BLS proof of possession,
owner/self P2P-address update, owner/self voluntary deactivation, self readiness
confirmation and owner-only manual reshare activation. Owner registration may omit
on-chain proof of possession for bootstrap and therefore carries an explicit
out-of-band key-possession trust assumption.

System commands invoked from Staking, SlashIndicator and consensus include
`mark_pending`, `unjail_to_pending`, jail/force-exit, participation accounting,
epoch transition, inactive cleanup and atomic reshare boundary activation. These
commands require a closed internal capability/seam; public construction of the raw
generated `ValidatorSet` facade is not itself authority.

## Persistent state and single-source invariants

The registry is a dense one-based address array with reverse address index and a
monotonic count. For every registered validator:

```text
address_to_index[a] = i > 0 <=> index_to_address[i] = a
hash(consensus_pubkey[a]) -> a
status in the closed ValidatorStatus set
has_bls_share => status in {ACTIVE, EXITING, JAILED}
join_confirmed => status == PENDING
JAILED => jailed_at_height is defined
INACTIVE => has_bls_share == false
```

Cleanup swap-removes only `INACTIVE` records and must atomically repair both dense
indexes, reverse pubkey ownership and every per-validator field. Re-registration
reuses the old dense index after cooldown, replaces pubkey ownership, clears
lifecycle/readiness/P2P/counter state, and does not invent stake.

`active_consensus_set_hash` must commit to exactly the live post-activation set.
`pending_set_change` is false only when every `ACTIVE` validator has a share in the
committed set. Historical snapshot `exists` is written last and gates all reads;
snapshot pruning and finalized-participation replay guards use bounded rings whose
retention exceeds the accepted late-finalization horizon.

## Lifecycle state machine

```text
new -> REGISTERED --minimum stake--> PENDING --ready + reshare--> ACTIVE
ACTIVE --voluntary/forced exit--> EXITING --reshare exclusion--> UNBONDING
UNBONDING --Staking completion--> INACTIVE --re-register cooldown--> REGISTERED

ACTIVE --felony--> JAILED --self unjail + stake + cooldown--> PENDING
                        \--full unstake--> EXITING
```

An `EXITING` or newly `JAILED` validator remains accountable while its threshold
share is live. `PENDING` readiness is reset on entry and after activation; this
prevents a stale joiner from entering a DKG target before catching up. Manual
`REGISTERED/PENDING -> ACTIVE` exists in the raw runtime but is not the normal PoS
path and must not bypass share establishment.

Unknown persisted status bytes are corruption and must fail closed. All unlisted
state/event combinations reject without writes; terminal replay semantics must be
defined per command rather than implemented as incidental no-op branches.

## Atomicity domains and side effects

Ordinary precompile writes and EVM events are transactionally journaled. V2 boundary
activation opens its own checkpoint and atomically writes the outgoing snapshot,
changes membership/share flags and active-set hash, then writes the incoming
snapshot. A failed step rolls all three back.

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
Inactive cleanup is optionally bounded, but `max_removals == 0` is unbounded. Snapshot
retention is eight epochs and participation replay retention is 64 finalized blocks;
changing either alters state roots and is a hard-fork decision.

Counters and timestamps must use checked arithmetic or a named exhaustion policy.
Saturating cooldown/deadline arithmetic must not silently convert corrupt/future
heights into permanent states.

## Replay, retry and failure classification

Duplicate registration rejects except deliberate `INACTIVE` re-registration.
Readiness confirmation is currently effect-idempotent but re-signals the change.
Boundary activation is retry-safe only after rollback; committed replay needs the
same canonical input/result contract rather than relying on current state rejection.

Validation and user errors are reverts. Corrupt indexes/status/snapshots, impossible
committee artifacts and unsupported identity formats must be fatal invariant errors
when encountered in consensus execution. Diagnostics must never replace a typed
committed receipt.

## Security and compatibility

BLS key uniqueness and self-registration proof of possession defend aggregate
signature identity. Owner registration without proof is trusted bootstrap authority
and must be removed or operationally constrained before that role becomes unsafe.
P2P bytes use an Outbe-owned versioned envelope, not Commonware's unstable codec.

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
historical verification. Treating `ACTIVE` as the only consensus predicate was
rejected because exiting/jailed members retain shares until boundary activation.
Immediate activation on stake was rejected because a joining node must sync and
participate in DKG first. Deleting inactive entries immediately was rejected because
unbonding and cooldown semantics require an explicit terminal transition.

## Open questions and technical debt

- Close the effective mutation interface: public raw facade methods currently let
  in-process callers invoke `mark_pending`, activation, jail, counters or cleanup
  without the ABI/system authority checks and orchestration that give them meaning.
- Persisted statuses are raw `u8`; introduce a closed decoder and fail fatally on
  corrupt tags instead of letting unknown values flow through queries and matches.
- `punish_validator` increments `slash_count` even for repeated JAILED/EXITING and
  UNBONDING/INACTIVE calls, and repeats events/signals. Define intent-bound replay
  semantics and ensure one offense produces exactly one punishment receipt.
- `activate_validator` can move `REGISTERED` directly to `ACTIVE` without setting a
  BLS share. Remove it, restrict it to genesis capability, or specify the invariant.
- Owner bootstrap can register a BLS key without proof of possession. Define the
  sunset/rotation policy and production evidence for the out-of-band trust step.
- Diagnostic slashing-journal writes occur before EVM commit, use wall-clock time,
  and cannot roll back. Move committed receipts on-chain or explicitly label and
  reconcile attempted versus committed records.
- Replace unchecked `count + 1`, epoch/counter increments, slash/miss/proposal
  counters and participation-ring sequence with explicit exhaustion behavior.
- Prove `pending_set_change`, active-set hash, share flags and snapshot identity are
  mutually closed after every boundary failure and replay.
- Validate duplicate addresses and canonical ordering in `new_active_set` both
  inside the owning command and during upstream artifact validation.
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
