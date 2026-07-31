# ADR-S-STK-001: Staking owns bonded stake and unbonding claims

- **Status:** Proposed; current implementation profiled; not an architecture-conformance verdict
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/staking`; self-stake, bonded totals,
  unbonding claims, withdrawal delay and native-balance conservation
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-VAL-001
- **Related:** ADR-S-RWD-001 Rewards, ADR-S-SLS-001 SlashIndicator
- **Supersedes:** The Staking-local portions of the deleted pre-space validator aggregate

## Context

Stake is both native value held by the Staking address and eligibility input for
ValidatorSet. Those facts must change atomically, but they have different owners:
Staking owns money and claims; ValidatorSet owns membership status and committee
eligibility. Delegation is not implemented, so the current ledger is self-stake
only despite ABI parameters that name a validator.

## Decision

Staking is the sole source of truth for bonded amounts, total bonded supply,
per-validator unbonding claims and withdrawal maturity. Native value received by
the payable precompile is escrowed at `STAKING_ADDRESS`. ValidatorSet may mirror a
stake amount for compatibility, but it must not independently decide or mutate the
economic balance.

Only self-stake is accepted: transaction sender, withdrawal beneficiary and
validator address are the same identity. Reaching minimum stake asks ValidatorSet
to move a registered validator `REGISTERED -> PENDING`; falling below it asks for
the appropriate exit/demotion transition. The DKG boundary, not Staking, decides
when a pending joiner becomes an active committee member.

## Authoritative commands and authority

The production ABI exposes:

- payable `stake(validator, amount)`, requiring sender = validator and
  `msg.value == amount`;
- self `unstake(amount)`, which creates a time-locked claim;
- self `claimUnbonded()`, which consumes all matured claims and transfers value;
- self `unjailValidator()`, requiring bonded stake at least the configured minimum;
  and
- read-only per-validator and total bonded queries.

Slash and per-block unbonding processing are internal system commands. They must be
reachable only through a capability held by SlashIndicator and the block lifecycle.
Public construction of the generated raw `Staking` facade is not sufficient
authority to burn somebody else's stake or advance lifecycle state.

## Persistent state and conservation invariants

State contains configured minimum stake, unbonding and slashed-withdrawal delays,
optional maximum stake percentage, bonded amount per validator, `total_staked`, an
append-only indexed claim arena, and a per-validator singly linked list of live
claim indexes (`index + 1`, with zero as none).

For every committed state:

```text
total_staked = sum(stake_amount[v])
live claim belongs to exactly one validator linked list
sum(bonded) + sum(live unbonding) = native balance(STAKING_ADDRESS)
ValidatorSet.val_stake[v] = stake_amount[v]       (compatibility mirror)
INACTIVE validator => bonded == 0 and no live claim
unbonding_end[v] = 0 or a conservative latest-live-claim maturity
```

A zeroed claim arena entry must not remain reachable from a validator head. Tail
trimming may reduce `unbonding_count` only across zeroed suffix entries; stable
indexes referenced by linked lists must never be swap-removed.

## Stake and validator-lifecycle transitions

```text
self stake + new bonded >= minimum:
  REGISTERED -> PENDING

self unstake/slash + remaining bonded < minimum:
  PENDING -> REGISTERED
  ACTIVE  -> EXITING
  JAILED  -> EXITING (unstake path)

ValidatorSet reshare exclusion:
  EXITING -> UNBONDING

per-block Staking processing:
  move residual bonded to delayed claim
  UNBONDING + bonded=0 + no live claims -> INACTIVE

self unjail + bonded >= minimum + cooldown:
  JAILED -> PENDING
