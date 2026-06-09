---
oip: 00021
title: Make solver collateral custodial in SolverEscrow
description: Move solver collateral into escrow custody at lock time so a slash cannot be evaded by revoking the ERC-6909 operator grant, and an expired-order refund cannot be frozen by a reverting slash.
category: contract-upgrade
status: Final
author: outbe-canon
created: 2026-05-20
license: CC0-1.0
requires: []
supersedes: []
superseded-by: []
signal:
  kind: review-finding
  source: "runs/outbe-intent-contracts/run.md Session 2026-05-20 SolverEscrow §1-§9 review at HEAD 75224b7b — F-SE-1: lockCollateral (SolverEscrow.sol:177-196) is accounting-only and never takes custody; slashCollateral (SolverEscrow.sol:205-214) seizes funds via transferFrom(solver->escrow), which requires the escrow to remain an approved ERC-6909 operator (COMPACT.setOperator(escrow,true), solver-controlled and revocable). A solver who revokes the grant after the order is CLAIMED makes _onSlashed -> slashCollateral -> transferFrom revert, which reverts the entire DestinationSettlerBase.refund() call (DestinationSettlerBase.sol:81-102) before _refundOrders dispatches the refund message."
  fingerprint: sha256:5166a990e92fe6316c58dd5a52f318ce2fbe9d042083a05a862597db46b88ed7
  head: 75224b7b94342231d78c5d8d0aadfd6e77c3f13e
drift-scope:
  - runs/outbe-intent-contracts/source/src/SolverEscrow.sol
  - runs/outbe-intent-contracts/source/src/interfaces/ISolverEscrow.sol
---

# OIP-00021 — Make solver collateral custodial in SolverEscrow

## Abstract

`SolverEscrow.lockCollateral` (line 177-196) records a lock as bookkeeping only — `locks[orderId]` plus `totalLocked[solver][id]` — and never moves the solver's ERC-6909 collateral. The collateral stays in the solver's own ERC-6909 balance. `slashCollateral` (line 205-214) seizes it at slash time via `IERC6909(COMPACT).transferFrom(solver, escrow, ...)`, which the ERC-6909 layer permits only while the escrow is an approved operator of the solver (`COMPACT.setOperator(escrow, true)` — setup step 7, solver-controlled, revocable at any time). A solver who revokes that grant after an order is `CLAIMED` makes the slash `transferFrom` revert. The slash runs inside `DestinationSettlerBase.refund()` (line 81-102) before `_refundOrders` dispatches the refund message, so the revert propagates: the user's origin-chain input tokens are never refunded, and every order batched into the same `refund()` call is blocked. This OIP moves collateral into escrow custody at lock time, so `slashCollateral` retains funds the escrow already holds and can no longer revert on a transfer.

## Mechanism

`lockCollateral` takes custody; `unlockCollateral` returns custody; `slashCollateral` reclassifies escrow-held tokens into a slashed pool with no token transfer.

| Change | Where | Effect |
|---|---|---|
| Add `mapping(uint256 id => uint256) public slashedPool` | `SolverEscrow.sol` state | Per-`id` accounting of slashed collateral, separate from live locks held in escrow |
| `lockCollateral` transfers `amount` ERC-6909 from solver to escrow | `SolverEscrow.sol:177-196` | Collateral is in escrow custody from claim time onward |
| `unlockCollateral` transfers the locked ERC-6909 back to the solver | `SolverEscrow.sol:199-201` | Successful fill returns custody to the solver |
| `slashCollateral` drops the `transferFrom`; credits `slashedPool[id]` | `SolverEscrow.sol:205-214` | Slash is pure bookkeeping on tokens the escrow already holds — cannot revert |
| `distributeReward` spends `slashedPool[id]` instead of `balanceOf(escrow, id)` | `SolverEscrow.sol:219-245` | Reward payouts draw only from slashed funds, never from a solver's live lock |
| `available` becomes `balanceOf(solver, id)`; `total` becomes `balanceOf + totalLocked` | `withdraw`, `hasMinCollateral`, `getBalance`, `getBalances` | Locked collateral has physically left the solver's balance, so it is no longer subtracted again |

