//! Typed errors for enclave domain cryptography and computation.
pub mod compute;
pub use compute::{
    ComputeError, FidelityEnclaveError, GratisRejectReason, NodMineRejectReason, PayloadError,
    ProcessError, PromisRejectReason, TributeDayRejectReason,
};
pub type Result<T> = core::result::Result<T, TeeError>;
pub type EnclaveResult<T> = core::result::Result<T, EnclaveError>;

/// Error from enclave domain cryptography.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TeeError {
    #[error("{}", resident_error_message(.0))]
    Resident(#[from] outbe_tee::resident::TeeCryptoError),
    /// The nonce size is not 12 bytes.
    #[error("invalid nonce size: expected 12 bytes, got {0}")]
    InvalidNonce(usize),

    /// AEAD encryption failed.
    #[error("AEAD encryption failed")]
    EncryptFailed,

    /// AEAD decryption failed because of a bad key, nonce, or ciphertext.
    #[error("AEAD decryption failed (bad key, nonce, or tampered ciphertext)")]
    DecryptFailed,

    /// HKDF-SHA256 key derivation failed.
    #[error("HKDF-SHA256 derivation failed")]
    HkdfFailed,

    /// A fixed onboarding test fixture names a different offer key.
    #[cfg(any(test, feature = "dev-keys"))]
    #[error("DKG session error: onboarding context offer public does not match resident key")]
    OnboardingFixtureOfferKeyMismatch,

    /// An offer-level check rejected the input.
    #[error("offer rejected: {0}")]
    TributeOfferReject(String),

    /// A Fidelity operation failed.
    #[error("fidelity: {0}")]
    Fidelity(#[from] FidelityEnclaveError),

    /// Attestation preimage JSON did not serialize. The source stays structured.
    #[error("attestation preimage serialization failed: {0}")]
    AttestationPreimageSerialization(#[from] serde_json::Error),
}

// Resident causes stay typed while existing signed rejection text stays byte-identical.
fn resident_error_message(error: &outbe_tee::resident::TeeCryptoError) -> &'static str {
    use outbe_tee::{errors::TeeCryptoError as Primitive, resident::TeeCryptoError as Resident};
    match error {
        Resident::InvalidEnvelope
        | Resident::Primitive(Primitive::InvalidEnvelope | Primitive::DecryptionFailed) => {
            "AEAD decryption failed (bad key, nonce, or tampered ciphertext)"
        }
        Resident::Primitive(Primitive::HkdfExpand | Primitive::HkdfFill) => {
            "HKDF-SHA256 derivation failed"
        }
        Resident::Primitive(Primitive::AeadSeal) => "AEAD encryption failed",
        _ => "resident cryptographic operation failed",
    }
}

/// Typed error for pure enclave domain operations.
#[derive(Debug, thiserror::Error)]
pub enum EnclaveError {
    #[error(transparent)]
    Compute(#[from] ComputeError),
    #[error(transparent)]
    Payload(#[from] PayloadError),
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Tee(#[from] TeeError),
}

impl EnclaveError {
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }
}
impl PartialEq<&str> for EnclaveError {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &&str) -> bool {
        self.to_string().as_str() == *other
    }
}

impl PartialEq<str> for EnclaveError {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &str) -> bool {
        self.to_string().as_str() == other
    }
}

impl From<EnclaveError> for String {
    fn from(err: EnclaveError) -> Self {
        err.to_string()
    }
}
