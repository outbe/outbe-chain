// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IValidatorSet
/// @notice Validator set management precompile at 0x000000000000000000000000000000000000EE00
interface IValidatorSet {
    /// Emitted when a new validator is registered.
    event ValidatorRegistered(address indexed validator, uint64 index);

    /// Emitted when a validator is activated.
    event ValidatorActivated(address indexed validator);

    /// Emitted when a validator begins exiting.
    event ValidatorDeactivated(address indexed validator, uint64 atHeight);

    /// Emitted when a validator is forced to exit because of a severe fault.
    event ValidatorForcedExit(address indexed validator, uint64 atHeight);

    /// Emitted on epoch transition.
    event EpochTransition(uint256 indexed newEpochNumber, uint64 timestamp, uint32 activeValidatorCount);

    /// Emitted when DKG reshare updates the active consensus set.
    event ConsensusSetUpdated(uint32 activeCount);

    function getValidators() external view returns (address[] memory);
    function getActiveValidators() external view returns (address[] memory);
    function getActiveConsensusSet() external view returns (address[] memory);
    function validatorByAddress(address addr)
        external
        view
        returns (
            address validatorAddress,
            bytes memory consensusPubkey,
            uint256 stake,
            uint8 status,
            uint64 slashCount,
            uint64 missedBlocks,
            uint64 missedVotes,
            uint64 blocksProposed,
            uint64 joinedAtHeight,
            uint64 deactivatedAtHeight,
            uint64 unbondingEnd,
            bool hasBLSShare
        );
    function validatorByIndex(uint64 index)
        external
        view
        returns (
            address validatorAddress,
            bytes memory consensusPubkey,
            uint256 stake,
            uint8 status,
            uint64 slashCount,
            uint64 missedBlocks,
            uint64 missedVotes,
            uint64 blocksProposed,
            uint64 joinedAtHeight,
            uint64 deactivatedAtHeight,
            uint64 unbondingEnd,
            bool hasBLSShare
        );
    function validatorCount() external view returns (uint32);
    function activeValidatorCount() external view returns (uint32);
    function activeConsensusCount() external view returns (uint32);
    function isValidator(address addr) external view returns (bool);
    function isConsensusParticipant(address addr) external view returns (bool);
    function hasPendingSetChange() external view returns (bool);
    function getEpochNumber() external view returns (uint256);
    function getEpochStartTimestamp() external view returns (uint64);
    function getEpochStartBlock() external view returns (uint64);
    function registerValidator(address validatorAddress, bytes calldata consensusPubkey, bytes calldata blsSignature)
        external;
    function setP2pAddress(address validatorAddress, uint8 version, bytes calldata encoded) external;
    function getP2pAddress(address validatorAddress) external view returns (uint8 version, bytes memory encoded);
    function deactivateValidator(address validatorAddress) external;
    function activateResharedSet(address[] calldata newActiveSet, bytes32 groupPublicKey) external;
}
