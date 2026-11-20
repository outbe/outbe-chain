//! Typed resident verification and NodeHost record failures.

use crate::dcap_protocol::DcapRejectCodeV1;
use thiserror::Error;

use super::TransportError;

/// A resident verifier response or request failed a consensus-critical check.
#[derive(Debug, Error)]
pub enum TeeVerificationError {
    #[error("invalid verifier input dimensions: {:#06x}", .code.code())]
    InvalidVerifierInputDimensions { code: DcapRejectCodeV1 },
    #[error("invalid onboarding verifier input dimensions: {:#06x}", .code.code())]
    InvalidOnboardingVerifierInputDimensions { code: DcapRejectCodeV1 },
    #[error("enclave response outcome is non-canonical: {:#06x}", .code.code())]
    ResponseOutcomeNonCanonical { code: DcapRejectCodeV1 },
    #[error("enclave response outcome is oversized: {:#06x}", .code.code())]
    ResponseOutcomeOversized { code: DcapRejectCodeV1 },
    #[error("invalid enclave attestation key: {0}")]
    InvalidEnclaveAttestationKey(#[source] ed25519_dalek::SignatureError),
    #[error("invalid enclave verification tag length {0}")]
    InvalidVerificationTagLength(usize),
    #[error("enclave verification tag is invalid: {0}")]
    InvalidVerificationTag(#[source] ed25519_dalek::SignatureError),
    #[error("enclave onboarding response is oversized: {:#06x}", .code.code())]
    OnboardingResponseOversized { code: DcapRejectCodeV1 },
    #[error("invalid enclave onboarding tag length {0}")]
    InvalidOnboardingTagLength(usize),
    #[error("enclave onboarding tag is invalid: {0}")]
    InvalidOnboardingTag(#[source] ed25519_dalek::SignatureError),
    #[error("enclave onboarding outcome is non-canonical: {:#06x}", .code.code())]
    OnboardingOutcomeNonCanonical { code: DcapRejectCodeV1 },
    #[error("enclave onboarding artifact is non-canonical: {:#06x}", .code.code())]
    OnboardingArtifactNonCanonical { code: DcapRejectCodeV1 },
    #[error("resident verifier response request commitment mismatch")]
    VerificationCommitmentMismatch,
    #[error("resident onboarding response request commitment mismatch")]
    OnboardingCommitmentMismatch,
    #[error("resident onboarding outcome/artifact combination is invalid")]
    InvalidOnboardingOutcomeArtifactPair,
    #[error("DirectDev onboarding artifact is non-canonical: {:#06x}", .code.code())]
    DirectDevArtifactNonCanonical { code: DcapRejectCodeV1 },
    #[error("DirectDev response attestation failed: {0}")]
    DirectDevAttestation(#[source] crate::tee_call::TeeAttestationError),
}

/// Errors returned during NodeHost filesystem and record encoding/decoding.
#[derive(Debug, Error)]
pub enum NodeHostRecordError {
    #[error("NodeHost key path must be a regular non-symlink file")]
    IdentityKeyNotRegular,
    #[error("NodeHost key file mode must be exactly 0600")]
    IdentityKeyMode,
    #[error("NodeHost key file must be owned by the current effective user")]
    IdentityKeyOwner,
    #[error("NodeHost key must be exactly 32 bytes")]
    IdentityKeyLength,

    #[error("NodeHost {label} must be an owner-only bounded regular file")]
    NotOwnerOnlyBoundedRegularFile { label: &'static str },
    #[error("NodeHost {label} read limit overflow")]
    ReadLimitOverflow { label: &'static str },
    #[error("NodeHost {label} length overflow")]
    LengthOverflow { label: &'static str },
    #[error("NodeHost {label} grew beyond its byte bound while being read")]
    GrewBeyondByteBound { label: &'static str },
    #[error("durable NodeHost {label} conflicts with the requested value")]
    DurableRecordConflict { label: &'static str },
    #[error("finalized join chain id")]
    FinalizedJoinChainId,
    #[error("finalized join height")]
    FinalizedJoinHeight,
    #[error("finalized join timestamp")]
    FinalizedJoinTimestamp,
    #[error("committed join evidence length")]
    CommittedJoinEvidenceLength,
    #[error("committed join evidence length overflow")]
    CommittedJoinEvidenceLengthOverflow,
    #[error("promoted manifest is missing its authorization receipt")]
    PromotedManifestMissingAuthorizationReceipt,
    #[error("missing key-ready proof")]
    MissingKeyReadyProof,
    #[error("transition evidence is missing its key-ready proof")]
    TransitionEvidenceMissingKeyReadyProof,
    #[error("transition proof missing")]
    TransitionProofMissing,
    #[error("replacement evidence length overflow")]
    ReplacementEvidenceLengthOverflow,
    #[error("replacement candidate manifest exceeds its wire field")]
    ReplacementCandidateManifestExceedsWireField,
}

impl From<NodeHostRecordError> for TransportError {
    fn from(error: NodeHostRecordError) -> Self {
        match error {
            NodeHostRecordError::MissingKeyReadyProof => Self::NodeHostAttestation(error),
            _ => Self::NodeHostRecord(error),
        }
    }
}
