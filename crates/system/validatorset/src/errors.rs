//! Domain error types for the validator set precompile and runtime.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

/// Deterministic activation failures for the validator set.
///
/// `next_vrf_material_version` returns this error when the version overflows.
/// `activate_reshared_set` is a test helper and does not return this error.
/// The production boundary hook does not return this error either.
/// Consensus can reject the activation and avoid a node panic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ActivationError {
    /// `vrf_material_version` reached `u64::MAX` and cannot increment.
    ///
    /// Activation must reject deterministically instead of saturating.
    /// Proposer and validator paths then observe the same failure.
    #[error("vrf material version overflow at reshare activation")]
    VrfVersionOverflow,
}

impl From<ActivationError> for PrecompileError {
    fn from(err: ActivationError) -> Self {
        // The EVM layer cannot recover valid VRF material in place.
        PrecompileError::Revert(err.to_string())
    }
}

/// Domain errors for the validator set precompile and runtime.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidatorSetError {
    /// The caller provided an unknown delegate role identifier.
    #[error("unsupported validator delegate role")]
    UnsupportedDelegateRole,

    /// The delegate address is zero.
    #[error("delegate address must not be zero")]
    ZeroDelegateAddress,

    /// The delegate address equals the validator address.
    #[error("delegate must differ from validator")]
    DelegateMatchesValidator,

    /// The delegate address belongs to a registered validator.
    #[error("delegate must not be a registered validator")]
    DelegateIsRegisteredValidator,

    /// Another validator already assigned this delegate for the role.
    #[error("delegate already assigned for role")]
    DelegateAlreadyAssigned,

    /// The caller is not a registered validator.
    #[error("caller is not a registered validator")]
    CallerNotRegisteredValidator,

    /// A validator already assigned this address as an operational delegate.
    #[error("validator address is already assigned as an operational delegate")]
    ValidatorAlreadyOperationalDelegate,

    /// The finalized participation ring sequence counter overflowed `u64`.
    #[error("finalized participation ring sequence overflow")]
    FinalizedParticipationRingSeqOverflow,

    /// The requested validator address is not in the registry.
    #[error("validator not found")]
    ValidatorNotFound,

    /// No validator exists at the requested registry index.
    #[error("validator not found at index")]
    ValidatorNotFoundAtIndex,

    /// The consensus public key slice does not have 48 bytes.
    #[error("consensus pubkey must be 48 bytes")]
    ConsensusPubkeyWrongLength,

    /// Conversion of the consensus public key slice failed.
    #[error("consensus pubkey conversion failed")]
    ConsensusPubkeyConversionFailed,

    /// The BLS proof-of-possession slice does not have 96 bytes.
    #[error("BLS proof of possession must be exactly 96 bytes")]
    BlsPopWrongLength,

    /// Conversion of the BLS signature slice failed.
    #[error("BLS signature conversion failed")]
    BlsSignatureConversionFailed,

    /// The committee snapshot length exceeds `u64::MAX`.
    #[error("committee snapshot length exceeds u64")]
    CommitteeSnapshotLengthExceedsU64,

    /// The VRF group public key byte length exceeds `u64::MAX`.
    #[error("vrf group pk bytes length exceeds u64")]
    VrfGroupPkBytesLengthExceedsU64,

    /// A committee snapshot exists flag has no readable snapshot record.
    #[error("committee snapshot exists flag has no readable record")]
    SnapshotExistsFlagWithoutRecord,

    /// A replayed committee snapshot write does not match the stored snapshot.
    #[error("committee snapshot replay mismatch for key {key}")]
    CommitteeSnapshotReplayMismatch {
        /// Snapshot storage key.
        key: B256,
    },

    /// The stored VRF group public key length exceeds `usize::MAX`.
    #[error("vrf group pk length exceeds usize")]
    VrfGroupPkLengthExceedsUsize,

    /// Storage contains an unknown validator status code.
    #[error("unknown validator status {status}")]
    UnknownValidatorStatus {
        /// Raw status code.
        status: u8,
    },

    /// Stored validator state violates a state machine invariant.
    #[error("corrupt validator state for {address}: {detail}")]
    CorruptValidatorState {
        /// Validator address.
        address: Address,
        /// Invariant violation description.
        detail: String,
    },

    /// The requested maximum validator count exceeds the consensus bound.
    #[error("max validators exceeds consensus bound: {max_validators} > {bound}")]
    MaxValidatorsExceedsConsensusBound {
        /// Requested maximum validator count.
        max_validators: u32,
        /// Protocol consensus bound.
        bound: u32,
    },

    /// An unregistered lifecycle has no persisted status code.
    #[error("Unregistered lifecycle has no persisted status")]
    UnregisteredLifecycleNoStatus,

    /// The TEE expiry exclusion list exceeds the protocol cap.
    #[error("TEE expiry exclusions exceed protocol cap: {count} > {cap}")]
    TeeExpiryExclusionsExceedCap {
        /// Provided exclusion count.
        count: usize,
        /// Maximum allowed exclusion count.
        cap: usize,
    },

    /// The TEE expiry exclusion list contains zero or duplicate addresses.
    #[error("TEE expiry exclusions must contain unique non-zero validators")]
    TeeExpiryExclusionsNotUniqueNonZero,

    /// A TEE-expired validator also appears in the new active set.
    #[error("TEE-expired validator {address} is also present in the new active set")]
    TeeExpiredValidatorInNewActiveSet {
        /// Validator address.
        address: Address,
    },

    /// The TEE expiry exclusion list contains an unregistered validator.
    #[error("TEE expiry exclusions contain unregistered validator {address}")]
    TeeExpiryExclusionUnregistered {
        /// Validator address.
        address: Address,
    },

    /// The active set count exceeds `u32::MAX`.
    #[error("active set count exceeds u32")]
    ActiveSetCountExceedsU32,

    /// A validated boundary retained a validator that exited before the freeze height.
    #[error(
        "validated boundary retained validator {address} that exited at {changed_at} before freeze {freeze_height}"
    )]
    BoundaryRetainedValidatorExitedBeforeFreeze {
        /// Validator address.
        address: Address,
        /// Exit block height.
        changed_at: u64,
        /// Freeze block height.
        freeze_height: u64,
    },

    /// A validated boundary retained a validator jailed before the freeze height.
    #[error(
        "validated boundary retained validator {address} jailed at {jailed_at} before freeze {freeze_height}"
    )]
    BoundaryRetainedValidatorJailedBeforeFreeze {
        /// Validator address.
        address: Address,
        /// Jail block height.
        jailed_at: u64,
        /// Freeze block height.
        freeze_height: u64,
    },

    /// A validated boundary included an ineligible validator.
    #[error("validated boundary included ineligible validator {address} with status {status}")]
    BoundaryIncludedIneligibleValidator {
        /// Validator address.
        address: Address,
        /// Stored status code.
        status: u8,
    },

    /// Planned boundary participants do not match the certified active set.
    #[error(
        "validated boundary participant membership mismatch: planned {planned:?}, artifact {artifact:?}"
    )]
    BoundaryParticipantMembershipMismatch {
        /// Planned participant addresses.
        planned: Vec<Address>,
        /// Certified artifact addresses.
        artifact: Vec<Address>,
    },

    /// A TEE expiry exclusion targets a validator with an ineligible status.
    #[error("TEE expiry exclusion contains validator {address} with ineligible status {status}")]
    TeeExpiryExclusionIneligibleStatus {
        /// Validator address.
        address: Address,
        /// Stored status code.
        status: u8,
    },

    /// A validated boundary omitted an active validator.
    #[error("validated boundary omitted active validator {address}")]
    BoundaryOmittedActiveValidator {
        /// Validator address.
        address: Address,
    },

    /// A validated boundary omitted a validator that exited after the freeze height.
    #[error(
        "validated boundary omitted validator {address} that exited at {changed_at} after freeze {freeze_height}"
    )]
    BoundaryOmittedValidatorExitedAfterFreeze {
        /// Validator address.
        address: Address,
        /// Exit block height.
        changed_at: u64,
        /// Freeze block height.
        freeze_height: u64,
    },

    /// A validated boundary omitted a validator jailed after the freeze height.
    #[error(
        "validated boundary omitted validator {address} jailed at {jailed_at} after freeze {freeze_height}"
    )]
    BoundaryOmittedValidatorJailedAfterFreeze {
        /// Validator address.
        address: Address,
        /// Jail block height.
        jailed_at: u64,
        /// Freeze block height.
        freeze_height: u64,
    },

    /// An exiting validator has no recorded deactivation height.
    #[error("exiting validator {address} has no deactivation height")]
    ExitingValidatorMissingDeactivationHeight {
        /// Validator address.
        address: Address,
    },

    /// A validator deactivation height is zero.
    #[error("validator deactivation height must be non-zero")]
    ValidatorDeactivationHeightZero,

    /// An inactive validator has no recorded deactivation height.
    #[error("inactive validator {addr} has no deactivation height")]
    InactiveValidatorMissingDeactivationHeight {
        /// Validator address.
        addr: Address,
    },

    /// Computation of the inactive cleanup height overflowed `u64`.
    #[error("inactive cleanup height overflow")]
    InactiveCleanupHeightOverflow,

    /// The caller is neither the configuration owner nor the validator.
    #[error("unauthorized: caller must be owner or validator itself")]
    UnauthorizedOwnerOrValidator,

    /// The target validator is not registered.
    #[error("validator not registered")]
    ValidatorNotRegistered,

    /// The caller attempted to deactivate a validator that is not active.
    #[error("can only deactivate an active validator")]
    CanOnlyDeactivateActiveValidator,

    /// An active validator is missing its stake projection.
    #[error("active validator is missing stake projection")]
    ActiveValidatorMissingStakeProjection,

    /// The TEE expiry sweep selected a validator with an ineligible status.
    #[error("TEE expiry sweep selected validator {addr} with ineligible status {status}")]
    TeeExpirySweepIneligibleStatus {
        /// Validator address.
        addr: Address,
        /// Stored status code.
        status: u8,
    },

    /// A registered validator is missing its history record.
    #[error("registered validator is missing history")]
    RegisteredValidatorMissingHistory,

    /// A jailed validator remains retained in the current committee.
    #[error("jailed validator is still retained in the current committee")]
    JailedValidatorStillRetained,

    /// The caller attempted to unjail a validator that is not jailed.
    #[error("unjailValidator requires JAILED status, got {status}")]
    UnjailRequiresJailedStatus {
        /// Stored status code.
        status: u8,
    },

    /// The validator status does not allow punishment.
    #[error(
        "cannot {action} validator with status {status}: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed"
    )]
    CannotPunishValidatorStatus {
        /// Punishment action label.
        action: &'static str,
        /// Stored status code.
        status: u8,
    },

    /// The validator slash counter overflowed `u64`.
    #[error("slash count overflow")]
    SlashCountOverflow,

    /// The block proposer is not a current consensus participant.
    #[error("proposer is not a current consensus participant: {addr}")]
    ProposerNotConsensusParticipant {
        /// Proposer address.
        addr: Address,
    },

    /// A voter is not a current consensus participant.
    #[error("voter is not a current consensus participant: {addr}")]
    VoterNotConsensusParticipant {
        /// Voter address.
        addr: Address,
    },

    /// A finalized voter is not a registered validator.
    #[error("finalized voter is not a registered validator: {addr}")]
    FinalizedVoterNotRegistered {
        /// Voter address.
        addr: Address,
    },

    /// An absent voter is not a current consensus participant.
    #[error("absent voter is not a current consensus participant: {addr}")]
    AbsentVoterNotConsensusParticipant {
        /// Absent voter address.
        addr: Address,
    },

    /// A finalized absent voter is not a registered validator.
    #[error("finalized absent voter is not a registered validator: {addr}")]
    FinalizedAbsentVoterNotRegistered {
        /// Absent voter address.
        addr: Address,
    },

    /// The epoch number overflowed `U256`.
    #[error("epoch number overflow")]
    EpochNumberOverflow,

    /// A participation counter overflowed `u64`.
    #[error("{counter}")]
    ParticipationCounterOverflow {
        /// Overflow message for the specific counter.
        counter: &'static str,
    },

    /// An absent validator cannot project into a `ValidatorRecord`.
    #[error("cannot project absent validator into ValidatorRecord")]
    CannotProjectAbsentValidator,

    /// A registered validator is missing its consensus public key.
    #[error("registered validator is missing consensus public key")]
    RegisteredValidatorMissingConsensusPublicKey,

    /// The active validator count exceeds `u32::MAX`.
    #[error("active validator count exceeds u32")]
    ActiveValidatorCountExceedsU32,

    /// The registered validator count exceeds `u32::MAX`.
    #[error("registered validator count exceeds u32")]
    RegisteredValidatorCountExceedsU32,

    /// The active consensus count exceeds `u32::MAX`.
    #[error("active consensus count exceeds u32")]
    ActiveConsensusCountExceedsU32,

    /// A dense registry index below `validator_count` holds the zero address.
    #[error("validator registry index {index} is empty below validator_count {count}")]
    ValidatorRegistryIndexEmpty {
        /// One-based registry index.
        index: u64,
        /// Current validator count.
        count: u64,
    },

    /// The reverse index for a validator does not match its forward index.
    #[error(
        "validator registry reverse index mismatch for {address}: expected {expected}, got {actual}"
    )]
    ValidatorRegistryReverseIndexMismatch {
        /// Validator address.
        address: Address,
        /// Expected one-based index.
        expected: u64,
        /// Stored reverse index.
        actual: u64,
    },

    /// The stored epoch number exceeds `u64::MAX`.
    #[error("ValidatorSet.epoch_number exceeds u64")]
    EpochNumberExceedsU64,

    /// A registered validator is missing its consensus pubkey.
    #[error("registered validator is missing consensus pubkey")]
    RegisteredValidatorMissingConsensusPubkey,

    /// Computation of the validator identity hash failed.
    #[error("validator identity hash: {reason}")]
    ValidatorIdentityHashError {
        /// Underlying error text.
        reason: String,
    },

    /// Readiness confirmation requires a pending validator.
    #[error("confirmValidatorReady requires PENDING status, got {status}")]
    ConfirmValidatorReadyRequiresPending {
        /// Stored status code.
        status: u8,
    },

    /// A validator registry index exceeds `validator_count`.
    #[error("validator {addr} registry index {index} exceeds validator_count {count}")]
    ValidatorRegistryIndexExceedsCount {
        /// Validator address.
        addr: Address,
        /// Stored registry index.
        index: u64,
        /// Current validator count.
        count: u64,
    },

    /// The forward index mapping does not match the validator address.
    #[error(
        "validator registry forward index mismatch at {index}: expected {expected}, got {actual}"
    )]
    ValidatorRegistryForwardIndexMismatch {
        /// One-based registry index.
        index: u64,
        /// Expected validator address.
        expected: Address,
        /// Stored validator address.
        actual: Address,
    },

    /// The consensus pubkey reverse mapping does not match the validator address.
    #[error("validator consensus pubkey reverse mapping mismatch for {addr}: got {owner}")]
    ValidatorConsensusPubkeyReverseMismatch {
        /// Validator address.
        addr: Address,
        /// Stored reverse mapping owner.
        owner: Address,
    },

    /// A validator lifecycle transition attempted to change registry identity.
    #[error("validator lifecycle transition attempted to change registry identity")]
    LifecycleTransitionChangedRegistryIdentity,

    /// Generic validator persistence attempted to write an absent lifecycle.
    #[error("generic validator persistence cannot write an absent lifecycle")]
    CannotPersistAbsentLifecycle,

    /// A registered validator has no registry index.
    #[error("registered validator is missing registry index")]
    RegisteredValidatorMissingRegistryIndex,

    /// The BLS public key bytes are invalid.
    #[error("invalid BLS public key")]
    InvalidBlsPublicKey,

    /// The BLS signature bytes are invalid.
    #[error("invalid BLS signature")]
    InvalidBlsSignature,

    /// Verification of the BLS registration signature failed.
    #[error("invalid BLS registration signature")]
    InvalidBlsRegistrationSignature,

    /// The caller supplied an unsupported P2P address version.
    #[error("unsupported p2p address version {version}")]
    UnsupportedP2pAddressVersion {
        /// Provided P2P version byte.
        version: u8,
    },

    /// The encoded P2P address payload exceeds the maximum byte length.
    #[error("p2p address payload exceeds max length {max_len}")]
    P2pAddressPayloadTooLong {
        /// Maximum allowed byte length.
        max_len: usize,
    },

    /// Decoding of the versioned P2P address payload failed.
    #[error("invalid p2p address: {reason}")]
    InvalidP2pAddress {
        /// Underlying decode error text.
        reason: String,
    },

    /// The consensus public key is all zeros.
    #[error("consensus public key must not be zero")]
    ZeroConsensusPublicKey,

    /// The Radicle NodeId is zero.
    #[error("Radicle NodeId must not be zero")]
    ZeroRadicleNodeId,

    /// Another validator already registered this Radicle NodeId.
    #[error("Radicle NodeId already registered by another validator")]
    RadicleNodeIdAlreadyRegistered,

    /// Validator registration lacks a required BLS proof-of-possession signature.
    #[error("validator registration requires BLS proof-of-possession signature")]
    RegistrationRequiresBlsPop,

    /// Unstaked self-registered validators reached the protocol cap.
    #[error(
        "self-registration limit reached: too many unstaked REGISTERED validators (owner may register directly)"
    )]
    SelfRegistrationLimitReached,

    /// Another validator already registered this BLS consensus public key.
    #[error("BLS consensus pubkey already registered by another validator")]
    ConsensusPubkeyAlreadyRegistered,

    /// The validator is already registered and not inactive.
    #[error("validator already registered")]
    ValidatorAlreadyRegistered,

    /// An inactive validator attempted to change its Radicle NodeId before cleanup.
    #[error("inactive validator must keep its Radicle NodeId until final cleanup")]
    InactiveValidatorMustKeepRadicleNodeId,

    /// The Radicle NodeId reverse binding does not match the registered validator.
    #[error("registered validator has inconsistent Radicle NodeId reverse binding")]
    InconsistentRadicleNodeIdReverseBinding,

    /// Computation of the re-registration cooldown height overflowed `u64`.
    #[error("re-registration cooldown height overflow")]
    ReregistrationCooldownHeightOverflow,

    /// The re-registration cooldown period has not expired.
    #[error("re-registration cooldown not expired")]
    ReregistrationCooldownNotExpired,

    /// An unregistered validator still holds a Radicle NodeId reservation.
    #[error("unregistered validator retains a Radicle NodeId reservation")]
    UnregisteredValidatorRetainsRadicleReservation,

    /// The validator count reached the configured maximum.
    #[error("max validators reached")]
    MaxValidatorsReached,

    /// The validator count overflowed `u32`.
    #[error("validator count overflow")]
    ValidatorCountOverflow,

    /// The computed one-based validator registry index is zero.
    #[error("validator registry index must be non-zero")]
    ValidatorRegistryIndexZero,

    /// A validator attempted to increase stake while exiting or unbonding.
    #[error("cannot increase stake while validator is exiting or unbonding")]
    CannotIncreaseStakeWhileExitingOrUnbonding,

    /// The target validator is inactive.
    #[error("validator is inactive")]
    ValidatorIsInactive,

    /// Attempted to register a validator whose lifecycle is not absent.
    #[error("validator is already registered")]
    ValidatorIsAlreadyRegistered,

    /// A validator jail height is zero.
    #[error("validator jail height must be non-zero")]
    ValidatorJailHeightZero,

    /// Computation of the unjail cooldown height overflowed `u64`.
    #[error("unjail cooldown height overflow")]
    UnjailCooldownHeightOverflow,

    /// The unjail cooldown period has not elapsed.
    #[error("unjail cooldown not elapsed: ready at {ready_at}, current {current_height}")]
    UnjailCooldownNotElapsed {
        /// Earliest block height at which the validator can unjail.
        ready_at: u64,
        /// Current block height.
        current_height: u64,
    },

    /// Attempted to complete unbonding while bonded stake or a live-claim hint remains.
    #[error("cannot complete unbonding while bonded value or a live-claim hint remains")]
    CannotCompleteUnbondingWithRemainingStakeOrClaim,

    /// Attempted to stake for an unregistered validator.
    #[error("cannot stake before validator registration")]
    CannotStakeBeforeRegistration,

    /// Attempted to stake for an inactive validator before re-registration.
    #[error("inactive validator must re-register before staking")]
    InactiveValidatorMustReregisterBeforeStaking,

    /// A history record uses the zero sentinel as its deactivation height.
    #[error("deactivation height must not use the zero sentinel")]
    DeactivationHeightZeroSentinel,

    /// Active validator history contains a deactivation height.
    #[error("active history must not contain a deactivation height")]
    ActiveHistoryHasDeactivationHeight,

    /// Retained jailed validator history does not match the jail height.
    #[error("retained jail history must match the jail height")]
    RetainedJailHistoryHeightMismatch,

    /// Jailed validator history does not match the jail height.
    #[error("jail history must match the jail height")]
    JailHistoryHeightMismatch,

    /// Exiting validator history is missing a deactivation height.
    #[error("exiting history requires a deactivation height")]
    ExitingHistoryMissingDeactivationHeight,

    /// Unbonding validator history is missing a deactivation height.
    #[error("unbonding history requires a deactivation height")]
    UnbondingHistoryMissingDeactivationHeight,

    /// Inactive validator history is missing a deactivation height.
    #[error("inactive history requires a deactivation height")]
    InactiveHistoryMissingDeactivationHeight,

    /// Bonded stake is below the required minimum.
    #[error("bonded stake {bonded} is below minimum {minimum}")]
    BondedStakeBelowMinimum {
        /// Current bonded stake.
        bonded: alloy_primitives::U256,
        /// Required minimum stake.
        minimum: alloy_primitives::U256,
    },

    /// Bonded stake has not fallen below the required minimum.
    #[error("bonded stake {bonded} has not fallen below minimum {minimum}")]
    BondedStakeNotBelowMinimum {
        /// Current bonded stake.
        bonded: alloy_primitives::U256,
        /// Required minimum stake.
        minimum: alloy_primitives::U256,
    },

    /// A jailed validator attempted permanent exit while bonded stake remains.
    #[error("jailed validator must fully unstake before permanent exit")]
    JailedValidatorMustFullyUnstakeForExit,

    /// A stake projection uses the zero sentinel for `unbonding_end_hint`.
    #[error("unbonding-end hint must not use the zero sentinel")]
    UnbondingEndHintZeroSentinel,

    /// A registered validator is missing its stake projection.
    #[error("registered validator is missing stake projection")]
    RegisteredValidatorMissingStakeProjection,

    /// Test fixture attempted to activate a validator with an ineligible status.
    #[error(
        "cannot activate validator with status {status}: only REGISTERED or PENDING allowed in test fixtures"
    )]
    CannotActivateValidatorInTestFixture {
        /// Stored status code.
        status: u8,
    },

    /// Derivation of a test validator identity hash failed.
    #[error("cannot derive test validator identity: {reason}")]
    CannotDeriveTestValidatorIdentity {
        /// Underlying error text.
        reason: String,
    },

    /// Test target validator is not registered.
    #[error("test validator is not registered")]
    TestValidatorNotRegistered,

    /// Test boundary admission encountered an unsupported validator status.
    #[error("test boundary activation cannot admit validator status {status}")]
    TestBoundaryCannotAdmitStatus {
        /// Stored status code.
        status: u8,
    },

    /// A test validator disappeared during boundary activation setup.
    #[error("test validator disappeared")]
    TestValidatorDisappeared,

    /// Test fixture is not in PENDING status before boundary promotion.
    #[error("fixture {validator} is not PENDING before promotion")]
    FixtureNotPendingBeforePromotion {
        /// Validator address.
        validator: Address,
    },

    /// Test fixture is not in ACTIVE status after boundary promotion.
    #[error("fixture {validator} is not ACTIVE after promotion")]
    FixtureNotActiveAfterPromotion {
        /// Validator address.
        validator: Address,
    },

    /// Test fixture activation stake is below the required minimum.
    #[error("fixture activation stake {bonded} is below minimum {minimum}")]
    FixtureActivationStakeBelowMinimum {
        /// Provided bonded stake.
        bonded: alloy_primitives::U256,
        /// Required minimum stake.
        minimum: alloy_primitives::U256,
    },

    /// Test fixture canonical activation requires a join-path lifecycle.
    #[error("fixture activation requires a join-path lifecycle, got status {status:?}")]
    FixtureActivationRequiresJoinPath {
        /// Stored status code if present.
        status: Option<u8>,
    },

    /// Setting test fixture history requires a registered validator.
    #[error("test fixture history requires a registered validator")]
    TestFixtureHistoryRequiresRegisteredValidator,
}

impl ValidatorSetError {
    /// Returns true when the error represents fatal state corruption or invariant failure.
    pub const fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::CommitteeSnapshotReplayMismatch { .. }
                | Self::RegisteredValidatorMissingStakeProjection
                | Self::CannotDeriveTestValidatorIdentity { .. }
                | Self::TestValidatorDisappeared
                | Self::TestFixtureHistoryRequiresRegisteredValidator
        )
    }

    /// Builds a revert error for an invalid P2P address payload.
    pub fn invalid_p2p_address(error: impl std::fmt::Display) -> PrecompileError {
        Self::InvalidP2pAddress {
            reason: error.to_string(),
        }
        .into()
    }
}

impl From<ValidatorSetError> for PrecompileError {
    fn from(err: ValidatorSetError) -> Self {
        if err.is_fatal() {
            Self::Fatal(err.to_string())
        } else {
            Self::Revert(err.to_string())
        }
    }
}
