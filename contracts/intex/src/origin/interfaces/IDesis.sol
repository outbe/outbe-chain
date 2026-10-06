// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IDesis
/// @notice Inbound call surface for the Desis runtime precompile.
///         OriginRouter uses this minimal interface to call the precompile.
///         The authoritative source is in contracts/precompiles/src/IDesis.sol.
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

    function processBidsBatch(
        uint32 worldwideDay,
        uint32 srcChainId,
        uint16 batchIndex,
        uint16 totalBatches,
        address[] calldata bidderAddresses,
        uint256[] calldata packedBids
    ) external;

    /// @notice Per-chain completeness marker: the source relayed `totalBatches`/`totalBids` for this day.
    function processBidsDone(uint32 worldwideDay, uint32 srcChainId, uint16 totalBatches, uint32 totalBids) external;

    function getAuctionStage(uint32 worldwideDay) external view returns (AuctionStage);
    function getBidsCount(uint32 worldwideDay) external view returns (uint256);
}
