// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntexRegistry
/// @notice Read-only view surface for the IntexRegistry runtime module: the
///         canonical, cross-chain Intex series ledger (identity + lifecycle).
/// @dev Writes are Rust-to-Rust only (IntexFactory); this interface exposes
///      reads for off-chain observability. `intexSize` is returned as uint256
///      (its storage representation); it is bounded by the Origin `uint128`.
interface IIntexRegistry {
    struct SeriesData {
        uint32 seriesId;
        uint256 intexSize;
        uint64 intexStrikePrice;
        uint256 coenPriceFloor;
        uint32 issuedIntexCount;
        uint16 callWindowDays;
        uint16 callThresholdDays;
        uint256 coenPriceCallTrigger;
        uint8 state;
        uint32 issuedAt;
        uint32 calledAt;
        uint32 intexCallPeriod;
    }

    /// @notice Full identity + lifecycle record for a series. Reverts if the
    ///         series does not exist.
    function seriesData(uint32 seriesId) external view returns (SeriesData memory);

    /// @notice Whether a series exists.
    function seriesExists(uint32 seriesId) external view returns (bool);

    /// @notice Number of series ever created (dense-enumeration length).
    function totalSeries() external view returns (uint64);

    /// @notice The series id at a dense-enumeration index.
    function seriesAt(uint64 index) external view returns (uint32);
}
