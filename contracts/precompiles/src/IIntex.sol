// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntex
/// @notice The Intex runtime module: the canonical, cross-chain Intex series ledger
///         (identity + lifecycle), and the Settled Intex ERC-1155 collection on Outbe.
/// @dev Writes are Rust-to-Rust only (IntexFactory). Its call and expiry sweeps emit
///      the group events here, and its settlement and mining emit `TransferSingle`.
///      `promisLoadMinor` is returned as uint256 (its storage representation). The
///      Origin `uint128` bounds it.
interface IIntex {
    error SeriesNotFound();
    error ZeroIssuedAt();
    error InvalidStateValue(uint8 value);
    error InvalidSeriesId();
    error RealizedUnitsOverflow();
    error GroupAlreadyCalled();
    error SeriesIndexOutOfRange();
    error UnknownProfileSelector(uint8 value);
    error CalledGroupMemberOutOfRange();
    error DateStep(uint8 kind, uint32 dateKey);
    error NonTransferable();
    error BatchLengthMismatch();

    struct SeriesData {
        bytes14 seriesId;
        uint256 promisLoadMinor;
        uint256 entryPriceMinor;
        uint32 issuedUnits;
        uint32 callWindow;
        uint32 callThreshold;
        uint256 callPriceMinor;
        /// @notice Read-time state: 0 Issued, 1 Called, 2 Forfeited (a Called series past its
        ///         notice period).
        uint8 state;
        uint32 issuedAt;
        uint32 calledAt;
        uint32 callNoticePeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 worldwideDay;
        /// @notice Units paid for and not yet mined. Their load belongs to the settler.
        uint32 settledUnits;
        /// @notice Units already burned into Promis. They leave `settledUnits` for good.
        uint32 minedUnits;
        /// @notice Units sent to the Gem Factory. Their load moved with them.
        uint32 gemFactoryUnits;
        /// @notice Inclusive deadline: 0 when uncalled.
        uint64 settlementDeadline;
        /// @notice The call group the series is called and forfeited with.
        bytes32 groupKey;
    }

    /// @notice Full identity + lifecycle record for a series. Reverts if the
    ///         series does not exist, or was deleted once nothing in it was left to settle
    ///         or mine.
    function getIntexSeries(bytes14 seriesId) external view returns (SeriesData memory);

    /// @notice The classes an issued unit can be in. Disjoint: they sum to
    ///         `issuedUnits`. `activeUnits` is what is still unpaid while the series
    ///         lives, and `forfeitedUnits` is the same remainder once it is Forfeited.
    ///         `settledUnits` is what is paid and not yet mined, the same
    ///         measure `getIntexSeries` reports.
    struct UnitCounts {
        uint32 issuedUnits;
        uint32 activeUnits;
        uint32 settledUnits;
        uint32 minedUnits;
        uint32 gemFactoryUnits;
        uint32 forfeitedUnits;
    }

    /// @notice Read the disjoint unit counts of `seriesId`.
    function seriesUnitCounts(bytes14 seriesId) external view returns (UnitCounts memory);

    /// @notice Whether a series exists.
    function seriesExists(bytes14 seriesId) external view returns (bool);

    /// @notice Number of live series (dense-enumeration length).
    function totalSeries() external view returns (uint256);

    /// @notice The series id at a dense-enumeration index.
    function seriesAt(uint256 index) external view returns (bytes14);

    // ERC-165
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // ERC-1155: a series' Settled units, token id = `uint256(uint112(seriesId))`.
    function balanceOf(address account, uint256 id) external view returns (uint256);
    function balanceOfBatch(address[] calldata accounts, uint256[] calldata ids)
        external
        view
        returns (uint256[] memory);
    // Settled units are soulbound: transfers and approvals always revert NonTransferable.
    function setApprovalForAll(address operator, bool approved) external;
    function safeTransferFrom(address from, address to, uint256 id, uint256 value, bytes calldata data) external;
    function safeBatchTransferFrom(
        address from,
        address to,
        uint256[] calldata ids,
        uint256[] calldata values,
        bytes calldata data
    ) external;
    // No approval can exist: this reads false.
    function isApprovedForAll(address account, address operator) external view returns (bool);

    // ERC-1155 Metadata URI
    function uri(uint256 id) external view returns (string memory);
    function name() external view returns (string memory);
    function symbol() external view returns (string memory);

    /// @notice Emitted when a settlement mints Settled units and when mining burns them.
    event TransferSingle(address indexed operator, address indexed from, address indexed to, uint256 id, uint256 value);

    /// @notice The call group was force-called. Each of its series settles until
    ///         `settlementDeadline`, inclusive.
    event IntexGroupCalled(bytes32 indexed groupKey, bytes14[] seriesIds, uint32 calledAt, uint64 settlementDeadline);

    /// @notice The group is forfeited: its settlement window closed. Both are zero when
    ///         every unit was realized in time.
    event IntexGroupForfeited(bytes32 indexed groupKey, uint64 forfeitedUnits, uint256 returnedPromisMinor);
}
