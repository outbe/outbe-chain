// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntex
/// @notice View surface and lifecycle events of the Intex runtime module: the
///         canonical, cross-chain Intex series ledger (identity + lifecycle).
/// @dev Writes are Rust-to-Rust only (IntexFactory). Its call and expiry sweeps emit
///      the series events here. `promisLoadMinor` is returned as uint256
///      (its storage representation). The Origin `uint128` bounds it.
interface IIntex {
    struct SeriesData {
        bytes14 seriesId;
        uint256 promisLoadMinor;
        uint256 entryPriceMinor;
        uint256 floorPriceMinor;
        uint32 issuedUnits;
        uint32 callWindow;
        uint32 callThreshold;
        uint256 callPriceMinor;
        /// @notice Read-time state: 0 Issued, 1 Qualified (derived, never stored), 2 Called,
        ///         3 Expired (a Called series past its notice period).
        uint8 state;
        uint32 issuedAt;
        uint32 calledAt;
        uint32 callNoticePeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 worldwideDay;
        /// @notice Units paid for and not yet exercised. Their load belongs to the settler.
        uint32 settledUnits;
        /// @notice Units already burned into Promis. They leave `settledUnits` for good.
        uint32 exercisedUnits;
        /// @notice Units sent to the Gem Factory. Their load moved with them.
        uint32 gemFactoryUnits;
        /// @notice Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
    }

    /// @notice Full identity + lifecycle record for a series. Reverts if the
    ///         series does not exist.
    function getIntexSeries(bytes14 seriesId) external view returns (SeriesData memory);

    /// @notice Derived from finalized daily VWAPs on every call, never stored.
    function isQualified(bytes14 seriesId) external view returns (bool);

    /// @notice The classes an issued unit can be in. Disjoint: they sum to
    ///         `issuedUnits`. `activeUnits` is what is still unpaid while the series
    ///         lives, and `forfeitedUnits` is the same remainder once it is Expired.
    ///         `settledUnits` is what is paid and not yet exercised, the same
    ///         measure `getIntexSeries` reports.
    struct UnitCounts {
        uint32 issuedUnits;
        uint32 activeUnits;
        uint32 settledUnits;
        uint32 exercisedUnits;
        uint32 gemFactoryUnits;
        uint32 forfeitedUnits;
    }

    /// @notice Read the disjoint unit counts of `seriesId`.
    function seriesUnitCounts(bytes14 seriesId) external view returns (UnitCounts memory);

    /// @notice Whether a series exists.
    function seriesExists(bytes14 seriesId) external view returns (bool);

    /// @notice Number of series ever created (dense-enumeration length).
    function totalSeries() external view returns (uint64);

    /// @notice The series id at a dense-enumeration index.
    function seriesAt(uint64 index) external view returns (bytes14);

    /// @notice The series was force-called. It settles until `settlementDeadline`, inclusive.
    event IntexSeriesCalled(bytes14 indexed seriesId, uint32 calledAt, uint64 settlementDeadline);

    /// @notice The series' settlement window closed. Both are zero when every unit
    ///         was realized in time.
    event IntexSeriesExpired(bytes14 indexed seriesId, uint32 forfeitedUnits, uint256 returnedPromisMinor);
}