`totalLocked[solver][id]` is retained but its meaning shifts: it now counts ERC-6909 the escrow custodies on the solver's behalf, not a reservation against the solver's own balance. The `_consumeLock` helper (decrement `totalLocked`, delete the lock) is unchanged and serves both unlock and slash.

After this change `slashCollateral` performs no external token call, so the slash branch of `refund()` cannot revert. A solver who revokes the operator grant can now only break their *own* `claimOrder` (the `lockCollateral` transfer fails, the order stays `UNKNOWN`, and `refund()` later takes the no-slash `UNKNOWN` path) — self-griefing with no effect on user funds.

## Triggering signal

Per `runs/outbe-intent-contracts/run.md` Session 2026-05-20 SolverEscrow §1-§9 review at HEAD `75224b7b`.

`lockCollateral` body (line 186-195) — accounting only, no transfer:
```
if (locks[orderId].amount != 0) revert LockAlreadyExists();
uint256 id = _lockId(token);
uint256 total = IERC6909(address(COMPACT)).balanceOf(solver, id);
uint256 locked = totalLocked[solver][id];
uint256 availableCollateral = total > locked ? total - locked : 0;
if (availableCollateral < amount) revert InsufficientAvailableBalance();
locks[orderId] = Lock({ solver: solver, token: token, amount: amount });
totalLocked[solver][id] += amount;
```

`slashCollateral` body (line 206-213) — seizes via a transfer that needs a live operator grant:
```
Lock memory lock = _consumeLock(orderId);
uint256 id = _lockId(lock.token);
IERC6909(address(COMPACT)).transferFrom(lock.solver, address(this), id, lock.amount);
emit CollateralSlashed(orderId, lock.solver, lock.token, lock.amount);
```

`DestinationSettlerBase.refund()` (line 94-99) runs the slash before dispatching the refund:
```
if (status == CLAIMED) {
    _onSlashed(orderId);
}
// ... loop ...
_refundOrders(_orders, orderIds);
```

`_onSlashed` (`DestinationSettler.sol:137-141`) calls `escrow.slashCollateral(_orderId)` with no `try`/`catch`. A revert in `slashCollateral` therefore reverts the whole `refund()` transaction.

## Specification

Six edits to `SolverEscrow.sol`. No interface change to `ISolverEscrow.sol` (signatures stay; `ISolverEscrow.sol` is in drift-scope only because `slashedPool` could optionally be surfaced there — this OIP keeps it a public state variable, no interface edit required).

**1. New state** — after `mapping(address solver => mapping(uint256 id => uint256)) public totalLocked;` (line 64):
```
/// @notice Per-id pool of slashed collateral available to distributeReward.
mapping(uint256 id => uint256) public slashedPool;
```

**2. `lockCollateral`** — append one line after `totalLocked[solver][id] += amount;` (line 195):
```
IERC6909(address(COMPACT)).transferFrom(solver, address(this), id, amount);
```
The pre-existing `availableCollateral < amount` check (line 192) stays — it gives the typed `InsufficientAvailableBalance` error before the transfer.

**3. `unlockCollateral`** — return custody to the solver:
```
function unlockCollateral(bytes32 orderId) external onlyAuthorizedCaller {
    Lock memory lock = _consumeLock(orderId);
    IERC6909(address(COMPACT)).transfer(lock.solver, _lockId(lock.token), lock.amount);
}
```

**4. `slashCollateral`** — drop the `transferFrom`, credit the pool:
```
function slashCollateral(bytes32 orderId) external onlyAuthorizedCaller {
    Lock memory lock = _consumeLock(orderId);
    slashedPool[_lockId(lock.token)] += lock.amount;
    emit CollateralSlashed(orderId, lock.solver, lock.token, lock.amount);
}
```

**5. `distributeReward`** — replace the balance read (line 231-232) with the pool counter:
```
if (slashedPool[id] < reward) return 0;
slashedPool[id] -= reward;
```

