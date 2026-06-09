---
oip: 00029
title: Anchor the Auction commit-reveal window to the order fill deadline
description: commit self-starts a fixed-duration window, so an early commit shrinks it; anchor the window to the order fillDeadline instead.
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
  source: "runs/outbe-intent-contracts/run.md Session 2026-05-20 Auction review at HEAD 75224b7b — F-AUC-2: Auction.commit (Auction.sol:74-86) self-starts the auction clock — the first commit for any orderId sets _auctionStartedAt[orderId] = block.timestamp (Auction.sol:76-78) and _phase derives a fixed-duration commit window from it (Auction.sol:216-218). commit is permissionless and needs no collateral, and orderId is deterministic from the order, so a griefer can commit early — ahead of honest solvers — and the commit window expires before they can quote. The destination auction has no router trigger: open() emits no cross-chain message and LayerZeroRouter._lzReceive handles only settle/refund, so the window cannot be started by the router and must be made un-stealable instead."
  fingerprint: sha256:cbb3e5dfd462f98106a497c72c086a1d6faf17ef45afe6a7f4bea154c214b10f
  head: 75224b7b94342231d78c5d8d0aadfd6e77c3f13e
drift-scope:
  - runs/outbe-intent-contracts/source/src/Auction.sol
  - runs/outbe-intent-contracts/source/src/interfaces/IAuction.sol
---

# OIP-00029 — Anchor the Auction commit-reveal window to the order fill deadline

## Abstract

`Auction.commit` (`Auction.sol:74-86`) self-starts the auction clock: the first `commit` for an `orderId` sets `_auctionStartedAt[orderId] = block.timestamp` (`Auction.sol:76-78`), and `_phase` (`Auction.sol:215-221`) derives a fixed-duration commit window — `[start, start + commitPeriod)` — from that timestamp. `commit` is permissionless, costs no collateral, and `orderId` is deterministic from the order, so anyone watching the origin chain's pending `open()` can compute `orderId` and `commit` early on the destination chain. That early `commit` fixes `start`, and the `commitPeriod`-long window can elapse before honest solvers quote — every targeted order then ends with no usable quote and is forced to refund, a costless protocol-wide liveness denial. There is no router call site that could instead start the window: `open()` emits only the `Open` event (`OriginSettlerBase.sol:78`) and dispatches no cross-chain message, and `LayerZeroRouter._lzReceive` (`LayerZeroRouter.sol:142-168`) handles only settle and refund payloads, so the destination auction has no on-chain order-open trigger. This OIP makes the window edges a pure function of the order's `fillDeadline` — a value bound into `orderId` — so `commit` still self-bootstraps but an early `commit` can no longer move the window.

## Mechanism

The auction window stops being "first commit, plus a duration" and becomes a fixed sub-interval of the order's own timeline, ending a configurable `claimFillWindow` before the order's `fillDeadline`:

- `commit` takes the order's `originData` instead of a bare `orderId`. The Auction decodes it, derives `orderId = OrderEncoder.id(decoded)` and reads `decoded.fillDeadline`. Because `fillDeadline` is one of the fields hashed into `orderId`, a griefer cannot supply a different `fillDeadline` without producing a different `orderId` — the deadline is bound to the order.
- The Auction stores the order's `fillDeadline` (in a `_auctionDeadline` mapping that replaces `_auctionStartedAt`), set by the first `commit`.
- `_phase` computes, from that stored deadline: `end = fillDeadline - claimFillWindow`, `revealStart = end - revealPeriod`, `commitStart = revealStart - commitPeriod`. The commit phase is `[commitStart, revealStart)`, reveal is `[revealStart, end)`, and the auction is `ENDED` at `end` — leaving `claimFillWindow` for `claimOrder` and `fill` before `fillDeadline`.

Every solver computes the identical window from the same `fillDeadline`; an early `commit` sets `_auctionDeadline` to exactly the value an honest `commit` would, so it cannot shrink anyone's window. The destination auction still bootstraps on the first `commit` with no router involvement — same-chain and cross-chain orders behave identically.

