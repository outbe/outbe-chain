//! Typed errors for host-side enclave clients, offer encryption, and owner-local decryption.

use outbe_primitives::error::PrecompileError;
use outbe_tee::{AppErrorDescriptor, TeeAttestationError, TransportError};
use thiserror::Error;

use super::protocol::EnclaveResponse;

/// Tribute offer, read, day, or NOD attestation failure descriptor.
pub const TRIBUTE_OFFER_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "tribute_offer_attestation",
    "offer attestation signature invalid",
);

/// Gratis ledger operation attestation failure descriptor.
pub const GRATIS_OP_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "gratis_op_attestation",
    "gratis-op attestation signature invalid",
);

/// Promis ledger operation attestation failure descriptor.
pub const PROMIS_OP_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "promis_op_attestation",
    "promis-op attestation signature invalid",
);

/// Fidelity operation attestation failure descriptor.
pub const FIDELITY_ATTESTATION: AppErrorDescriptor = AppErrorDescriptor::new(
    "fidelity_attestation",
    "fidelity attestation signature invalid",
);

/// Tribute state-source proof or context rejection descriptor.
pub const TRIBUTE_SOURCE_REJECTED: AppErrorDescriptor =
    AppErrorDescriptor::new("tribute_source_rejected", "Tribute source rejected");

/// Encrypted NOD mint business rejection descriptor.
pub const NOD_MINT_REJECTED: AppErrorDescriptor =
    AppErrorDescriptor::new("nod_mint_rejected", "encrypted NOD mint rejected");

/// Errors returned by [`crate::balance_client::execute_confidential_balance_op`].
#[derive(Debug, Error)]
pub enum BalanceClientError {
    /// Request variant is not a Gratis or Promis balance operation.
    #[error("request is not a confidential balance operation")]
    NotConfidentialBalanceOperation,

    /// Enclave sidecar is not configured on this node.
    #[error("tee_sidecar_unavailable")]
    SidecarUnavailable,

