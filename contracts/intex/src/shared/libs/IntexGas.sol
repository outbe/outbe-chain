// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IntexGas
/// @author Outbe
/// @notice Destination gas budgets for intex cross-chain messages, passed into `ERC7786MessengerBase._send`
///         as the ERC-7786 executionGasLimit attribute whichever gateway is active.
/// @dev Each budget is 1.5x the measured cost of its message's heaviest path; the transport bills the limit
///      rather than the spend. `GasBudget.t.sol` pins them and must run with `--isolate`.
library IntexGas {
    /// @dev 383k at six rows; the head/row split is by shape.
    uint256 internal constant AUCTION_STAGE_START_BASE = 365_000;
    uint256 internal constant AUCTION_STAGE_START_PER_PRICE = 35_000;

    /// @dev Sized for the bids relay it fires, not the 452k stage flip: 5.0M once `RELAY_BIDS_CAP` binds.
    uint256 internal constant AUCTION_STAGE_CLEARING = 7_500_000;

    /// @dev 147k.
    uint256 internal constant AUCTION_RESULT = 225_000;

    /// @dev Cut from slotting, the dearer path: Called 114k at one series and 322k at eight, Qualified 122k
    ///      and 323k.
    uint256 internal constant MARK_CALLED_BASE = 130_000;
    uint256 internal constant MARK_CALLED_PER_SERIES = 45_000;
    uint256 internal constant MARK_QUALIFIED_BASE = 145_000;
    uint256 internal constant MARK_QUALIFIED_PER_SERIES = 43_000;

    /// @notice Per-series ceiling: uncapped, a runaway series takes 63/64 of the message's gas and starves
    ///         the slot write. Kept under the `markCalled` marginal so a runaway still fits its own budget.
    uint256 internal constant MARK_APPLY_CAP = 60_000;

    /// @notice Ceiling on the bids relay an inbound CLEARING fires. The origin cannot know the day's bid
    ///         count, so past this the relay parks.
    uint256 internal constant RELAY_BIDS_CAP = 5_000_000;

    /// @dev WCOEN unwrap plus IntexFactory distribute registration.
    uint256 internal constant PROCEEDS_COMPOSE = 300_000;

    /// @dev 88k.
    uint256 internal constant BIDS_DONE = 135_000;

    /// @dev The one budget no test can measure - the receiver forwards into the Desis precompile. Derived
    ///      from its tariff (read 100, write 2,900, six per bid), 17.6k on the generation-reset branch and
    ///      the router's measured 144k share: ~921k for 64 bids. Replace with a live receipt.
    uint256 internal constant BIDS_BASE = 250_000;
    uint256 internal constant BIDS_PER_ITEM = 17_700;

    /// @dev 2.64M for the widest chunk, 1.73M for one series at the recipient cap.
    uint256 internal constant ISSUANCE_BASE = 250_000;
    uint256 internal constant ISSUANCE_PER_SERIES = 195_000;
    uint256 internal constant ISSUANCE_PER_ITEM = 90_000;

    /// @dev 3.68M for 64 bidders against the canonical Compact over a mainnet fork, 5.9k per bidder over the
    ///      `MockTheCompact` the suite runs on - do not recalibrate from the stand. The chunk closing a day
    ///      also routes the paid wCOEN home, which lands on the base.
    uint256 internal constant REFUND_BASE = 560_000;
    uint256 internal constant REFUND_PER_ITEM = 78_500;

    /// @dev Cut from the failure path: 2.06M for a fully rejected batch against 664k for one that all lands.
    uint256 internal constant NFT_MINT_BASE = 225_000;
    uint256 internal constant NFT_MINT_PER_ITEM = 178_000;

    function bidsBatch(uint256 itemCount) internal pure returns (uint256) {
        return BIDS_BASE + itemCount * BIDS_PER_ITEM;
    }

    function auctionStart(uint256 priceCount) internal pure returns (uint256) {
        return AUCTION_STAGE_START_BASE + priceCount * AUCTION_STAGE_START_PER_PRICE;
    }

    function issuance(uint256 seriesCount, uint256 recipientCount) internal pure returns (uint256) {
        return ISSUANCE_BASE + seriesCount * ISSUANCE_PER_SERIES + recipientCount * ISSUANCE_PER_ITEM;
    }

    function markCalled(uint256 seriesCount) internal pure returns (uint256) {
        return MARK_CALLED_BASE + seriesCount * MARK_CALLED_PER_SERIES;
    }

    function markQualified(uint256 seriesCount) internal pure returns (uint256) {
        return MARK_QUALIFIED_BASE + seriesCount * MARK_QUALIFIED_PER_SERIES;
    }

    function refund(uint256 bidderCount) internal pure returns (uint256) {
        return REFUND_BASE + bidderCount * REFUND_PER_ITEM;
    }

    function nftMint(uint256 itemCount) internal pure returns (uint256) {
        return NFT_MINT_BASE + itemCount * NFT_MINT_PER_ITEM;
    }
}