**6. View / withdraw available-math** — in `withdraw` (line 147-149), `hasMinCollateral` (line 258-260), `getBalance` (line 273-276), `getBalances` (line 283-287), the available figure becomes the solver's raw balance, since locked tokens have left it:
```
uint256 available = IERC6909(address(COMPACT)).balanceOf(owner, id);
uint256 locked = totalLocked[owner][id];
// getBalance / getBalances: total = available + locked
```
`withdraw`'s post-transfer guard at line 168-169 keeps working: `total` there is `balanceOf(msg.sender, id)` and `withdrawAmount <= available` still holds.

## Rationale

**Why custody at lock time rather than hardening the slash call.** The slash must succeed for the collateral mechanism to mean anything and for `refund()` to complete. As long as the seizure is a transfer out of the solver's wallet, it depends on a grant the solver can revoke. Moving the tokens while the solver is cooperative — at `claimOrder`, which the winning solver wants to succeed — removes the runtime dependency entirely. The slash then touches only escrow-held state.

**Why the failure mode inverts safely.** After this change, a revoked operator grant breaks `lockCollateral`'s transfer, so `claimOrder` reverts and the order stays `UNKNOWN`. `refund()` of an `UNKNOWN` order takes the no-slash branch and dispatches normally. The solver harms only their own auction win; user funds are never the hostage.

**Why a separate `slashedPool` counter.** Once locks are custodial, the escrow's ERC-6909 balance for an `id` holds both live locks and slashed funds. `distributeReward` must spend only the slashed portion — paying from `balanceOf(escrow, id)` would let a reward payout consume a still-live lock, leaving the escrow unable to honor a later `unlockCollateral`. An explicit counter draws a hard line between the two.

**Why `unlockCollateral` uses `transfer` not `transferFrom`.** The escrow is the owner of the locked tokens after step 2, so `COMPACT.transfer(solver, id, amount)` moves its own balance. `SolverAllocator.attest` is invoked with `operator == escrow == arbiter` and returns success, so the return transfer is never blocked.

## Alternatives Considered

**Wrap `_onSlashed` in `try`/`catch` in `DestinationSettler`.** Stops `refund()` from reverting, but the slash still silently fails — a solver escapes the penalty for free every time, gutting the collateral mechanism that backs every fill. It trades a fund-freeze for a total defeat of slashing. Rejected: not a sound standalone fix, and it edits a different contract.

**Pull collateral at slash time from a pre-authorized allowance instead of an operator grant.** ERC-6909 allowances are as revocable as operator status; the dependency is unchanged. Rejected.

**Keep accounting-only locks but have `slashCollateral` mint a debt the solver must repay before withdrawing.** The solver has no remaining incentive to repay once the order is lost; the debt is uncollectable. Rejected.

**Have the escrow hold all deposited collateral (not just locked amounts).** Simpler invariant, but it removes the solver's direct ERC-6909 ownership that the deposit/withdraw design relies on, and broadens the change well past the failing surface. Rejected — custody at lock time is the minimal change that closes F-SE-1.

## Backwards Compatibility

- **Storage layout:** `slashedPool` is appended after `totalLocked`. `SolverEscrow` is non-upgradeable (no proxy) and unborn at this HEAD — a fresh deployment, no migration.
- **Public ABI:** unchanged. All `ISolverEscrow` signatures are preserved; `slashedPool` adds one auto-generated getter.
- **`getBalance` / `getBalances` semantics:** the `total` field changes from raw `balanceOf(solver, id)` to `balanceOf + totalLocked`. Before this OIP locked tokens sat in the solver's balance so `total` already included them; after it, they do not, so the sum reproduces the same figure. Net observable value is unchanged for any solver.
- **`claimOrder` behavior:** a winning solver without a live operator grant now fails `claimOrder` (previously it succeeded and the failure surfaced only at `refund()`). The order stays `UNKNOWN` and refunds cleanly.
- **The Compact integration:** `lockCollateral` now performs an ERC-6909 `transferFrom` at claim time; the escrow must be an approved operator of the solver at that moment. This is the same grant `deposit` already requires.

## Security Considerations

