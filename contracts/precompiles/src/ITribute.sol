// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITribute {
    event TributeIssued(
        address indexed owner,
        uint256 tributeId,
        uint32 worldwideDay,
        bytes issuanceAmountMinor,
        uint16 settlementCurrency,
        bytes nominalAmountMinor
    );

    event TributeBurned(uint256 tributeId, address owner, uint32 worldwideDay);

    event TributeWorldwideDaySealed(uint32 indexed worldwideDay, bool isSealed);

    event WorldwideDayStarted(
        uint32 indexed worldwideDay,
        uint64 formingStart,
        uint64 formingEnd,
        uint64 offeringStart,
        uint64 offeringEnd,
        uint64 scheduledTime
    );

    event WorldwideDayStatusChange(uint32 indexed worldwideDay, uint8 oldStatus, uint8 newStatus, uint64 blockNumber);

    /// @notice Emitted when a terminal WorldwideDay record leaves the bounded
    /// delete-queue. Eviction is oldest-first, once terminal records exceed the cap.
    /// `finalStatus` is the day's terminal status (COMPLETED or FAILED).
    event WorldwideDayCleanedUp(uint32 indexed worldwideDay, uint8 finalStatus);

    /// @notice Canonical projection event for certified Lysis retirement and
    /// the two closed Metadosis terminal authorities (empty missed OFFERING or
    /// exact-aggregate capacity forfeiture). No arbitrary FAILED day may emit it.
    event TributePartitionRetired(uint32 indexed worldwideDay);

    event CertifiedTributePartitionRetired(
        bytes32 indexed activationCallId,
        uint32 indexed worldwideDay,
        uint64 sourceGeneration,
        uint32 consumedCount,
        bytes consumedNominalTotalMinor,
        uint64 retiredGeneration,
        bytes32 stateEventDigest
    );

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function totalSupply() external view returns (uint256);
    function balanceOf(address owner) external view returns (uint256);
    function ownerOf(uint256 tributeId) external view returns (address);
    function tokenURI(uint256 tributeId) external view returns (string memory);
    function getDayTotals(uint32 worldwideDay)
        external
        view
        returns (uint32 tributeCount, bytes memory tributeNominalTotalMinor, bool isSealed);
    function getWorldwideDay(uint32 wwd)
        external
        view
        returns (
            uint8 status,
            uint8 dayType,
            uint64 formingStart,
            uint64 formingEnd,
            uint64 lookbackEnd,
            uint64 offeringEnd,
            uint64 scheduledProcessTime,
            uint256 previousVwapMinor,
            uint256 currentVwapMinor
        );
    function getActiveWorldwideDays() external view returns (uint32[] memory wwds);
    /// @notice Returns days for a closed WwdStatus discriminant.
    /// @dev Unknown status bytes revert. The call never interprets them as empty.
    function getWorldwideDaysByStatus(uint8 status) external view returns (uint32[] memory wwds);
    function getTributesByOwner(address owner) external view returns (uint256[] memory tributeIds);
    function getTributesByDay(uint32 worldwideDay) external view returns (uint256[] memory tributeIds);
    function getLeague(uint256 tributeId) external view returns (uint16 league);
    function getDayLeagueFraction(uint32 worldwideDay, uint16 league) external view returns (uint64 fraction);
    function createNod(uint256 tributeId) external returns (uint256 nodId);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
