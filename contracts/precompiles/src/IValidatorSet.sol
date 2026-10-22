// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IValidatorSet
/// @notice Validator set management precompile at 0x000000000000000000000000000000000000EE00
interface IValidatorSet {
    /// Caller-facing domain revert.
    error VrfVersionOverflow();
    /// Caller-facing domain revert.
    error UnsupportedDelegateRole();
    error ZeroDelegateAddress();
    error DelegateMatchesValidator();
    error DelegateIsRegisteredValidator();
    error DelegateAlreadyAssigned();
    error CallerNotRegisteredValidator();
    error ValidatorAlreadyOperationalDelegate();
    error FinalizedParticipationRingSeqOverflow();
    error ValidatorNotFound();
    error ValidatorNotFoundAtIndex();
    error ConsensusPubkeyWrongLength();
    error ConsensusPubkeyConversionFailed();
    error BlsPopWrongLength();
    error BlsSignatureConversionFailed();
    error CommitteeSnapshotLengthExceedsU64();
    error VrfGroupPkBytesLengthExceedsU64();
    error SnapshotExistsFlagWithoutRecord();
    error VrfGroupPkLengthExceedsUsize();
    error UnknownValidatorStatus(uint8 status);
    error CorruptValidatorState(address account, string detail);
    error MaxValidatorsExceedsConsensusBound(uint32 max_validators, uint32 bound);
    error UnregisteredLifecycleNoStatus();
    error TeeExpiryExclusionsExceedCap(uint64 count, uint64 cap);
    error TeeExpiryExclusionsNotUniqueNonZero();
    error TeeExpiredValidatorInNewActiveSet(address account);
    error TeeExpiryExclusionUnregistered(address account);
    error ActiveSetCountExceedsU32();
    error BoundaryRetainedValidatorExitedBeforeFreeze(address account, uint64 changed_at, uint64 freeze_height);
    error BoundaryRetainedValidatorJailedBeforeFreeze(address account, uint64 jailed_at, uint64 freeze_height);
    error BoundaryIncludedIneligibleValidator(address account, uint8 status);
    error BoundaryParticipantMembershipMismatch(address[] planned, address[] artifact);
    error TeeExpiryExclusionIneligibleStatus(address account, uint8 status);
    error BoundaryOmittedActiveValidator(address account);
    error BoundaryOmittedValidatorExitedAfterFreeze(address account, uint64 changed_at, uint64 freeze_height);
    error BoundaryOmittedValidatorJailedAfterFreeze(address account, uint64 jailed_at, uint64 freeze_height);
    error ExitingValidatorMissingDeactivationHeight(address account);
    error ValidatorDeactivationHeightZero();
    error InactiveValidatorMissingDeactivationHeight(address addr);
    error InactiveCleanupHeightOverflow();
    error UnauthorizedOwnerOrValidator();
    error ValidatorNotRegistered();
    error CanOnlyDeactivateActiveValidator();
    error ActiveValidatorMissingStakeProjection();
    error TeeExpirySweepIneligibleStatus(address addr, uint8 status);
    error RegisteredValidatorMissingHistory();
    error JailedValidatorStillRetained();
    error UnjailRequiresJailedStatus(uint8 status);
    error CannotPunishValidatorStatus(string action, uint8 status);
    error SlashCountOverflow();
    error ProposerNotConsensusParticipant(address addr);
    error VoterNotConsensusParticipant(address addr);
    error FinalizedVoterNotRegistered(address addr);
    error AbsentVoterNotConsensusParticipant(address addr);
    error FinalizedAbsentVoterNotRegistered(address addr);
    error EpochNumberOverflow();
    error ParticipationCounterOverflow(string counter);
    error CannotProjectAbsentValidator();
    error RegisteredValidatorMissingConsensusPublicKey();
    error ActiveValidatorCountExceedsU32();
    error RegisteredValidatorCountExceedsU32();
    error ActiveConsensusCountExceedsU32();
    error ValidatorRegistryIndexEmpty(uint64 index, uint64 count);
    error ValidatorRegistryReverseIndexMismatch(address account, uint64 expected, uint64 actual);
    error EpochNumberExceedsU64();
    error RegisteredValidatorMissingConsensusPubkey();
    error ValidatorIdentityHashError(string reason);
    error ConfirmValidatorReadyRequiresPending(uint8 status);
    error ValidatorRegistryIndexExceedsCount(address addr, uint64 index, uint64 count);
    error ValidatorRegistryForwardIndexMismatch(uint64 index, address expected, address actual);
    error ValidatorConsensusPubkeyReverseMismatch(address addr, address owner);
    error LifecycleTransitionChangedRegistryIdentity();
    error CannotPersistAbsentLifecycle();
    error RegisteredValidatorMissingRegistryIndex();
    error InvalidBlsPublicKey();
    error InvalidBlsSignature();
    error InvalidBlsRegistrationSignature();
    error UnsupportedP2pAddressVersion(uint8 version);
    error P2pAddressPayloadTooLong(uint64 max_len);
    error InvalidP2pAddress(string reason);
    error ZeroConsensusPublicKey();
    error ZeroRadicleNodeId();
    error RadicleNodeIdAlreadyRegistered();
    error RegistrationRequiresBlsPop();
    error SelfRegistrationLimitReached();
    error ConsensusPubkeyAlreadyRegistered();
    error ValidatorAlreadyRegistered();
    error InactiveValidatorMustKeepRadicleNodeId();
    error InconsistentRadicleNodeIdReverseBinding();
    error ReregistrationCooldownHeightOverflow();
    error ReregistrationCooldownNotExpired();
    error UnregisteredValidatorRetainsRadicleReservation();
    error MaxValidatorsReached();
    error ValidatorCountOverflow();
    error ValidatorRegistryIndexZero();
    error CannotIncreaseStakeWhileExitingOrUnbonding();
    error ValidatorIsInactive();
    error ValidatorIsAlreadyRegistered();
    error ValidatorJailHeightZero();
    error UnjailCooldownHeightOverflow();
    error UnjailCooldownNotElapsed(uint64 ready_at, uint64 current_height);
    error CannotCompleteUnbondingWithRemainingStakeOrClaim();
    error CannotStakeBeforeRegistration();
    error InactiveValidatorMustReregisterBeforeStaking();
    error DeactivationHeightZeroSentinel();
    error ActiveHistoryHasDeactivationHeight();
    error RetainedJailHistoryHeightMismatch();
    error JailHistoryHeightMismatch();
    error ExitingHistoryMissingDeactivationHeight();
    error UnbondingHistoryMissingDeactivationHeight();
    error InactiveHistoryMissingDeactivationHeight();
    error BondedStakeBelowMinimum(uint256 bonded, uint256 minimum);
    error BondedStakeNotBelowMinimum(uint256 bonded, uint256 minimum);
    error JailedValidatorMustFullyUnstakeForExit();
    error UnbondingEndHintZeroSentinel();

