// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IUpdate
/// @notice Protocol update / hardfork precompile at 0x000000000000000000000000000000000000EE0B
/// @dev Vote manages this precompile through primary, replacement and activation windows.
/// Live plans carry version/info/mrenclave, not a caller-selected activationHeight.
/// Linked actions cancel or replace a specific plan at the end of its replace window.
/// TEE preparation starts only after that window closes; getScheduledUpdate exposes
/// the derived activation height. Existing software-only records retain their heights.
interface IUpdate {
    /// Caller-facing domain revert.
    error ScheduledUpdateNotFound();
    error ScheduledUpdateAlreadyExists();
    error HeightInPast();
    error InvalidVersion();
    error DowngradeNotAllowed();
    error InvalidPayload();
    error ActivationConflict();
    error TooManyWaitingForActivation();
    error InvalidScheduledUpdateStatus();
    error InvalidTeePolicy();
    error TeePolicyChainIdentityMismatch();
    error TeePolicyActivationMismatch();
    error ActiveVersionDoesNotFitU32();
    error UpgradeHandlerFailed(string label, string reason);
    error StagedTeePolicyMissingProposal(uint256 proposal_id);
    error TeePolicyActivationFailed(uint256 proposal_id, string reason);
    error StoredProposalPayloadInvalid(string value);
    error EnclaveRolloutRequiresExclusiveScheduledUpgrade();
    error InvalidActiveTeePolicy(string value);
    error VoteTargetInvariantFailure(string value);
    error UnexpectedAppliedClassification();

    enum ScheduledUpdateStatus {
        Scheduled,
        Activated,
        Canceled
    }

    struct ScheduledUpdate {
        uint256 proposalId;
        uint32 version;
        uint64 activationHeight;
        bytes info;
        ScheduledUpdateStatus status;
    }

    /// @notice Emitted when the proposed upgrade is activated.
    event UpgradeActivated(uint32 version, uint64 activationHeight);

    /// @notice Emitted when a scheduled update is canceled and dropped from activation.
    event UpgradeCanceled(uint256 indexed proposalId, uint32 version, uint64 activationHeight);

    /// @notice Emitted when vote accepts a new update.
    event ScheduledUpdateCreated(uint256 indexed proposalId, uint32 version, uint64 activationHeight, bytes info);

    /// @notice Returns the active protocol version.
    function getActiveVersion() external view returns (uint32);

    /// @notice Returns the block height at which the active version was set.
    function getActiveVersionHeight() external view returns (uint64);

    /// @notice Returns true when `version` is active at the current height.
    function isVersionActive(uint32 version) external view returns (bool);

    /// @notice Returns a scheduled update by vote proposal id.
    function getScheduledUpdate(uint256 proposalId) external view returns (ScheduledUpdate memory);

    /// @notice Returns proposal ids with scheduled updates waiting for activation height.
    function listWaitingForActivation() external view returns (uint256[] memory);
}
