//! Typed errors for host-side enclave clients, offer encryption, and owner-local decryption.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::PrecompileError;
use outbe_tee::{tee_decrypt::V2BalanceDomain, TeeCryptoError, TransportError};
use thiserror::Error;

use super::protocol::EnclaveResponse;

/// Errors returned by [`crate::balance_client::execute_confidential_balance_op`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BalanceClientError {
    /// Request variant is not a Gratis or Promis balance operation.
    #[error("request is not a confidential balance operation")]
    NotConfidentialBalanceOperation,

    /// Enclave sidecar is not configured on this node.
    #[error("tee_sidecar_unavailable")]
    SidecarUnavailable,

    /// Enclave sidecar failed with a node-local or enclave-state error.
    #[error("tee_sidecar_unavailable: {0}")]
    SidecarRequestUnavailable(String),

    /// Enclave request failed with a deterministic protocol error.
    #[error("enclave request failed: {0}")]
    RequestFailed(String),

    /// Gratis attestation signature check failed.
    #[error("tee_gratis_attestation_invalid: {0}")]
    GratisAttestationInvalid(String),

    /// Promis attestation signature check failed.
    #[error("tee_promis_attestation_invalid: {0}")]
    PromisAttestationInvalid(String),

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
    /// Convert a transport failure into the corresponding precompile error.
    pub fn request_failure(error: TransportError) -> PrecompileError {
        if error.is_node_local() || matches!(error, TransportError::EnclaveError(_)) {
            Self::SidecarRequestUnavailable(error.to_string()).into()
        } else {
            Self::RequestFailed(error.to_string()).into()
        }
    }

    /// Convert a Gratis attestation failure into a fatal precompile error.
    pub fn gratis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::GratisAttestationInvalid(error.to_string()).into()
    }

    /// Convert a Promis attestation failure into a fatal precompile error.
    pub fn promis_attestation_invalid(error: TransportError) -> PrecompileError {
        Self::PromisAttestationInvalid(error.to_string()).into()
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
    ProofVerification(String),
}

/// Typed host-client errors for domain NOD and Tribute enclave operations.
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
    #[error("invalid Tribute day predecessor version")]
    InvalidTributeDayPredecessorVersion,

    /// Underlying enclave transport, codec, or attestation error.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

