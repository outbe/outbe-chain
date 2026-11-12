//! Maps validator-set errors onto precompile failures.

use super::ValidatorSetError;
use crate::precompile::IValidatorSet;
use alloy_primitives::Bytes;
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;

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
    use alloy_primitives::Address;

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
