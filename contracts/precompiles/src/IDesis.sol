// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         The Desis runtime drives the auction schedule from a Metadosis brief;
///         bid ingestion is called by OriginRouter and clearing runs from the
///         Desis begin-block gate.
interface IDesis {
    /// @notice Auction lifecycle stages. Values map 1:1 to the Rust `AuctionStage` enum.
    enum AuctionStage {
        None,
        Briefed,
        Started,
        Revealing,
        Clearing,
        Cleared,
        Cancelled
    }

    // --- Bid ingestion (from OriginRouter) ---
    /// @notice Accept a relayed bid batch from a target chain. Batches of one `relayGeneration` may arrive in any order over
    ///         the unordered bridge; the receiver collects all `totalBatches` (by `batchIndex`) before finalizing.
    function processBidsBatch(
        uint32 worldwideDay,
        uint32 srcChainId,
        uint32 relayGeneration,
        uint16 batchIndex,
        uint16 totalBatches,
        address[] calldata bidderAddresses,
        uint16[] calldata intexQuantities,
        uint32[] calldata intexBidRates,
        uint32[] calldata timestamps
    ) external;

    /// @notice Per-chain completeness marker: the source relayed `totalBatches`/`totalBids` for this day/generation.
    ///         The gate clears the auction once every snapshot chain has reported (or the fan-in deadline passes).
    function processBidsDone(
        uint32 worldwideDay,
        uint32 srcChainId,
        uint32 relayGeneration,
        uint16 totalBatches,
        uint32 totalBids
    ) external;

    // --- Views ---
    function getAuctionStage(uint32 worldwideDay) external view returns (AuctionStage);
    function getBidsCount(uint32 worldwideDay) external view returns (uint256);
    function getChainBidsCount(uint32 worldwideDay, uint32 srcChainId) external view returns (uint256);
    /// @notice Whether the chain's bid intake for the day is complete (marker + all batches arrived).
    function isChainDone(uint32 worldwideDay, uint32 srcChainId) external view returns (bool);

    /// @notice ERC-165 interface support check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // --- Events ---
    event AuctionCreated(uint32 indexed worldwideDay);
    /// @notice The chain's bid intake finalized: BIDS_DONE marker and all batches arrived with matching totals.
    event ChainBidsDone(uint32 indexed worldwideDay, uint32 indexed srcChainId, uint32 bidsCount);
    /// @notice The chain missed the fan-in deadline; the clearing excluded its bids.
    event ChainSkipped(uint32 indexed worldwideDay, uint32 indexed srcChainId);
    event AuctionCancelledRedDay(uint32 indexed worldwideDay);
    event AuctionOverdue(uint32 indexed worldwideDay);
    event AuctionCleared(uint32 indexed worldwideDay, uint32 issuedIntexCount, uint32 clearingRate, uint64 totalDemand);
    event AuctionClearedEmpty(uint32 indexed worldwideDay, uint64 totalDemand);
    event UnusedSupplyReported(uint32 indexed worldwideDay, uint256 unusedPromis);
    /// @notice A best-effort auction-stage dispatch from Metadosis failed; the caller
    /// falls back (e.g. routes supply to PromisLimit) instead of halting the block.
    event AuctionDispatchFailed(uint32 indexed worldwideDay, string stage, string reason);
}
