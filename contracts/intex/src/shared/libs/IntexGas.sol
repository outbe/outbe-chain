// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IntexGas
/// @author Outbe
/// @notice Transport-independent destination-gas budgets for intex cross-chain messages. The numbers here are the
///         single source of gas policy; each messenger passes the result into `ERC7786MessengerBase._send`, which
///         wraps it as the ERC-7786 executionGasLimit attribute honored by whichever gateway is active. Swapping
///         transport never touches these values.
/// @dev Every budget is 1.5x the measured cost of its heaviest message, taking the failure path where one
///      exists - parking a mark, rejecting a whole bridge batch. `test/foundry/cross-chain/GasBudget.t.sol`
///      fails if a formula drifts under the measurement, and measures with `forge test --isolate`: a
///      delivery is its own transaction against cold storage, which the default shared-context run
///      understates by a third or more. The stand charges the hub's dedup write, so a measurement carries
///      what a real delivery pays before the client is even called.
library IntexGas {
    // --- Outbe -> target chain fixed-size messages (TargetRouter handlers) ---
    /// @dev auctionStart creates the series' auction on the target chain.
    /// @notice Fixed head of an AUCTION_STAGE_START, before its price rows.
    /// @dev A six-row start measured 383k, which `auctionStart(6)` quotes at 575k. The split between this
    ///      head and the per-row marginal is by shape, not by separate measurement.
    uint256 internal constant AUCTION_STAGE_START_BASE = 365_000;
    /// @notice Marginal cost of storing one reference-price row on the target.
    uint256 internal constant AUCTION_STAGE_START_PER_PRICE = 35_000;
    /// @dev Also relays the day's bids from inside the same delivery, a cost the origin cannot see, so the
    ///      target caps that relay and a heavier day parks. Measured at ~5.0M once the cap binds.
    uint256 internal constant AUCTION_STAGE_CLEARING = 7_500_000;
    /// @dev Measured at ~147k.
    uint256 internal constant AUCTION_RESULT = 225_000;
    /// @dev A mark is a bounded state flip, and slotting one for a series that has not landed yet is the
    ///      dearer path, so both budgets are cut from it. The slot holds the mark and its call time in one
    ///      word, so the two marginals now sit close together. Called measured ~114k at one series and
    ///      ~322k at eight; Qualified ~122k and ~323k.
    uint256 internal constant MARK_CALLED_BASE = 130_000;
    uint256 internal constant MARK_CALLED_PER_SERIES = 45_000;
    uint256 internal constant MARK_QUALIFIED_BASE = 145_000;
    uint256 internal constant MARK_QUALIFIED_PER_SERIES = 43_000;

    /// @notice Ceiling the target puts on one series' mark. Uncapped, a runaway series takes 63/64 of the
    ///         message's gas and starves the slot write, sending the whole batch into endless redelivery.
    ///         Kept under what `markCalled` allows per series so a runaway still fits its own budget.
    uint256 internal constant MARK_APPLY_CAP = 60_000;

    /// @notice Ceiling the target puts on the bids relay an inbound CLEARING fires. Its cost grows with the
    ///         day's bid count, which the origin cannot know, so past this the relay parks for a flush.
    uint256 internal constant RELAY_BIDS_CAP = 5_000_000;
    /// @dev Destination hook for composed proceeds: WCOEN unwrap + IntexFactory distribute registration.
    uint256 internal constant PROCEEDS_COMPOSE = 300_000;

    // --- Variable-size messages: base + per-item marginal ---
    /// @notice Destination gas for a fixed-size BIDS_DONE completeness marker. Measured at ~88k.
    uint256 internal constant BIDS_DONE = 135_000;

    /// @dev The receiver forwards into the Desis precompile, which no test can execute, so the precompile's
    ///      share is derived from its own gas model rather than measured: reads cost 100 and writes 2,900,
    ///      and `append_bid` spends six of them per bid (11,800). Its fixed part is ~3.7k, plus ~17.6k on the
    ///      branch that supersedes a generation - the dearer path, so the budget is cut from it. The router's
    ///      own share of a 64-bid batch is ~144k. Together that puts a 64-bid batch near 921k, which
    ///      `bidsBatch(64)` quotes at 1.38M.
    uint256 internal constant BIDS_BASE = 250_000;
    uint256 internal constant BIDS_PER_ITEM = 17_700;
    /// @dev Handler overhead only; createSeries is charged per series. Two measurements separate the
    ///      marginals: 2.64M for the widest chunk, which `issuance(8, 24)` quotes at 3.97M, and 1.73M for
    ///      one series at the recipient cap, quoted at 2.61M.
    uint256 internal constant ISSUANCE_BASE = 250_000;
    uint256 internal constant ISSUANCE_PER_SERIES = 195_000;
    uint256 internal constant ISSUANCE_PER_ITEM = 90_000;
    /// @dev The chunk completing a day also routes the paid wCOEN home at a fixed cost, which lands on the
    ///      base. `LocalLoopback.t.sol` walks the narrow case. Measured end to end at ~3.68M for a full
    ///      64-bidder chunk against the canonical Compact over a mainnet fork
    ///      (`EscrowAdapter.compactgas.t.sol`), which is ~5.9k per bidder dearer than the `MockTheCompact`
    ///      the rest of the suite runs on - so the budget is cut from the real custody, not the stand-in.
    ///      `refund(64)` quotes that chunk at 5.58M.
    uint256 internal constant REFUND_BASE = 560_000;
    uint256 internal constant REFUND_PER_ITEM = 78_500;
    /// @dev Sized on the failure path: a rejected item is recorded while the tokens are already burned on
    ///      the source. Measured 2.06M for a full rejected batch against 664k for one that all lands;
    ///      `nftMint(16)` quotes 3.07M, so the rejected path keeps the 1.5x margin and the happy one runs
    ///      well under it.
    uint256 internal constant NFT_MINT_BASE = 225_000;
    uint256 internal constant NFT_MINT_PER_ITEM = 178_000;

    /// @notice Destination gas for a BIDS_BATCH carrying `itemCount` bids.
    function bidsBatch(uint256 itemCount) internal pure returns (uint256) {
        return BIDS_BASE + itemCount * BIDS_PER_ITEM;
    }

    /// @notice Destination gas for an AUCTION_STAGE_START carrying `priceCount` rows.
    function auctionStart(uint256 priceCount) internal pure returns (uint256) {
        return AUCTION_STAGE_START_BASE + priceCount * AUCTION_STAGE_START_PER_PRICE;
    }

    /// @notice Destination gas for an ISSUANCE_INSTRUCTIONS creating `seriesCount` series and
    ///         minting to `recipientCount` recipients.
    function issuance(uint256 seriesCount, uint256 recipientCount) internal pure returns (uint256) {
        return ISSUANCE_BASE + seriesCount * ISSUANCE_PER_SERIES + recipientCount * ISSUANCE_PER_ITEM;
    }

    /// @notice Destination gas for a MARK_CALLED carrying `seriesCount` series.
    function markCalled(uint256 seriesCount) internal pure returns (uint256) {
        return MARK_CALLED_BASE + seriesCount * MARK_CALLED_PER_SERIES;
    }

    /// @notice Destination gas for a MARK_QUALIFIED carrying `seriesCount` series.
    function markQualified(uint256 seriesCount) internal pure returns (uint256) {
        return MARK_QUALIFIED_BASE + seriesCount * MARK_QUALIFIED_PER_SERIES;
    }

    /// @notice Destination gas for a REFUND_INSTRUCTIONS with `bidderCount` bidders.
    function refund(uint256 bidderCount) internal pure returns (uint256) {
        return REFUND_BASE + bidderCount * REFUND_PER_ITEM;
    }

    /// @notice Destination gas for a bridge batch/multi message crosschainMinting `itemCount` items.
    function nftMint(uint256 itemCount) internal pure returns (uint256) {
        return NFT_MINT_BASE + itemCount * NFT_MINT_PER_ITEM;
    }
}
