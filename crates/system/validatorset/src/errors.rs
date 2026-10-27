//! Domain error types for the validator set precompile and runtime.

use crate::precompile::IValidatorSet;
use alloy_primitives::Bytes;
use alloy_primitives::{Address, B256};
use alloy_sol_types::SolError;
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
    fn from(error: ActivationError) -> Self {
        match error {
            ActivationError::VrfVersionOverflow => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::VrfVersionOverflow {}.abi_encode(),
            )),
        }
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
    fn from(error: ValidatorSetError) -> Self {
        match error {
            ValidatorSetError::UnsupportedDelegateRole => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnsupportedDelegateRole {}.abi_encode()),
            ),
            ValidatorSetError::ZeroDelegateAddress => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ZeroDelegateAddress {}.abi_encode(),
            )),
            ValidatorSetError::DelegateMatchesValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::DelegateMatchesValidator {}.abi_encode()),
            ),
            ValidatorSetError::DelegateIsRegisteredValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::DelegateIsRegisteredValidator {}.abi_encode()),
            ),
            ValidatorSetError::DelegateAlreadyAssigned => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::DelegateAlreadyAssigned {}.abi_encode()),
            ),
            ValidatorSetError::CallerNotRegisteredValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::CallerNotRegisteredValidator {}.abi_encode()),
            ),
            ValidatorSetError::ValidatorAlreadyOperationalDelegate => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorAlreadyOperationalDelegate {}.abi_encode()),
            ),
            ValidatorSetError::FinalizedParticipationRingSeqOverflow => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::FinalizedParticipationRingSeqOverflow {}.abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorNotFound => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorNotFound {}.abi_encode(),
            )),
            ValidatorSetError::ValidatorNotFoundAtIndex => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorNotFoundAtIndex {}.abi_encode()),
            ),
            ValidatorSetError::ConsensusPubkeyWrongLength => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ConsensusPubkeyWrongLength {}.abi_encode()),
            ),
            ValidatorSetError::ConsensusPubkeyConversionFailed => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ConsensusPubkeyConversionFailed {}.abi_encode()),
            ),
            ValidatorSetError::BlsPopWrongLength => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BlsPopWrongLength {}.abi_encode(),
            )),
            ValidatorSetError::BlsSignatureConversionFailed => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::BlsSignatureConversionFailed {}.abi_encode()),
            ),
            ValidatorSetError::CommitteeSnapshotLengthExceedsU64 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::CommitteeSnapshotLengthExceedsU64 {}.abi_encode()),
            ),
            ValidatorSetError::VrfGroupPkBytesLengthExceedsU64 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::VrfGroupPkBytesLengthExceedsU64 {}.abi_encode()),
            ),
            ValidatorSetError::SnapshotExistsFlagWithoutRecord => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::SnapshotExistsFlagWithoutRecord {}.abi_encode()),
            ),
            error @ ValidatorSetError::CommitteeSnapshotReplayMismatch { key: _ } => {
                PrecompileError::Fatal(error.to_string())
            }
            ValidatorSetError::VrfGroupPkLengthExceedsUsize => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::VrfGroupPkLengthExceedsUsize {}.abi_encode()),
            ),
            ValidatorSetError::UnknownValidatorStatus { status } => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnknownValidatorStatus { status }.abi_encode()),
            ),
            ValidatorSetError::CorruptValidatorState {
                address: address_value,
                detail,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::CorruptValidatorState {
                    account: address_value,
                    detail: detail.to_string(),
                }
                .abi_encode(),
            )),
            ValidatorSetError::MaxValidatorsExceedsConsensusBound {
                max_validators,
                bound,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::MaxValidatorsExceedsConsensusBound {
                    max_validators,
                    bound,
                }
                .abi_encode(),
            )),
            ValidatorSetError::UnregisteredLifecycleNoStatus => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnregisteredLifecycleNoStatus {}.abi_encode()),
            ),
            ValidatorSetError::TeeExpiryExclusionsExceedCap { count, cap } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::TeeExpiryExclusionsExceedCap {
                        count: count as u64,
                        cap: cap as u64,
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::TeeExpiryExclusionsNotUniqueNonZero => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::TeeExpiryExclusionsNotUniqueNonZero {}.abi_encode()),
            ),
            ValidatorSetError::TeeExpiredValidatorInNewActiveSet {
                address: address_value,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::TeeExpiredValidatorInNewActiveSet {
                    account: address_value,
                }
                .abi_encode(),
            )),
            ValidatorSetError::TeeExpiryExclusionUnregistered {
                address: address_value,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::TeeExpiryExclusionUnregistered {
                    account: address_value,
                }
                .abi_encode(),
            )),
            ValidatorSetError::ActiveSetCountExceedsU32 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ActiveSetCountExceedsU32 {}.abi_encode()),
            ),
            ValidatorSetError::BoundaryRetainedValidatorExitedBeforeFreeze {
                address: address_value,
                changed_at,
                freeze_height,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryRetainedValidatorExitedBeforeFreeze {
                    account: address_value,
                    changed_at,
                    freeze_height,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryRetainedValidatorJailedBeforeFreeze {
                address: address_value,
                jailed_at,
                freeze_height,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryRetainedValidatorJailedBeforeFreeze {
                    account: address_value,
                    jailed_at,
                    freeze_height,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryIncludedIneligibleValidator {
                address: address_value,
                status,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryIncludedIneligibleValidator {
                    account: address_value,
                    status,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryParticipantMembershipMismatch { planned, artifact } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::BoundaryParticipantMembershipMismatch { planned, artifact }
                        .abi_encode(),
                ))
            }
            ValidatorSetError::TeeExpiryExclusionIneligibleStatus {
                address: address_value,
                status,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::TeeExpiryExclusionIneligibleStatus {
                    account: address_value,
                    status,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryOmittedActiveValidator {
                address: address_value,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryOmittedActiveValidator {
                    account: address_value,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryOmittedValidatorExitedAfterFreeze {
                address: address_value,
                changed_at,
                freeze_height,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryOmittedValidatorExitedAfterFreeze {
                    account: address_value,
                    changed_at,
                    freeze_height,
                }
                .abi_encode(),
            )),
            ValidatorSetError::BoundaryOmittedValidatorJailedAfterFreeze {
                address: address_value,
                jailed_at,
                freeze_height,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::BoundaryOmittedValidatorJailedAfterFreeze {
                    account: address_value,
                    jailed_at,
                    freeze_height,
                }
                .abi_encode(),
            )),
            ValidatorSetError::ExitingValidatorMissingDeactivationHeight {
                address: address_value,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ExitingValidatorMissingDeactivationHeight {
                    account: address_value,
                }
                .abi_encode(),
            )),
            ValidatorSetError::ValidatorDeactivationHeightZero => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorDeactivationHeightZero {}.abi_encode()),
            ),
            ValidatorSetError::InactiveValidatorMissingDeactivationHeight { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InactiveValidatorMissingDeactivationHeight { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::InactiveCleanupHeightOverflow => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::InactiveCleanupHeightOverflow {}.abi_encode()),
            ),
            ValidatorSetError::UnauthorizedOwnerOrValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnauthorizedOwnerOrValidator {}.abi_encode()),
            ),
            ValidatorSetError::ValidatorNotRegistered => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorNotRegistered {}.abi_encode(),
            )),
            ValidatorSetError::CanOnlyDeactivateActiveValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::CanOnlyDeactivateActiveValidator {}.abi_encode()),
            ),
            ValidatorSetError::ActiveValidatorMissingStakeProjection => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ActiveValidatorMissingStakeProjection {}.abi_encode(),
                ))
            }
            ValidatorSetError::TeeExpirySweepIneligibleStatus { addr, status } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::TeeExpirySweepIneligibleStatus { addr, status }.abi_encode(),
                ))
            }
            ValidatorSetError::RegisteredValidatorMissingHistory => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::RegisteredValidatorMissingHistory {}.abi_encode()),
            ),
            ValidatorSetError::JailedValidatorStillRetained => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::JailedValidatorStillRetained {}.abi_encode()),
            ),
            ValidatorSetError::UnjailRequiresJailedStatus { status } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::UnjailRequiresJailedStatus { status }.abi_encode(),
                ))
            }
            ValidatorSetError::CannotPunishValidatorStatus { action, status } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::CannotPunishValidatorStatus {
                        action: action.to_string(),
                        status,
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::SlashCountOverflow => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::SlashCountOverflow {}.abi_encode(),
            )),
            ValidatorSetError::ProposerNotConsensusParticipant { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ProposerNotConsensusParticipant { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::VoterNotConsensusParticipant { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::VoterNotConsensusParticipant { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::FinalizedVoterNotRegistered { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::FinalizedVoterNotRegistered { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::AbsentVoterNotConsensusParticipant { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::AbsentVoterNotConsensusParticipant { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::FinalizedAbsentVoterNotRegistered { addr } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::FinalizedAbsentVoterNotRegistered { addr }.abi_encode(),
                ))
            }
            ValidatorSetError::EpochNumberOverflow => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::EpochNumberOverflow {}.abi_encode(),
            )),
            ValidatorSetError::ParticipationCounterOverflow { counter } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ParticipationCounterOverflow {
                        counter: counter.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::CannotProjectAbsentValidator => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::CannotProjectAbsentValidator {}.abi_encode()),
            ),
            ValidatorSetError::RegisteredValidatorMissingConsensusPublicKey => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::RegisteredValidatorMissingConsensusPublicKey {}.abi_encode(),
                ))
            }
            ValidatorSetError::ActiveValidatorCountExceedsU32 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ActiveValidatorCountExceedsU32 {}.abi_encode()),
            ),
            ValidatorSetError::RegisteredValidatorCountExceedsU32 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::RegisteredValidatorCountExceedsU32 {}.abi_encode()),
            ),
            ValidatorSetError::ActiveConsensusCountExceedsU32 => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ActiveConsensusCountExceedsU32 {}.abi_encode()),
            ),
            ValidatorSetError::ValidatorRegistryIndexEmpty { index, count } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ValidatorRegistryIndexEmpty { index, count }.abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorRegistryReverseIndexMismatch {
                address: address_value,
                expected,
                actual,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorRegistryReverseIndexMismatch {
                    account: address_value,
                    expected,
                    actual,
                }
                .abi_encode(),
            )),
            ValidatorSetError::EpochNumberExceedsU64 => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::EpochNumberExceedsU64 {}.abi_encode(),
            )),
            ValidatorSetError::RegisteredValidatorMissingConsensusPubkey => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::RegisteredValidatorMissingConsensusPubkey {}.abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorIdentityHashError { reason } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ValidatorIdentityHashError {
                        reason: reason.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::ConfirmValidatorReadyRequiresPending { status } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ConfirmValidatorReadyRequiresPending { status }.abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorRegistryIndexExceedsCount { addr, index, count } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ValidatorRegistryIndexExceedsCount { addr, index, count }
                        .abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorRegistryForwardIndexMismatch {
                index,
                expected,
                actual,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorRegistryForwardIndexMismatch {
                    index,
                    expected,
                    actual,
                }
                .abi_encode(),
            )),
            ValidatorSetError::ValidatorConsensusPubkeyReverseMismatch { addr, owner } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ValidatorConsensusPubkeyReverseMismatch { addr, owner }
                        .abi_encode(),
                ))
            }
            ValidatorSetError::LifecycleTransitionChangedRegistryIdentity => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::LifecycleTransitionChangedRegistryIdentity {}.abi_encode(),
                ))
            }
            ValidatorSetError::CannotPersistAbsentLifecycle => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::CannotPersistAbsentLifecycle {}.abi_encode()),
            ),
            ValidatorSetError::RegisteredValidatorMissingRegistryIndex => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::RegisteredValidatorMissingRegistryIndex {}.abi_encode(),
                ))
            }
            ValidatorSetError::InvalidBlsPublicKey => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::InvalidBlsPublicKey {}.abi_encode(),
            )),
            ValidatorSetError::InvalidBlsSignature => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::InvalidBlsSignature {}.abi_encode(),
            )),
            ValidatorSetError::InvalidBlsRegistrationSignature => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::InvalidBlsRegistrationSignature {}.abi_encode()),
            ),
            ValidatorSetError::UnsupportedP2pAddressVersion { version } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::UnsupportedP2pAddressVersion { version }.abi_encode(),
                ))
            }
            ValidatorSetError::P2pAddressPayloadTooLong { max_len } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::P2pAddressPayloadTooLong {
                        max_len: max_len as u64,
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::InvalidP2pAddress { reason } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InvalidP2pAddress {
                        reason: reason.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            ValidatorSetError::ZeroConsensusPublicKey => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ZeroConsensusPublicKey {}.abi_encode(),
            )),
            ValidatorSetError::ZeroRadicleNodeId => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ZeroRadicleNodeId {}.abi_encode(),
            )),
            ValidatorSetError::RadicleNodeIdAlreadyRegistered => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::RadicleNodeIdAlreadyRegistered {}.abi_encode()),
            ),
            ValidatorSetError::RegistrationRequiresBlsPop => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::RegistrationRequiresBlsPop {}.abi_encode()),
            ),
            ValidatorSetError::ConsensusPubkeyAlreadyRegistered => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ConsensusPubkeyAlreadyRegistered {}.abi_encode()),
            ),
            ValidatorSetError::ValidatorAlreadyRegistered => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorAlreadyRegistered {}.abi_encode()),
            ),
            ValidatorSetError::InactiveValidatorMustKeepRadicleNodeId => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InactiveValidatorMustKeepRadicleNodeId {}.abi_encode(),
                ))
            }
            ValidatorSetError::InconsistentRadicleNodeIdReverseBinding => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InconsistentRadicleNodeIdReverseBinding {}.abi_encode(),
                ))
            }
            ValidatorSetError::ReregistrationCooldownHeightOverflow => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ReregistrationCooldownHeightOverflow {}.abi_encode(),
                ))
            }
            ValidatorSetError::ReregistrationCooldownNotExpired => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ReregistrationCooldownNotExpired {}.abi_encode()),
            ),
            ValidatorSetError::UnregisteredValidatorRetainsRadicleReservation => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::UnregisteredValidatorRetainsRadicleReservation {}.abi_encode(),
                ))
            }
            ValidatorSetError::MaxValidatorsReached => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::MaxValidatorsReached {}.abi_encode(),
            )),
            ValidatorSetError::ValidatorCountOverflow => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorCountOverflow {}.abi_encode(),
            )),
            ValidatorSetError::ValidatorRegistryIndexZero => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorRegistryIndexZero {}.abi_encode()),
            ),
            ValidatorSetError::CannotIncreaseStakeWhileExitingOrUnbonding => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::CannotIncreaseStakeWhileExitingOrUnbonding {}.abi_encode(),
                ))
            }
            ValidatorSetError::ValidatorIsInactive => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::ValidatorIsInactive {}.abi_encode(),
            )),
            ValidatorSetError::ValidatorIsAlreadyRegistered => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorIsAlreadyRegistered {}.abi_encode()),
            ),
            ValidatorSetError::ValidatorJailHeightZero => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ValidatorJailHeightZero {}.abi_encode()),
            ),
            ValidatorSetError::UnjailCooldownHeightOverflow => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnjailCooldownHeightOverflow {}.abi_encode()),
            ),
            ValidatorSetError::UnjailCooldownNotElapsed {
                ready_at,
                current_height,
            } => PrecompileError::RevertBytes(Bytes::from(
                IValidatorSet::UnjailCooldownNotElapsed {
                    ready_at,
                    current_height,
                }
                .abi_encode(),
            )),
            ValidatorSetError::CannotCompleteUnbondingWithRemainingStakeOrClaim => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::CannotCompleteUnbondingWithRemainingStakeOrClaim {}.abi_encode(),
                ))
            }
            ValidatorSetError::InactiveValidatorMustReregisterBeforeStaking => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InactiveValidatorMustReregisterBeforeStaking {}.abi_encode(),
                ))
            }
            ValidatorSetError::DeactivationHeightZeroSentinel => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::DeactivationHeightZeroSentinel {}.abi_encode()),
            ),
            ValidatorSetError::ActiveHistoryHasDeactivationHeight => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::ActiveHistoryHasDeactivationHeight {}.abi_encode()),
            ),
            ValidatorSetError::RetainedJailHistoryHeightMismatch => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::RetainedJailHistoryHeightMismatch {}.abi_encode()),
            ),
            ValidatorSetError::JailHistoryHeightMismatch => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::JailHistoryHeightMismatch {}.abi_encode()),
            ),
            ValidatorSetError::ExitingHistoryMissingDeactivationHeight => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::ExitingHistoryMissingDeactivationHeight {}.abi_encode(),
                ))
            }
            ValidatorSetError::UnbondingHistoryMissingDeactivationHeight => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::UnbondingHistoryMissingDeactivationHeight {}.abi_encode(),
                ))
            }
            ValidatorSetError::InactiveHistoryMissingDeactivationHeight => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::InactiveHistoryMissingDeactivationHeight {}.abi_encode(),
                ))
            }
            ValidatorSetError::BondedStakeBelowMinimum { bonded, minimum } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::BondedStakeBelowMinimum { bonded, minimum }.abi_encode(),
                ))
            }
            ValidatorSetError::BondedStakeNotBelowMinimum { bonded, minimum } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::BondedStakeNotBelowMinimum { bonded, minimum }.abi_encode(),
                ))
            }
            ValidatorSetError::JailedValidatorMustFullyUnstakeForExit => {
                PrecompileError::RevertBytes(Bytes::from(
                    IValidatorSet::JailedValidatorMustFullyUnstakeForExit {}.abi_encode(),
                ))
            }
            ValidatorSetError::UnbondingEndHintZeroSentinel => PrecompileError::RevertBytes(
                Bytes::from(IValidatorSet::UnbondingEndHintZeroSentinel {}.abi_encode()),
            ),
            error @ ValidatorSetError::RegisteredValidatorMissingStakeProjection => {
                PrecompileError::Fatal(error.to_string())
            }
            error @ (ValidatorSetError::CannotActivateValidatorInTestFixture { .. }
            | ValidatorSetError::TestValidatorNotRegistered
            | ValidatorSetError::TestBoundaryCannotAdmitStatus { .. }
            | ValidatorSetError::FixtureNotPendingBeforePromotion { .. }
            | ValidatorSetError::FixtureNotActiveAfterPromotion { .. }
            | ValidatorSetError::FixtureActivationStakeBelowMinimum { .. }
            | ValidatorSetError::FixtureActivationRequiresJoinPath { .. }) => {
                PrecompileError::Revert(error.to_string())
            }
            error @ (ValidatorSetError::CannotDeriveTestValidatorIdentity { .. }
            | ValidatorSetError::TestValidatorDisappeared) => {
                PrecompileError::Fatal(error.to_string())
            }
            error @ ValidatorSetError::TestFixtureHistoryRequiresRegisteredValidator => {
                PrecompileError::Fatal(error.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(err: ValidatorSetError) -> Bytes {
        match PrecompileError::from(err) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    }

    #[test]
    fn caller_revert_keeps_address_list_and_string_fields() {
        let account = Address::repeat_byte(0x11);
        let bytes = encoded(ValidatorSetError::CorruptValidatorState {
            address: account,
            detail: "stake".to_string(),
        });
        let decoded =
            IValidatorSet::CorruptValidatorState::abi_decode(&bytes).expect("state payload");
        assert_eq!(decoded.account, account);
        assert_eq!(decoded.detail, "stake");

        let planned = vec![account];
        let artifact = vec![Address::repeat_byte(0x22)];
        let bytes = encoded(ValidatorSetError::BoundaryParticipantMembershipMismatch {
            planned: planned.clone(),
            artifact: artifact.clone(),
        });
        let decoded = IValidatorSet::BoundaryParticipantMembershipMismatch::abi_decode(&bytes)
            .expect("membership payload");
        assert_eq!(decoded.planned, planned);
        assert_eq!(decoded.artifact, artifact);

        assert!(matches!(
            PrecompileError::from(ValidatorSetError::TestValidatorDisappeared),
            PrecompileError::Fatal(_)
        ));
    }
}
