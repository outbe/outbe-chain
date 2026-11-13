//! Typed errors for enclave domain cryptography and computation.
pub mod compute;
pub use compute::{
    ComputeError, FidelityEnclaveError, GratisRejectReason, NodMineRejectReason, PayloadError,
    ProcessError, PromisRejectReason, TributeDayRejectReason,
};
use std::fmt;
pub type Result<T> = core::result::Result<T, TeeError>;
pub type EnclaveResult<T> = core::result::Result<T, EnclaveError>;

/// Typed DKG ceremony and dispatch errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DkgError {
    #[error("participant count exceeds u32")]
    ParticipantCountOverflow,
    #[error("empty participant set")]
    EmptyParticipantSet,
    #[error("dealer log failed verification")]
    DealerLogVerificationFailed,
    #[error("incomplete dealer set: {actual} of {expected} committee dealers")]
    IncompleteDealerSet { actual: usize, expected: usize },
    #[error("ceremony {0} already open")]
    CeremonyAlreadyOpen(String),
    #[error("DkgOpen: enc key missing for participant")]
    MissingParticipantEncKey,
    #[error("DKG announcement requires initialized state")]
    AnnouncementRequiresInitializedState,
}

impl From<DkgError> for TeeError {
    fn from(err: DkgError) -> Self {
        Self::Dkg(err.to_string())
    }
}

/// Typed DKG seam-order errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DkgSeamOrderError {
    #[error("start_dealer called twice")]
    StartDealerCalledTwice,
    #[error("player already finalized")]
    PlayerAlreadyFinalized,
}

impl DkgSeamOrderError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StartDealerCalledTwice => "start_dealer called twice",
            Self::PlayerAlreadyFinalized => "player already finalized",
        }
    }
}

impl From<DkgSeamOrderError> for TeeError {
    fn from(err: DkgSeamOrderError) -> Self {
        Self::DkgSeamOrder(err.as_str())
    }
}

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

    /// Attestation preimage JSON did not serialize. The source stays structured.
    #[error("attestation preimage serialization failed: {0}")]
    AttestationPreimageSerialization(#[from] serde_json::Error),
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

    /// Wrap a prefixed debuggable error as a `TeeError::Dkg` variant.
    pub fn dkg_debug_context(prefix: &'static str, err: impl fmt::Debug) -> Self {
        Self::Dkg(format!("{prefix}: {err:?}"))
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
