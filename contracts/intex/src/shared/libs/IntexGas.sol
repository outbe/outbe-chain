// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IntexGas
/// @author Outbe
/// @notice Destination gas budgets for intex cross-chain messages, passed into `ERC7786Sender._sendTo`
///         as the ERC-7786 executionGasLimit attribute whichever gateway is active.
/// @dev Each budget is 1.5x the measured cost of its message's heaviest path. The transport bills
///      the limit rather than the spend. `GasBudget.t.sol` pins them and must run with `--isolate`.
library IntexGas {
    /// @dev 383k at six rows. The head/row split is by shape.
    uint256 internal constant INTEX_AUCTION_START_BASE = 365_000;
    uint256 internal constant INTEX_AUCTION_START_PER_PRICE = 35_000;

    /// @dev Floor for a COLLECTING_BIDS round, and what the router clamps a smaller ask up to. Driven
    ///      through the whole inbound path, it spends 1.56M and still places a chunk
    ///      (`ClearingRelayMailboxGas.t.sol`).
    uint256 internal constant INTEX_AUCTION_CLEARING_START = 2_300_000;

    /// @dev Ceiling for a COLLECTING_BIDS round, under the tightest per-transaction cap our targets
    ///      enforce (Ethereum's EIP-7825 is 16 777 216). A heavier day takes several rounds
    ///      instead.
    uint256 internal constant INTEX_AUCTION_CLEARING_START_MAX = 14_000_000;

    /// @dev 167k.
    uint256 internal constant INTEX_AUCTION_RESULT = 251_000;
    /// @dev 135k: the cancel and the close of the day in the escrow.
    uint256 internal constant INTEX_AUCTION_CANCEL = 225_000;

    /// @dev Cut from the dearer paths: 68k for one slotted series, 303k for eight, 306k for eight refused applies.
    uint256 internal constant INTEX_SERIES_CALL_NOTICE_BASE = 130_000;
    uint256 internal constant INTEX_SERIES_CALL_NOTICE_PER_SERIES = 45_000;

    /// @notice Per-series ceiling: uncapped, a runaway series takes 63/64 of the message's gas and starves
    ///         the slot write. Kept under the `callSeries` marginal so a runaway still fits its own budget.
    uint256 internal constant CALL_NOTICE_APPLY_CAP = 60_000;

    /// @dev What a round must have left to send one more BIDS_BATCH: a full 64-bid chunk measures ~714k
    ///      against the canonical mailbox - ~157k the send, ~8.7k a bid.
    uint256 internal constant SEND_PAGE_GAS = 800_000;

    /// @dev Held back on top of the last chunk for the completeness marker, ~157k measured. Without
    ///      it, a round that just affords its final chunk reverts whole and reports nothing.
    uint256 internal constant SEND_TOTALS_GAS = 250_000;

    /// @dev Held back from the relay so an unfinished day can still be reported home. The 63/64
    ///      rule leaves the outer frame far too little to send a message of its own.
    uint256 internal constant SEND_PAUSE_GAS = 400_000;

    /// @dev The WCOEN credit to IntexFactory plus its burn in `receive_crosschain_proceeds`.
    uint256 internal constant PROCEEDS_COMPOSE = 300_000;

    /// @dev 88k.
    uint256 internal constant BIDS_TOTALS = 135_000;

    /// @dev The remainder report a stopped relay sends home. Dearer than the other inbound bids
    ///      messages because its handler answers with an outbound CLEARING. 136k of it is the
    ///      handler's own work against the stand. The rest is what a real dispatch costs over the
    ///      mock's.
    uint256 internal constant BIDS_PAUSE = 400_000;

    /// @dev The one budget no test can measure. The receiver forwards into the Desis precompile.
    ///      Derived from its tariff (read 100, write 2,900, six per bid), 17.6k on the
    ///      generation-reset branch and the router's measured 144k share: ~921k for 64 bids.
    ///      Replace with a live receipt.
    uint256 internal constant BIDS_BASE = 250_000;
    uint256 internal constant BIDS_PER_ITEM = 17_700;

    /// @dev A chunk creates the series it carries. Winners issue their own units. Measured: 292k for
    ///      one series, 3.78M for 24, so ~143k base and ~151k a series.
    uint256 internal constant ISSUANCE_BASE = 250_000;
    uint256 internal constant ISSUANCE_PER_SERIES = 230_000;

    /// @dev A refund chunk costs three separable things. Each is measured against the canonical
    ///      Compact, with the proceeds leg wired to our own token bridge and ERC-7786 hub:
    ///      - any chunk: 159k.
    ///      - a chunk that carries winners: 83k more (the day's clearing snapshot, one Compact
    ///        withdrawal, one transfer to the router), plus 7.1k a winner.
    ///      - a chunk that routes its winners' payments: 82k more.
    ///      The mailbox's own dispatch is outside that reading (it needs a fork).
    ///      `ClearingRelayMailboxGas.t.sol` prices a send at ~157k against the canonical mailbox,
    ///      and production runs one hub hop more. So the routing part carries 170k for it. Every
    ///      part then takes a 1.3x margin.
    ///      On Outbe itself the route hands the WCOEN to IntexFactory in the same call, so it also
    ///      runs the burn: `PROCEEDS_COMPOSE`. That route is the dearer one, and the routing part
    ///      covers it.
    uint256 internal constant RETURN_BASE = 207_000;
    uint256 internal constant RETURN_SETTLE_BASE = 110_000;
    uint256 internal constant RETURN_PER_ITEM = 9_250;
    uint256 internal constant RETURN_PROCEEDS_ROUTE = 400_000;

    /// @dev Cut from the failure path: 2.08M for a fully rejected batch against 611k for one that all lands.
    uint256 internal constant NFT_MINT_BASE = 225_000;
    uint256 internal constant NFT_MINT_PER_ITEM = 178_000;

    function bidsPage(uint256 itemCount) internal pure returns (uint256) {
        return BIDS_BASE + itemCount * BIDS_PER_ITEM;
    }

    function intexAuctionStart(uint256 priceCount) internal pure returns (uint256) {
        return INTEX_AUCTION_START_BASE + priceCount * INTEX_AUCTION_START_PER_PRICE;
    }

    function issuance(uint256 seriesCount) internal pure returns (uint256) {
        return ISSUANCE_BASE + seriesCount * ISSUANCE_PER_SERIES;
    }

    function intexSeriesCallNotice(uint256 seriesCount) internal pure returns (uint256) {
        return INTEX_SERIES_CALL_NOTICE_BASE + seriesCount * INTEX_SERIES_CALL_NOTICE_PER_SERIES;
    }

    /// @param winnersCount Winners the chunk carries. Zero for the chunk that only closes a day.
    /// @param routesProceeds Whether this chunk sends its proceeds home.
    function assetReturn(uint256 winnersCount, bool routesProceeds) internal pure returns (uint256) {
        return RETURN_BASE + (winnersCount == 0 ? 0 : RETURN_SETTLE_BASE + winnersCount * RETURN_PER_ITEM)
            + (routesProceeds ? RETURN_PROCEEDS_ROUTE : 0);
    }

    function nftMint(uint256 itemCount) internal pure returns (uint256) {
        return NFT_MINT_BASE + itemCount * NFT_MINT_PER_ITEM;
    }
}
