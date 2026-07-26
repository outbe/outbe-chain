// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IMetadosis {
    error OcompActivationRejected(uint16 code);

    event MetadosisAccumulation(
        uint32 indexed date, uint256 dayMetadosisLimitAmount, uint256 totalAccumulated, uint64 blockNumber
    );

    event OcompDayLimitFormed(
        uint32 indexed worldwideDay,
        uint256 baseLimit,
        uint256 carryOverBefore,
        uint256 carryOverTaken,
        uint256 carryOverAfter,
        uint256 formedDayLimit,
        uint64 blockNumber
    );

    event WorldwideDayStarted(
        uint32 indexed worldwideDay,
        uint64 formingStart,
        uint64 formingEnd,
        uint64 offeringStart,
        uint64 offeringEnd,
        uint64 scheduledTime
    );

    event WorldwideDayStatusChange(uint32 indexed worldwideDay, uint8 oldStatus, uint8 newStatus, uint64 blockNumber);

    event MetadosisSkipped(uint32 indexed worldwideDay, string reason, string status, uint64 blockNumber);

    event MetadosisExecuted(
        uint32 indexed worldwideDay,
        uint256 tributeTotals,
        uint256 dayGratisDemand,
        uint256 dayGratisLimit,
        uint256 dayGratisAllocation,
        uint256 dayGratisAllocationRemainder,
        uint256 netDayGratisAllocation,
        uint256 dayMetadosisLimitRemainder,
        string status,
        uint64 blockNumber
    );

    event MetadosisWorldwideDayProcessed(
        uint32 indexed worldwideDay,
        uint256 dayMetadosisLimit,
        uint256 dayMetadosisLimitRemainder,
        string status,
        string dayState,
        string action
    );

    /// @notice Emitted when a terminal WorldwideDay record is evicted from the
    /// bounded delete-queue (oldest-first, once terminal records exceed the cap).
    /// `finalStatus` is the day's terminal status (COMPLETED or FAILED).
    event WorldwideDayCleanedUp(uint32 indexed worldwideDay, uint8 finalStatus);

    event OffchainJobRequested(
        bytes32 indexed intentId,
        uint32 indexed wwd,
        uint64 pendingNonce,
        uint32 attempt,
        uint64 deadlineHeight,
        bytes32 activationPreconditionsHash
    );

    event OffchainJobExpired(
        bytes32 indexed intentId,
        uint32 indexed wwd,
        uint64 oldPendingNonce,
        uint64 nextPendingNonce,
        uint64 expiredAtHeight
    );

    event OffchainJobConflicted(
        bytes32 indexed intentId,
        bytes32 indexed jobId,
        uint32 attempt,
        uint64 oldPendingNonce,
        uint64 nextPendingNonce,
        bytes32 resultDigest
    );

    event LysisActivated(
        bytes32 indexed intentId,
        bytes32 indexed jobId,
        bytes32 activationCallId,
        bytes32 resultDigest,
        bytes32 terminalReceiptHash,
        uint32 wwd
    );

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
            uint256 previousVwap,
            uint256 currentVwap
        );

    function getActiveWorldwideDays() external view returns (uint32[] memory wwds);
    function getWorldwideDaysByStatus(uint8 status) external view returns (uint32[] memory wwds);
    function getBootstrapEndTime() external view returns (uint64 endTime);

    /// @notice Return the canonical OCB1 record for one off-chain computation job.
    /// @param intentId Canonical JobIntent identifier.
    /// @return ocompJobRecordV1 Canonically encoded OcompJobRecordV1 bytes.
    function getOffchainJob(bytes32 intentId) external view returns (bytes memory ocompJobRecordV1);

    /// @notice Activate one certified LYSIS_V1 result through a normal paid transaction.
    function activateLysis(bytes calldata pocActivationV1)
        external
        returns (bytes32 activationCallId, bytes32 resultDigest, uint8 outcome);

    /// @notice Return the canonical active generation selected by Metadosis state.
    function getActiveLysisGeneration(uint32 wwd) external view returns (bytes memory activeGenerationV1);

    /// @notice Return the canonical aggregate terminal receipt for an activation attempt.
    function getLysisTerminalReceipt(bytes32 intentId)
        external
        view
        returns (bytes memory aggregateActivationReceiptV1);
}
