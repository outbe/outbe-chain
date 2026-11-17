//! RAM-only TEE ceremonies and genesis-anchored key admission.
pub mod admission;
mod context;
pub mod dkg;
mod proof_source;
mod runtime_dispatch;
pub use context::InProcessTeeContext;
pub use proof_source::{HardwareProofSourceV1, ProofSourceV1};

#[derive(Debug, thiserror::Error)]
/// Typed in-process rejection; secret-bearing library errors never enter its formatting.
pub enum InProcessError {
    #[error("resident cryptography rejected operation")]
    Crypto(#[from] crate::resident::TeeCryptoError),
    #[error("DKG rejected operation")]
    Dkg(#[from] dkg::DkgError),
    #[error("DKG seam order rejected operation")]
    Seam(#[from] dkg::DkgSeamOrderError),
    #[error("hardware attestation source unavailable")]
    HardwareProofUnavailable,
    #[error("resident state lock poisoned")]
    LockPoisoned,
    #[error("resident offer material unavailable")]
    NotReady,
    #[error("resident identity or onboarding context mismatch")]
    IdentityMismatch,
    #[error("invalid encrypted envelope")]
    InvalidEnvelope,
    #[error("canonical identity codec rejected input")]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("finalized admission rejected")]
    Admission(#[from] admission::AdmissionError),
    #[error("DCAP rejected registration: {0:?}")]
    Dcap(crate::dcap_protocol::DcapRejectCodeV1),
    #[error("attestation preimage rejected")]
    Attestation(#[from] serde_json::Error),
    #[error("resident key already active or admission anchor regressed")]
    ActivationConflict,
}
/// Result of a resident operation, preserving typed rejection causes.
pub type Result<T> = std::result::Result<T, InProcessError>;

#[cfg(test)]
mod test_support;