## Triggering signal

Per `runs/outbe-intent-contracts/run.md` Session 2026-05-20 Auction review at HEAD `75224b7b` — F-AUC-2.

`commit` self-starts the clock on the first call for an `orderId` (`Auction.sol:74-78`):
```
function commit(bytes32 orderId, bytes32 commitHash) external {
    // First commit starts the auction
    if (_auctionStartedAt[orderId] == 0) {
        _auctionStartedAt[orderId] = block.timestamp;
    }
```

`_phase` derives a fixed-duration window from that self-set `start` (`Auction.sol:216-218`):
```
uint256 start = _auctionStartedAt[orderId];
if (start == 0) return PHASE_NONE;
if (block.timestamp < start + commitPeriod) return PHASE_COMMIT;
```

`resetAuction` likewise re-bases the window on `block.timestamp` (`Auction.sol:112`):
```
_auctionStartedAt[orderId] = block.timestamp;
```

## Specification

Edits to `Auction.sol`; the `commit` signature change is mirrored in `IAuction.sol`.

**1. Storage** — replace `_auctionStartedAt` (`Auction.sol:48`) with a fill-deadline store, and add the owner-settable claim/fill reservation:
```
mapping(bytes32 => uint256) internal _auctionDeadline; // orderId => order fillDeadline (0 = no auction)
uint256 public claimFillWindow = 5 minutes;            // time reserved before fillDeadline for claim + fill
```

**2. `commit`** — take `originData`, derive `orderId` and `fillDeadline`, store the deadline, keep the existing phase/dup checks (`Auction.sol:74-86`):
```
function commit(bytes calldata originData, bytes32 commitHash) external {
    OrderData memory order = OrderEncoder.decode(originData);
    bytes32 orderId = OrderEncoder.id(order);
    if (_auctionDeadline[orderId] == 0) _auctionDeadline[orderId] = order.fillDeadline;
    if (_phase(orderId) != PHASE_COMMIT) revert CommitPhaseEnded();
    if (_commits[orderId][msg.sender] != bytes32(0)) revert AlreadyCommitted();
    _commits[orderId][msg.sender] = commitHash;
    emit Committed(orderId, msg.sender);
}
```
`Auction.sol` adds `import { OrderData, OrderEncoder } from "./libs/OrderEncoder.sol";`.

**3. `_phase`** — anchor every boundary to the stored deadline (`Auction.sol:215-221`):
```
function _phase(bytes32 orderId) internal view returns (uint8) {
    uint256 deadline = _auctionDeadline[orderId];
    if (deadline == 0) return PHASE_NONE;
    uint256 minSpan = claimFillWindow + revealPeriod + commitPeriod;
    if (deadline <= block.timestamp || deadline <= minSpan) return PHASE_ENDED;
    uint256 end = deadline - claimFillWindow;
    if (block.timestamp >= end) return PHASE_ENDED;
    if (block.timestamp >= end - revealPeriod) return PHASE_REVEAL;
    if (block.timestamp >= end - revealPeriod - commitPeriod) return PHASE_COMMIT;
    return PHASE_NONE;
}
```

**4. `resetAuction`** — drop the `_auctionStartedAt = block.timestamp` re-base (`Auction.sol:112`); the window is now fixed by `fillDeadline`, so a reset re-runs in whatever time is left in the same commit phase. `resetAuction` keeps `delete _quotes[orderId]` and the event.

**5. View functions** — `getCommitDeadline`, `getRevealDeadline` (`Auction.sol:189-200`) and `auctionStartedAt` (`Auction.sol:208-210`) re-derive from `_auctionDeadline`: `commit` ends at `end - revealPeriod`, `reveal` ends at `end`, where `end = _auctionDeadline[orderId] - claimFillWindow`; the legacy `auctionStartedAt` getter returns `end - revealPeriod - commitPeriod` (the commit-phase start).

