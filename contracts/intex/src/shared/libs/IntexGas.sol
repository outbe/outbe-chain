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

    /// @dev Floor for a CLEARING round, and what the router clamps a smaller ask up to: driven through the
    ///      whole inbound path it spends 1.56M and still places a chunk (`ClearingRelayMailboxGas.t.sol`).
    uint256 internal constant AUCTION_STAGE_CLEARING = 2_300_000;

    /// @dev Ceiling for a CLEARING round, under the tightest per-transaction cap our targets enforce
    ///      (Ethereum's EIP-7825 is 16 777 216); a heavier day takes several rounds instead.
    uint256 internal constant AUCTION_STAGE_CLEARING_MAX = 14_000_000;

    /// @dev 147k.
    uint256 internal constant AUCTION_RESULT = 225_000;

    /// @dev Cut from slotting, the dearer path: 114k at one series and 322k at eight.
    uint256 internal constant MARK_CALLED_BASE = 130_000;
    uint256 internal constant MARK_CALLED_PER_SERIES = 45_000;

    /// @notice Per-series ceiling: uncapped, a runaway series takes 63/64 of the message's gas and starves
    ///         the slot write. Kept under the `markCalled` marginal so a runaway still fits its own budget.
    uint256 internal constant MARK_APPLY_CAP = 60_000;

    /// @dev What a round must have left to send one more BIDS_BATCH: a full 64-bid chunk measures ~714k
    ///      against the canonical mailbox - ~157k the send, ~8.7k a bid.
    uint256 internal constant RELAY_CHUNK_GAS = 800_000;

    /// @dev Held back on top of the last chunk for the completeness marker, ~157k measured; without it a
    ///      round that just affords its final chunk reverts whole and reports nothing.
    uint256 internal constant RELAY_MARKER_GAS = 250_000;

    /// @dev Held back from the relay so an unfinished day can still be reported home: the 63/64 rule
    ///      leaves the outer frame far too little to send a message of its own.
    uint256 internal constant RELAY_REPORT_GAS = 400_000;

    /// @dev Recording a day into the VWAP registry: 145k at one currency, 356k at six.
    uint256 internal constant DAILY_VWAP_BASE = 155_000;
    uint256 internal constant DAILY_VWAP_PER_ROW = 64_000;

    /// @dev WCOEN unwrap plus IntexFactory distribute registration.
    uint256 internal constant PROCEEDS_COMPOSE = 300_000;

    /// @dev 88k.
    uint256 internal constant BIDS_DONE = 135_000;

    /// @dev The remainder report a stopped relay sends home. Dearer than the other inbound bids messages
    ///      because its handler answers with an outbound CLEARING: 136k of it is the handler's own work
    ///      against the stand, and the rest is what a real dispatch costs over the mock's.
    uint256 internal constant BIDS_REMAINING = 400_000;

    /// @dev The one budget no test can measure - the receiver forwards into the Desis precompile. Derived
    ///      from its tariff (read 100, write 2,900, six per bid), 17.6k on the generation-reset branch and
    ///      the router's measured 144k share: ~921k for 64 bids. Replace with a live receipt.
    uint256 internal constant BIDS_BASE = 250_000;
    uint256 internal constant BIDS_PER_ITEM = 17_700;

    /// @dev 2.64M for the widest chunk, 1.73M for one series at the recipient cap.
    uint256 internal constant ISSUANCE_BASE = 250_000;
    uint256 internal constant ISSUANCE_PER_SERIES = 195_000;
    uint256 internal constant ISSUANCE_PER_ITEM = 90_000;

    /// @dev A refund chunk costs three separable things, measured against the canonical Compact with the
    ///      proceeds leg wired to our own token bridge and ERC-7786 hub: any chunk 159k, a chunk that carries
    ///      winners 83k more (the day's clearing snapshot, one Compact withdrawal, one transfer to the router)
    ///      plus 7.1k a winner, and the chunk that routes the day's proceeds 82k more. The mailbox's own
    ///      dispatch is outside that reading (it needs a fork): `ClearingRelayMailboxGas.t.sol` prices a send
    ///      at ~157k against the canonical mailbox and production runs one hub hop more, so the routing part
    ///      carries 170k for it. Every part then takes a 1.3x margin.
    uint256 internal constant REFUND_BASE = 207_000;
    uint256 internal constant REFUND_SETTLE_BASE = 110_000;
    uint256 internal constant REFUND_PER_ITEM = 9_250;
    uint256 internal constant REFUND_PROCEEDS_ROUTE = 330_000;

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

    function dailyVwap(uint256 rowCount) internal pure returns (uint256) {
        return DAILY_VWAP_BASE + rowCount * DAILY_VWAP_PER_ROW;
    }

    /// @param winnerCount Winners the chunk carries; zero for the chunk that only closes a day.
    /// @param routesProceeds Whether this chunk sends the day's proceeds home.
    function refund(uint256 winnerCount, bool routesProceeds) internal pure returns (uint256) {
        return REFUND_BASE + (winnerCount == 0 ? 0 : REFUND_SETTLE_BASE + winnerCount * REFUND_PER_ITEM)
            + (routesProceeds ? REFUND_PROCEEDS_ROUTE : 0);
    }

    function nftMint(uint256 itemCount) internal pure returns (uint256) {
        return NFT_MINT_BASE + itemCount * NFT_MINT_PER_ITEM;
    }
}