    /// Emitted when a new validator is registered.
    event ValidatorRegistered(address indexed validator, uint64 index);

    /// Emitted when a validator is activated.
    event ValidatorActivated(address indexed validator);

    /// Emitted when a validator begins exiting.
    event ValidatorDeactivated(address indexed validator, uint64 atHeight);

    /// Emitted when a validator is forced to exit because of a severe fault.
    event ValidatorForcedExit(address indexed validator, uint64 atHeight);

    /// Emitted when a validator is JAILED (slashed + frozen) on a felony, instead
    /// of being force-exited. It is dropped from the next reshare and may later
    /// unjail (-> PENDING -> ACTIVE) or unstake out.
    event ValidatorJailed(address indexed validator, uint64 atHeight);

    /// Emitted when a JAILED validator calls unjailValidator() and returns to
    /// PENDING (then ACTIVE via the next reshare).
    event ValidatorUnjailed(address indexed validator, uint64 atHeight);

    /// Emitted when stake moves a validator from WaitingForStake to
    /// WaitingForReadiness. `validator` is that account.
    event ValidatorPending(address indexed validator);

    /// Emitted when readiness confirmation moves a validator from
    /// WaitingForReadiness to Joining. `validator` is that account.
    event ValidatorReadyConfirmed(address indexed validator);

    /// Emitted when cleanup removes an INACTIVE validator.
    /// `index` is that validator's dense registry index before the removal.
    event ValidatorRemoved(address indexed validator, uint64 index);

    /// Emitted when cleanup moves the tail validator into a removed slot.
    /// `oldIndex` is the tail index. `newIndex` is the removed slot.
    event ValidatorIndexRemapped(address indexed validator, uint64 oldIndex, uint64 newIndex);

    /// Emitted on epoch transition.
    event EpochTransition(uint256 indexed newEpochNumber, uint64 timestamp, uint32 activeValidatorCount);

    /// Emitted when DKG reshare updates the active consensus set.
    event ConsensusSetUpdated(uint32 activeCount);

    /// Emitted when a validator assigns a role-scoped operational key.
    event ValidatorDelegateSet(address indexed validator, uint8 indexed role, address indexed delegate);

    /// Emitted when a validator revokes a role-scoped operational key.
    event ValidatorDelegateRevoked(address indexed validator, uint8 indexed role, address indexed delegate);

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
    /// Stable role id: 1 = ORACLE. Unknown ids revert.
    function setDelegate(uint8 role, address delegate) external;
    function revokeDelegate(uint8 role) external;
    function getDelegate(address validator, uint8 role) external view returns (address);
    function resolveValidator(uint8 role, address signer) external view returns (address);
    function registerValidator(
        address validatorAddress,
        bytes calldata consensusPubkey,
        bytes32 radicleNodeId,
        bytes calldata blsRegistrationSignature
    ) external;
    function getRadicleNodeId(address validator) external view returns (bytes32);
    function validatorByRadicleNodeId(bytes32 nodeId) external view returns (address);
    function setP2pAddress(address validatorAddress, uint8 version, bytes calldata encoded) external;
    function getP2pAddress(address validatorAddress) external view returns (uint8 version, bytes memory encoded);
    function deactivateValidator(address validatorAddress) external;
    /// Stale-join guard: a PENDING joiner confirms on-chain that its node has
    /// caught up to head and may be frozen into the next DKG reshare target.
    /// Caller must be the validator itself and currently PENDING. Until called,
    /// a staked joiner stays PENDING and is excluded from the reshare target.
    function confirmValidatorReady() external;
}