**6. `setClaimFillWindow`** — an `onlyOwner` setter mirroring `setCommitPeriod` (`Auction.sol:120-125`), emitting an update event with old and new values.

## Rationale

**Why anchor to `fillDeadline` rather than gate the start.** A `startAuction` entry — router-gated or permissionless — does not help: whoever may call it can call it early, and the fixed-duration window still elapses. The griefing is killed only by removing "who acts first" from the window math. `fillDeadline` is the one order-bound, griefer-immutable time value available, and the auction must finish before it anyway.

**Why `commit` must carry `originData`, not a bare `fillDeadline`.** If `commit` accepted `(orderId, fillDeadline)`, a griefer's first `commit` could store a false `fillDeadline` for a real `orderId`. Passing `originData` and deriving `orderId = OrderEncoder.id(decoded)` binds the two: a false `fillDeadline` yields a different `orderId`, so it cannot poison the real order's window.

**Why `claimFillWindow` is configurable.** `claimOrder` and `fill` both revert once `block.timestamp > orderData.fillDeadline` (`DestinationSettler.sol:33`, `:71`); the auction must end early enough to leave them room. `claimFillWindow` is that reservation, owner-tunable like `commitPeriod` and `revealPeriod`.

**Why this is a clear-Material fix.** A costless, repeatable denial of order fulfillment across every targeted order is a liveness loss under `canon/rubric.md` §"Tier 1 — Admissibility gates"; it is one concern — the window anchor — confined to `Auction` and its interface, so it ships as an atomic OIP.

## Alternatives Considered

**Router-gated `startAuction` called from `open()`.** `open()` runs on the origin chain and emits no cross-chain message (`OriginSettlerBase.sol:78`); the auction runs on the destination chain, which `LayerZeroRouter._lzReceive` (`LayerZeroRouter.sol:142-168`) decodes only into settle/refund handling — it never reaches an order-open payload. No router call site can start a cross-chain order's destination auction. Rejected — structurally impossible.

**A permissionless `startAuction(orderId)` separate from `commit`.** Moves the self-start out of `commit` but anyone can still call it early; the fixed-duration window is unchanged. Rejected — does not touch the griefing.

**Lengthen `commitPeriod`.** A longer window raises the cost of a successful grief but does not remove it — an early enough `commit` still expires any fixed duration. Rejected.

**Keep `_auctionStartedAt`; cap the start at a `fillDeadline`-derived latest value.** Equivalent end state to anchoring, but carries a redundant timestamp and a `min()` the reader must reason about. Rejected — a single deadline-anchored computation is clearer.

## Backwards Compatibility

- **`IAuction.commit` signature** changes from `commit(bytes32 orderId, bytes32 commitHash)` to `commit(bytes calldata originData, bytes32 commitHash)` — a breaking ABI change. Every solver's commit path must pass the order's `originData` (the same bytes already passed to `claimOrder`/`fill`). `reveal`, `getWinner`, `isAuctionEnded`, and `resetAuction` keep their signatures.
- **Storage layout:** `_auctionStartedAt` is replaced by `_auctionDeadline` (same slot kind); `claimFillWindow` is appended. `Auction` is non-upgradeable and unborn at HEAD `75224b7b` — a fresh deployment, no migration.
- **`auctionStartedAt` getter** is retained but now returns the derived commit-phase start rather than a recorded first-commit time; an off-chain reader treating it as "when the first commit landed" must move to the commit-phase semantics.
- **Interaction with OIP-00024:** OIP-00024 T-04 re-keys `_commits` by an auction epoch and clears them on `resetAuction`; this OIP changes `commit`'s signature and `resetAuction`'s body. Both edit `commit` and `resetAuction` — apply OIP-00024's epoch keying to the `commit` body shown here, and keep both this OIP's deadline store and OIP-00024's epoch counter in `resetAuction`. OIP-00024 T-03 (`reveal` `uint128` guard) is independent.

## Security Considerations

