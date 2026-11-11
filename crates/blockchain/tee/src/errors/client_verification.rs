//! Typed errors for client verification and NodeHost filesystem/record operations.

use thiserror::Error;

use super::TransportError;

/// Errors returned by enclave client verification and stream framing in [`crate::client`].
#[derive(Debug, Error)]
pub enum TeeClientVerificationError {
    #[error("stream has no call context")]
    StreamMissingCallContext,
    #[error("enclave session is not configured")]
    EnclaveSessionNotConfigured,
    #[error("trailing enclave call bytes")]
    TrailingEnclaveCallBytes,
    #[error("trailing legacy request bytes")]
    TrailingLegacyRequestBytes,
    #[error("TEE sidecar unavailable")]
    TeeSidecarUnavailable,
    #[error("TEE sidecar is not configured")]
    TeeSidecarNotConfigured,
    #[error("invalid Tribute day predecessor version")]
    InvalidTributeDayPredecessorVersion,
    #[error("finalized admission proof offset exceeds u32")]
    FinalizedAdmissionProofOffsetExceedsU32,
    #[error("finalized admission proof offset overflow")]
    FinalizedAdmissionProofOffsetOverflow,
    #[error("invalid verifier input dimensions: {code:#06x}")]
    InvalidVerifierInputDimensions { code: u16 },
    #[error("evidence length exceeds u32")]
    EvidenceLengthExceedsU32,
    #[error("policy length exceeds u32")]
    PolicyLengthExceedsU32,
    #[error("invalid onboarding verifier input dimensions: {code:#06x}")]
    InvalidOnboardingVerifierInputDimensions { code: u16 },
    #[error("GramineDirectDev onboarding artifact is non-canonical: {code:#06x}")]
    GramineDirectDevOnboardingArtifactNonCanonical { code: u16 },
    #[error("verification offset exceeds u32")]
    VerificationOffsetExceedsU32,
    #[error("verification offset overflow")]
    VerificationOffsetOverflow,
    #[error("system time precedes Unix epoch")]
    SystemTimePrecedesUnixEpoch,
    #[error("enclave response outcome is non-canonical: {code:#06x}")]
    ResponseOutcomeNonCanonical { code: u16 },
    #[error("enclave response outcome is oversized: {code:#06x}")]
    ResponseOutcomeOversized { code: u16 },
    #[error("invalid enclave attestation key: {0}")]
    InvalidEnclaveAttestationKey(ed25519_dalek::SignatureError),
    #[error("invalid enclave verification tag length {0}")]
    InvalidVerificationTagLength(usize),
    #[error("enclave verification tag is invalid: {0}")]
    InvalidVerificationTag(ed25519_dalek::SignatureError),
    #[error("enclave onboarding response is oversized: {code:#06x}")]
    OnboardingResponseOversized { code: u16 },
    #[error("invalid enclave onboarding tag length {0}")]
    InvalidOnboardingTagLength(usize),
    #[error("enclave onboarding tag is invalid: {0}")]
    InvalidOnboardingTag(ed25519_dalek::SignatureError),
    #[error("enclave onboarding outcome is non-canonical: {code:#06x}")]
    OnboardingOutcomeNonCanonical { code: u16 },
    #[error("enclave onboarding artifact is non-canonical: {code:#06x}")]
    OnboardingArtifactNonCanonical { code: u16 },
    #[error("bad attestation key: {0}")]
    BadAttestationKey(ed25519_dalek::SignatureError),
    #[error("bad tag length {0}")]
    BadTagLength(usize),
    #[error("signature invalid: {0}")]
    SignatureInvalid(ed25519_dalek::SignatureError),
    #[error("{0:?}")]
    NoiseParams(#[from] snow::Error),
    #[error("quote parse: {0}")]
    QuoteParse(#[from] super::QuoteParseError),
    #[error("{reason}: {detail}")]
    ReconnectIdentityMismatch {
        reason: &'static str,
        detail: String,
    },
}

impl From<TeeClientVerificationError> for TransportError {
    fn from(error: TeeClientVerificationError) -> Self {
        match error {
            TeeClientVerificationError::StreamMissingCallContext
            | TeeClientVerificationError::EnclaveSessionNotConfigured
            | TeeClientVerificationError::TeeSidecarUnavailable
            | TeeClientVerificationError::InvalidTributeDayPredecessorVersion
            | TeeClientVerificationError::GramineDirectDevOnboardingArtifactNonCanonical {
                ..
            } => Self::EnclaveError(error.to_string()),
            TeeClientVerificationError::TrailingEnclaveCallBytes
            | TeeClientVerificationError::TrailingLegacyRequestBytes => {
                Self::Codec(error.to_string())
            }
            TeeClientVerificationError::TeeSidecarNotConfigured => {
                Self::Unavailable(error.to_string())
            }
            TeeClientVerificationError::SystemTimePrecedesUnixEpoch => {
                Self::Handshake(error.to_string())
            }
            TeeClientVerificationError::NoiseParams(_) => Self::Noise(error.to_string()),
            TeeClientVerificationError::QuoteParse(_) => Self::Attestation(error.to_string()),
            TeeClientVerificationError::ReconnectIdentityMismatch { .. } => {
                Self::IdentityMismatch(error.to_string())
            }
            _ => Self::DcapVerification(error.to_string()),
        }
    }
}

/// Errors returned during NodeHost filesystem and record encoding/decoding.
#[derive(Debug, Error)]
pub enum NodeHostRecordError {
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
            NodeHostRecordError::MissingKeyReadyProof => Self::Attestation(error.to_string()),
            _ => Self::Codec(error.to_string()),
        }
    }
}
