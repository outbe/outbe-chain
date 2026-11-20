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
        CommittingBids,
        RevealingBids,
        CollectingBids,
        Completed,
        Canceled,
        Failed
    }

    function collectIntexAuctionBids(
        bytes32 receiveId,
        uint32 emissionDay,
        uint32 srcChainId,
        uint16 pageIndex,
        uint16 totalPages,
        address[] calldata bidders,
        uint256[] calldata packedBids
    ) external;

    /// @notice Per-chain completeness marker: the source relayed `totalPages`/`totalBids` for this day.
    function completeIntexAuctionBids(
        bytes32 receiveId,
        uint32 emissionDay,
        uint32 srcChainId,
        uint16 totalPages,
        uint32 totalBids
    ) external;

    function auctionStage(uint32 emissionDay) external view returns (AuctionStage);
    function intexAuctionBidsCount(uint32 emissionDay) external view returns (uint256);
}