    /// Enclave sidecar failed with a node-local or enclave-state error.
    #[error("tee_sidecar_unavailable: {0}")]
    SidecarRequestUnavailable(#[source] TransportError),

    /// Enclave request failed with a deterministic protocol error.
    #[error("enclave request failed: {0}")]
    RequestFailed(#[source] TransportError),

    /// Gratis attestation signature check failed.
    #[error("tee_gratis_attestation_invalid: {0}")]
    GratisAttestationInvalid(#[source] TransportError),

    /// Promis attestation signature check failed.
    #[error("tee_promis_attestation_invalid: {0}")]
    PromisAttestationInvalid(#[source] TransportError),

    /// Enclave returned an explicit operation error.
    #[error("enclave {operation} error: {message}")]
    EnclaveOperationError {
        /// Name of the balance operation.
        operation: &'static str,
        /// Error message from the enclave.
        message: String,
    },

    /// Enclave returned a response variant that does not match the request.
    #[error("unexpected enclave response: {0:?}")]
    UnexpectedResponse(Box<EnclaveResponse>),

    /// Enclave echoed a canonical input hash that differs from the request hash.
    #[error("tee_enclave_nondeterminism")]
    EnclaveNondeterminism,
}

impl BalanceClientError {
    /// Classify a transport failure while preserving the structured [`TransportError`] cause.
    #[must_use]
    pub fn from_request_failure(error: TransportError) -> Self {
        if error.is_node_local() || matches!(error, TransportError::EnclaveError(_)) {
            Self::SidecarRequestUnavailable(error)
        } else {
            Self::RequestFailed(error)
        }
    }

    /// Convert a transport failure into the corresponding precompile error.
    pub fn request_failure(error: TransportError) -> PrecompileError {
        Self::from_request_failure(error).into()
    }

    /// Convert a Gratis attestation failure into a fatal precompile error.
    pub fn gratis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::GratisAttestationInvalid(error).into()
    }

    /// Convert a Promis attestation failure into a fatal precompile error.
    pub fn promis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::PromisAttestationInvalid(error).into()
    }

    /// Convert an enclave operation error response into an unavailable precompile error.
    pub fn enclave_operation_error(operation: &'static str, message: &str) -> PrecompileError {
        Self::EnclaveOperationError {
            operation,
            message: message.to_owned(),
        }
        .into()
    }

    /// Convert an unexpected enclave response into a fatal precompile error.
    pub fn unexpected_response(response: &EnclaveResponse) -> PrecompileError {
        Self::UnexpectedResponse(Box::new(response.clone())).into()
    }
}

impl From<BalanceClientError> for PrecompileError {
    fn from(error: BalanceClientError) -> Self {
        match error {
            BalanceClientError::SidecarUnavailable
            | BalanceClientError::SidecarRequestUnavailable(_)
            | BalanceClientError::EnclaveOperationError { .. }
            | BalanceClientError::RequestFailed(_)
            | BalanceClientError::GratisAttestationInvalid(_)
            | BalanceClientError::PromisAttestationInvalid(_)
            | BalanceClientError::UnexpectedResponse(_)
            | BalanceClientError::EnclaveNondeterminism => {
                Self::EnclaveUnavailable(error.to_string())
            }
            BalanceClientError::NotConfidentialBalanceOperation => Self::Revert(error.to_string()),
        }
    }
}

/// Typed rejection causes for [`crate::tribute_source::TributeStateSource::verify`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum TributeSourceError {
    /// Checkpoint or Tribute context identity failed structural validation.
    #[error("Tribute source context mismatch")]
    ContextMismatch,

    /// Expected storage slot list is empty, unsorted, oversized, or misaligned with proof slots.
    #[error("Tribute source slot set mismatch")]
    SlotSetMismatch,

    /// Source binding hash differed from the node's expected authority binding.
    #[error("Tribute source does not match node authority")]
    AuthorityMismatch,

    /// Proven storage word differed from the expected word value.
    #[error("Tribute source word mismatch")]
    WordMismatch,

    /// Underlying MPT account/storage proof verification failed.
    #[error("{0}")]
    ProofVerification(#[from] outbe_primitives::storage::proof::StateProofError),
}

/// Typed host-client errors for domain enclave operations.
#[derive(Debug, Error)]
pub enum EnclaveClientError {
    /// Enclave deterministically rejected an encrypted NOD mint operation.
    #[error("encrypted NOD mint rejected: {reason}")]
    NodMintRejected {
        /// Enclave-signed rejection reason.
        reason: String,
    },

    /// Tribute state source verification rejected the supplied opening.
    #[error("Tribute source rejected: {0}")]
    TributeSourceRejected(#[from] TributeSourceError),

    /// Tribute day or league predecessor record carried an invalid/overflowing version.
    #[error("invalid Tribute day predecessor version: expected {expected}, got {actual}")]
    InvalidTributeDayPredecessorVersion {
        /// Minimum expected predecessor version.
        expected: u8,
        /// Actual predecessor version byte observed.
        actual: u8,
    },

    /// Tribute offer or Tribute V2 attestation signature check failed.
    #[error("offer attestation signature invalid: {0}")]
    TributeOfferAttestation(#[source] TeeAttestationError),

    /// Tribute day or league attestation signature check failed.
    #[error("tribute-day attestation signature invalid: {0}")]
    TributeDayAttestation(#[source] TeeAttestationError),

    /// Gratis ledger operation attestation signature check failed.
    #[error("gratis-op attestation signature invalid: {0}")]
    GratisOpAttestation(#[source] TeeAttestationError),

    /// Promis ledger operation attestation signature check failed.
    #[error("promis-op attestation signature invalid: {0}")]
    PromisOpAttestation(#[source] TeeAttestationError),

    /// Fidelity operation attestation signature check failed.
    #[error("fidelity attestation signature invalid: {0}")]
    FidelityAttestation(#[source] TeeAttestationError),

    /// NOD operation attestation signature check failed.
    #[error("nod attestation signature invalid: {0}")]
    NodAttestation(#[source] TeeAttestationError),

    /// Underlying enclave transport or codec error.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl From<EnclaveClientError> for TransportError {
    fn from(error: EnclaveClientError) -> Self {
        match error {
            EnclaveClientError::NodMintRejected { reason } => {
                Self::app_rejected(NOD_MINT_REJECTED, reason)
            }
            EnclaveClientError::TributeSourceRejected(cause) => {
                Self::app_rejected(TRIBUTE_SOURCE_REJECTED, cause)
            }
            EnclaveClientError::InvalidTributeDayPredecessorVersion { expected, actual } => {
                Self::EnclaveError(format!(
                    "invalid Tribute day predecessor version: expected {expected}, got {actual}"
                ))
            }
            EnclaveClientError::TributeOfferAttestation(cause)
            | EnclaveClientError::TributeDayAttestation(cause)
            | EnclaveClientError::NodAttestation(cause) => {
                Self::app_attestation(TRIBUTE_OFFER_ATTESTATION, cause)
            }
            EnclaveClientError::GratisOpAttestation(cause) => {
                Self::app_attestation(GRATIS_OP_ATTESTATION, cause)
            }
            EnclaveClientError::PromisOpAttestation(cause) => {
                Self::app_attestation(PROMIS_OP_ATTESTATION, cause)
            }
            EnclaveClientError::FidelityAttestation(cause) => {
                Self::app_attestation(FIDELITY_ATTESTATION, cause)
            }
            EnclaveClientError::Transport(err) => err,
        }
    }
}

/// Errors returned by local encryption and owner-local opening helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum LocalCryptoError {
    /// System random number generator failed during offer encryption.
    #[error("offer encryption rng failure")]
    OfferEncryptionRngFailure,

    /// HKDF-SHA256 expand step failed.
    #[error("hkdf expand")]
    HkdfExpand,

    /// HKDF-SHA256 output fill step failed.
    #[error("hkdf fill")]
    HkdfFill,

    /// ChaCha20Poly1305 key initialization failed.
    #[error("aead key")]
    AeadKey,

    /// ChaCha20Poly1305 in-place sealing failed.
    #[error("aead seal")]
    AeadSeal,

    /// Encrypted Fidelity cohort blob failed envelope validation.
    #[error("invalid encrypted Fidelity cohort envelope")]
    InvalidFidelityCohortEnvelope,

    /// Fidelity view key failed AEAD key initialization.
    #[error("invalid Fidelity view key")]
    InvalidFidelityViewKey,

    /// Fidelity cohort ciphertext failed AEAD tag verification.
    #[error("Fidelity cohort decryption failed")]
    FidelityCohortDecryptionFailed,

    /// Encrypted Gratis balance blob failed envelope validation.
    #[error("invalid encrypted Gratis balance")]
    InvalidGratisBalance,

    /// Gratis view key failed AEAD key initialization.
    #[error("invalid Gratis view key")]
    InvalidGratisViewKey,

    /// Gratis balance ciphertext failed AEAD tag verification.
    #[error("Gratis balance decryption failed")]
    GratisBalanceDecryptionFailed,

    /// Opened Gratis plaintext did not have the 32-byte U256 length.
    #[error("invalid Gratis amount length")]
    InvalidGratisAmountLength,

    /// Encrypted Promis balance blob failed envelope validation.
    #[error("invalid encrypted Promis balance")]
    InvalidPromisBalance,

    /// Promis view key failed AEAD key initialization.
    #[error("invalid Promis view key")]
    InvalidPromisViewKey,

    /// Promis balance ciphertext failed AEAD tag verification.
    #[error("Promis balance decryption failed")]
    PromisBalanceDecryptionFailed,

    /// Opened Promis plaintext did not have the 32-byte U256 length.
    #[error("invalid Promis amount length")]
    InvalidPromisAmountLength,

    /// Encrypted NOD record failed structural encoding validation.
    #[error("invalid encrypted NOD encoding")]
    InvalidNodEncoding,

    /// Derived NOD amount key failed AEAD key initialization.
    #[error("invalid NOD amount key")]
    InvalidNodAmountKey,

    /// Encrypted NOD amount failed AEAD tag verification.
    #[error("NOD amount decryption failed")]
    NodAmountDecryptionFailed,

    /// Opened NOD plaintext did not have the 32-byte U256 length.
    #[error("invalid NOD amount length")]
    InvalidNodAmountLength,

    /// Encrypted Tribute record failed structural encoding validation.
    #[error("invalid encrypted Tribute encoding")]
    InvalidTributeEncoding,

    /// Derived Tribute amount key failed AEAD key initialization.
    #[error("invalid Tribute amount key")]
    InvalidTributeAmountKey,

    /// Encrypted Tribute amounts failed AEAD tag verification.
    #[error("Tribute amount decryption failed")]
    TributeAmountDecryptionFailed,

    /// Opened Tribute plaintext did not have the 64-byte amounts length.
    #[error("invalid Tribute amount length")]
    InvalidTributeAmountLength,
}

impl LocalCryptoError {
    /// Return the static error message for this variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OfferEncryptionRngFailure => "offer encryption rng failure",
            Self::HkdfExpand => "hkdf expand",
            Self::HkdfFill => "hkdf fill",
            Self::AeadKey => "aead key",
            Self::AeadSeal => "aead seal",
            Self::InvalidFidelityCohortEnvelope => "invalid encrypted Fidelity cohort envelope",
            Self::InvalidFidelityViewKey => "invalid Fidelity view key",
            Self::FidelityCohortDecryptionFailed => "Fidelity cohort decryption failed",
            Self::InvalidGratisBalance => "invalid encrypted Gratis balance",
            Self::InvalidGratisViewKey => "invalid Gratis view key",
            Self::GratisBalanceDecryptionFailed => "Gratis balance decryption failed",
            Self::InvalidGratisAmountLength => "invalid Gratis amount length",
            Self::InvalidPromisBalance => "invalid encrypted Promis balance",
            Self::InvalidPromisViewKey => "invalid Promis view key",
            Self::PromisBalanceDecryptionFailed => "Promis balance decryption failed",
            Self::InvalidPromisAmountLength => "invalid Promis amount length",
            Self::InvalidNodEncoding => "invalid encrypted NOD encoding",
            Self::InvalidNodAmountKey => "invalid NOD amount key",
            Self::NodAmountDecryptionFailed => "NOD amount decryption failed",
            Self::InvalidNodAmountLength => "invalid NOD amount length",
            Self::InvalidTributeEncoding => "invalid encrypted Tribute encoding",
            Self::InvalidTributeAmountKey => "invalid Tribute amount key",
            Self::TributeAmountDecryptionFailed => "Tribute amount decryption failed",
            Self::InvalidTributeAmountLength => "invalid Tribute amount length",
        }
    }
}

impl std::ops::Deref for LocalCryptoError {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for LocalCryptoError {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq<&str> for LocalCryptoError {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<str> for LocalCryptoError {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<String> for LocalCryptoError {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl From<LocalCryptoError> for &'static str {
    fn from(error: LocalCryptoError) -> Self {
        error.as_str()
    }
}

impl From<LocalCryptoError> for String {
    fn from(error: LocalCryptoError) -> Self {
        error.as_str().to_owned()
    }
}
