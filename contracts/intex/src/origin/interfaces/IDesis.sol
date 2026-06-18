// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         This is the minimal interface used by OriginMessenger to call the precompile;
///         the authoritative source lives in contracts/precompiles/src/IDesis.sol.
interface IDesis {
    /// @notice Auction lifecycle stages. Values map 1:1 to the Rust `AuctionStage` enum.
    enum AuctionStage {
        None,
        Started,
        Revealing,
        BidsReceived,
        Cleared,
        Cancelled
    }

    function processBidsBatch(
        uint32 seriesId,
        uint32 srcEid,
        bool isLast,
        uint32 relayGeneration,
        address[] calldata bidderAddresses,
        uint16[] calldata intexQuantities,
        uint64[] calldata intexBidPrices,
        uint32[] calldata timestamps
    ) external;

    function clearAuction(uint32 seriesId) external payable;

    function getAuctionStage(uint32 seriesId) external view returns (AuctionStage);
    function getBidsCount(uint32 seriesId) external view returns (uint256);
}
