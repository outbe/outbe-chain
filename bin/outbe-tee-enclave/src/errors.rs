//! Typed error definitions for `outbe-tee-enclave`.
//!
//! Every fallible operation in the cryptographic and sealing core returns a `TeeError`.
//! Higher-level enclave modules return `EnclaveError` or domain error enums.

pub mod attestation;
pub mod compute;
pub mod init;
pub mod transport;

use std::fmt;

pub use attestation::{DcapVerifyError, FinalizedAdmissionError, GramineError};
pub use compute::{
    ComputeError, GratisRejectReason, PayloadError, ProcessError, PromisRejectReason,
};
pub use init::{InitError, OnboardingUploadError, RunError, SgxSealingError};
pub use transport::{
    transport_handshake, transport_noise, transport_noise_debug, TransportDispatchError,
};

/// Result alias for enclave cryptographic, sealing, and DKG operations.
pub type Result<T> = core::result::Result<T, TeeError>;

/// Result alias for enclave domain, initialization, attestation, and dispatch operations.
pub type EnclaveResult<T> = core::result::Result<T, EnclaveError>;

/// Error from enclave cryptographic, sealing, or DKG operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TeeError {
    /// The nonce size is not 12 bytes.
    #[error("invalid nonce size: expected 12 bytes, got {0}")]
    InvalidNonce(usize),

    /// The key material length is invalid.
    #[error("invalid key material length")]
    InvalidKeyLen,

    /// AEAD encryption failed.
    #[error("AEAD encryption failed")]
    EncryptFailed,

    /// AEAD decryption failed because of a bad key, nonce, or ciphertext.
    #[error("AEAD decryption failed (bad key, nonce, or tampered ciphertext)")]
    DecryptFailed,

    /// HKDF-SHA256 key derivation failed.
    #[error("HKDF-SHA256 derivation failed")]
    HkdfFailed,

    /// The sealed blob is shorter than the minimum header length.
    #[error("sealed blob too short")]
    SealedBlobTooShort,

    /// The sealed blob magic prefix is invalid.
    #[error("sealed blob bad magic")]
    SealedBlobBadMagic,

    /// The sealed blob format version is not supported.
    #[error("sealed blob unsupported format version: {0}")]
    SealedBlobBadVersion(u8),

    /// The sealed blob key policy byte is unknown.
    #[error("sealed blob unknown key policy: {0}")]
    SealedBlobBadKeyPolicy(u8),

    /// The sealed blob SVN exceeds the running enclave SVN.
    #[error("sealed blob anti-rollback: blob isv_svn {blob} > running {running}")]
    SealedBlobRollback { blob: u16, running: u16 },

    /// Sealed blob AEAD decryption failed.
    #[error("sealed blob unseal failed (bad sealing key or tampered blob)")]
    SealedBlobUnsealFailed,

    /// The sealed blob contains an invalid network binding.
    #[error("sealed blob contains an invalid network binding: {0}")]
    SealedBlobBadNetworkBinding(String),

    /// The sealed blob belongs to another network binding.
    #[error("sealed blob belongs to another network binding")]
    SealedBlobNetworkBindingMismatch,

    /// The sealed blob decrypted payload has an unexpected length.
    #[error("sealed blob payload had unexpected length: {0}")]
    SealedBlobBadPayload(usize),

    /// A DKG session operation failed.
    #[error("DKG session error: {0}")]
    Dkg(String),

    /// The requested DKG ceremony ID is not present in the session store.
    #[error("DKG ceremony {0} not found in session store")]
    DkgSessionMissing(String),

    /// A DKG seam was called out of order.
    #[error("DKG seam called out of order: {0}")]
    DkgSeamOrder(&'static str),

    /// An offer-level check rejected the input.
    #[error("offer rejected: {0}")]
    TributeOfferReject(String),

    /// A Fidelity operation failed.
    #[error("fidelity: {0}")]
    Fidelity(String),
}

impl TeeError {
    /// Wrap a network binding encoding or decoding error.
    pub fn sealed_blob_bad_network_binding(err: impl fmt::Display) -> Self {
        Self::SealedBlobBadNetworkBinding(err.to_string())
    }

    /// Wrap a displayable error as a `TeeError::Dkg` variant.
    pub fn dkg_err(err: impl fmt::Display) -> Self {
        Self::Dkg(err.to_string())
    }

    /// Wrap a prefixed displayable error as a `TeeError::Dkg` variant.
    pub fn dkg_context(prefix: &'static str, err: impl fmt::Display) -> Self {
        Self::Dkg(format!("{prefix}: {err}"))
    }

    /// Wrap a displayable error as a `TeeError::TributeOfferReject` variant.
    pub fn tribute_offer_reject(err: impl fmt::Display) -> Self {
        Self::TributeOfferReject(err.to_string())
    }
}

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