impl From<EnclaveClientError> for TransportError {
    fn from(error: EnclaveClientError) -> Self {
        match error {
            EnclaveClientError::NodMintRejected { reason } => {
                Self::payload_rejected("encrypted NOD mint rejected", "nod_mint_rejected", reason)
            }
            EnclaveClientError::TributeSourceRejected(cause) => {
                Self::payload_rejected("Tribute source rejected", "tribute_source_rejected", cause)
            }
            EnclaveClientError::InvalidTributeDayPredecessorVersion => {
                Self::EnclaveError("invalid Tribute day predecessor version".into())
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

    /// Map a domain-neutral [`TeeCryptoError`] from client-side sealing into [`Self`].
    #[must_use]
    pub const fn from_tee_encrypt(error: TeeCryptoError) -> Self {
        match error {
            TeeCryptoError::RngFailure => Self::OfferEncryptionRngFailure,
            TeeCryptoError::HkdfExpand => Self::HkdfExpand,
            TeeCryptoError::HkdfFill => Self::HkdfFill,
            TeeCryptoError::AeadKey => Self::AeadKey,
            TeeCryptoError::AeadSeal => Self::AeadSeal,
            TeeCryptoError::InvalidEnvelope
            | TeeCryptoError::DecryptionFailed
            | TeeCryptoError::InvalidPlaintextLength { .. } => Self::AeadSeal,
        }
    }

    /// Map a domain-neutral [`TeeCryptoError`] from Fidelity `FID2` opening into [`Self`].
    #[must_use]
    pub const fn from_fidelity_v2(error: TeeCryptoError) -> Self {
        match error {
            TeeCryptoError::InvalidEnvelope => Self::InvalidFidelityCohortEnvelope,
            TeeCryptoError::HkdfExpand => Self::HkdfExpand,
            TeeCryptoError::HkdfFill => Self::HkdfFill,
            TeeCryptoError::AeadKey => Self::InvalidFidelityViewKey,
            TeeCryptoError::DecryptionFailed
            | TeeCryptoError::RngFailure
            | TeeCryptoError::AeadSeal
            | TeeCryptoError::InvalidPlaintextLength { .. } => Self::FidelityCohortDecryptionFailed,
        }
    }

    /// Map a domain-neutral [`TeeCryptoError`] from NOD counter-slot opening into [`Self`].
    #[must_use]
    pub const fn from_nod_counter_slot(error: TeeCryptoError) -> Self {
        match error {
            TeeCryptoError::InvalidEnvelope => Self::InvalidNodEncoding,
            TeeCryptoError::HkdfExpand => Self::HkdfExpand,
            TeeCryptoError::HkdfFill => Self::HkdfFill,
            TeeCryptoError::AeadKey => Self::InvalidNodAmountKey,
            TeeCryptoError::InvalidPlaintextLength { .. } => Self::InvalidNodAmountLength,
            TeeCryptoError::DecryptionFailed
            | TeeCryptoError::RngFailure
            | TeeCryptoError::AeadSeal => Self::NodAmountDecryptionFailed,
        }
    }

    /// Map a domain-neutral [`TeeCryptoError`] from Tribute counter-slot opening into [`Self`].
    #[must_use]
    pub const fn from_tribute_counter_slot(error: TeeCryptoError) -> Self {
        match error {
            TeeCryptoError::InvalidEnvelope => Self::InvalidTributeEncoding,
            TeeCryptoError::HkdfExpand => Self::HkdfExpand,
            TeeCryptoError::HkdfFill => Self::HkdfFill,
            TeeCryptoError::AeadKey => Self::InvalidTributeAmountKey,
            TeeCryptoError::InvalidPlaintextLength { .. } => Self::InvalidTributeAmountLength,
            TeeCryptoError::DecryptionFailed
            | TeeCryptoError::RngFailure
            | TeeCryptoError::AeadSeal => Self::TributeAmountDecryptionFailed,
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

/// Shared descriptor mapping a V2 confidential `U256` balance domain onto [`LocalCryptoError`].
#[derive(Clone, Copy)]
pub(crate) struct V2LedgerSpec {
    domain: V2BalanceDomain<'static>,
    invalid_balance: LocalCryptoError,
    invalid_view_key: LocalCryptoError,
    decryption_failed: LocalCryptoError,
    invalid_length: LocalCryptoError,
}

impl V2LedgerSpec {
    pub(crate) const GRATIS: Self = Self {
        domain: V2BalanceDomain::new(
            b"GRA2",
            b"outbe/gratis/amount-key/v2",
            b"outbe/gratis/amount-nonce/v2",
        ),
        invalid_balance: LocalCryptoError::InvalidGratisBalance,
        invalid_view_key: LocalCryptoError::InvalidGratisViewKey,
        decryption_failed: LocalCryptoError::GratisBalanceDecryptionFailed,
        invalid_length: LocalCryptoError::InvalidGratisAmountLength,
    };

    pub(crate) const PROMIS: Self = Self {
        domain: V2BalanceDomain::new(
            b"PRO2",
            b"outbe/promis/amount-key/v2",
            b"outbe/promis/amount-nonce/v2",
        ),
        invalid_balance: LocalCryptoError::InvalidPromisBalance,
        invalid_view_key: LocalCryptoError::InvalidPromisViewKey,
        decryption_failed: LocalCryptoError::PromisBalanceDecryptionFailed,
        invalid_length: LocalCryptoError::InvalidPromisAmountLength,
    };

    pub(crate) fn open_u256(
        self,
        view_key: &[u8; 32],
        account: Address,
        blob: &[u8],
    ) -> Result<U256, LocalCryptoError> {
        self.domain
            .open_u256(view_key, account, blob)
            .map_err(|err| match err {
                TeeCryptoError::InvalidEnvelope => self.invalid_balance,
                TeeCryptoError::HkdfExpand => LocalCryptoError::HkdfExpand,
                TeeCryptoError::HkdfFill => LocalCryptoError::HkdfFill,
                TeeCryptoError::AeadKey => self.invalid_view_key,
                TeeCryptoError::InvalidPlaintextLength { .. } => self.invalid_length,
                TeeCryptoError::DecryptionFailed
                | TeeCryptoError::RngFailure
                | TeeCryptoError::AeadSeal => self.decryption_failed,
            })
    }
}
