//! External contract ABIs invoked via `storage.call` / `staticcall`.

use alloy_sol_types::sol;

sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    struct MessagingFee {
        uint256 nativeFee;
        uint256 lzTokenFee;
    }

    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IOriginMessenger {
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
        }

        function quoteSendAuctionStageStart(
            AuctionStageStartParams calldata params,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendAuctionStageStart(
            AuctionStageStartParams calldata params,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendAuctionStageReveal(
            uint32 seriesId,
            bool isGreenDay,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendAuctionStageReveal(
            uint32 seriesId,
            bool isGreenDay,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendAuctionStageClearing(
            uint32 seriesId,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendAuctionStageClearing(
            uint32 seriesId,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendAuctionResult(
            uint32 seriesId,
            uint32 issuedIntexCount,
            uint64 auctionClearingRate,
            uint32 wonBidsCount,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendAuctionResult(
            uint32 seriesId,
            uint32 issuedIntexCount,
            uint64 auctionClearingRate,
            uint32 wonBidsCount,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);

        function quoteSendRefundInstructions(
            uint32 seriesId,
            address[] calldata bidders,
            uint128[] calldata refundedAmounts,
            uint128[] calldata paidAmounts,
            bytes calldata extraOptions,
            bool payInLzToken
        ) external view returns (MessagingFee memory fee);

        function sendRefundInstructions(
            uint32 seriesId,
            address[] calldata bidders,
            uint128[] calldata refundedAmounts,
            uint128[] calldata paidAmounts,
            bytes calldata extraOptions,
            MessagingFee calldata fee,
            address refundAddress
        ) external payable returns (bytes32 guid);
    }
}
