//! Typed, public-only finality rejection reasons; never format cryptographic inputs.
use super::Result;
pub use verifier::{FinalizedAdmissionVerifierV1, VerifiedAdmissionAnchorV1};
mod onboarding;
mod upgrade;
pub mod verifier;
pub use onboarding::{
    DcapOnboardingResultV1, DcapRegistrationSealV1, FinalizedOnboardingV1,
    PreparedOnboardingArtifactV2,
};
#[derive(Debug, thiserror::Error)]
/// Public finality and Registry proof rejection reasons with safe typed causes.
pub enum AdmissionError {
    #[error("finalized admission rejected: AdmissionEnvelopeOrOrder")]
    AdmissionEnvelopeOrOrder,
    #[error("finalized admission rejected: AdmissionFinalization")]
    AdmissionFinalization(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: AdmissionSkipsCommittee")]
    AdmissionSkipsCommittee,
    #[error("finalized admission rejected: AdmissionWitnessCodec")]
    AdmissionWitnessCodec(#[source] crate::finalized_admission::FinalizedAdmissionCodecError),
    #[error("finalized admission rejected: CanonicalHeaderRlp")]
    CanonicalHeaderRlp(#[source] alloy_rlp::Error),
    #[error("finalized admission rejected: CommitteeFinalityHeightOverflow")]
    CommitteeFinalityHeightOverflow,
    #[error("finalized admission rejected: ConsensusChainBinding")]
    ConsensusChainBinding(#[source] outbe_primitives::proof::constants::ConsensusChainIdError),
    #[error("finalized admission rejected: ConsensusChainIdOverflow")]
    ConsensusChainIdOverflow,
    #[error("finalized admission rejected: Epoch0NotRegistered")]
    Epoch0NotRegistered,
    #[error("finalized admission rejected: Epoch0Anchor")]
    Epoch0Anchor(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: FinalityAncestryNotConsecutive")]
    FinalityAncestryNotConsecutive,
    #[error("finalized admission rejected: FinalizationPayloadMismatch")]
    FinalizationPayloadMismatch,
    #[error("finalized admission rejected: GenesisCommittee")]
    GenesisCommittee,
    #[error("finalized admission rejected: GenesisConsensusKey")]
    GenesisConsensusKey(#[source] commonware_codec::Error),
    #[error("finalized admission rejected: GenesisConsensusKeyTrailingBytes")]
    GenesisConsensusKeyTrailingBytes,
    #[error("finalized admission rejected: HeaderTrailingBytes")]
    HeaderTrailingBytes,
    #[error("finalized admission rejected: MissingUpgradeSlot")]
    MissingUpgradeSlot,
    #[error("finalized admission rejected: NetworkIdentityNotReleaseMeasured")]
    NetworkIdentityNotReleaseMeasured,
    #[error("finalized admission rejected: PublicFinalization")]
    PublicFinalization(#[source] outbe_simplex::committee_chain::PublicFinalizedBlockDecodeError),
    #[error("finalized admission rejected: RegistryAccountProof")]
    RegistryAccountProof(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: RegistryLacksLeaseExpiry")]
    RegistryLacksLeaseExpiry,
    #[error("finalized admission rejected: RegistryLeaseNotLive")]
    RegistryLeaseNotLive,
    #[error("finalized admission rejected: RegistryMissingSlot")]
    RegistryMissingSlot,
    #[error("finalized admission rejected: RegistrySlotCount")]
    RegistrySlotCount,
    #[error("finalized admission rejected: RegistryStorageProof")]
    RegistryStorageProof(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: RegistryValueMismatch")]
    RegistryValueMismatch,
    #[error("finalized admission rejected: SuccessorCommittee")]
    SuccessorCommittee(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: TransitionArtifacts")]
    TransitionArtifacts(#[source] outbe_primitives::error::PrecompileError),
    #[error("finalized admission rejected: TransitionCodec")]
    TransitionCodec(#[source] crate::finalized_admission::FinalizedAdmissionCodecError),
    #[error("finalized admission rejected: TransitionEnvelopeOrOrder")]
    TransitionEnvelopeOrOrder,
    #[error("finalized admission rejected: TransitionFinalization")]
    TransitionFinalization(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("finalized admission rejected: TransitionLacksPreAnnounce")]
    TransitionLacksPreAnnounce,
    #[error("finalized admission rejected: TransitionNotCurrentCommittee")]
    TransitionNotCurrentCommittee,
    #[error("finalized admission rejected: TransitionNotImmediateSuccessor")]
    TransitionNotImmediateSuccessor,
    #[error("finalized admission rejected: UpgradeAuthorizationAbsentOrExpired")]
    UpgradeAuthorizationAbsentOrExpired,
}
