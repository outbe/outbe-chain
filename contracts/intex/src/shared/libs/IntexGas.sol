// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IntexGas
/// @author Outbe
/// @notice Transport-independent destination-gas budgets for intex cross-chain messages. The numbers here are the
///         single source of gas policy; each messenger passes the result into `ERC7786MessengerBase._send`, which
///         wraps it as the ERC-7786 executionGasLimit attribute honored by whichever gateway is active. Swapping
///         transport never touches these values.
/// @dev A per-message base plus a per-item marginal for the batched messages (the receiver loops over the array).
///      Every budget here is one and a half times the measured consumption of its heaviest realistic message,
///      taking the failure path where one exists — that is the expensive one, and it is the one that must not run
///      out. The measurement is already the pessimistic case and the EVM is deterministic, so the margin covers
///      only what varies: the length of revert data a failure records. The attribute is a ceiling the sender pays
///      for, so the same factor everywhere keeps the cost honest per message type.
///      Measurements live in `test/foundry/cross-chain/GasBudget.t.sol` and fail if a formula drifts under them.
library IntexGas {
    // --- Outbe -> target chain fixed-size messages (TargetRouter handlers) ---
    /// @dev auctionStart creates the series' auction on the target chain.
    /// @notice Fixed head of an AUCTION_STAGE_START, before its price rows.
    /// @dev Measured at ~332k for the six-row maximum.
    uint256 internal constant AUCTION_STAGE_START_BASE = 350_000;
    /// @notice Marginal cost of storing one reference-price row on the target.
    uint256 internal constant AUCTION_STAGE_START_PER_PRICE = 25_000;
    /// @dev Clearing also relays the day's bids back to Outbe from inside the same delivery, and that
    ///      cost grows with the bid count the origin cannot see. Measured: ~0.5M with no bids, ~3.5M at
    ///      one chunk, ~4.2M at four, ~8M at sixteen. The relay is capped on the target, so this covers
    ///      the stage flip, that cap, and the parking write; a heavier day parks and is flushed. Measured at
    ///      ~4.05M once the cap binds.
    uint256 internal constant AUCTION_STAGE_CLEARING = 6_000_000;
    /// @dev Measured at ~66k.
    uint256 internal constant AUCTION_RESULT = 100_000;
    /// @dev A mark is a bounded state flip. Measured: a batch of 8 takes ~123k applied and ~404k when
    ///      every series parks, one series ~50k and ~70k. The parking path sets the marginal.
    uint256 internal constant MARK_CALLED_BASE = 100_000;
    uint256 internal constant MARK_CALLED_PER_SERIES = 65_000;
    uint256 internal constant MARK_QUALIFIED_BASE = 100_000;
    uint256 internal constant MARK_QUALIFIED_PER_SERIES = 65_000;
    /// @dev Destination hook for composed proceeds: WCOEN unwrap + IntexFactory distribute registration.
    uint256 internal constant PROCEEDS_COMPOSE = 300_000;

    // --- Variable-size messages: base + per-item marginal ---
    /// @notice Destination gas for a fixed-size BIDS_DONE completeness marker. Measured at ~43k.
    uint256 internal constant BIDS_DONE = 70_000;

    /// @dev Not measured against the same rule: on Outbe the receiver forwards into the Desis precompile,
    ///      whose work this harness cannot execute. The router's own share of a 64-bid batch is ~73k, so these
    ///      numbers stand for the precompile and are left where they were.
    uint256 internal constant BIDS_BASE = 1_300_000;
    uint256 internal constant BIDS_PER_ITEM = 160_000;
    /// @dev Handler overhead only; a message carries several series, so the createSeries cost is
    ///      per series rather than in the base. Measured at the 64-recipient cap: ~10.1M against a
    ///      16.6M budget.
    uint256 internal constant ISSUANCE_BASE = 200_000;
    uint256 internal constant ISSUANCE_PER_SERIES = 400_000;
    uint256 internal constant ISSUANCE_PER_ITEM = 230_000;
    /// @dev Measured at ~3.58M for a full 64-bidder chunk against a live escrow.
    uint256 internal constant REFUND_BASE = 250_000;
    uint256 internal constant REFUND_PER_ITEM = 80_000;
    /// @dev The mint loop's cost is set by its failure path, not its happy one: a rejected item is
    ///      recorded with its revert bytes so the owner can retry, and the tokens are already burned on
    ///      the source, so an under-provisioned delivery strands them for good. Measured at the 64-item
    ///      cap: ~8.3M when every mint lands, ~17.1M when every one is rejected.
    uint256 internal constant NFT_MINT_BASE = 150_000;
    uint256 internal constant NFT_MINT_PER_ITEM = 350_000;

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

    /// @notice Destination gas for a bridge batch/multi/system message crosschainMinting `itemCount` items.
    function nftMint(uint256 itemCount) internal pure returns (uint256) {
        return NFT_MINT_BASE + itemCount * NFT_MINT_PER_ITEM;
    }
}
