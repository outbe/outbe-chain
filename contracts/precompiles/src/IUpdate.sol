// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The Update precompile. Matches `UPDATE_ADDRESS` in
///      crates/blockchain/primitives/src/addresses.rs.
address constant UPDATE = 0x000000000000000000000000000000000000ee0b;

/// @title IUpdate
/// @notice Protocol update / hardfork precompile at 0x000000000000000000000000000000000000EE0B
/// @dev Vote manages this precompile. After approval, the inner cross-module API
/// handles the proposal. (see: /crates/system/update/)
/// An approved `upgradeContracts` proposal upgrades our UUPS contracts on the listed
/// chains. Their owner CrosschainBridge (`0x…EE15`) makes the calls when anyone calls
/// `applyApproved`.
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
    error UnknownApprovedUpgrade(uint256 proposalId);
    error ApprovedPayloadMismatch(uint256 proposalId);

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

    /// @notice Emitted when `applyApproved` runs an approved contract upgrade.
    event ContractUpgradeApplied(uint256 indexed proposalId);

    /// @notice Emitted when `applyApproved` drops an approved contract upgrade after the voting window.
    event ContractUpgradeExpired(uint256 indexed proposalId);

    /// @notice Runs the approved `upgradeContracts` proposal `proposalId` once. Anyone can call it.
    /// `payload` must be the approved JSON payload.
    function applyApproved(uint256 proposalId, bytes calldata payload) external;

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