| Threat | Status |
|---|---|
| Slash evaded by revoking the operator grant | Closed — slash no longer performs a token transfer |
| `refund()` frozen by a reverting slash, user input tokens stranded | Closed — `slashCollateral` cannot revert |
| Batched `refund()` DoS via one poisoned order | Closed — same root cause |
| `distributeReward` pays out a still-live lock | Closed — payouts draw from `slashedPool` only |
| `unlockCollateral` return transfer blocked by the allocator | Not opened — `operator == arbiter`, `attest` passes |
| Winning solver griefs their own `claimOrder` by revoking the grant | Opened, low impact — order stays `UNKNOWN`, refunds normally; equivalent to never bidding |
| `claimOrder` no longer auto-resets the auction when `lockCollateral` reverts | Out of scope — a follow-up may add a `try`/`catch` reset in `DestinationSettler`; F-SE-1's Material harm is closed without it |

## Drawbacks

**`claimOrder` gains an ERC-6909 transfer.** Each claim now moves collateral instead of writing two storage slots — a modest gas increase on the claim path, paid by whoever calls `claimOrder`.

**`unlockCollateral` gains a transfer.** A successful fill's collateral return is now an on-chain transfer rather than a storage decrement. Expected cost on the common (honest) path.

**`claimOrder` can fail later in the pipeline.** Previously `claimOrder` only failed its own checks; now an absent operator grant fails it inside `lockCollateral`. The auction is not auto-reset in that case (see Security Considerations) — a separate follow-up addresses the reset.

## Test plan

(Conditional in Draft.) At Final:

- Foundry `test_slash_afterOperatorRevoked_succeeds` — solver deposits, wins, order `CLAIMED`; solver calls `COMPACT.setOperator(escrow, false)`; advance past `fillDeadline`; `refund()` succeeds, `CollateralSlashed` emitted, `slashedPool` credited.
- Foundry `test_refund_notFrozenByRevokedSolver` — batch `refund()` of two `CLAIMED` orders, one solver revoked; assert both refund messages dispatch.
- Foundry `test_unlock_returnsCustodyToSolver` — lock then unlock; assert solver ERC-6909 balance restored, escrow balance net-zero.
- Foundry `test_distributeReward_doesNotConsumeLiveLock` — one live lock + one slashed lock of the same `id`; `distributeReward` pays only from the slashed amount; the live lock unlocks afterward in full.
- Foundry `test_claimOrder_revokedGrant_leavesOrderUnknown` — winner revoked; `claimOrder` reverts; order stays `UNKNOWN`; `refund()` later dispatches with no slash.
- Foundry `test_getBalance_totalUnchanged` — `getBalance.total` before and after a lock returns the same figure.

Op-checks:

1. `op-check: grep -c 'slashedPool' runs/outbe-intent-contracts/source/src/SolverEscrow.sol` — Expected: ≥`4` (declaration + slash credit + 2 distributeReward lines).
2. `op-check: grep -c 'transferFrom(lock.solver' runs/outbe-intent-contracts/source/src/SolverEscrow.sol` — Expected: `0` (slash no longer transfers).
3. `op-check: grep -c 'transferFrom(solver, address(this)' runs/outbe-intent-contracts/source/src/SolverEscrow.sol` — Expected: ≥`1` (lockCollateral takes custody).

## Open questions

- **Auto-reset on `claimOrder` lock failure.** A `try`/`catch` around `lockCollateral` in `DestinationSettler.claimOrder` could reset the auction (mirroring the existing `hasMinCollateral` reset) when a winner's grant is absent. It edits a different contract and is not needed to close F-SE-1; deferred to a follow-up.
- **Surface `slashedPool` on `ISolverEscrow`.** The reward pool size is useful to off-chain observers; adding a `slashedPool` view to the interface is a cosmetic follow-up.
- **Reset-period interaction.** The lockTag uses `ResetPeriod.TenMinutes`. Whether an escrow-held lock can be force-released by a Compact reset before `unlockCollateral`/`slashCollateral` runs is a property of The Compact's resource-lock semantics; the §1-§9 walk did not have The Compact source in scope. Flagged for the cross-contract integration review.

## Copyright

Copyright and related rights waived via [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