```

Creating an unbonding claim is distinct from ValidatorSet's `UNBONDING` status: an
active validator may withdraw only its excess above minimum while remaining active.
The terminology must not imply those two states are identical.

## Atomicity and side-effect ledger

The payable call-value transfer is performed by the EVM before Staking logic and is
inside the same transaction journal. Stake accounting, ValidatorSet mirror/status
effects and success/revert therefore share the outer EVM checkpoint.

Unstake atomically decreases bonded state, updates total/mirror/status, allocates a
claim and records maturity. Claim first plans/zeroes matured linked-list entries,
rebuilds the pending list, transfers the accumulated native value, then finalizes
ValidatorSet `INACTIVE` when no economic state remains; any propagated error must
roll the entire transaction back.

Slash proportionally reduces bonded and every live unbonding claim, burns the exact
total from Staking's native balance, updates the mirror and may demote membership.
The returned `U256` is the only typed amount receipt; the caller owns any offense or
evidence-reward semantics under ADR-S-SLS-001.

## Determinism, ordering and bounded work

Per-validator claim traversal follows deterministic prepend-list order but is
currently unbounded by the number of that validator's historical live entries.
Per-block processing scans all ValidatorSet records, then performs at most 64 tail
trims. The scan itself has no cursor or cap. Arena holes away from the tail are
retained indefinitely until all later entries are cleared.

Maximum-stake percentage is checked against the post-deposit total using integer
cross multiplication. It is bypassed for values zero or at least 100. The first
deposit has a special path because the prior total is zero; both paths must preserve
the same postconditions.

## Replay, retry and failure classification

Stake and unstake are intentionally repeatable economic commands, each distinguished
by transaction intent and amount. `claimUnbonded` is effect-idempotent after all
mature claims are consumed. Slash is not replay-safe by itself: applying the same
percentage twice burns twice, so ADR-S-SLS-001 must bind it atomically to a durable unique
offense receipt.

User validation and insufficient balance/value are reverts with semantic pre-state
restored. Arithmetic/accounting underflow, linked-list cycles/out-of-range links,
mirror divergence and native-balance insolvency are invariant failures and must fail
closed rather than partially process or loop indefinitely.

## Security and compatibility

Self-stake deliberately rejects delegation because no delegator ownership or
withdrawal-right ledger exists. Configuration values are genesis/upgrade state;
their mutation authority is not exposed by the inspected ABI. Block timestamp is
the maturity clock and inherits consensus timestamp constraints.

Storage layout, native-value semantics, linked-list encoding, percentage rounding,
minimum stake and delay activation affect funds and consensus membership. Changes
require migration and mixed-version execution evidence.

## Production-interface and architectural evidence

Inspected evidence includes `contract.rs`, `logic.rs`, `precompile.rs`, `hooks.rs`,
module tests, ValidatorSet transitions and SlashIndicator call sites. Tests cover
many lifecycle and conservation cases but frequently construct the raw facade and
write schema directly, so they do not establish production-interface closure.

The current module has not passed architecture review. Closure requires a small command/query
interface, unforgeable internal slash/lifecycle capabilities, typed claim ids and
receipts, validated linked-list traversal, module-owned multi-effect checkpoint or
an explicit required transaction capability, and independent state-model tests
through ABI plus real system seams.

## Consequences and rejected alternatives

Self-stake keeps ownership and withdrawal unambiguous. Third-party delegation was
rejected until shares, reward ownership, slashing allocation and delegator exit are
modeled. Paying matured claims automatically in begin-block was rejected: explicit
claim keeps beneficiary interaction and transfer failure in a user transaction.
Swap-removing claim entries was rejected because it invalidates linked-list indexes.

## Open questions and technical debt

- Close the raw mutation seam: any in-process caller can currently construct
  `Staking` and invoke `slash_stake`, `process_unbonding` or cross-module lifecycle
  writes without SlashIndicator/block-lifecycle authority.
- Staking directly writes ValidatorSet raw fields and reproduces its FSM. Replace
  these writes with typed ValidatorSet commands/receipts so membership invariants
  have one owner.
- Define whether `val_stake` remains a compatibility mirror; validate it on reads or
  remove it to prevent silent divergence from `stake_amount`.
- `enqueue_unbonding` uses unchecked `idx + 1`; claim sums, bonded additions and
  percentage products also need explicit overflow/exhaustion behavior.
- A corrupt/cyclic per-validator linked list can cause unbounded or infinite work.
  Validate ownership, index bounds, uniqueness and acyclicity with a deterministic
  cap and fatal corruption error.
- `claimUnbonded` is unbounded in live entries and `process_unbonding` scans every
  validator before its 64-operation tail cap. Introduce fair durable cursors and
  prove cap-1/cap/cap+1, no skip/repeat and no starvation.
- `slash_stake(percent = 0)` currently extends withdrawal maturity even though it
  burns nothing. Decide whether zero-percent slash rejects or is effect-free.
- Slash percentage truncation can produce zero burn for small entries while still
  delaying them. Specify rounding and minimum-slash economics.
- Bind every slash to an offense id and atomic receipt so retry cannot slash the
  same evidence twice or apply the same key to different intent.
- Define `unbonding_end` for multiple concurrent claims: current code overwrites it
  with the latest created/processed claim rather than deriving the maximum live
  maturity, and claim does not recompute it until terminal inactivity.
- The hook documentation says matured entries are zeroed and swap-compacted, while
  implementation only moves `UNBONDING` residual stake and tail-trims already
  claimed holes. Correct the contract and add observable hook tests.
- Define behavior for stake held by an unregistered address; current self-stake is
  accepted and withdrawable but does not affect membership.
- Prove the global native-balance conservation equation after every failure point,
  especially call-value transfer, claim transfer, slash burn and cross-module error.
- Add production-interface tests for repeated stake/unstake/claim, exact maturity
  `T-1/T/T+1`, backward timestamp, max-percent boundaries, multiple claims, corrupt
  lists, slash replay and rollback after every distinct write/effect.
- Add an independent stateful reference model covering multiple validators, queue
  holes, interleaved claims/slashes, membership thresholds and retained generator
  seeds/distribution.
