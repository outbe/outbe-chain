# outbe-intex

Solidity cross-chain protocol that mints "Intex" option NFTs against escrowed bidder funds on BNB and settles them on the Outbe chain via LayerZero. The terms below are the project-specific concepts the contracts and reviews keep referring back to.

## Language

### Auction & series

**Series**:
One auction's worth of options on a single underlying, identified by `uint32 seriesId`. Created by `IntexNFT1155.createSeries`.
_Avoid_: auction (use **Auction** when meaning the bidding phase, **Series** when meaning the lifecycle entity).

**Auction**:
The bidding phase that produces a **Series**. Lives on `IntexAuction`. Funds bid into an auction are held by **The Compact** through **EscrowAdapter**.

**IntexState**:
Series lifecycle enum on the Issued token id's `seriesData[iTok]`. Values: `Issued → Qualified → Called`. There is **no** `Settled` series state — "Settled" is a per-token-id classifier (**IntexStatus**), not a series state.

**IntexStatus**:
Per-token-id classifier on `seriesData[*].status` distinguishing the **Issued token id** from the **Settled token id**. Values: `Issued`, `Settled`.

**Issued token id**:
`uint256(seriesId)` — the transferable ERC1155 id minted to auction winners.

**Settled token id**:
`keccak256("SETTLED", seriesId)` cast to `uint256` — the soulbound ERC1155 id minted by `settle` when the option pays out.

**Issuance / `issuedIntexCount`**:
The cap of how many Issued ERC1155 tokens a series may have minted, equal to the auction-cleared count. Not yet stored on-chain at the time of writing; QC-1170 introduces it as a `uint32` field on `SeriesData`.

**Settle window**:
The time between `markCalled` and `calledAt + intexCallPeriod` during which `IntexSettlement` may call `intex.settle`.

**Clearing price**:
The per-unit price at which an **Auction** clears, computed on Outbe by `Desis` and relayed to BNB. It **floors at `minIntexBidPrice`** — `Desis` initialises it to `minIntexBidPrice`, so a cleared auction never reports a price below the floor and `clearingPrice == 0` is always a malformed/corrupt result.
_Invariant_: `clearingPrice >= minIntexBidPrice` (not "`clearingPrice == 0` is rejected" — that is the weaker consequence).

**No-sale auction**:
An **Auction** that clears with zero winners (`wonBidsCount == 0`, `issuedIntexCount == 0`) because no bid met the floors. Its **Clearing price** is still non-zero (it equals `minIntexBidPrice`). _Avoid_: equating "no winners" with "zero clearing price" — the two are independent; a no-sale has zero winners **and** a non-zero (floor) clearing price.

### Escrow & locks

**The Compact**:
Vendored protocol (`contracts/vendor/the-compact/`) that escrows bidder funds as ERC6909 resource locks. `EscrowAdapter` acts as both SPONSOR and ALLOCATOR.

**EscrowAdapter**:
The BNB-side contract that wraps bidder ERC20 deposits into resource locks on **The Compact** and unwinds them on auction finalization or emergency refund.

**lockId**:
The ERC6909 id under which **The Compact** tracks **EscrowAdapter**'s aggregated lock. `IERC6909(address(compact)).balanceOf(address(this), lockId)` is the **canonical, authoritative amount locked** — load-bearing for QC-1192's Option D (see [docs/adr/0001-escrow-cache-elimination.md](docs/adr/0001-escrow-cache-elimination.md)).

**BidLock**:
Per-bidder, per-series record on `EscrowAdapter` capturing one bidder's contribution to a series. Has a `LockStatus` (`None`, `Locked`, `Finalized`) and a `lockedAmount`.

### Cross-chain & roles

**Relayer / RELAYER_ROLE**:
The cross-chain bridge contract (`TargetMessenger` on BNB) permitted to call `createSeries`, `mint`, `mintBatch`, `markCalled` on `IntexNFT1155`.

**Promis**:
Outbe-side facade (`contracts/outbe/MockPromis.sol` today, real precompile later). Calls `intex.burnSettled` when a holder mines their Promis.

## Relationships

- An **Auction** produces exactly one **Series**.
- A **Series** has exactly one **Issued token id** and exactly one **Settled token id**.
- A **Series** is gated by an **IntexState** machine; each of its token ids carries an **IntexStatus**.
- A **BidLock** belongs to exactly one **Series** and one bidder; the sum of all **Locked** BidLocks for a series equals `auctionEscrowState[seriesId].totalLocked`.
- **EscrowAdapter** holds one `lockId` on **The Compact**; the ERC6909 balance under that `lockId` equals the sum of `auctionEscrowState[*].totalLocked` across all live series.
- The **Relayer** mediates `IntexAuction` (BNB) → `IntexNFT1155.mint` (Outbe) cross-chain flow.
- **Promis** is the downstream consumer of **Settled token ids**.

## Flagged ambiguities

- **"Settled"** was used to mean both an IntexState value and an IntexStatus value — resolved: there is no `Settled` IntexState; "Settled" is exclusively an **IntexStatus** classifier on the Settled token id. Any reference to a "Settled series state" is incorrect.
- **"totalCompactBalance"** (former `EscrowAdapter` slot) was used as a synonym for the ERC6909 balance under `lockId` — resolved: the slot was removed (ADR-0001); the ERC6909 balance is the only source of truth.
- **"`clearingPrice == 0 ⇔ winners.length == 0`"** (proposed in QC-1156/B5.3) was used as the auction-clearing truthiness invariant — resolved: it is **incorrect** for this system. `Desis` floors **Clearing price** at `minIntexBidPrice`, so a **No-sale auction** has zero winners but a non-zero clearing price. The real invariant is `clearingPrice >= minIntexBidPrice`; `clearingPrice == 0` is unconditionally malformed and is already rejected.

## Example dialogue

> **Reviewer:** "What happens to the **Series** when its option pays out? Does the **IntexState** become `Settled`?"
> **Maintainer:** "No — `IntexState` only goes `Issued → Qualified → Called`. When the option pays out, the **Settled token id** is minted and its **IntexStatus** is `Settled`. The original Issued token id keeps its own status; the series state machine is independent."
>
> **Reviewer:** "So how do I know how much **The Compact** is holding for a given series?"
> **Maintainer:** "Per-series, read `auctionEscrowState[seriesId].totalLocked` on **EscrowAdapter**. Globally, read `IERC6909(compact).balanceOf(escrow, lockId)` — that's the authoritative number. Don't trust any cached aggregate on **EscrowAdapter**; we removed the one we had."
