//! External contract ABIs invoked via `storage.call`.
//!
//! `OriginRouter` sends are relay-float-funded: called with value 0, the router quotes and
//! pays the bridge fee from its own native balance, so the precompile passes no fee/options/refund.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginRouter {
        struct AuctionStageStartParams {
            uint32 seriesId;
            uint32 commitEnd;
            uint32 revealEnd;
            uint32 issuanceEnd;
            uint16 issuanceCurrency;
            uint16 referenceCurrency;
            uint128 promisLoadMinor;
            uint32 minIntexBidRate;
            uint64 entryPrice;
            uint64 floorPriceMinor;
            uint64 callPriceMinor;
            uint32 intexCallPeriod;
            uint16 callWindowDays;
            uint16 callThresholdDays;
            uint16 minIntexBidQuantity;
            uint128 commitBondMinor;
        }

        function sendAuctionStageStart(AuctionStageStartParams calldata params)
            external payable returns (bytes32 sendId);

        function sendAuctionStageReveal(uint32 seriesId, bool isGreenDay)
            external payable returns (bytes32 sendId);

        function sendAuctionStageClearing(uint32 seriesId) external payable returns (bytes32 sendId);

        function sendAuctionResult(
            uint32 seriesId,
            uint32 issuedIntexCount,
            uint64 auctionClearingRate,
            uint32 wonBidsCount
        ) external payable returns (bytes32 sendId);

        function sendRefundInstructions(
            uint32 seriesId,
            address[] calldata bidders,
            uint128[] calldata refundedAmounts,
            uint128[] calldata paidAmounts
        ) external payable returns (bytes32 sendId);
    }
}
