// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         Auction lifecycle (Start/Reveal/Clearing) is called by the Metadosis
///         runtime module; bid ingestion and clearing are called by OriginMessenger.
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

    // --- Bid ingestion / clearing (from OriginMessenger) ---
    /// @notice Accept a relayed bid batch from BNB.
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

    /// @notice Run clearing and hand issuance to IntexFactory.
    function clearAuction(uint32 seriesId) external payable;

    // --- Views ---
    function getAuctionStage(uint32 seriesId) external view returns (AuctionStage);
    function getBidsCount(uint32 seriesId) external view returns (uint256);

    /// @notice ERC-165 interface support check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // --- Events ---
    event AuctionCreated(uint32 indexed seriesId);
    event BidsReceived(uint32 indexed seriesId, uint32 srcEid, uint256 bidsCount);
    event AuctionCancelledNoBids(uint32 indexed seriesId);
    event AuctionCancelledRedDay(uint32 indexed seriesId);
    event AuctionCleared(uint32 indexed seriesId, uint32 issuedIntexCount, uint64 clearingPrice, uint64 totalDemand);
    event UnusedSupplyReported(uint32 indexed seriesId, uint256 unusedPromis);
    /// @notice A best-effort auction-stage dispatch from Metadosis failed; the caller
    /// falls back (e.g. routes supply to PromisLimit) instead of halting the block.
    event AuctionDispatchFailed(uint32 indexed seriesId, string stage, string reason);
}