| Threat | Status |
|---|---|
| Early `commit` shrinks the commit window; honest solvers cannot quote | Closed — the window is a pure function of `fillDeadline`, identical for every caller |
| Griefer supplies a false `fillDeadline` to mis-set the window | Closed — `commit` derives `orderId` from `originData`; a false deadline yields a different `orderId` |
| Auction runs past `fillDeadline`, leaving no time for `claimOrder`/`fill` | Closed — the auction ends `claimFillWindow` before `fillDeadline` |
| Order with `fillDeadline` too soon for a full window | Auction never enters `PHASE_COMMIT`; the order is uncommittable and expires to refund — no funds at risk |
| Re-auction (`resetAuction`) after a collateral-short winner | Re-runs only if commit-phase time remains; otherwise the order refunds — a bounded liveness cost, not a loss |

## Drawbacks

**Breaking `commit` ABI.** Solver tooling must switch to passing `originData`. This is unavoidable — the Auction needs the order data to bind the window to `fillDeadline`.

**Re-auctions lose their own window.** Because the window is fixed to `fillDeadline`, `resetAuction` can re-run only within the time left in the original commit phase; an order whose winner is disqualified late simply refunds. The regression surface is `Auction.sol` — `commit`, `_phase`, `resetAuction`, the three deadline view functions — and `IAuction.commit`; `reveal`, `getWinner`, and the quote-selection logic are untouched.

**Orders need a longer minimum lead time.** `fillDeadline` must now exceed `block.timestamp + commitPeriod + revealPeriod + claimFillWindow` for the order to be auctionable; short-deadline orders silently route to refund. `claimFillWindow` is owner-tunable to manage this.

## Test plan

(Conditional in Draft.) At Final:

- Foundry `test_commit_earlyGrief_doesNotShrinkWindow` — a griefer `commit`s long before honest solvers; assert an honest `commit` still succeeds at `revealStart - 1` and `_phase` is `PHASE_COMMIT` for both.
- Foundry `test_commit_falseFillDeadline_yieldsDifferentOrderId` — `commit` with `originData` whose `fillDeadline` is altered; assert the derived `orderId` differs and the real order's `_auctionDeadline` is untouched.
- Foundry `test_phase_boundariesAnchoredToFillDeadline` — assert `commit`/`reveal`/`ended` transitions occur at `fillDeadline - claimFillWindow` minus `revealPeriod`/`commitPeriod`.
- Foundry `test_auction_endsBeforeFillDeadline_leavesClaimFillWindow` — assert `isAuctionEnded` is true at `fillDeadline - claimFillWindow` and `claimOrder` still has time before `fillDeadline`.
- Foundry `test_commit_shortDeadlineOrder_revertsCommitPhaseEnded` — order with `fillDeadline` inside `commitPeriod + revealPeriod + claimFillWindow`; assert `commit` reverts `CommitPhaseEnded`.

Op-checks:

1. `op-check: grep -c '_auctionStartedAt' runs/outbe-intent-contracts/source/src/Auction.sol` — Expected: `0` (fully replaced).
2. `op-check: grep -c '_auctionDeadline' runs/outbe-intent-contracts/source/src/Auction.sol` — Expected: ≥`4`.
3. `op-check: grep -c 'commit(bytes calldata originData' runs/outbe-intent-contracts/source/src/Auction.sol` — Expected: `1`.

## Open questions

- **`claimFillWindow` default.** `5 minutes` is a placeholder; the right reservation depends on destination-chain block time and expected `claimOrder`/`fill` latency. The owner tunes it post-deploy; a deploy-review check that it is set sanely against the chain is recommended.
- **Re-auction viability.** With anchored windows a late `resetAuction` may find no commit-phase time left. Whether the protocol should instead reserve a dedicated re-auction sub-window before `fillDeadline` is a follow-on design question, deferred until the re-auction path is exercised in tests.

## Copyright

Copyright and related rights waived via [CC0](https://creativecommons.org/publicdomain/zero/1.0/).
