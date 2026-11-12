//! Typed error definitions for `outbe-tee-enclave`.
//!
//! Every fallible operation in the cryptographic and sealing core returns a `TeeError`.
//! Higher-level enclave modules return `EnclaveError` or domain error enums.

pub mod attestation;
pub use outbe_enclave::errors::compute;
pub mod init;
pub mod transport;

pub use attestation::{DcapVerifyError, FinalizedAdmissionError, GramineError};
pub use compute::{
    ComputeError, FidelityEnclaveError, GratisRejectReason, NodMineRejectReason, PayloadError,
    ProcessError, PromisRejectReason, TributeDayRejectReason,
};
pub use init::{InitError, OnboardingUploadError, RunError, SgxSealingError};
pub use transport::{
    transport_handshake, transport_noise, transport_noise_debug, RemoteSessionHandshakeError,
    TransportDispatchError,
};

/// Result alias for enclave cryptographic, sealing, and DKG operations.
pub use outbe_enclave::errors::{DkgError, DkgSeamOrderError, Result, TeeError};

/// Result alias for enclave domain, initialization, attestation, and dispatch operations.
pub type EnclaveResult<T> = core::result::Result<T, EnclaveError>;

/// Unified typed error for enclave modules outside the `TeeError` boundary.
#[derive(Debug, thiserror::Error)]
pub enum EnclaveError {
    /// Amount parsing, nominal calculation, or token ID error.
    #[error(transparent)]
    Compute(#[from] ComputeError),
    /// Decrypted Tribute JSON payload error.
    #[error(transparent)]
    Payload(#[from] PayloadError),
    /// Tribute offer processing or ZK claim derivation error.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// DCAP quote verification or onboarding artifact creation error.
    #[error(transparent)]
    DcapVerify(#[from] DcapVerifyError),
    /// Finalized consensus admission or Registry proof verification error.
    #[error(transparent)]
    FinalizedAdmission(#[from] FinalizedAdmissionError),
    /// Gramine pseudo-filesystem or enclave key derivation error.
    #[error(transparent)]
    Gramine(#[from] GramineError),
    /// Enclave initialization or remote session admission error.
    #[error(transparent)]
    Init(#[from] InitError),
    /// Streaming onboarding artifact upload error.
    #[error(transparent)]
    OnboardingUpload(#[from] OnboardingUploadError),
    /// Combined SGX sealing or hardware probe error.
    #[error(transparent)]
    SgxSealing(#[from] SgxSealingError),
    /// Enclave startup or CLI argument error.
    #[error(transparent)]
    Run(#[from] RunError),
    /// Transport session, offer key, or dispatch error.
    #[error(transparent)]
    TransportDispatch(#[from] TransportDispatchError),
    /// Underlying `TeeError` from cryptographic or sealing primitives.
    #[error(transparent)]
    Tee(#[from] TeeError),
}

impl EnclaveError {
    /// Check whether the formatted error message contains `pattern`.
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }

    /// Return a static string representation for session liveness checks.
    pub const fn as_static_str(&self) -> &'static str {
        match self {
            Self::Init(err) => err.as_static_str(),
            _ => "initialization state unavailable",
        }
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

impl From<outbe_enclave::errors::EnclaveError> for EnclaveError {
    fn from(error: outbe_enclave::errors::EnclaveError) -> Self {
        use outbe_enclave::errors::EnclaveError as DomainError;
        match error {
            DomainError::Compute(error) => Self::Compute(error),
            DomainError::Payload(error) => Self::Payload(error),
            DomainError::Process(error) => Self::Process(error),
            DomainError::Tee(error) => Self::Tee(error),
        }
    }
}
