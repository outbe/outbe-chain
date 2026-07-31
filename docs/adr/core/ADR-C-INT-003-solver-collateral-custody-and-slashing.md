# ADR-C-INT-003: Solver collateral is conserved through lock, unlock, slash and reward

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `SolverEscrow`, Router/Solver allocators and The Compact seams
- **Depends on:** ADR-C-VLT-001

## Context

Solvers deposit native/ERC-20 collateral through The Compact. The authorized Router
locks collateral for claimed orders, unlocks after fill, slashes on expiry and may
distribute rewards from slashed value. This is a monetary ledger distinct from order FSM.

## Decision

Per `(solver,token)` available and locked balances plus per-order lock are authoritative.
One order owns at most one immutable lock `(solver,token,amount)`. State transitions are:

```text
Absent -> Locked -> Unlocked
                 -> Slashed -> Rewarded/Reserve
```

Deposit measures actual received value. Withdraw consumes only available balance.
Authorized Router commands consume one lock exactly once. Allocator callbacks authorize
only claims matching a planned escrow transition.

## Authoritative interfaces

Public `deposit/withdraw`; Router-only `lockCollateral`, `unlockCollateral`,
`slashCollateral`; authorized reward distribution; allocator attestation/claim seams;
owner configuration are the closed surface.

## Invariants

- External Compact/token/native custody equals sum of available, locked and explicit
  slashed reserve liabilities.
- A lock cannot exceed solver available collateral or be consumed twice.
- Unlock returns exactly the lock; slash transfers it exactly once to stated reserve.
- Reward distribution cannot spend active locks or another token's balance.
- Collateral rate calculation is checked and versioned.

## Atomicity, replay and failure

Custody effect and internal accounting are one transaction. Lock consumption marks
terminal state before external calls under reentrancy guard and rolls back on failure.
Duplicate terminal commands are explicit non-effects/errors.

## Determinism and bounds

Basis points, multiplication and narrowing are checked. Token set and batch queries are
bounded. Non-standard token behavior is rejected or measured by balance delta.

## Compatibility, trust and activation

Router, Compact, allocator, lock tag, collateral basis points and reward rule form one
governed profile. Reconfiguration is forbidden with active locks unless migrated.

## Production-interface verification evidence

Inspected deposit/withdraw/lock/unlock/slash/reward and allocator interfaces. Tests cover
principal flows, but no stateful conservation suite currently spans real Compact
callbacks, adversarial tokens and configuration changes.

## Consequences

Order settlement can consume typed collateral receipts rather than infer custody from
transfers. Monetary recovery remains localized.

## Rejected alternatives

- Router-owned raw collateral balances are rejected.
- Slash/reward without per-order terminal state is rejected.
- Trusting requested ERC-20 amount as received is rejected.

## Open questions and technical debt

- **Critical:** add a stateful invariant equating actual custody with available + locked
  + reserve across deposits, withdrawals, locks, unlocks, slashes and rewards.
- Audit native/ERC-20 reentrancy, fee-on-transfer and false/no-return token behavior.
- Prove allocator callbacks cannot authorize arbitrary Compact claims or bypass escrow.
- Forbid/change-manage Router and collateral-bps updates while locks exist.
- Define slashed reserve ownership, reward cap, rounding dust and recovery if reward
  transfer fails.
